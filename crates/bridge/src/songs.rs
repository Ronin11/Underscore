//! Scoring the music on this device: every song the library has that has no
//! score yet is fetched, heard, and let go, one at a time. One run at a
//! time, on the caller's thread, until it is done or told to stop.
//!
//! A song fetched only to be heard is deleted again straight after. Only
//! its score stays, in a file per library. See `underscore_library::hearing`.

use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use underscore_library::{Cache, Library, LocalLibrary, Role, SongScores, hearing};

/// Songs in a row that may fail to arrive before the server counts as gone.
const GIVE_UP_AFTER: u32 = 3;
const SAVE_EVERY: Duration = Duration::from_secs(20);

static RUNNING: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);
static STATUS: Mutex<Status> = Mutex::new(Status {
    scored: 0,
    total: 0,
    failed: 0,
    error: None,
    finished: false,
});

/// How the current or last run stands.
#[derive(Debug, Clone)]
pub struct Status {
    /// Songs with a score.
    pub scored: usize,
    /// Songs in the library.
    pub total: usize,
    /// Songs that could not be decoded.
    pub failed: usize,
    /// Why the last run gave up, if it did.
    pub error: Option<String>,
    /// The last run got through every song.
    pub finished: bool,
}

fn lock() -> std::sync::MutexGuard<'static, Status> {
    STATUS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// How scoring stands, as JSON for the app.
#[must_use]
pub fn status() -> serde_json::Value {
    let s = lock().clone();
    serde_json::json!({
        "running": RUNNING.load(Ordering::SeqCst),
        "scored": s.scored,
        "total": s.total,
        "failed": s.failed,
        "error": s.error,
        "finished": s.finished,
    })
}

/// Ask a run to stop after the song it is on.
pub fn stop() {
    STOP.store(true, Ordering::SeqCst);
}

/// A stable order that is not the library's, so a run stopped early has
/// heard a little of everything rather than all of one album.
fn scatter(id: &str) -> u64 {
    id.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// Score the music folder `music_dir`. See [`run`].
///
/// # Errors
/// As for [`run`], or the folder cannot be read.
pub fn run_folder(music_dir: &Path, cache_dir: &Path, scores_dir: &Path) -> Result<(), String> {
    let library = LocalLibrary::new("music", vec![(Role::Music, music_dir.to_path_buf())], None);
    library
        .scan()
        .map_err(|e| format!("{}: {e}", music_dir.display()))?;
    run(&library, cache_dir, scores_dir)
}

/// Score every song in `library` that has no score. Blocks until done or
/// stopped: a thread of its own, at low priority. Slow on a big library,
/// and over a network it downloads each song.
///
/// # Errors
/// Another run is going, the library cannot list its songs, or songs stop
/// arriving. What was scored by then is kept.
pub fn run(library: &dyn Library, cache_dir: &Path, scores_dir: &Path) -> Result<(), String> {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return Err("already scoring".to_string());
    }
    STOP.store(false, Ordering::SeqCst);
    lock().error = None;
    let result = score_all(library, cache_dir, scores_dir);
    if let Err(e) = &result {
        lock().error = Some(e.clone());
    }
    RUNNING.store(false, Ordering::SeqCst);
    result
}

