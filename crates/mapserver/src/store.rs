//! What the server keeps: each book's maps and its default, the maps
//! themselves, and the queue of maps to build.
//!
//! Maps are files, `maps/<id>.toml`, so they can be read, copied, and
//! shared as they are. Everything else is one small JSON file, rewritten
//! whole on every change. A family's library is hundreds of books, not
//! millions, and a rewrite is what keeps a crash from leaving it half
//! written.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use underscore_map::Map;

/// A worker that has said nothing for this long has stopped; its job goes
/// back on the queue.
pub const STALE_SECS: u64 = 20 * 60;

/// One book's maps, by the Audiobookshelf item id.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookMaps {
    /// Map ids, oldest first.
    #[serde(default)]
    pub maps: Vec<String>,
    /// The map a phone opens the book with, without asking.
    #[serde(default)]
    pub default: Option<String>,
}

/// Where a job is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Queued,
    Running,
    Done,
    Failed,
}

/// What a job makes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A music map. Voices come along with it.
    #[default]
    Map,
    /// Only who says what, for a book that has its map already.
    Voices,
}

/// Something being made for a book.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    /// The Audiobookshelf item id.
    pub book: String,
    #[serde(default)]
    pub kind: Kind,
    /// The id the map gets, chosen when the job is made. Empty for voices.
    pub map: String,
    pub state: State,
    /// The chapter the listener is in, from 1. The worker starts there.
    pub chapter: u32,
    /// Chapters with cues so far.
    #[serde(default)]
    pub done: Vec<u32>,
    /// Chapters in the book, once the worker knows.
    #[serde(default)]
    pub total: u32,
    /// What the worker is doing, or why it failed.
    #[serde(default)]
    pub message: String,
    /// Seconds since 1970.
    pub created: u64,
    pub updated: u64,
}

impl Job {
    fn active(&self) -> bool {
        matches!(self.state, State::Queued | State::Running)
    }
}

/// What a list of maps says about each one.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MapInfo {
    pub id: String,
    pub name: String,
    pub based_on: Option<String>,
    pub complete: bool,
    pub cues: usize,
    /// When the file last changed, in seconds since 1970. A phone reloads
    /// a map whose time has moved.
    pub updated: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Data {
    #[serde(default)]
    books: BTreeMap<String, BookMaps>,
    #[serde(default)]
    jobs: Vec<Job>,
    /// When a worker last asked for work.
    #[serde(default)]
    worker_seen: Option<u64>,
}

