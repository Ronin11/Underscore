//! Transcribing the book on the phone while it plays.
//!
//! One worker thread keeps a lead on the listener: it transcribes the
//! stretch just past the furthest point already done from where the book
//! is now, a minute at a time, until it is [`LEAD`] ahead, then waits. A
//! seek past what is done starts it from the new place. It decodes the
//! book with a decoder of its own, at the 16 kHz mono the model takes, so
//! playback never waits on it, and it runs at a low priority so the render
//! thread always wins the CPU.
//!
//! What it hears goes to one file per book, a line of JSON per segment and
//! per stretch covered, appended as each chunk finishes. Opening the book
//! again reads the file back, so nothing is transcribed twice. The file
//! holds words and times only: text for placing music, never audio.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::fd::FromRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use speech::{Model, SAMPLE_RATE, Segment};
use underscore_core::Source;
use underscore_decode::FileSource;

/// How far ahead of the listener to stay.
pub const LEAD: Duration = Duration::from_mins(15);
/// How much audio goes to the model at once. Long enough for context,
/// short enough that the first words show up quickly.
const CHUNK: Duration = Duration::from_secs(60);

/// The speech engine, when this build has one. Without the `transcribe`
/// feature the worker still loads transcripts made elsewhere and serves
/// their captions; only hearing new audio is missing.
#[cfg(feature = "transcribe")]
mod speech {
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    pub use underscore_transcribe::{SAMPLE_RATE, Segment, Transcriber as Model};
    use underscore_transcribe::{download_model, model_path};

    /// CPU threads for the model.
    const THREADS: usize = 4;

    /// Load model `name` from `dir`, downloading it first if need be.
    pub fn open(
        dir: &Path,
        name: &str,
        stop: &AtomicBool,
        downloading: impl FnOnce(),
        progress: impl FnMut(u64, Option<u64>),
        loading: impl FnOnce(),
    ) -> Result<Model, String> {
        if !model_path(dir, name).is_file() {
            downloading();
            download_model(dir, name, stop, progress)
                .map_err(|e| format!("downloading the model: {e}"))?;
        }
        loading();
        Model::open(&model_path(dir, name), THREADS).map_err(|e| e.to_string())
    }

    /// What was said in `audio`, which starts at `from` in the book.
    /// `Err(None)` when stopped.
    pub fn hear(
        model: &Model,
        audio: &[f32],
        from: Duration,
        stop: &Arc<AtomicBool>,
    ) -> Result<Vec<Segment>, Option<String>> {
        match model.transcribe(audio, from, stop) {
            Ok(h) => Ok(h),
            Err(underscore_transcribe::Error::Stopped) => Err(None),
            Err(e) => Err(Some(e.to_string())),
        }
    }
}

#[cfg(not(feature = "transcribe"))]
mod speech {
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    /// The rate the transcriber's audio would be read at.
    pub const SAMPLE_RATE: u32 = 16_000;

    /// One stretch of speech, as `underscore_transcribe` has it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Segment {
        pub start: Duration,
        pub end: Duration,
        pub text: String,
    }

    pub struct Model;

    const NONE: &str = "this build cannot transcribe; it shows transcripts made elsewhere";

    pub fn open(
        _dir: &Path,
        _name: &str,
        _stop: &AtomicBool,
        _downloading: impl FnOnce(),
        _progress: impl FnMut(u64, Option<u64>),
        _loading: impl FnOnce(),
    ) -> Result<Model, String> {
        Err(NONE.into())
    }

    pub fn hear(
        _model: &Model,
        _audio: &[f32],
        _from: Duration,
        _stop: &Arc<AtomicBool>,
    ) -> Result<Vec<Segment>, Option<String>> {
        Err(Some(NONE.into()))
    }
}

/// Where the audio comes from.
pub enum Audio {
    /// A file by path, such as a server book in the cache.
    Path(PathBuf),
    /// An open descriptor, owned from here on.
    Fd(i32),
}

/// What to transcribe and where to keep it.
pub struct Job {
    /// Names the book's transcript file. Any text; it is hashed.
    pub key: String,
    pub audio: Audio,
    pub length: Duration,
    /// Where models and transcripts live.
    pub dir: PathBuf,
    /// Which model, such as `base.en`.
    pub model: String,
}