fn score_all(library: &dyn Library, cache_dir: &Path, scores_dir: &Path) -> Result<(), String> {
    let cache = Cache::open(cache_dir).map_err(|e| format!("cache: {e}"))?;
    let mut scores = SongScores::open(scores_dir, library.name());
    let items = library.items(Role::Music).map_err(|e| e.to_string())?;
    let mut todo: Vec<_> = items.iter().filter(|i| !scores.settled(&i.id.0)).collect();
    todo.sort_by_key(|i| scatter(&i.id.0));
    let show = |scores: &SongScores, finished: bool| {
        let mut s = lock();
        s.scored = scores.songs().len();
        s.total = items.len();
        s.failed = scores.failed();
        s.finished = finished;
    };
    show(&scores, todo.is_empty());

    let mut lost = 0;
    let mut saved = Instant::now();
    let mut problem = None;
    let mut stopped = false;
    for item in todo {
        if STOP.load(Ordering::SeqCst) {
            stopped = true;
            break;
        }
        let had = library
            .cache_path(&item.id, &cache)
            .is_some_and(|p| p.is_file());
        let path = match library.fetch(&item.id, &cache) {
            Ok(path) => {
                lost = 0;
                path
            }
            Err(e) => {
                lost += 1;
                if lost >= GIVE_UP_AFTER {
                    problem = Some(e.to_string());
                    stopped = true;
                    break;
                }
                continue;
            }
        };
        // A decoder that panics on a strange file costs that song only.
        let heard = std::panic::catch_unwind(|| hearing::score_file(&path));
        match heard {
            Ok(Ok(axes)) => scores.set(&item.id.0, axes),
            // A file trimmed from the cache meanwhile is tried again later.
            _ if path.is_file() => scores.set_failed(&item.id.0),
            _ => {}
        }
        if !had && library.cache_path(&item.id, &cache).is_some() {
            let _ = std::fs::remove_file(&path);
        }
        show(&scores, false);
        if saved.elapsed() >= SAVE_EVERY {
            scores.save().map_err(|e| format!("song scores: {e}"))?;
            saved = Instant::now();
        }
    }
    scores.save().map_err(|e| format!("song scores: {e}"))?;
    show(&scores, !stopped);
    problem.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use underscore_library::{Error, Item, ItemId, Query};

    /// Runs share one set of statics, so the tests here take turns.
    static TURN: Mutex<()> = Mutex::new(());

    fn turn() -> std::sync::MutexGuard<'static, ()> {
        TURN.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn wav(path: &Path, hz: f32, secs: u32) {
        wav_frames(path, hz, secs * 44_100);
    }

    fn wav_frames(path: &Path, hz: f32, frames: u32) {
        let rate = 44_100u32;
        let samples: Vec<u8> = (0..frames)
            .flat_map(|i| {
                #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
                let s =
                    ((std::f32::consts::TAU * hz * i as f32 / rate as f32).sin() * 12_000.0) as i16;
                s.to_le_bytes()
            })
            .collect();
        let len = u32::try_from(samples.len()).unwrap();
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&samples);
        std::fs::write(path, out).unwrap();
    }

    fn folder(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("underscore-songs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("music")).unwrap();
        dir
    }

    #[test]
    fn a_folder_is_heard_once_and_a_song_too_short_is_not_tried_again() {
        let _turn = turn();
        let dir = folder("once");
        wav(&dir.join("music/one.wav"), 220.0, 3);
        wav(&dir.join("music/two.wav"), 880.0, 3);
        wav_frames(&dir.join("music/blip.wav"), 440.0, 2_000);
        let (music, cache, scores) = (dir.join("music"), dir.join("cache"), dir.join("scores"));

        run_folder(&music, &cache, &scores).unwrap();
        let s = status();
        assert_eq!(s["scored"], 2, "{s}");
        assert_eq!(s["failed"], 1, "{s}");
        assert_eq!(s["finished"], true, "{s}");

        let kept = SongScores::open(&scores, "music");
        assert_eq!(kept.songs().len(), 2);
        assert!(
            kept.songs()
                .values()
                .all(|a| (1.0..=5.0).contains(&a.energy))
        );
        // A music folder's own files stay where they are.
        assert!(music.join("one.wav").is_file());

        run_folder(&music, &cache, &scores).unwrap();
        assert_eq!(status()["scored"], 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A library that fetches into the cache as a music server does.
    struct Fetching {
        items: Vec<Item>,
        source: std::path::PathBuf,
        missing: bool,
        stop_after_first: bool,
    }

    impl Library for Fetching {
        fn name(&self) -> &'static str {
            "fetching"
        }
        fn items(&self, _role: Role) -> Result<Vec<Item>, Error> {
            Ok(self.items.clone())
        }
        fn item(&self, id: &ItemId) -> Result<Item, Error> {
            Err(Error::NotFound(id.clone()))
        }
        fn search(&self, _query: &Query) -> Result<Vec<Item>, Error> {
            Ok(Vec::new())
        }
        fn fetch(&self, id: &ItemId, cache: &Cache) -> Result<std::path::PathBuf, Error> {
            if self.missing {
                return Err(Error::Backend("the server is gone".to_string()));
            }
            if self.stop_after_first {
                stop();
            }
            cache
                .get_or_fill("fetching", id, "wav", |out| {
                    out.write_all(&std::fs::read(&self.source)?)
                })
                .map_err(Error::from)
        }
        fn cache_path(&self, id: &ItemId, cache: &Cache) -> Option<std::path::PathBuf> {
            Some(cache.path_for("fetching", id, "wav"))
        }
        fn progress(&self, _id: &ItemId) -> Result<Option<Duration>, Error> {
            Ok(None)
        }
        fn report_progress(&self, _id: &ItemId, _at: Duration) -> Result<(), Error> {
            Ok(())
        }
    }

    use std::io::Write;

    fn songs(n: usize) -> Vec<Item> {
        (0..n)
            .map(|i| Item {
                id: ItemId(format!("song-{i}")),
                role: Role::Music,
                title: format!("Song {i}"),
                artist: None,
                album: None,
                duration: None,
                tags: Vec::new(),
                chapters: Vec::new(),
                format: Some("wav".to_string()),
                axes: None,
            })
            .collect()
    }

    fn cached_files(cache: &Path) -> usize {
        std::fs::read_dir(cache).map_or(0, |d| d.flatten().filter(|e| e.path().is_file()).count())
    }

    #[test]
    fn a_song_fetched_to_hear_is_let_go_and_one_already_kept_stays() {
        let _turn = turn();
        let dir = folder("fetched");
        let source = dir.join("music/source.wav");
        wav(&source, 440.0, 2);
        let (cache_dir, scores) = (dir.join("cache"), dir.join("scores"));
        let cache = Cache::open(&cache_dir).unwrap();
        // One song is already on the phone for a book.
        let items = songs(3);
        let kept = cache
            .get_or_fill("fetching", &items[0].id, "wav", |out| {
                out.write_all(&std::fs::read(&source)?)
            })
            .unwrap();
        let lib = Fetching {
            items,
            source,
            missing: false,
            stop_after_first: false,
        };
        run(&lib, &cache_dir, &scores).unwrap();
        assert_eq!(status()["scored"], 3);
        assert!(kept.is_file(), "a song a book uses stays");
        assert_eq!(cached_files(&cache_dir), 1, "the rest are let go");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stopped_run_keeps_its_work_and_is_not_finished() {
        let _turn = turn();
        let dir = folder("stopped");
        let source = dir.join("music/source.wav");
        wav(&source, 440.0, 2);
        let (cache, scores) = (dir.join("cache"), dir.join("scores"));
        let lib = Fetching {
            items: songs(4),
            source: source.clone(),
            missing: false,
            stop_after_first: true,
        };
        run(&lib, &cache, &scores).unwrap();
        let s = status();
        assert_eq!(s["scored"], 1, "{s}");
        assert_eq!(s["finished"], false, "{s}");
        assert_eq!(SongScores::open(&scores, "fetching").songs().len(), 1);

        let rest = Fetching {
            items: songs(4),
            source,
            missing: false,
            stop_after_first: false,
        };
        run(&rest, &cache, &scores).unwrap();
        let s = status();
        assert_eq!(s["scored"], 4, "{s}");
        assert_eq!(s["finished"], true, "{s}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_server_that_goes_away_ends_the_run_with_no_song_marked_bad() {
        let _turn = turn();
        let dir = folder("gone");
        let (cache, scores) = (dir.join("cache"), dir.join("scores"));
        let lib = Fetching {
            items: songs(5),
            source: dir.join("nothing"),
            missing: true,
            stop_after_first: false,
        };
        let err = run(&lib, &cache, &scores).unwrap_err();
        assert!(err.contains("gone"), "{err}");
        let s = status();
        assert_eq!(s["failed"], 0, "{s}");
        assert_eq!(s["finished"], false, "{s}");
        assert_eq!(s["running"], false, "{s}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