/// Why a request could not be done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    NotFound(String),
    Invalid(String),
    Io(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(e) | Self::Invalid(e) | Self::Io(e) => f.write_str(e),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

pub struct Store {
    dir: PathBuf,
    data: Mutex<Data>,
}

impl Store {
    /// Open the store in `dir`, creating it if need be.
    ///
    /// # Errors
    /// The folder cannot be made, or the state file is not readable JSON.
    pub fn open(dir: &Path) -> Result<Self, Error> {
        fs::create_dir_all(dir.join("maps"))?;
        let data = match fs::read_to_string(dir.join("state.json")) {
            Ok(text) => {
                serde_json::from_str(&text).map_err(|e| Error::Io(format!("state.json: {e}")))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Data::default(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            dir: dir.to_path_buf(),
            data: Mutex::new(data),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Data> {
        self.data
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn save(&self, data: &Data) -> Result<(), Error> {
        let tmp = self.dir.join("state.json.tmp");
        let text = serde_json::to_string_pretty(data).map_err(|e| Error::Io(e.to_string()))?;
        fs::write(&tmp, text)?;
        fs::rename(&tmp, self.dir.join("state.json"))?;
        Ok(())
    }

    fn map_path(&self, id: &str) -> PathBuf {
        self.dir.join("maps").join(format!("{id}.toml"))
    }

    fn transcript_path(&self, book: &str) -> PathBuf {
        self.dir.join("transcripts").join(format!("{book}.json"))
    }

    /// The transcript the worker made for `book`, in the format the phone
    /// imports: `{"chapters": [{"segments": [{"start", "end", "text"}]}]}`.
    ///
    /// # Errors
    /// There is none.
    pub fn transcript(&self, book: &str) -> Result<String, Error> {
        check_id(book)?;
        fs::read_to_string(self.transcript_path(book))
            .map_err(|_| Error::NotFound(format!("no transcript for {book}")))
    }

    /// Keep a transcript for `book`, replacing the last.
    ///
    /// # Errors
    /// It is not JSON, or could not be written.
    pub fn set_transcript(&self, book: &str, json: &str) -> Result<(), Error> {
        check_id(book)?;
        serde_json::from_str::<serde_json::Value>(json)
            .map_err(|e| Error::Invalid(format!("not JSON: {e}")))?;
        fs::create_dir_all(self.dir.join("transcripts"))?;
        let path = self.transcript_path(book);
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, json)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    fn voices_path(&self, book: &str) -> PathBuf {
        self.dir.join("voices").join(format!("{book}.json"))
    }

    /// Who says what in `book`: the transcript's lines split into spans,
    /// each with a speaker, and the cast. See `docs/map-server.md`.
    ///
    /// # Errors
    /// There is none yet.
    pub fn voices(&self, book: &str) -> Result<String, Error> {
        check_id(book)?;
        fs::read_to_string(self.voices_path(book))
            .map_err(|_| Error::NotFound(format!("no voices for {book}")))
    }

    /// Keep who says what for `book`, replacing the last.
    ///
    /// # Errors
    /// It is not JSON, or could not be written.
    pub fn set_voices(&self, book: &str, json: &str) -> Result<(), Error> {
        check_id(book)?;
        serde_json::from_str::<serde_json::Value>(json)
            .map_err(|e| Error::Invalid(format!("not JSON: {e}")))?;
        fs::create_dir_all(self.dir.join("voices"))?;
        let path = self.voices_path(book);
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, json)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// A map's text.
    ///
    /// # Errors
    /// There is no such map.
    pub fn map(&self, id: &str) -> Result<String, Error> {
        check_id(id)?;
        fs::read_to_string(self.map_path(id)).map_err(|_| Error::NotFound(format!("no map {id}")))
    }

    fn info(&self, id: &str) -> Option<MapInfo> {
        let path = self.map_path(id);
        let text = fs::read_to_string(&path).ok()?;
        let map = Map::parse(&text).ok()?;
        let updated = fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        Some(MapInfo {
            id: id.to_string(),
            name: map.name.unwrap_or_else(|| "Untitled map".into()),
            based_on: map.based_on,
            complete: map.complete,
            cues: map.cues.len(),
            updated,
        })
    }

    /// A book's maps, its default, and the job building one, if any.
    #[must_use]
    pub fn book(&self, book: &str) -> serde_json::Value {
        let data = self.lock();
        let entry = data.books.get(book).cloned().unwrap_or_default();
        let maps: Vec<MapInfo> = entry.maps.iter().filter_map(|id| self.info(id)).collect();
        // The newest job of each kind: the one working, or the last to finish or fail.
        let newest = |kind| {
            data.jobs
                .iter()
                .rev()
                .find(|j| j.book == book && j.kind == kind)
        };
        let voices = fs::metadata(self.voices_path(book))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        serde_json::json!({
            "default": entry.default,
            "maps": maps,
            "job": newest(Kind::Map),
            "voices": voices,
            "voices_job": newest(Kind::Voices),
        })
    }

    /// Choose the map a book opens with.
    ///
    /// # Errors
    /// The map is not one of the book's.
    pub fn set_default(&self, book: &str, map: &str) -> Result<(), Error> {
        let mut data = self.lock();
        let entry = data.books.entry(book.to_string()).or_default();
        if !entry.maps.iter().any(|m| m == map) {
            return Err(Error::NotFound(format!("{map} is not a map of {book}")));
        }
        entry.default = Some(map.to_string());
        self.save(&data)
    }

    /// Take a map someone made by hand for `book`. It keeps its id when
    /// it has one, so a better version of a shared map replaces the old.
    /// One without gets a new id.
    ///
    /// # Errors
    /// The text is not a valid map, or its id belongs to another book.
    pub fn add_map(&self, book: &str, text: &str) -> Result<MapInfo, Error> {
        let map = Map::parse(text).map_err(|e| Error::Invalid(e.to_string()))?;
        let mut data = self.lock();
        let (id, text) = if let Some(id) = map.id {
            check_id(&id)?;
            if data
                .books
                .iter()
                .any(|(b, e)| b != book && e.maps.contains(&id))
            {
                return Err(Error::Invalid(format!("map {id} belongs to another book")));
            }
            (id, text.to_string())
        } else {
            let id = new_id()?;
            let text = format!("id = \"{id}\"\n{text}");
            (id, text)
        };
        write_atomic(&self.map_path(&id), &text)?;
        let entry = data.books.entry(book.to_string()).or_default();
        if !entry.maps.contains(&id) {
            entry.maps.push(id.clone());
        }
        if entry.default.is_none() {
            entry.default = Some(id.clone());
        }
        self.save(&data)?;
        drop(data);
        self.info(&id)
            .ok_or_else(|| Error::Io("the map was written but cannot be read".into()))
    }

    /// Ask for a map for `book`, starting at `chapter`. A job already
    /// building one for the book takes the new chapter instead.
    ///
    /// # Errors
    /// The state could not be saved.
    pub fn build(&self, book: &str, chapter: u32) -> Result<Job, Error> {
        self.make(book, chapter, Kind::Map)
    }

    /// Ask for who says what in `book`, starting at `chapter`, for a book
    /// whose map is already made.
    ///
    /// # Errors
    /// The state could not be saved.
    pub fn build_voices(&self, book: &str, chapter: u32) -> Result<Job, Error> {
        self.make(book, chapter, Kind::Voices)
    }

    fn make(&self, book: &str, chapter: u32, kind: Kind) -> Result<Job, Error> {
        let mut data = self.lock();
        let now = now();
        if let Some(job) = data
            .jobs
            .iter_mut()
            .find(|j| j.book == book && j.kind == kind && j.active())
        {
            job.chapter = chapter.max(1);
            let job = job.clone();
            self.save(&data)?;
            return Ok(job);
        }
        let job = Job {
            id: new_id()?,
            book: book.to_string(),
            kind,
            map: if kind == Kind::Map {
                new_id()?
            } else {
                String::new()
            },
            state: State::Queued,
            chapter: chapter.max(1),
            done: Vec::new(),
            total: 0,
            message: "Waiting for the worker".into(),
            created: now,
            updated: now,
        };
        data.jobs.push(job.clone());
        // Keep the last few finished jobs per book, for their messages.
        let finished = data.jobs.iter().filter(|j| !j.active()).count();
        if finished > 200 {
            let mut drop_n = finished - 200;
            data.jobs.retain(|j| {
                if drop_n > 0 && !j.active() {
                    drop_n -= 1;
                    false
                } else {
                    true
                }
            });
        }
        self.save(&data)?;
        Ok(job)
    }

    /// Hand the oldest waiting job to a worker. A running job the worker
    /// has gone quiet on counts as waiting.
    ///
    /// # Errors
    /// The state could not be saved.
    pub fn claim(&self) -> Result<Option<Job>, Error> {
        let mut data = self.lock();
        let now = now();
        data.worker_seen = Some(now);
        let next = data.jobs.iter_mut().find(|j| {
            j.state == State::Queued
                || (j.state == State::Running && now.saturating_sub(j.updated) > STALE_SECS)
        });
        let job = next.map(|j| {
            j.state = State::Running;
            j.updated = now;
            j.message = "Starting".into();
            j.clone()
        });
        self.save(&data)?;
        Ok(job)
    }

    /// When a worker last asked for work, in seconds since 1970.
    #[must_use]
    pub fn worker_seen(&self) -> Option<u64> {
        self.lock().worker_seen
    }

    /// The worker's news on a job: what it is doing and, with `map`, the
    /// map so far, which replaces the last. The map becomes one of the
    /// book's at its first chapter, and its default if it has none.
    /// Returns the job, whose chapter may have moved since the last call.
    ///
    /// # Errors
    /// No such job, or the map is invalid or is not this job's.
    pub fn progress(&self, job: &str, news: &Progress) -> Result<Job, Error> {
        let parsed = match &news.map {
            Some(text) => Some(Map::parse(text).map_err(|e| Error::Invalid(e.to_string()))?),
            None => None,
        };
        let mut data = self.lock();
        let j = data
            .jobs
            .iter_mut()
            .find(|j| j.id == job)
            .ok_or_else(|| Error::NotFound(format!("no job {job}")))?;
        if parsed.is_some() && j.kind != Kind::Map {
            return Err(Error::Invalid("this job makes no map".into()));
        }
        if let Some(map) = &parsed
            && map.id.as_deref() != Some(j.map.as_str())
        {
            return Err(Error::Invalid(format!("the map's id should be {}", j.map)));
        }
        j.updated = now();
        if let Some(m) = &news.message {
            j.message.clone_from(m);
        }
        if let Some(d) = &news.done {
            j.done.clone_from(d);
        }
        if let Some(t) = news.total {
            j.total = t;
        }
        match news.state {
            Some(State::Done) => j.state = State::Done,
            Some(State::Failed) => j.state = State::Failed,
            _ => {}
        }
        let out = j.clone();
        if let Some(text) = &news.map {
            write_atomic(&self.map_path(&out.map), text)?;
            let entry = data.books.entry(out.book.clone()).or_default();
            if !entry.maps.contains(&out.map) {
                entry.maps.push(out.map.clone());
            }
            if entry.default.is_none() {
                entry.default = Some(out.map.clone());
            }
        }
        self.save(&data)?;
        Ok(out)
    }
}

/// What a worker reports.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Progress {
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub done: Option<Vec<u32>>,
    #[serde(default)]
    pub total: Option<u32>,
    /// `done` or `failed` to finish the job.
    #[serde(default)]
    pub state: Option<State>,
    /// The whole map so far.
    #[serde(default)]
    pub map: Option<String>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Ids go into file names, so they are kept to letters, digits, `-` and `_`.
fn check_id(id: &str) -> Result<(), Error> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(Error::Invalid(format!("{id:?} is not a map id")));
    }
    Ok(())
}

/// Sixteen random hex digits.
pub(crate) fn new_id() -> Result<String, Error> {
    let mut bytes = [0u8; 8];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    }))
}

fn write_atomic(path: &Path, text: &str) -> Result<(), Error> {
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!("underscore-maps-{}", new_id().unwrap()));
        (Store::open(&dir).unwrap(), dir)
    }

    const MAP: &str =
        "version = 1\nname = \"By hand\"\n\n[[cue]]\nat = \"0:00\"\nvibe = [\"calm\"]\n";

    #[test]
    fn a_hand_made_map_gets_an_id_and_becomes_the_default() {
        let (s, dir) = store();
        let info = s.add_map("b1", MAP).unwrap();
        assert_eq!(info.name, "By hand");
        assert_eq!(info.cues, 1);
        let book = s.book("b1");
        assert_eq!(book["default"], info.id.as_str());
        assert!(
            s.map(&info.id)
                .unwrap()
                .starts_with(&format!("id = \"{}\"", info.id))
        );
        // Sent again with its id, it replaces itself rather than adding one.
        let again = s.map(&info.id).unwrap().replace("By hand", "Better");
        assert_eq!(s.add_map("b1", &again).unwrap().id, info.id);
        assert_eq!(s.book("b1")["maps"].as_array().unwrap().len(), 1);
        // And no other book can take it.
        assert!(s.add_map("b2", &again).is_err());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn asking_twice_moves_the_job_rather_than_making_another() {
        let (s, dir) = store();
        let a = s.build("b1", 3).unwrap();
        let b = s.build("b1", 9).unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(b.chapter, 9);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_worker_claims_reports_and_finishes() {
        let (s, dir) = store();
        let job = s.build("b1", 2).unwrap();
        let claimed = s.claim().unwrap().unwrap();
        assert_eq!(claimed.id, job.id);
        assert_eq!(claimed.state, State::Running);
        assert!(
            s.claim().unwrap().is_none(),
            "a running job is not handed out twice"
        );

        let wrong = Progress {
            map: Some(MAP.into()),
            ..Progress::default()
        };
        assert!(
            s.progress(&job.id, &wrong).is_err(),
            "a map without the job's id is refused"
        );

        let text = format!("id = \"{}\"\ncomplete = false\n{MAP}", job.map);
        let news = Progress {
            map: Some(text),
            done: Some(vec![2]),
            total: Some(10),
            message: Some("Chapter 3".into()),
            ..Progress::default()
        };
        s.progress(&job.id, &news).unwrap();
        let book = s.book("b1");
        assert_eq!(book["default"], job.map.as_str());
        assert_eq!(book["maps"][0]["complete"], false);
        assert_eq!(book["job"]["done"], serde_json::json!([2]));

        s.build("b1", 7).unwrap();
        let done = Progress {
            state: Some(State::Done),
            ..Progress::default()
        };
        let back = s.progress(&job.id, &done).unwrap();
        assert_eq!(back.chapter, 7, "the worker hears where the listener moved");
        assert_eq!(back.state, State::Done);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn transcripts_are_kept_per_book() {
        let (s, dir) = store();
        assert!(s.transcript("b1").is_err());
        assert!(s.set_transcript("b1", "not json").is_err());
        s.set_transcript("b1", r#"{"chapters": []}"#).unwrap();
        assert_eq!(s.transcript("b1").unwrap(), r#"{"chapters": []}"#);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn voices_jobs_stand_apart_from_map_jobs() {
        let (s, dir) = store();
        let map = s.build("b1", 1).unwrap();
        let voices = s.build_voices("b1", 1).unwrap();
        assert_ne!(map.id, voices.id, "one of each kind may run at once");
        assert_eq!(voices.kind, Kind::Voices);
        assert!(voices.map.is_empty());
        let book = s.book("b1");
        assert_eq!(book["job"]["id"], map.id.as_str());
        assert_eq!(book["voices_job"]["id"], voices.id.as_str());
        assert!(book["voices"].is_null());
        let news = Progress {
            map: Some(MAP.into()),
            ..Progress::default()
        };
        assert!(
            s.progress(&voices.id, &news).is_err(),
            "a voices job sends no map"
        );
        s.set_voices("b1", r#"{"cast": [], "chapters": []}"#)
            .unwrap();
        assert!(s.book("b1")["voices"].is_u64());
        assert!(s.voices("b1").unwrap().contains("cast"));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn state_survives_a_restart() {
        let (s, dir) = store();
        let info = s.add_map("b1", MAP).unwrap();
        drop(s);
        let s = Store::open(&dir).unwrap();
        assert_eq!(s.book("b1")["default"], info.id.as_str());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn ids_cannot_reach_outside_the_folder() {
        let (s, dir) = store();
        assert!(s.map("../state").is_err());
        assert!(s.add_map("b1", "version = 1\nid = \"../x\"\n").is_err());
        assert!(s.set_transcript("../b", "{}").is_err());
        fs::remove_dir_all(dir).ok();
    }
}

#[cfg(test)]
mod generated {
    use std::time::Duration;

    use underscore_map::{Map, Schedule, Timeline};

    /// What the worker writes, as its own test wrote it
    /// (`worker/tests/test_compose.py`), read the way the phone reads it.
    #[test]
    fn generated_maps_parse_and_schedule() {
        let text = include_str!("../../../worker/tests/sample-map.toml");
        let map = Map::parse(text).unwrap();
        assert_eq!(map.id.as_deref(), Some("8f3c0011"));
        assert!(!map.complete);
        let timeline = Timeline::chapters(vec![Duration::ZERO, Duration::from_secs(300)]);
        Schedule::new(&map, &timeline).unwrap();
    }
}
