//! Where samples come from.
//!
//! A [`Source`] yields interleaved `f32` frames at a fixed channel count and
//! sample rate. Decoders, network clients, and generators all implement it
//! in other crates; this crate ships only the trivial ones tests and shells
//! need.

use std::time::Duration;

/// A pull-based stream of interleaved `f32` audio.
pub trait Source: Send {
    /// Fill `out` with interleaved samples and return the number of
    /// *frames* written. `out.len()` is a multiple of [`Source::channels`].
    /// Returning fewer frames than requested means the stream has ended;
    /// later calls return zero.
    fn read(&mut self, out: &mut [f32]) -> usize;

    /// Interleaved channel count. Fixed for the life of the source.
    fn channels(&self) -> usize;

    /// Sample rate in Hz. Fixed for the life of the source.
    fn sample_rate(&self) -> u32;

    /// Move to `to`, measured in content time. Returns `false` if the
    /// source cannot seek, in which case its position is unchanged.
    fn seek(&mut self, to: Duration) -> bool {
        let _ = to;
        false
    }

    /// Current position in content time, if known.
    fn position(&self) -> Option<Duration> {
        None
    }

    /// Total length in content time, if known.
    fn duration(&self) -> Option<Duration> {
        None
    }
}

impl Source for Box<dyn Source> {
    fn read(&mut self, out: &mut [f32]) -> usize {
        (**self).read(out)
    }
    fn channels(&self) -> usize {
        (**self).channels()
    }
    fn sample_rate(&self) -> u32 {
        (**self).sample_rate()
    }
    fn seek(&mut self, to: Duration) -> bool {
        (**self).seek(to)
    }
    fn position(&self) -> Option<Duration> {
        (**self).position()
    }
    fn duration(&self) -> Option<Duration> {
        (**self).duration()
    }
}

/// Endless silence.
#[derive(Debug, Clone, Copy)]
pub struct Silence {
    channels: usize,
    sample_rate: u32,
}

impl Silence {
    /// Silence at the given format.
    #[must_use]
    pub fn new(channels: usize, sample_rate: u32) -> Self {
        Self {
            channels,
            sample_rate,
        }
    }
}

impl Source for Silence {
    fn read(&mut self, out: &mut [f32]) -> usize {
        out.fill(0.0);
        out.len() / self.channels
    }
    fn channels(&self) -> usize {
        self.channels
    }
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}

/// A source backed by a buffer of interleaved samples. Seekable.
#[derive(Debug, Clone)]
pub struct SliceSource {
    samples: Vec<f32>,
    channels: usize,
    sample_rate: u32,
    cursor: usize,
}

impl SliceSource {
    /// Wrap `samples`, which must hold whole frames.
    ///
    /// # Panics
    /// If `channels` is zero or `samples.len()` is not a multiple of it.
    #[must_use]
    pub fn new(samples: Vec<f32>, channels: usize, sample_rate: u32) -> Self {
        assert!(channels > 0, "channels must be positive");
        assert!(
            samples.len().is_multiple_of(channels),
            "samples must hold whole frames"
        );
        Self {
            samples,
            channels,
            sample_rate,
            cursor: 0,
        }
    }

    /// A mono sine wave, handy for tests and demos.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn sine(hz: f32, amplitude: f32, frames: usize, sample_rate: u32) -> Self {
        let step = std::f32::consts::TAU * hz / sample_rate as f32;
        let samples = (0..frames)
            .map(|i| (i as f32 * step).sin() * amplitude)
            .collect();
        Self::new(samples, 1, sample_rate)
    }

    /// Frames remaining before the end.
    #[must_use]
    pub fn remaining_frames(&self) -> usize {
        (self.samples.len() - self.cursor) / self.channels
    }

    #[allow(clippy::cast_precision_loss)]
    fn frames_to_duration(&self, frames: usize) -> Duration {
        Duration::from_secs_f64(frames as f64 / f64::from(self.sample_rate))
    }
}