/// Stretches of the book that are done, sorted and merged.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Covered(Vec<(Duration, Duration)>);

impl Covered {
    /// Mark `from..to` done.
    pub fn add(&mut self, from: Duration, to: Duration) {
        if to <= from {
            return;
        }
        self.0.push((from, to));
        self.0.sort();
        let mut merged: Vec<(Duration, Duration)> = Vec::with_capacity(self.0.len());
        for (a, b) in self.0.drain(..) {
            match merged.last_mut() {
                Some(last) if a <= last.1 => last.1 = last.1.max(b),
                _ => merged.push((a, b)),
            }
        }
        self.0 = merged;
    }

    /// The first point at or after `at` that is not done.
    #[must_use]
    pub fn next_gap(&self, at: Duration) -> Duration {
        self.0
            .iter()
            .find(|(a, b)| *a <= at && at < *b)
            .map_or(at, |(_, b)| *b)
    }

    /// Where the next done stretch after `at` begins, if any.
    #[must_use]
    pub fn next_start(&self, at: Duration) -> Option<Duration> {
        self.0.iter().map(|(a, _)| *a).find(|a| *a > at)
    }

    /// How much is done in all.
    #[must_use]
    pub fn total(&self) -> Duration {
        self.0.iter().map(|(a, b)| b.saturating_sub(*a)).sum()
    }
}

/// What the worker is doing, for the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Off,
    Downloading,
    Loading,
    Working,
    Ahead,
    Done,
    Failed,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Downloading => "downloading",
            Self::Loading => "loading",
            Self::Working => "working",
            Self::Ahead => "ahead",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }
}

#[derive(Default)]
struct State {
    job: Option<Job>,
    /// Bumped when a new job arrives, so the worker drops the old one.
    generation: u64,
    position: Duration,
    segments: Vec<Segment>,
    covered: Covered,
    phase: Option<Phase>,
    error: Option<String>,
    downloaded: (u64, Option<u64>),
    /// Audio transcribed and wall time spent on it, this run.
    heard: Duration,
    spent: Duration,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    stop: Arc<AtomicBool>,
    running: AtomicBool,
}

fn shared() -> &'static Shared {
    static S: OnceLock<Shared> = OnceLock::new();
    S.get_or_init(|| Shared {
        state: Mutex::new(State::default()),
        wake: Condvar::new(),
        stop: Arc::new(AtomicBool::new(false)),
        running: AtomicBool::new(false),
    })
}

fn lock() -> std::sync::MutexGuard<'static, State> {
    shared()
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Follow `job`'s book from here on, replacing any book followed before.
/// Starts the worker the first time.
pub fn start(job: Job) {
    let store = store_path(&job.dir, &job.key);
    let (segments, covered) = load(&store);
    {
        let mut s = lock();
        s.segments = segments;
        s.covered = covered;
        s.job = Some(job);
        s.generation += 1;
        s.error = None;
        s.phase = Some(Phase::Loading);
        s.heard = Duration::ZERO;
        s.spent = Duration::ZERO;
    }
    let sh = shared();
    sh.stop.store(false, Ordering::Relaxed);
    if !sh.running.swap(true, Ordering::SeqCst) {
        std::thread::Builder::new()
            .name("underscore-transcribe".into())
            .spawn(work)
            .ok();
    }
    sh.wake.notify_all();
}

/// Stop following. What is done stays in the book's file.
pub fn stop() {
    let sh = shared();
    sh.stop.store(true, Ordering::Relaxed);
    {
        let mut s = lock();
        s.job = None;
        s.generation += 1;
        s.phase = Some(Phase::Off);
    }
    sh.wake.notify_all();
}

/// Where the listener is. Cheap; call it every second or so.
pub fn set_position(at: Duration) {
    let moved = {
        let mut s = lock();
        let jump = at.abs_diff(s.position) > Duration::from_secs(5);
        s.position = at;
        jump
    };
    if moved {
        shared().wake.notify_all();
    }
}

