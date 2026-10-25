//! Speech to text for a book, on the device, with whisper.cpp.
//!
//! The words are only ever text: what was said and when. They are for
//! placing music, the same job the desktop transcription does, and never
//! leave the device from here. Nothing here touches the mix.
//!
//! A model is a ggml file from the whisper.cpp project, downloaded once
//! with [`download_model`]. [`Transcriber`] takes 16 kHz mono samples, the
//! format the model wants, and returns timed [`Segment`]s. Decoding the
//! book into that format is the caller's job.
//!
//! Everything here is slow: run it on a thread of its own, never on the
//! audio thread.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once};
use std::time::Duration;

use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// The sample rate the models take.
pub const SAMPLE_RATE: u32 = 16_000;

/// The models offered, smallest first: name, size in megabytes.
pub const MODELS: [(&str, u32); 3] = [("tiny.en", 75), ("base.en", 142), ("small.en", 466)];

/// Something went wrong.
#[derive(Debug)]
pub enum Error {
    /// The model file could not be fetched or written.
    Io(io::Error),
    /// whisper.cpp refused the model or the audio.
    Whisper(String),
    /// Stopped on request.
    Stopped,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Whisper(e) => write!(f, "whisper: {e}"),
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

/// A loaded model.
pub struct Transcriber {
    ctx: WhisperContext,
    threads: i32,
}

impl fmt::Debug for Transcriber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Transcriber")
            .field("threads", &self.threads)
            .finish_non_exhaustive()
    }
}

impl Transcriber {
    /// Load the model at `model`, to run on `threads` CPU threads.
    ///
    /// # Errors
    /// The file is missing or is not a whisper.cpp model.
    pub fn open(model: &Path, threads: usize) -> Result<Self, Error> {
        quiet();
        let path = model
            .to_str()
            .ok_or_else(|| Error::Whisper("model path is not text".into()))?;
        let ctx = WhisperContext::new_with_params(path, WhisperContextParameters::default())
            .map_err(|e| Error::Whisper(e.to_string()))?;
        Ok(Self {
            ctx,
            threads: i32::try_from(threads.max(1)).unwrap_or(4),
        })
    }

    /// Transcribe `audio`, 16 kHz mono, which starts at `offset` in the
    /// book. Setting `stop` ends the work early with [`Error::Stopped`].
    ///
    /// # Errors
    /// whisper.cpp failed, or `stop` was set.
    pub fn transcribe(
        &self,
        audio: &[f32],
        offset: Duration,
        stop: &Arc<AtomicBool>,
    ) -> Result<Vec<Segment>, Error> {
        let mut state = self
            .ctx
            .create_state()
            .map_err(|e| Error::Whisper(e.to_string()))?;
        // Greedy decoding: several times faster than a beam search, and
        // placing music needs the gist and the timing, not every word.
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(self.threads);
        params.set_language(Some("en"));
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        // whisper-rs 0.16's set_abort_callback_safe hands whisper.cpp a
        // pointer of the wrong type, which reads as "abort" and fails the
        // encoder with -6. Pass the flag itself instead.
        // SAFETY: `stop` outlives `full`, which is the only caller of the
        // callback, and the callback only reads the flag.
        unsafe {
            params.set_abort_callback(Some(abort_when_set));
            params.set_abort_callback_user_data(Arc::as_ptr(stop).cast_mut().cast());
        }
        let ran = state.full(params, audio);
        if stop.load(Ordering::Relaxed) {
            return Err(Error::Stopped);
        }
        ran.map_err(|e| Error::Whisper(e.to_string()))?;
        Ok(state
            .as_iter()
            .filter_map(|s| {
                let text = s.to_str_lossy().ok()?.trim().to_string();
                (!text.is_empty()).then(|| Segment {
                    start: offset + centis(s.start_timestamp()),
                    end: offset + centis(s.end_timestamp()),
                    text,
                })
            })
            .collect())
    }
}

/// whisper.cpp asks this between steps; true stops it.
unsafe extern "C" fn abort_when_set(flag: *mut std::ffi::c_void) -> bool {
    // SAFETY: the pointer is the `AtomicBool` set in `transcribe`.
    unsafe { (*flag.cast::<AtomicBool>()).load(Ordering::Relaxed) }
}

/// whisper.cpp timestamps are in hundredths of a second.
fn centis(t: i64) -> Duration {
    Duration::from_millis(u64::try_from(t.max(0)).unwrap_or(0) * 10)
}

/// Send whisper.cpp's chatter to `log` instead of stderr, once.
fn quiet() {
    static ONCE: Once = Once::new();
    ONCE.call_once(whisper_rs::install_logging_hooks);
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
        assert_eq!(centis(150), Duration::from_millis(1500));
        assert_eq!(centis(-3), Duration::ZERO);
    }

    #[test]
    fn a_missing_model_is_an_error_not_a_crash() {
        let err = Transcriber::open(Path::new("/no/such/model.bin"), 2).unwrap_err();
        assert!(matches!(err, Error::Whisper(_)), "{err}");
    }
}
