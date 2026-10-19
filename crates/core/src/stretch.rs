//! Playback speed without pitch shift.
//!
//! [`TimeStretch`] wraps a [`Source`] and plays it faster or slower using
//! WSOLA (waveform-similarity overlap-add). Speech at 1.8x still sounds like
//! the same narrator. The speed is read from a [`SpeedControl`], a lock-free
//! handle a UI thread can write while the audio thread renders.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use crate::source::Source;

/// Tuning for the stretcher. Defaults suit speech.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct StretchConfig {
    /// Analysis frame length. Longer is smoother on music, blurrier on
    /// consonants.
    pub frame_ms: f32,
    /// How far either side of the nominal position to search for the best
    /// splice.
    pub tolerance_ms: f32,
    /// Slowest speed the control will accept.
    pub min_speed: f32,
    /// Fastest speed the control will accept.
    pub max_speed: f32,
}

impl Default for StretchConfig {
    fn default() -> Self {
        Self {
            frame_ms: 30.0,
            tolerance_ms: 10.0,
            min_speed: 0.5,
            max_speed: 4.0,
        }
    }
}

/// A shared, lock-free playback speed. Clone it freely; all clones agree.
#[derive(Debug, Clone)]
pub struct SpeedControl {
    bits: Arc<AtomicU32>,
    min: f32,
    max: f32,
}

impl SpeedControl {
    /// A control at 1.0x, clamped to the config's range.
    #[must_use]
    pub fn new(cfg: &StretchConfig) -> Self {
        Self {
            bits: Arc::new(AtomicU32::new(1.0f32.to_bits())),
            min: cfg.min_speed,
            max: cfg.max_speed,
        }
    }

    /// Set the speed, clamped to the allowed range. Returns what was set.
    #[allow(clippy::must_use_candidate)]
    pub fn set(&self, speed: f32) -> f32 {
        let s = if speed.is_finite() {
            speed.clamp(self.min, self.max)
        } else {
            1.0
        };
        self.bits.store(s.to_bits(), Ordering::Relaxed);
        s
    }

    /// The current speed.
    #[must_use]
    pub fn get(&self) -> f32 {
        f32::from_bits(self.bits.load(Ordering::Relaxed))
    }
}

/// WSOLA time-stretcher over any [`Source`].
pub struct TimeStretch<S: Source> {
    inner: S,
    speed: SpeedControl,
    channels: usize,
    rate: u32,
    frame: usize,
    hop: usize,
    tolerance: usize,
    window: Vec<f32>,
    /// Interleaved input not yet fully consumed.
    input: Vec<f32>,
    /// Channel-summed mirror of `input`, for the similarity search.
    mono: Vec<f32>,
    /// Absolute frame index (since the last seek) of `input[0]`.
    input_origin: u64,
    input_ended: bool,
    /// Nominal analysis position, in frames relative to `input[0]`.
    nominal: f64,
    /// Start of the previously spliced segment, relative to `input[0]`.
    prev: Option<usize>,
    /// Output frames synthesised so far (for placing the next segment).
    synth_frames: usize,
    /// Absolute output frame just past the last sample written.
    synth_end: usize,
    /// Overlap-add accumulator, interleaved, from `ola_origin`.
    ola: Vec<f32>,
    /// Window weight per frame of `ola`, for normalisation.
    ola_w: Vec<f32>,
    /// Absolute output frame index of `ola[0]`.
    ola_origin: usize,
    /// Frames of `ola` that are final and not yet handed out.
    ready: usize,
    done: bool,
}

impl<S: Source> std::fmt::Debug for TimeStretch<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TimeStretch")
            .field("speed", &self.speed.get())
            .field("frame", &self.frame)
            .field("hop", &self.hop)
            .field("done", &self.done)
            .finish_non_exhaustive()
    }
}

impl<S: Source> TimeStretch<S> {
    /// Wrap `inner`. The returned stretcher reads its speed from `speed`.
    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    pub fn new(inner: S, cfg: &StretchConfig, speed: SpeedControl) -> Self {
        let rate = inner.sample_rate();
        let channels = inner.channels();
        let frame = ((cfg.frame_ms * rate as f32 / 1000.0).round() as usize).max(64) & !1;
        let hop = frame / 2;
        let tolerance = ((cfg.tolerance_ms * rate as f32 / 1000.0).round() as usize).min(hop);
        let window = (0..frame)
            .map(|i| {
                let x = std::f32::consts::TAU * i as f32 / frame as f32;
                0.5 - 0.5 * x.cos()
            })
            .collect();
        Self {
            inner,
            speed,
            channels,
            rate,
            frame,
            hop,
            tolerance,
            window,
            input: Vec::new(),
            mono: Vec::new(),
            input_origin: 0,
            input_ended: false,
            nominal: 0.0,
            prev: None,
            synth_frames: 0,
            synth_end: 0,
            ola: Vec::new(),
            ola_w: Vec::new(),
            ola_origin: 0,
            ready: 0,
            done: false,
        }
    }

