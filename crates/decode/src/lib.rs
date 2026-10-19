//! Decoding for underscore.
//!
//! [`FileSource`] turns an audio file (MP3, AAC in M4A or M4B, FLAC, Ogg
//! Vorbis, WAV) into interleaved `f32` frames at the engine's sample rate
//! and channel count, and implements the engine's [`Source`] trait. The
//! only I/O is reading the file the caller handed over. Nothing is written.
//!
//! Resampling is linear interpolation for now. It is fine for speech and
//! acceptable for music under speech; a higher quality resampler is on the
//! roadmap.

use std::fmt;
use std::fs::File;
use std::path::Path;
use std::time::Duration;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, Decoder, DecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
pub use symphonia::core::io::MediaSource;
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;
use underscore_core::Source;

/// Why a file could not be opened.
#[derive(Debug)]
pub enum DecodeError {
    /// The container or codec is not recognised or not enabled.
    Unsupported(String),
    /// No audio track in the file.
    NoAudioTrack,
    /// The file could not be read.
    Io(std::io::Error),
    /// A codec or container error from symphonia.
    Codec(SymError),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(what) => write!(f, "unsupported audio: {what}"),
            Self::NoAudioTrack => write!(f, "no audio track in file"),
            Self::Io(e) => write!(f, "read error: {e}"),
            Self::Codec(e) => write!(f, "decode error: {e}"),
        }
    }
}

impl std::error::Error for DecodeError {}

impl From<std::io::Error> for DecodeError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<SymError> for DecodeError {
    fn from(e: SymError) -> Self {
        match e {
            SymError::Unsupported(what) => Self::Unsupported(what.to_string()),
            SymError::IoError(io) => Self::Io(io),
            other => Self::Codec(other),
        }
    }
}

/// A decoded audio file, resampled and channel-mapped to the engine format.
pub struct FileSource {
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    src_rate: u32,
    src_channels: usize,
    out_rate: u32,
    out_channels: usize,
    /// Decoded, source-format samples not yet resampled.
    sample_buf: Option<SampleBuffer<f32>>,
    /// Engine-format frames ready to hand out.
    pending: Vec<f32>,
    pending_pos: usize,
    /// Resampler state: the last source frame (engine channel layout).
    last_frame: Vec<f32>,
    /// Fractional position between `last_frame` and the next source frame.
    phase: f64,
    /// Engine-rate frames handed out since the last seek.
    out_frames: u64,
    /// Frames to discard after a seek that landed early.
    skip: u64,
    /// Content time at the last seek.
    base: Duration,
    duration: Option<Duration>,
    ended: bool,
}

impl fmt::Debug for FileSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileSource")
            .field("src_rate", &self.src_rate)
            .field("src_channels", &self.src_channels)
            .field("out_rate", &self.out_rate)
            .field("out_channels", &self.out_channels)
            .field("duration", &self.duration)
            .finish_non_exhaustive()
    }
}

impl FileSource {
    /// Open a file by path.
    ///
    /// # Errors
    /// If the file cannot be read or is not a supported audio file.
    pub fn open_path(
        path: impl AsRef<Path>,
        out_rate: u32,
        out_channels: usize,
    ) -> Result<Self, DecodeError> {
        let path = path.as_ref();
        let ext = path.extension().and_then(|e| e.to_str());
        let file = File::open(path)?;
        Self::open(Box::new(file), ext, out_rate, out_channels)
    }

