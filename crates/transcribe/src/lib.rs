//! Speech to text for a book, on the device, with zwhisper.
//!
//! The words are only ever text: what was said and when. They are for
//! placing music, the same job the desktop transcription does, and never
//! leave the device from here. Nothing here touches the mix.
//!
//! A model is a ggml file from the whisper.cpp project, downloaded once
//! with [`download_model`]. [`Transcriber`] takes 16 kHz mono samples, the
//! format the model wants, and returns timed [`Segment`]s, or [`Span`]s
//! when the caller also wants who was speaking: spans are split at quotes
//! and tagged with a voice label that stays stable across calls on the
//! same transcriber. Decoding the book into that format is the caller's job.
//!
//! Everything here is slow: run it on a thread of its own, never on the
//! audio thread.

use std::ffi::{CString, c_int, c_void};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// The sample rate the models take.
pub const SAMPLE_RATE: u32 = 16_000;

/// The models offered, smallest first: name, size in megabytes.
pub const MODELS: [(&str, u32); 3] = [("tiny.en", 75), ("base.en", 142), ("small.en", 466)];

/// Something went wrong.
#[derive(Debug)]
pub enum Error {
    /// The model file could not be fetched or written.
    Io(io::Error),
    /// The engine refused the model or the audio.
    Engine(String),
    /// Stopped on request.
    Stopped,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Engine(e) => write!(f, "zwhisper: {e}"),
            Self::Stopped => f.write_str("stopped"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// One stretch of speech.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// Where it starts in the book.
    pub start: Duration,
    /// Where it ends in the book.
    pub end: Duration,
    /// What was said.
    pub text: String,
}

/// A [`Segment`] with who said it. Spans are split at quotation marks, so a line of
/// dialogue and the narration around it come out separately.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub start: Duration,
    pub end: Duration,
    pub text: String,
    /// Inside quotation marks.
    pub quoted: bool,
    /// Voice label, 1-based in order of first appearance on this transcriber; 1 is
    /// almost always the narrator. 0 when voices were not asked for.
    pub voice: u32,
    /// Median pitch over the span in Hz, 0 if nothing voiced.
    pub pitch_hz: f32,
    /// How far the span was from the voice it joined, in standard units.
    pub voice_dist: f32,
}

impl From<Span> for Segment {
    fn from(s: Span) -> Self {
        Self {
            start: s.start,
            end: s.end,
            text: s.text,
        }
    }
}

mod ffi {
    use std::ffi::{c_char, c_int, c_void};

    #[repr(C)]
    pub struct Handle {
        _private: [u8; 0],
    }

    #[repr(C)]
    pub struct Segment {
        pub t0_ms: i64,
        pub t1_ms: i64,
        pub text: *const u8,
        pub text_len: usize,
        pub tokens: *const u32,
        pub n_tokens: usize,
        pub voice: u32,
        pub quoted: bool,
        pub pitch_hz: f32,
        pub voice_dist: f32,
    }

    #[repr(C)]
    #[derive(Default)]
    pub struct Stats {
        pub windows: u64,
        pub tokens: u64,
        pub segments: u64,
        pub voices: u64,
        pub encode_ns: u64,
        pub decode_ns: u64,
        pub voice_ns: u64,
    }

    pub type Emit = unsafe extern "C" fn(*mut c_void, *const Segment);

    unsafe extern "C" {
        pub fn zw_open(path: *const c_char, threads: c_int) -> *mut Handle;
        pub fn zw_close(h: *mut Handle);
        pub fn zw_transcribe(
            h: *mut Handle,
            samples: *const f32,
            n: usize,
            offset_ms: i64,
            voices: bool,
            split_quotes: bool,
            condition_on_previous: bool,
            stop: *const bool,
            emit: Emit,
            user: *mut c_void,
            stats: *mut Stats,
        ) -> c_int;
        pub fn zw_reset_voices(h: *mut Handle);
        pub fn zw_n_voices(h: *mut Handle) -> u32;
        pub fn zw_version() -> c_int;
    }
}