    /// The wrapped source.
    pub fn inner(&self) -> &S {
        &self.inner
    }

    /// The wrapped source, mutably.
    pub fn inner_mut(&mut self) -> &mut S {
        &mut self.inner
    }

    /// The speed handle this stretcher reads.
    #[must_use]
    pub fn speed(&self) -> &SpeedControl {
        &self.speed
    }

    fn input_frames(&self) -> usize {
        self.input.len() / self.channels
    }

    /// Make sure at least `frames` frames of input are buffered, or the
    /// inner source has ended.
    fn fill_input(&mut self, frames: usize) {
        let ch = self.channels;
        while !self.input_ended && self.input_frames() < frames {
            let want = (frames - self.input_frames()).max(self.frame) * ch;
            let start = self.input.len();
            self.input.resize(start + want, 0.0);
            let got = self.inner.read(&mut self.input[start..]);
            self.input.truncate(start + got * ch);
            if got * ch < want {
                self.input_ended = true;
            }
            for f in self.input[start..].chunks_exact(ch) {
                self.mono.push(f.iter().sum());
            }
        }
    }

    /// Normalised cross-correlation of two mono spans of `len` frames.
    fn similarity(&self, a: usize, b: usize, len: usize) -> f32 {
        let xa = &self.mono[a..a + len];
        let xb = &self.mono[b..b + len];
        let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
        for (p, q) in xa.iter().zip(xb) {
            dot = p.mul_add(*q, dot);
            na = p.mul_add(*p, na);
            nb = q.mul_add(*q, nb);
        }
        if na <= 1e-12 || nb <= 1e-12 {
            return 0.0;
        }
        dot / (na * nb).sqrt()
    }