    /// Open any seekable byte source. `hint` is a file extension such as
    /// `"m4b"`, used only to speed up format detection.
    ///
    /// # Errors
    /// If the bytes are not a supported audio file.
    pub fn open(
        source: Box<dyn MediaSource>,
        hint: Option<&str>,
        out_rate: u32,
        out_channels: usize,
    ) -> Result<Self, DecodeError> {
        let mss = MediaSourceStream::new(source, MediaSourceStreamOptions::default());
        let mut probe_hint = Hint::new();
        if let Some(ext) = hint {
            probe_hint.with_extension(ext);
        }
        let probed = symphonia::default::get_probe().format(
            &probe_hint,
            mss,
            &FormatOptions {
                enable_gapless: true,
                ..FormatOptions::default()
            },
            &MetadataOptions::default(),
        )?;
        let reader = probed.format;
        let track = reader
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or(DecodeError::NoAudioTrack)?;
        let params = &track.codec_params;
        let src_rate = params
            .sample_rate
            .ok_or_else(|| DecodeError::Unsupported("unknown sample rate".into()))?;
        let src_channels = params
            .channels
            .map_or(0, symphonia::core::audio::Channels::count);
        let duration = match (params.n_frames, params.time_base) {
            (Some(n), Some(tb)) => {
                let t = tb.calc_time(n);
                Some(Duration::from_secs_f64(f64::from_u64(t.seconds) + t.frac))
            }
            (Some(n), None) => Some(Duration::from_secs_f64(
                f64::from_u64(n) / f64::from(src_rate),
            )),
            _ => None,
        };
        let decoder = symphonia::default::get_codecs().make(params, &DecoderOptions::default())?;
        Ok(Self {
            track_id: track.id,
            reader,
            decoder,
            src_rate,
            src_channels,
            out_rate,
            out_channels,
            sample_buf: None,
            pending: Vec::new(),
            pending_pos: 0,
            last_frame: vec![0.0; out_channels],
            phase: 1.0,
            out_frames: 0,
            skip: 0,
            base: Duration::ZERO,
            duration,
            ended: false,
        })
    }

    /// Decode the next packet into `pending`. Returns false at end of file.
    fn decode_more(&mut self) -> bool {
        loop {
            let packet = match self.reader.next_packet() {
                Ok(p) => p,
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return false;
                }
                Err(SymError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(_) => return false,
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(decoded) => {
                    let spec = *decoded.spec();
                    let frames = decoded.capacity();
                    let buf = self
                        .sample_buf
                        .get_or_insert_with(|| SampleBuffer::<f32>::new(frames as u64, spec));
                    if buf.capacity() < frames * spec.channels.count() {
                        *buf = SampleBuffer::<f32>::new(frames as u64, spec);
                    }
                    buf.copy_interleaved_ref(decoded);
                    self.src_channels = spec.channels.count();
                    self.src_rate = spec.rate;
                    let samples = buf.samples().to_vec();
                    self.resample(&samples);
                    return true;
                }
                Err(SymError::DecodeError(_)) => {}
                Err(_) => return false,
            }
        }
    }

    /// Linear-interpolation resample of interleaved source samples into
    /// `pending`, in engine channel layout.
    fn resample(&mut self, samples: &[f32]) {
        let src_ch = self.src_channels.max(1);
        let out_ch = self.out_channels;
        let frames = samples.len() / src_ch;
        if frames == 0 {
            return;
        }
        let step = f64::from(self.src_rate) / f64::from(self.out_rate);
        let mut mapped = vec![0.0f32; out_ch];

        if (step - 1.0).abs() < 1e-9 {
            // Same rate: map channels straight through.
            self.pending.reserve(frames * out_ch);
            for frame in samples.chunks_exact(src_ch) {
                map_channels(frame, &mut mapped);
                self.pending.extend_from_slice(&mapped);
            }
            if let Some(last) = samples.chunks_exact(src_ch).last() {
                map_channels(last, &mut mapped);
                self.last_frame.copy_from_slice(&mapped);
            }
            return;
        }

        // `phase` is the fractional distance from last_frame to frame 0 of
        // this batch for the next output sample. Walk output positions.
        let mut prev = self.last_frame.clone();
        let mut phase = self.phase;
        for frame in samples.chunks_exact(src_ch) {
            map_channels(frame, &mut mapped);
            while phase < 1.0 {
                #[allow(clippy::cast_possible_truncation)]
                let t = phase as f32;
                for c in 0..out_ch {
                    let v = prev[c] + (mapped[c] - prev[c]) * t;
                    self.pending.push(v);
                }
                phase += step;
            }
            phase -= 1.0;
            prev.copy_from_slice(&mapped);
        }
        self.last_frame.copy_from_slice(&prev);
        self.phase = phase;
    }