/// The state for the screen, as JSON: the phase, the lead, how fast it
/// runs, and the lines around the listener.
#[must_use]
pub fn status() -> serde_json::Value {
    let s = lock();
    let pos = s.position;
    let lead = s.covered.next_gap(pos).saturating_sub(pos);
    let at = s.segments.partition_point(|g| g.end <= pos);
    let line = |g: &Segment| serde_json::json!({ "startMs": ms(g.start), "endMs": ms(g.end), "text": g.text });
    let near: Vec<_> = s.segments[at.saturating_sub(1)..(at + 3).min(s.segments.len())]
        .iter()
        .map(line)
        .collect();
    let current = s
        .segments
        .get(at)
        .filter(|g| g.start <= pos + Duration::from_millis(500))
        .map(|g| g.text.clone());
    let speed = if s.spent.is_zero() {
        0.0
    } else {
        s.heard.as_secs_f64() / s.spent.as_secs_f64()
    };
    serde_json::json!({
        "phase": s.phase.unwrap_or(Phase::Off).name(),
        "error": s.error,
        "model": s.job.as_ref().map(|j| j.model.clone()),
        "leadMs": ms(lead),
        "coveredMs": ms(s.covered.total()),
        "lengthMs": s.job.as_ref().map_or(0, |j| ms(j.length)),
        "segments": s.segments.len(),
        "speed": speed,
        "downloaded": s.downloaded.0,
        "downloadTotal": s.downloaded.1,
        "current": current,
        "near": near,
    })
}

fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// The worker. Lives for the rest of the process once started.
fn work() {
    lower_priority();
    let sh = shared();
    let mut model: Option<(String, Model)> = None;
    let mut decoder: Option<(u64, FileSource)> = None;
    loop {
        // What to do next, decided under the lock; the work happens outside it.
        let next = {
            let mut s = lock();
            loop {
                if let Some(job) = &s.job {
                    let from = s.covered.next_gap(s.position);
                    let end = job.length;
                    if from >= end {
                        s.phase = Some(Phase::Done);
                    } else if from.saturating_sub(s.position) >= LEAD {
                        s.phase = Some(Phase::Ahead);
                    } else {
                        let to = (from + CHUNK)
                            .min(end)
                            .min(s.covered.next_start(from).unwrap_or(end));
                        break (s.generation, from, to, job.model.clone(), job.dir.clone());
                    }
                }
                s = sh
                    .wake
                    .wait_timeout(s, Duration::from_secs(5))
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .0;
            }
        };
        let (generation, from, to, name, dir) = next;

        if model.as_ref().is_none_or(|(n, _)| *n != name) {
            model = None;
            match load_model(&dir, &name, &sh.stop) {
                Ok(t) => model = Some((name.clone(), t)),
                Err(e) => {
                    fail(generation, &e);
                    continue;
                }
            }
        }
        if decoder.as_ref().is_none_or(|(g, _)| *g != generation) {
            decoder = open_decoder(generation).map(|d| (generation, d));
            if decoder.is_none() {
                fail(generation, "could not open the book to transcribe it");
                continue;
            }
        }
        let (Some((_, transcriber)), Some((_, source))) = (model.as_ref(), decoder.as_mut()) else {
            continue;
        };
        set_phase(generation, Phase::Working);

        let started = Instant::now();
        let Some(audio) = read_span(source, from, to) else {
            fail(generation, "could not read the book here");
            continue;
        };
        let heard = match speech::hear(transcriber, &audio, from, &sh.stop) {
            Ok(h) => h,
            Err(None) => continue,
            Err(Some(e)) => {
                fail(generation, &e);
                continue;
            }
        };
        let spent = started.elapsed();
        let mut s = lock();
        if s.generation != generation {
            continue;
        }
        let (keep, done_to) = trim_tail(heard, from, to, s.job.as_ref().map(|j| j.length));
        if let Some(job) = &s.job {
            append(&store_path(&job.dir, &job.key), &keep, from, done_to);
        }
        let at = s.segments.partition_point(|g| g.start < from);
        let after = s.segments.partition_point(|g| g.start < done_to);
        s.segments.splice(at..after, keep);
        s.covered.add(from, done_to);
        s.heard += done_to.saturating_sub(from);
        s.spent += spent;
    }
}