impl Source for SliceSource {
    fn read(&mut self, out: &mut [f32]) -> usize {
        let want = out.len() / self.channels * self.channels;
        let have = (self.samples.len() - self.cursor).min(want);
        out[..have].copy_from_slice(&self.samples[self.cursor..self.cursor + have]);
        self.cursor += have;
        have / self.channels
    }

    fn channels(&self) -> usize {
        self.channels
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn seek(&mut self, to: Duration) -> bool {
        let frame = (to.as_secs_f64() * f64::from(self.sample_rate)).round() as usize;
        let total = self.samples.len() / self.channels;
        self.cursor = frame.min(total) * self.channels;
        true
    }

    fn position(&self) -> Option<Duration> {
        Some(self.frames_to_duration(self.cursor / self.channels))
    }

    fn duration(&self) -> Option<Duration> {
        Some(self.frames_to_duration(self.samples.len() / self.channels))
    }
}

/// A source scaled by a fixed gain. For level offsets that belong to the
/// material rather than to the listener's level control.
pub struct Gain {
    inner: Box<dyn Source>,
    gain: f32,
}

impl Gain {
    /// Scale `inner` by `gain`, a linear factor.
    #[must_use]
    pub fn new(inner: Box<dyn Source>, gain: f32) -> Self {
        Self {
            inner,
            gain: gain.max(0.0),
        }
    }

    /// The factor applied.
    #[must_use]
    pub fn gain(&self) -> f32 {
        self.gain
    }
}

impl std::fmt::Debug for Gain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gain")
            .field("gain", &self.gain)
            .finish_non_exhaustive()
    }
}

impl Source for Gain {
    fn read(&mut self, out: &mut [f32]) -> usize {
        let got = self.inner.read(out);
        if (self.gain - 1.0).abs() > f32::EPSILON {
            for s in &mut out[..got * self.inner.channels()] {
                *s *= self.gain;
            }
        }
        got
    }
    fn channels(&self) -> usize {
        self.inner.channels()
    }
    fn sample_rate(&self) -> u32 {
        self.inner.sample_rate()
    }
    fn seek(&mut self, to: Duration) -> bool {
        self.inner.seek(to)
    }
    fn position(&self) -> Option<Duration> {
        self.inner.position()
    }
    fn duration(&self) -> Option<Duration> {
        self.inner.duration()
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn slice_source_reads_then_ends() {
        let mut s = SliceSource::new(vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 2, 10);
        let mut out = [0.0; 4];
        assert_eq!(s.read(&mut out), 2);
        assert_eq!(out, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(s.read(&mut out), 1);
        assert_eq!(&out[..2], &[5.0, 6.0]);
        assert_eq!(s.read(&mut out), 0);
    }

    #[test]
    fn slice_source_seeks_in_content_time() {
        let mut s = SliceSource::new(vec![0.0; 100], 1, 10);
        assert!(s.seek(Duration::from_secs(3)));
        assert_eq!(s.remaining_frames(), 70);
        assert_eq!(s.position(), Some(Duration::from_secs(3)));
        assert_eq!(s.duration(), Some(Duration::from_secs(10)));
    }

    #[test]
    fn gain_scales_and_passes_everything_else_through() {
        let inner = Box::new(SliceSource::new(vec![1.0, -1.0, 0.5, 0.0], 2, 10));
        let mut g = Gain::new(inner, 0.5);
        assert_eq!(g.duration(), Some(Duration::from_millis(200)));
        let mut out = [0.0; 4];
        assert_eq!(g.read(&mut out), 2);
        assert_eq!(out, [0.5, -0.5, 0.25, 0.0]);
        assert!(g.seek(Duration::ZERO));
        assert_eq!(g.read(&mut out[..2]), 1);
        assert_eq!(&out[..2], &[0.5, -0.5]);
    }

    #[test]
    fn silence_never_ends() {
        let mut s = Silence::new(2, 48_000);
        let mut out = [1.0; 8];
        assert_eq!(s.read(&mut out), 4);
        assert!(out.iter().all(|v| *v == 0.0));
    }
}