    fn pending_frames(&self) -> usize {
        (self.pending.len() - self.pending_pos) / self.out_channels
    }

    fn compact(&mut self) {
        if self.pending_pos > 0 && self.pending_pos >= self.pending.len() / 2 {
            self.pending.drain(..self.pending_pos);
            self.pending_pos = 0;
        }
    }
}

/// Channel-map one source frame into `out.len()` samples: mono is copied
/// to every channel, many-to-one averages, otherwise channels line up and
/// extras are dropped or zero-filled.
fn map_channels(frame: &[f32], out: &mut [f32]) {
    match (frame.len(), out.len()) {
        (0, _) => out.fill(0.0),
        (1, _) => out.fill(frame[0]),
        (n, 1) => {
            #[allow(clippy::cast_precision_loss)]
            let avg = frame.iter().sum::<f32>() / n as f32;
            out[0] = avg;
        }
        (n, _) => {
            for (i, o) in out.iter_mut().enumerate() {
                *o = if i < n { frame[i] } else { 0.0 };
            }
        }
    }
}

trait FromU64 {
    fn from_u64(v: u64) -> f64;
}

impl FromU64 for f64 {
    #[allow(clippy::cast_precision_loss)]
    fn from_u64(v: u64) -> f64 {
        v as f64
    }
}

impl Source for FileSource {
    fn read(&mut self, out: &mut [f32]) -> usize {
        let ch = self.out_channels;
        let want = out.len() / ch;
        let mut done = 0;
        while done < want {
            if self.pending_frames() == 0 {
                self.compact();
                if self.ended || !self.decode_more() {
                    self.ended = true;
                    break;
                }
                continue;
            }
            if self.skip > 0 {
                let drop = usize::try_from(self.skip)
                    .unwrap_or(usize::MAX)
                    .min(self.pending_frames());
                self.pending_pos += drop * ch;
                self.skip -= drop as u64;
                continue;
            }
            let take = (want - done).min(self.pending_frames());
            let start = self.pending_pos;
            let end = start + take * ch;
            out[done * ch..(done + take) * ch].copy_from_slice(&self.pending[start..end]);
            self.pending_pos = end;
            done += take;
        }
        self.out_frames += done as u64;
        done
    }

    fn channels(&self) -> usize {
        self.out_channels
    }

    fn sample_rate(&self) -> u32 {
        self.out_rate
    }