/// Collects spans from the engine's callback into the Vec passed as `user`.
unsafe extern "C" fn emit(user: *mut c_void, seg: *const ffi::Segment) {
    // SAFETY: user is the Vec<Span> passed below; seg is valid for this call.
    let (out, seg) = unsafe { (&mut *user.cast::<Vec<Span>>(), &*seg) };
    // SAFETY: text points at text_len bytes of UTF-8 owned by the engine.
    let text = unsafe { std::slice::from_raw_parts(seg.text, seg.text_len) };
    out.push(Span {
        start: Duration::from_millis(u64::try_from(seg.t0_ms.max(0)).unwrap_or(0)),
        end: Duration::from_millis(u64::try_from(seg.t1_ms.max(0)).unwrap_or(0)),
        text: String::from_utf8_lossy(text).trim().to_string(),
        quoted: seg.quoted,
        voice: seg.voice,
        pitch_hz: seg.pitch_hz,
        voice_dist: seg.voice_dist,
    });
}

/// A loaded model.
pub struct Transcriber {
    h: NonNull<ffi::Handle>,
    threads: usize,
}

// The handle is only ever used from one thread at a time, and the engine's own worker
// threads are internal to it.
unsafe impl Send for Transcriber {}

impl fmt::Debug for Transcriber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Transcriber")
            .field("threads", &self.threads)
            .finish_non_exhaustive()
    }
}

impl Drop for Transcriber {
    fn drop(&mut self) {
        // SAFETY: the handle came from zw_open and is closed exactly once.
        unsafe { ffi::zw_close(self.h.as_ptr()) }
    }
}

impl Transcriber {
    /// Load the model at `model`, to run on `threads` CPU threads.
    ///
    /// # Errors
    /// The file is missing or is not a whisper.cpp model.
    pub fn open(model: &Path, threads: usize) -> Result<Self, Error> {
        let path = model
            .to_str()
            .ok_or_else(|| Error::Engine("model path is not text".into()))?;
        let c = CString::new(path).map_err(|_| Error::Engine("model path has a NUL".into()))?;
        let threads = threads.max(1);
        // SAFETY: no arguments, no state.
        let version = unsafe { ffi::zw_version() };
        if version != 1 {
            return Err(Error::Engine(format!("zwhisper ABI {version}, expected 1")));
        }
        // SAFETY: c is a valid NUL-terminated path.
        let h = unsafe { ffi::zw_open(c.as_ptr(), c_int::try_from(threads).unwrap_or(4)) };
        let h = NonNull::new(h)
            .ok_or_else(|| Error::Engine(format!("could not open model {}", model.display())))?;
        Ok(Self { h, threads })
    }

    /// Transcribe `audio`, 16 kHz mono, which starts at `offset` in the
    /// book. Setting `stop` ends the work early with [`Error::Stopped`].
    ///
    /// # Errors
    /// The engine failed, or `stop` was set.
    pub fn transcribe(
        &self,
        audio: &[f32],
        offset: Duration,
        stop: &AtomicBool,
    ) -> Result<Vec<Segment>, Error> {
        Ok(self
            .run(audio, offset, stop, false)?
            .into_iter()
            .map(Segment::from)
            .collect())
    }

    /// Like [`transcribe`](Self::transcribe), but split at quotes and tagged with voices.
    ///
    /// # Errors
    /// The engine failed, or `stop` was set.
    pub fn transcribe_spans(
        &self,
        audio: &[f32],
        offset: Duration,
        stop: &AtomicBool,
    ) -> Result<Vec<Span>, Error> {
        self.run(audio, offset, stop, true)
    }

    /// Forget the voices seen so far (a new book).
    pub fn reset_voices(&self) {
        // SAFETY: valid handle.
        unsafe { ffi::zw_reset_voices(self.h.as_ptr()) }
    }

    /// How many voices have been seen on this transcriber.
    #[must_use]
    pub fn voices(&self) -> u32 {
        // SAFETY: valid handle.
        unsafe { ffi::zw_n_voices(self.h.as_ptr()) }
    }