/// The model cuts the last words of a chunk short. Unless the chunk ends
/// the book, drop its last segment and count the chunk done only to where
/// that segment began, so the next chunk hears it whole.
fn trim_tail(
    mut heard: Vec<Segment>,
    from: Duration,
    to: Duration,
    length: Option<Duration>,
) -> (Vec<Segment>, Duration) {
    let at_end = length.is_some_and(|l| to >= l);
    if !at_end && heard.len() > 1 {
        let last = heard.pop().map_or(to, |g| g.start);
        if last > from {
            return (heard, last);
        }
    }
    (heard, to)
}

fn load_model(dir: &Path, name: &str, stop: &AtomicBool) -> Result<Model, String> {
    speech::open(
        dir,
        name,
        stop,
        || set_phase(lock().generation, Phase::Downloading),
        |got, total| lock().downloaded = (got, total),
        || set_phase(lock().generation, Phase::Loading),
    )
}

fn open_decoder(generation: u64) -> Option<FileSource> {
    let mut s = lock();
    if s.generation != generation {
        return None;
    }
    let job = s.job.as_mut()?;
    match std::mem::replace(&mut job.audio, Audio::Path(PathBuf::new())) {
        Audio::Path(p) => {
            let src = FileSource::open_path(&p, SAMPLE_RATE, 1).ok();
            job.audio = Audio::Path(p);
            src
        }
        Audio::Fd(fd) => {
            // SAFETY: the caller handed this descriptor over and does not
            // use it again; it is taken out of the job, so it is wrapped once.
            let file = unsafe { File::from_raw_fd(fd) };
            FileSource::open(Box::new(file), None, SAMPLE_RATE, 1).ok()
        }
    }
}

/// `from..to` of the book, decoded to 16 kHz mono.
fn read_span(source: &mut FileSource, from: Duration, to: Duration) -> Option<Vec<f32>> {
    if !source.seek(from) {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let want = (to.saturating_sub(from).as_secs_f64() * f64::from(SAMPLE_RATE)) as usize;
    let mut audio = vec![0.0f32; want];
    let mut got = 0;
    while got < want {
        let n = source.read(&mut audio[got..]);
        if n == 0 {
            break;
        }
        got += n;
    }
    audio.truncate(got);
    Some(audio)
}

fn set_phase(generation: u64, phase: Phase) {
    let mut s = lock();
    if s.generation == generation {
        s.phase = Some(phase);
    }
}

fn fail(generation: u64, why: &str) {
    {
        let mut s = lock();
        if s.generation == generation {
            s.phase = Some(Phase::Failed);
            s.error = Some(why.to_string());
            // Give up on this book until it is started again.
            s.job = None;
        }
    }
    log(why);
}

fn log(why: &str) {
    eprintln!("underscore transcribe: {why}");
}

/// Background priority for this thread and the model's threads, which
/// inherit it.
fn lower_priority() {
    // SAFETY: setpriority on the calling thread (who = 0) with a plain
    // nice value; it touches no memory.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
}

/// The book's transcript file, named by a hash of its key.
#[must_use]
pub fn store_path(dir: &Path, key: &str) -> PathBuf {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    dir.join("transcripts").join(format!("{h:016x}.jsonl"))
}

/// How far an imported transcript says it covers: past the end of any book,
/// so the worker finds nothing left to do and never loads a model.
const WHOLE_BOOK: Duration = Duration::from_hours(10_000);

/// Take a finished transcript made elsewhere as the book's transcript,
/// replacing what was there: every segment, and the whole book covered.
/// `json` is the desktop transcriber's format, `{"chapters": [{"segments":
/// [{"start", "end", "text"}]}]}` with times in seconds from the start of
/// the book, or the same segments at the top level. Returns how many
/// segments it took.
///
/// # Errors
/// The JSON is not a transcript, or the file could not be written.
pub fn import(dir: &Path, key: &str, json: &[u8]) -> Result<usize, String> {
    let v: serde_json::Value =
        serde_json::from_slice(json).map_err(|e| format!("not a transcript: {e}"))?;
    let lists: Vec<&serde_json::Value> = match v.get("chapters").and_then(|c| c.as_array()) {
        Some(chapters) => chapters.iter().filter_map(|c| c.get("segments")).collect(),
        None => v.get("segments").into_iter().collect(),
    };
    let secs = |g: &serde_json::Value, k: &str| {
        g.get(k)
            .and_then(serde_json::Value::as_f64)
            .filter(|t| t.is_finite() && *t >= 0.0)
            .map(Duration::from_secs_f64)
    };
    let mut segments: Vec<Segment> = lists
        .iter()
        .filter_map(|l| l.as_array())
        .flatten()
        .filter_map(|g| {
            let text = g.get("text")?.as_str()?.trim();
            let (start, end) = (secs(g, "start")?, secs(g, "end")?);
            (!text.is_empty()).then(|| Segment {
                start,
                end,
                text: text.to_string(),
            })
        })
        .collect();
    if segments.is_empty() {
        return Err("the transcript has no segments".into());
    }
    segments.sort_by_key(|g| g.start);
    let path = store_path(dir, key);
    let part = path.with_extension("part");
    let _ = fs::remove_file(&part);
    append(&part, &segments, Duration::ZERO, WHOLE_BOOK);
    fs::rename(&part, &path).map_err(|e| format!("could not keep the transcript: {e}"))?;
    Ok(segments.len())
}

/// Read a transcript file back: its segments, sorted, and what it covers.
#[must_use]
pub fn load(path: &Path) -> (Vec<Segment>, Covered) {
    let mut segments = Vec::new();
    let mut covered = Covered::default();
    let Ok(file) = File::open(path) else {
        return (segments, covered);
    };
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let at = |k: &str| {
            v.get(k)
                .and_then(serde_json::Value::as_u64)
                .map(Duration::from_millis)
        };
        if let (Some(a), Some(b)) = (at("from"), at("to")) {
            covered.add(a, b);
        } else if let (Some(a), Some(b), Some(t)) = (
            at("s"),
            at("e"),
            v.get("t").and_then(serde_json::Value::as_str),
        ) {
            segments.push(Segment {
                start: a,
                end: b,
                text: t.to_string(),
            });
        }
    }
    // Words from a chunk cut off before its cover line are heard again,
    // so drop them; and a later chunk's words replace an earlier one's.
    segments.retain(|g| covered.next_gap(g.start) > g.start);
    segments.sort_by_key(|g| g.start);
    segments.reverse();
    segments.dedup_by(|later, kept| later.start == kept.start);
    segments.reverse();
    (segments, covered)
}