    /// Pick the splice point near `nominal` that best continues the
    /// previous segment. Coarse search, then refine.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn choose(&self, nominal: usize, avail: usize) -> usize {
        let Some(prev) = self.prev else {
            return nominal.min(avail.saturating_sub(self.frame));
        };
        let template = prev + self.hop;
        let max_start = avail.saturating_sub(self.frame);
        let lo = nominal.saturating_sub(self.tolerance);
        let hi = (nominal + self.tolerance).min(max_start);
        if lo >= hi || template + self.hop > avail {
            return nominal.min(max_start);
        }
        let overlap = self.hop;
        let mut best = (nominal.min(max_start), f32::MIN);
        let coarse = 4;
        let mut c = lo;
        while c <= hi {
            let s = self.similarity(c, template, overlap);
            if s > best.1 {
                best = (c, s);
            }
            c += coarse;
        }
        let (centre, _) = best;
        for c in centre.saturating_sub(coarse - 1)..=(centre + coarse - 1).min(hi) {
            if c >= lo {
                let s = self.similarity(c, template, overlap);
                if s > best.1 {
                    best = (c, s);
                }
            }
        }
        best.0
    }

    /// Synthesise one hop of output.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    fn step(&mut self) {
        let ch = self.channels;
        let speed = f64::from(self.speed.get());
        let nominal = self.nominal.max(0.0) as usize;
        self.fill_input(nominal + self.tolerance + self.frame);
        let avail = self.input_frames();

        if avail == 0 || nominal >= avail {
            self.finish();
            return;
        }

        let start = if (speed - 1.0).abs() < 1e-6 {
            nominal
        } else {
            self.choose(nominal, avail)
        };
        let len = self.frame.min(avail - start);

        // Grow the accumulator to hold this segment.
        let out_pos = self.synth_frames - self.ola_origin;
        let need = (out_pos + self.frame) * ch;
        if self.ola.len() < need {
            self.ola.resize(need, 0.0);
            self.ola_w.resize(out_pos + self.frame, 0.0);
        }
        for i in 0..len {
            let w = self.window[i];
            self.ola_w[out_pos + i] += w;
            let src = (start + i) * ch;
            let dst = (out_pos + i) * ch;
            for c in 0..ch {
                self.ola[dst + c] = self.input[src + c].mul_add(w, self.ola[dst + c]);
            }
        }

        self.prev = Some(start);
        self.synth_end = self.synth_end.max(self.synth_frames + len);
        self.synth_frames += self.hop;
        self.nominal += self.hop as f64 * speed;
        // Frames before the next segment's start are final.
        self.ready = self.synth_frames - self.ola_origin;

        if self.input_ended && start + len >= avail && len < self.frame {
            self.finish();
        }
        self.trim_input();
    }

    /// Mark everything synthesised so far as final and stop.
    fn finish(&mut self) {
        self.done = true;
        let end = self.synth_end.saturating_sub(self.ola_origin);
        self.ola.truncate(end * self.channels);
        self.ola_w.truncate(end);
        self.ready = end;
    }

    /// Drop input that no later step can reference.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    fn trim_input(&mut self) {
        let keep_from_nominal = (self.nominal.max(0.0) as usize).saturating_sub(self.tolerance);
        let keep_from_prev = self.prev.unwrap_or(usize::MAX);
        let drop = keep_from_nominal
            .min(keep_from_prev)
            .min(self.input_frames());
        if drop < self.frame * 4 {
            return;
        }
        self.input.drain(..drop * self.channels);
        self.mono.drain(..drop);
        self.input_origin += drop as u64;
        self.nominal -= drop as f64;
        self.prev = self.prev.map(|p| p - drop);
    }

    /// Hand out up to `frames` final frames, normalised by window weight.
    fn drain(&mut self, out: &mut [f32]) -> usize {
        let ch = self.channels;
        let frames = (out.len() / ch).min(self.ready);
        for f in 0..frames {
            let w = self.ola_w[f];
            let g = if w > 1e-6 { 1.0 / w } else { 0.0 };
            for c in 0..ch {
                out[f * ch + c] = self.ola[f * ch + c] * g;
            }
        }
        self.ola.drain(..frames * ch);
        self.ola_w.drain(..frames);
        self.ola_origin += frames;
        self.ready -= frames;
        frames
    }

    fn reset_buffers(&mut self) {
        self.input.clear();
        self.mono.clear();
        self.input_ended = false;
        self.nominal = 0.0;
        self.prev = None;
        self.synth_frames = 0;
        self.synth_end = 0;
        self.ola.clear();
        self.ola_w.clear();
        self.ola_origin = 0;
        self.ready = 0;
        self.done = false;
    }
}

impl<S: Source> Source for TimeStretch<S> {
    fn read(&mut self, out: &mut [f32]) -> usize {
        let ch = self.channels;
        let want = out.len() / ch;
        let mut written = 0;
        while written < want {
            if self.ready == 0 {
                if self.done {
                    break;
                }
                self.step();
                continue;
            }
            written += self.drain(&mut out[written * ch..want * ch]);
        }
        written
    }

    fn channels(&self) -> usize {
        self.channels
    }

    fn sample_rate(&self) -> u32 {
        self.rate
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn seek(&mut self, to: Duration) -> bool {
        if !self.inner.seek(to) {
            return false;
        }
        self.reset_buffers();
        self.input_origin = (to.as_secs_f64() * f64::from(self.rate)).round() as u64;
        true
    }

    #[allow(clippy::cast_precision_loss)]
    fn position(&self) -> Option<Duration> {
        // Content time of the next frame to be spliced, which is what the
        // listener is hearing give or take one frame of latency.
        let frames = self.input_origin as f64 + self.nominal.max(0.0);
        Some(Duration::from_secs_f64(frames / f64::from(self.rate)))
    }

    fn duration(&self) -> Option<Duration> {
        self.inner.duration()
    }
}

#[cfg(test)]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
mod tests {
    use super::*;
    use crate::source::SliceSource;

    const RATE: u32 = 48_000;

    fn stretch(src: SliceSource, speed: f32) -> TimeStretch<SliceSource> {
        let cfg = StretchConfig::default();
        let ctl = SpeedControl::new(&cfg);
        ctl.set(speed);
        TimeStretch::new(src, &cfg, ctl)
    }