    fn seek(&mut self, to: Duration) -> bool {
        let time = Time::new(to.as_secs(), f64::from(to.subsec_nanos()) / 1e9);
        let seeked = self.reader.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time,
                track_id: Some(self.track_id),
            },
        );
        match seeked {
            Ok(s) => {
                self.decoder.reset();
                self.pending.clear();
                self.pending_pos = 0;
                self.phase = 1.0;
                self.last_frame.fill(0.0);
                self.out_frames = 0;
                self.ended = false;
                let tb = self
                    .reader
                    .tracks()
                    .iter()
                    .find(|t| t.id == self.track_id)
                    .and_then(|t| t.codec_params.time_base);
                // Containers seek to a packet boundary at or before the
                // target; drop the frames between there and the target so
                // the position is exact.
                let landed = tb.map_or(to, |tb| {
                    let t = tb.calc_time(s.actual_ts);
                    Duration::from_secs_f64(f64::from_u64(t.seconds) + t.frac)
                });
                let early = to.saturating_sub(landed);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                {
                    self.skip = (early.as_secs_f64() * f64::from(self.out_rate)).round() as u64;
                }
                self.base = landed.max(to);
                true
            }
            Err(_) => false,
        }
    }

    fn position(&self) -> Option<Duration> {
        let secs = f64::from_u64(self.out_frames) / f64::from(self.out_rate);
        Some(self.base + Duration::from_secs_f64(secs))
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A little-endian 16-bit PCM WAV of a mono sine.
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    fn wav_sine(hz: f32, rate: u32, secs: f32, channels: u16) -> Vec<u8> {
        let frames = (rate as f32 * secs) as usize;
        let data_len = frames * channels as usize * 2;
        let mut v = Vec::with_capacity(44 + data_len);
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
        v.extend_from_slice(b"WAVEfmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
        v.extend_from_slice(&(channels * 2).to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&(data_len as u32).to_le_bytes());
        for i in 0..frames {
            let s = (i as f32 * std::f32::consts::TAU * hz / rate as f32).sin() * 0.5;
            let q = (s * 32767.0) as i16;
            for _ in 0..channels {
                v.extend_from_slice(&q.to_le_bytes());
            }
        }
        v
    }

    fn open(bytes: Vec<u8>, rate: u32, ch: usize) -> FileSource {
        FileSource::open(Box::new(Cursor::new(bytes)), Some("wav"), rate, ch).unwrap()
    }

    fn read_all(s: &mut FileSource) -> Vec<f32> {
        let mut all = Vec::new();
        let mut buf = vec![0.0; 1024 * s.channels()];
        loop {
            let n = s.read(&mut buf);
            if n == 0 {
                break;
            }
            all.extend_from_slice(&buf[..n * s.channels()]);
        }
        all
    }

    fn zero_crossings_per_sec(x: &[f32], ch: usize, rate: u32) -> f32 {
        let mono: Vec<f32> = x.chunks_exact(ch).map(|f| f[0]).collect();
        let zc = mono
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        #[allow(clippy::cast_precision_loss)]
        {
            zc as f32 / (mono.len() as f32 / rate as f32)
        }
    }

    #[test]
    fn same_rate_mono_to_stereo() {
        let mut s = open(wav_sine(440.0, 48_000, 1.0, 1), 48_000, 2);
        assert_eq!(s.duration(), Some(Duration::from_secs(1)));
        let out = read_all(&mut s);
        assert_eq!(out.len(), 48_000 * 2);
        assert!((out[101] - out[100]).abs() < 1e-6, "channels duplicated");
        let zc = zero_crossings_per_sec(&out, 2, 48_000);
        assert!((zc - 880.0).abs() < 5.0, "{zc}");
    }

    #[test]
    fn resamples_44100_to_48000_keeping_pitch() {
        let mut s = open(wav_sine(440.0, 44_100, 2.0, 2), 48_000, 2);
        let out = read_all(&mut s);
        let frames = out.len() / 2;
        assert!((frames as i64 - 96_000).abs() < 20, "{frames}");
        let zc = zero_crossings_per_sec(&out, 2, 48_000);
        assert!((zc - 880.0).abs() < 5.0, "{zc}");
        let peak = out.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak > 0.45 && peak <= 0.5, "{peak}");
    }

    #[test]
    fn downsamples_and_downmixes() {
        let mut s = open(wav_sine(300.0, 48_000, 1.0, 2), 44_100, 1);
        let out = read_all(&mut s);
        assert!((out.len() as i64 - 44_100).abs() < 20, "{}", out.len());
        let zc = zero_crossings_per_sec(&out, 1, 44_100);
        assert!((zc - 600.0).abs() < 5.0, "{zc}");
    }

    #[test]
    fn seek_moves_position_and_shortens_the_rest() {
        let mut s = open(wav_sine(440.0, 48_000, 2.0, 1), 48_000, 1);
        let mut buf = vec![0.0; 4096];
        s.read(&mut buf);
        assert!(s.seek(Duration::from_millis(1500)));
        let p = s.position().unwrap();
        assert!((p.as_secs_f32() - 1.5).abs() < 0.01, "{p:?}");
        let rest = read_all(&mut s);
        assert!((rest.len() as i64 - 24_000).abs() < 100, "{}", rest.len());
        let p = s.position().unwrap();
        assert!((p.as_secs_f32() - 2.0).abs() < 0.01, "{p:?}");
    }

    #[test]
    fn garbage_is_rejected() {
        let r = FileSource::open(Box::new(Cursor::new(vec![0u8; 4096])), None, 48_000, 2);
        assert!(r.is_err());
    }
}