/// Add one chunk to the book's file: its segments, then what it covers.
/// The cover line goes last, so a chunk cut off halfway is heard again.
fn append(path: &Path, segments: &[Segment], from: Duration, to: Duration) {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) else {
        log("could not write the transcript");
        return;
    };
    let mut out = String::new();
    for g in segments {
        out.push_str(
            &serde_json::json!({ "s": ms(g.start), "e": ms(g.end), "t": g.text }).to_string(),
        );
        out.push('\n');
    }
    out.push_str(&serde_json::json!({ "from": ms(from), "to": ms(to) }).to_string());
    out.push('\n');
    if f.write_all(out.as_bytes()).is_err() {
        log("could not write the transcript");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(secs: u64) -> Duration {
        Duration::from_secs(secs)
    }

    fn seg(a: u64, b: u64, t: &str) -> Segment {
        Segment {
            start: s(a),
            end: s(b),
            text: t.into(),
        }
    }

    #[test]
    fn covered_merges_and_finds_gaps() {
        let mut c = Covered::default();
        c.add(s(60), s(120));
        c.add(s(0), s(30));
        c.add(s(25), s(61));
        assert_eq!(c.0, vec![(s(0), s(120))]);
        c.add(s(300), s(360));
        assert_eq!(c.next_gap(s(10)), s(120));
        assert_eq!(c.next_gap(s(200)), s(200));
        assert_eq!(c.next_start(s(120)), Some(s(300)));
        assert_eq!(c.total(), s(180));
    }

    #[test]
    fn the_cut_off_last_segment_waits_for_the_next_chunk() {
        let heard = vec![seg(0, 20, "one"), seg(20, 50, "two"), seg(50, 60, "thr")];
        let (keep, to) = trim_tail(heard.clone(), s(0), s(60), Some(s(600)));
        assert_eq!(keep.len(), 2);
        assert_eq!(to, s(50));
        // At the end of the book everything stays.
        let (keep, to) = trim_tail(heard, s(0), s(60), Some(s(60)));
        assert_eq!((keep.len(), to), (3, s(60)));
    }

    #[test]
    fn the_file_reads_back_what_was_written() {
        let dir = std::env::temp_dir().join(format!("underscore-tx-{}", std::process::id()));
        let path = store_path(&dir, "abs:li1");
        let _ = fs::remove_file(&path);
        append(&path, &[seg(0, 5, "hello"), seg(5, 9, "there")], s(0), s(9));
        append(&path, &[seg(120, 125, "later")], s(120), s(125));
        // A chunk heard again replaces the old words.
        append(&path, &[seg(5, 9, "there again")], s(5), s(9));
        let (segments, covered) = load(&path);
        let texts: Vec<_> = segments.iter().map(|g| g.text.as_str()).collect();
        assert_eq!(texts, ["hello", "there again", "later"]);
        assert_eq!(covered.0, vec![(s(0), s(9)), (s(120), s(125))]);
        assert_eq!(store_path(&dir, "abs:li1"), path);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn an_imported_transcript_covers_the_whole_book() {
        let dir = std::env::temp_dir().join(format!("underscore-im-{}", std::process::id()));
        let json = br#"{"model": "distil-large-v3", "chapters": [
            {"index": 0, "start": 0.0, "segments": [{"start": 0.14, "end": 1.58, "text": " This is Audible."}]},
            {"index": 1, "start": 100.0, "segments": [
                {"start": 101.5, "end": 104.0, "text": "Chapter one."},
                {"start": 104.0, "end": 104.5, "text": "  "}
            ]}
        ]}"#;
        assert_eq!(import(&dir, "abs:b1", json), Ok(2));
        let (segments, covered) = load(&store_path(&dir, "abs:b1"));
        let texts: Vec<_> = segments.iter().map(|g| g.text.as_str()).collect();
        assert_eq!(texts, ["This is Audible.", "Chapter one."]);
        assert_eq!(segments[1].start, Duration::from_millis(101_500));
        assert_eq!(covered.next_gap(s(50_000)), WHOLE_BOOK);
        assert!(import(&dir, "abs:b1", b"{}").is_err());
        let _ = fs::remove_dir_all(dir);
    }
}