    fn run(
        &self,
        audio: &[f32],
        offset: Duration,
        stop: &AtomicBool,
        voices: bool,
    ) -> Result<Vec<Span>, Error> {
        let mut out: Vec<Span> = Vec::new();
        let offset_ms = i64::try_from(offset.as_millis()).unwrap_or(i64::MAX);
        let mut stats = ffi::Stats::default();
        // SAFETY: the handle is valid; audio outlives the call; the stop flag is read
        // atomically as a plain bool; emit only touches `out`, which outlives the call.
        let rc = unsafe {
            ffi::zw_transcribe(
                self.h.as_ptr(),
                audio.as_ptr(),
                audio.len(),
                offset_ms,
                voices,
                voices,
                true,
                stop.as_ptr().cast_const(),
                emit,
                std::ptr::from_mut(&mut out).cast::<c_void>(),
                &raw mut stats,
            )
        };
        match rc {
            0 => Ok(out),
            1 => Err(Error::Stopped),
            _ => Err(Error::Engine("transcription failed".into())),
        }
    }
}

/// Where model `name` lives once downloaded into `dir`.
#[must_use]
pub fn model_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("ggml-{name}.bin"))
}

/// Download model `name` into `dir`, unless it is already there.
/// `progress` hears bytes so far and the total, when known. Setting
/// `stop` ends it early. Returns the model's path.
///
/// # Errors
/// The download failed or was stopped, or the file could not be written.
pub fn download_model(
    dir: &Path,
    name: &str,
    stop: &AtomicBool,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<PathBuf, Error> {
    let path = model_path(dir, name);
    if path.is_file() {
        return Ok(path);
    }
    fs::create_dir_all(dir)?;
    let url = format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{name}.bin");
    let mut resp = ureq::get(&url)
        .call()
        .map_err(|e| Error::Io(io::Error::other(format!("{url}: {e}"))))?;
    let total = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    let part = path.with_extension("part");
    let mut out = File::create(&part)?;
    let mut body = resp.body_mut().with_config().limit(u64::MAX).reader();
    let mut buf = vec![0u8; 1 << 16];
    let mut got = 0u64;
    loop {
        if stop.load(Ordering::Relaxed) {
            drop(out);
            let _ = fs::remove_file(&part);
            return Err(Error::Stopped);
        }
        let n = body.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        got += n as u64;
        progress(got, total);
    }
    out.flush()?;
    drop(out);
    if total.is_some_and(|t| t != got) {
        let _ = fs::remove_file(&part);
        return Err(Error::Io(io::Error::other(
            "the model download was cut short",
        )));
    }
    fs::rename(&part, &path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_are_named_where_the_project_puts_them() {
        assert_eq!(
            model_path(Path::new("/m"), "base.en"),
            PathBuf::from("/m/ggml-base.en.bin")
        );
    }

    #[test]
    fn a_missing_model_is_an_error_not_a_crash() {
        let err = Transcriber::open(Path::new("/no/such/model.bin"), 2).unwrap_err();
        assert!(matches!(err, Error::Engine(_)), "{err}");
    }

    #[test]
    fn the_engine_links() {
        // SAFETY: no arguments, no state.
        assert_eq!(unsafe { ffi::zw_version() }, 1);
    }

    /// Runs only when a model is around: ZWHISPER_MODEL=/path/to/ggml-base.en.bin.
    #[test]
    fn transcribes_silence_without_segments() {
        let Some(model) = std::env::var_os("ZWHISPER_MODEL") else {
            return;
        };
        let t = Transcriber::open(Path::new(&model), 4).unwrap();
        let audio = vec![0.0f32; SAMPLE_RATE as usize * 3];
        let stop = AtomicBool::new(false);
        let segs = t.transcribe(&audio, Duration::ZERO, &stop).unwrap();
        assert!(segs.iter().all(|s| s.end >= s.start));
    }
}