    fn read_all(s: &mut impl Source) -> Vec<f32> {
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

    fn zero_crossings(x: &[f32]) -> usize {
        x.windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count()
    }

    #[test]
    fn unity_speed_reproduces_the_input() {
        let frames = RATE as usize;
        let src = SliceSource::sine(440.0, 0.5, frames, RATE);
        let original = src.clone();
        let mut s = stretch(src, 1.0);
        let out = read_all(&mut s);
        assert_eq!(out.len(), frames, "same length at 1.0x");
        let mut orig = Vec::new();
        let mut o = original;
        orig.resize(frames, 0.0);
        o.read(&mut orig);
        let max_err = out
            .iter()
            .zip(&orig)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(max_err < 1e-4, "max error {max_err}");
    }

    #[test]
    fn double_speed_halves_length_and_keeps_pitch() {
        let frames = 2 * RATE as usize;
        let src = SliceSource::sine(440.0, 0.5, frames, RATE);
        let mut s = stretch(src, 2.0);
        let out = read_all(&mut s);
        let ratio = out.len() as f32 / frames as f32;
        assert!((ratio - 0.5).abs() < 0.03, "length ratio {ratio}");
        // 440 Hz gives 880 zero crossings per second, at any speed.
        let per_sec = zero_crossings(&out) as f32 / (out.len() as f32 / RATE as f32);
        assert!((per_sec - 880.0).abs() < 20.0, "crossings/s {per_sec}");
    }

    #[test]
    fn half_speed_doubles_length_and_keeps_pitch() {
        let frames = RATE as usize;
        let src = SliceSource::sine(220.0, 0.5, frames, RATE);
        let mut s = stretch(src, 0.5);
        let out = read_all(&mut s);
        let ratio = out.len() as f32 / frames as f32;
        assert!((ratio - 2.0).abs() < 0.06, "length ratio {ratio}");
        let per_sec = zero_crossings(&out) as f32 / (out.len() as f32 / RATE as f32);
        assert!((per_sec - 440.0).abs() < 15.0, "crossings/s {per_sec}");
    }

    #[test]
    fn stereo_is_supported() {
        let frames = 2 * RATE as usize;
        let mut samples = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let t = i as f32 / RATE as f32;
            samples.push((t * 440.0 * std::f32::consts::TAU).sin());
            samples.push((t * 660.0 * std::f32::consts::TAU).sin());
        }
        let src = SliceSource::new(samples, 2, RATE);
        let mut s = stretch(src, 1.5);
        let out = read_all(&mut s);
        assert_eq!(out.len() % 2, 0);
        let ratio = (out.len() / 2) as f32 / frames as f32;
        assert!((ratio - 1.0 / 1.5).abs() < 0.05, "ratio {ratio}");
    }

    #[test]
    fn speed_can_change_mid_stream() {
        let frames = 2 * RATE as usize;
        let src = SliceSource::sine(300.0, 0.5, frames, RATE);
        let cfg = StretchConfig::default();
        let ctl = SpeedControl::new(&cfg);
        ctl.set(1.0);
        let mut s = TimeStretch::new(src, &cfg, ctl.clone());
        let mut buf = vec![0.0; RATE as usize];
        let first = s.read(&mut buf);
        assert_eq!(first, RATE as usize);
        ctl.set(2.0);
        let rest = read_all(&mut s);
        // One second left at 2x is about half a second of output.
        let ratio = rest.len() as f32 / RATE as f32;
        assert!((ratio - 0.5).abs() < 0.08, "ratio {ratio}");
    }

    #[test]
    fn seek_clears_and_reports_position() {
        let src = SliceSource::sine(300.0, 0.5, 3 * RATE as usize, RATE);
        let mut s = stretch(src, 1.5);
        let mut buf = vec![0.0; 4096];
        s.read(&mut buf);
        assert!(s.seek(Duration::from_secs(2)));
        let p = s.position().unwrap();
        assert!((p.as_secs_f32() - 2.0).abs() < 1e-3, "{p:?}");
        let out = read_all(&mut s);
        let ratio = out.len() as f32 / RATE as f32;
        assert!((ratio - 1.0 / 1.5).abs() < 0.05, "ratio after seek {ratio}");
    }

    #[test]
    fn speed_control_clamps() {
        let ctl = SpeedControl::new(&StretchConfig::default());
        assert!((ctl.set(10.0) - 4.0).abs() < f32::EPSILON);
        assert!((ctl.set(0.1) - 0.5).abs() < f32::EPSILON);
        assert!((ctl.set(f32::NAN) - 1.0).abs() < f32::EPSILON);
        assert!((ctl.get() - 1.0).abs() < f32::EPSILON);
    }
}