/// Against a real book and model: `UNDERSCORE_TX_BOOK=book.m4b
/// UNDERSCORE_TX_DIR=dir cargo test -p underscore-android --release
/// follows_a_real_book -- --ignored --nocapture`. Downloads `tiny.en`
/// into the folder the first time.
#[cfg(test)]
mod real {
    use super::*;

    #[test]
    #[ignore = "needs a book, a model download, and minutes of CPU"]
    fn follows_a_real_book() {
        let book = std::env::var("UNDERSCORE_TX_BOOK").expect("UNDERSCORE_TX_BOOK");
        let dir = PathBuf::from(std::env::var("UNDERSCORE_TX_DIR").expect("UNDERSCORE_TX_DIR"));
        let length = underscore_decode::probe_path(&book)
            .ok()
            .and_then(|p| p.duration)
            .expect("book length");
        set_position(Duration::from_secs(600));
        start(Job {
            key: book.clone(),
            audio: Audio::Path(book.into()),
            length,
            dir,
            model: "tiny.en".into(),
        });
        let began = Instant::now();
        while began.elapsed() < Duration::from_secs(150) {
            std::thread::sleep(Duration::from_secs(10));
            let st = status();
            println!(
                "{:>4}s {} lead {}s speed {:.1}x segments {} now {:?}",
                began.elapsed().as_secs(),
                st["phase"],
                st["leadMs"].as_u64().unwrap_or(0) / 1000,
                st["speed"].as_f64().unwrap_or(0.0),
                st["segments"],
                st["current"]
            );
        }
        stop();
        assert!(
            status()["segments"].as_u64().unwrap_or(0) > 0,
            "{}",
            status()
        );
    }
}
