//! Play part of a source, and optionally keep playing it.
//!
//! A [`Region`] wraps any [`Source`] and exposes a window of it: from an in
//! point to an out point. It can stop there, or repeat the window with a
//! crossfade at the seam so a two-minute passage can sit under a
//! twenty-minute chapter without an audible restart.
//!
//! The seam needs the first few seconds of the window at the same time as
//! the last few, and a single seekable source cannot be in two places at
//! once. So the head of the window is read once at construction and kept.
//! Nothing allocates after that.

use std::time::Duration;

use crate::fade::Curve;
use crate::source::Source;

/// Which part of a source to play, and whether to keep playing it.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RegionConfig {
    /// In point, in the source's content time.
    pub start: Duration,
    /// Out point. `None` means the end of the source.
    pub end: Option<Duration>,
    /// Go back to the in point instead of ending.
    pub repeat: bool,
    /// Length of the crossfade at the seam when repeating. Zero is a hard
    /// cut. Clamped to half the window.
    pub crossfade: Duration,
    /// Shape of that crossfade.
    pub curve: Curve,
}

impl Default for RegionConfig {
    fn default() -> Self {
        Self {
            start: Duration::ZERO,
            end: None,
            repeat: false,
            crossfade: Duration::ZERO,
            curve: Curve::EqualPower,
        }
    }
}

impl RegionConfig {
    /// A window from `start` to `end`, played once.
    #[must_use]
    pub fn window(start: Duration, end: Duration) -> Self {
        Self {
            start,
            end: Some(end),
            ..Self::default()
        }
    }

    /// The same window, repeated with `crossfade` at the seam.
    #[must_use]
    pub fn repeating(mut self, crossfade: Duration) -> Self {
        self.repeat = true;
        self.crossfade = crossfade;
        self
    }
}

/// A window onto another source. See the module docs.
pub struct Region {
    inner: Box<dyn Source>,
    channels: usize,
    sample_rate: u32,
    start: Duration,
    /// Window length in frames, when the out point is known.
    len: Option<usize>,
    repeat: bool,
    curve: Curve,
    /// The first `seam` frames of the window, interleaved.
    head: Vec<f32>,
    /// Crossfade length in frames. Zero when not repeating.
    seam: usize,
    /// Frames since the in point, within the current pass.
    cursor: usize,
    /// The last read returned nothing; a second one in a row ends us.
    starved: bool,
    ended: bool,
}

impl std::fmt::Debug for Region {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Region")
            .field("start", &self.start)
            .field("len", &self.len)
            .field("repeat", &self.repeat)
            .field("seam", &self.seam)
            .field("cursor", &self.cursor)
            .field("ended", &self.ended)
            .finish_non_exhaustive()
    }
}

impl Region {
    /// Open a window onto `inner`.
    ///
    /// # Errors
    /// The source back, unchanged in format but possibly moved, when the
    /// window is empty, when `repeat` is asked for without a known out
    /// point, or when the source cannot seek to the in point.
    pub fn new(mut inner: Box<dyn Source>, cfg: RegionConfig) -> Result<Self, Box<dyn Source>> {
        let channels = inner.channels();
        let sample_rate = inner.sample_rate();
        let frames = |d: Duration| frames_at(d, sample_rate);

        let end = cfg.end.or_else(|| inner.duration());
        if end.is_some_and(|e| e <= cfg.start) {
            return Err(inner);
        }
        if cfg.repeat && end.is_none() {
            return Err(inner);
        }
        let needs_seek = cfg.repeat || cfg.start > Duration::ZERO;
        if needs_seek && !inner.seek(cfg.start) {
            return Err(inner);
        }

        let len = end.map(|e| frames(e.saturating_sub(cfg.start)));
        let mut seam = 0;
        let mut head = Vec::new();
        if let (true, Some(len)) = (cfg.repeat, len)
            && cfg.crossfade > Duration::ZERO
        {
            seam = frames(cfg.crossfade).min(len / 2);
            head = vec![0.0; seam * channels];
            let got = read_fully(&mut *inner, &mut head, channels);
            head.truncate(got * channels);
            seam = got;
            if !inner.seek(cfg.start) {
                return Err(inner);
            }
        }

        Ok(Self {
            inner,
            channels,
            sample_rate,
            start: cfg.start,
            len,
            repeat: cfg.repeat,
            curve: cfg.curve,
            head,
            seam,
            cursor: 0,
            starved: false,
            ended: false,
        })
    }

    /// The window length, when the out point is known.
    #[must_use]
    pub fn window(&self) -> Option<Duration> {
        self.len.map(|n| self.frames_to_duration(n))
    }

    /// Jump back to the top of the window, past the part the seam already
    /// played. Ends the region if the source will not seek.
    fn restart(&mut self) {
        let to = self.start + self.frames_to_duration(self.seam);
        if self.inner.seek(to) {
            self.cursor = self.seam;
        } else {
            self.ended = true;
        }
    }

    /// Hard restart from the in point, for when the source ran dry early.
    fn restart_hard(&mut self) {
        if self.inner.seek(self.start) {
            self.cursor = 0;
        } else {
            self.ended = true;
        }
    }

    /// Blend `buf`, which holds frames `offset..offset + got` of the seam,
    /// with the stored head of the window.
    #[allow(clippy::cast_precision_loss)]
    fn blend_seam(&self, buf: &mut [f32], offset: usize, got: usize) {
        let ch = self.channels;
        let total = self.seam as f32;
        for f in 0..got {
            let t = (offset + f) as f32 / total;
            let g_out = self.curve.complement(t);
            let g_in = self.curve.shape(t);
            let h = (offset + f) * ch;
            for c in 0..ch {
                buf[f * ch + c] = buf[f * ch + c].mul_add(g_out, self.head[h + c] * g_in);
            }
        }
    }

    #[allow(clippy::cast_precision_loss)]
    fn frames_to_duration(&self, frames: usize) -> Duration {
        Duration::from_secs_f64(frames as f64 / f64::from(self.sample_rate))
    }
}

impl Source for Region {
    fn read(&mut self, out: &mut [f32]) -> usize {
        let ch = self.channels;
        let want = out.len() / ch;
        let mut done = 0;

        while done < want && !self.ended {
            let Some(len) = self.len else {
                let got = self.inner.read(&mut out[done * ch..want * ch]);
                done += got;
                self.cursor += got;
                if done < want {
                    self.ended = true;
                }
                break;
            };

            if self.cursor >= len {
                if self.repeat {
                    self.restart();
                    continue;
                }
                self.ended = true;
                break;
            }

            let seam_start = len - self.seam;
            let in_seam = self.cursor >= seam_start;
            let phase_end = if in_seam { len } else { seam_start };
            let n = (phase_end - self.cursor).min(want - done);
            let buf = &mut out[done * ch..(done + n) * ch];
            let got = self.inner.read(buf);
            if in_seam {
                self.blend_seam(buf, self.cursor - seam_start, got);
            }
            done += got;
            self.cursor += got;

            if got < n {
                // The source ran dry before the out point.
                if !self.repeat || (got == 0 && self.starved) {
                    self.ended = true;
                    break;
                }
                self.starved = got == 0;
                self.restart_hard();
            } else {
                self.starved = false;
            }
        }
        done
    }

    fn channels(&self) -> usize {
        self.channels
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Seek within the window. `to` is measured from the in point.
    fn seek(&mut self, to: Duration) -> bool {
        let mut frame = frames_at(to, self.sample_rate);
        if let Some(len) = self.len {
            frame = frame.min(len);
        }
        let target = self.start + self.frames_to_duration(frame);
        if !self.inner.seek(target) {
            return false;
        }
        self.cursor = frame;
        self.ended = false;
        self.starved = false;
        true
    }

    /// Position within the window, measured from the in point.
    fn position(&self) -> Option<Duration> {
        Some(self.frames_to_duration(self.cursor))
    }

    /// The window length when played once. `None` when repeating.
    fn duration(&self) -> Option<Duration> {
        if self.repeat { None } else { self.window() }
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn frames_at(d: Duration, sample_rate: u32) -> usize {
    (d.as_secs_f64() * f64::from(sample_rate)).round() as usize
}

/// Read until `buf` is full or the source ends. Returns frames read.
fn read_fully(source: &mut dyn Source, buf: &mut [f32], channels: usize) -> usize {
    let mut done = 0;
    let want = buf.len() / channels;
    while done < want {
        let got = source.read(&mut buf[done * channels..]);
        if got == 0 {
            break;
        }
        done += got;
    }
    done
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::cast_precision_loss)]
mod tests {
    use super::*;
    use crate::source::SliceSource;

    const RATE: u32 = 10;

    /// Mono, one frame per tenth of a second, sample value = frame index.
    fn ramp(frames: usize) -> Box<dyn Source> {
        let samples = (0..frames).map(|i| i as f32).collect();
        Box::new(SliceSource::new(samples, 1, RATE))
    }

    fn secs(s: f64) -> Duration {
        Duration::from_secs_f64(s)
    }

    fn open(inner: Box<dyn Source>, cfg: RegionConfig) -> Region {
        Region::new(inner, cfg).ok().expect("region opens")
    }

    /// Read `frames` frames in blocks of `block`, so phase boundaries land
    /// mid-block as they would in real use.
    fn pull(r: &mut Region, frames: usize, block: usize) -> Vec<f32> {
        let mut all = Vec::new();
        let mut buf = vec![0.0; block];
        while all.len() < frames {
            let got = r.read(&mut buf);
            if got == 0 {
                break;
            }
            all.extend_from_slice(&buf[..got]);
        }
        all.truncate(frames);
        all
    }

    #[test]
    fn plays_only_the_window_then_ends() {
        let cfg = RegionConfig::window(secs(2.0), secs(5.0));
        let mut r = open(ramp(100), cfg);
        assert_eq!(r.duration(), Some(secs(3.0)));
        let got = pull(&mut r, 100, 7);
        let want: Vec<f32> = (20..50).map(|i| i as f32).collect();
        assert_eq!(got, want);
        let mut buf = [0.0; 4];
        assert_eq!(r.read(&mut buf), 0);
    }

    #[test]
    fn open_ended_window_runs_to_the_source_end() {
        let cfg = RegionConfig {
            start: secs(9.0),
            ..RegionConfig::default()
        };
        let mut r = open(ramp(100), cfg);
        assert_eq!(
            pull(&mut r, 100, 3),
            vec![90.0, 91.0, 92.0, 93.0, 94.0, 95.0, 96.0, 97.0, 98.0, 99.0]
        );
    }

    #[test]
    fn repeat_with_hard_cut_wraps_to_the_in_point() {
        let cfg = RegionConfig::window(secs(0.0), secs(1.0)).repeating(Duration::ZERO);
        let mut r = open(ramp(100), cfg);
        assert_eq!(r.duration(), None);
        let got = pull(&mut r, 25, 4);
        let want: Vec<f32> = (0..25).map(|i| (i % 10) as f32).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn repeat_with_crossfade_blends_tail_into_head() {
        // Window of ten frames, four-frame linear seam. The seam plays
        // frames 6..10 of the tail against frames 0..4 of the head, then
        // the next pass resumes at frame 4.
        let cfg = RegionConfig {
            curve: Curve::Linear,
            ..RegionConfig::window(secs(0.0), secs(1.0)).repeating(secs(0.4))
        };
        let mut r = open(ramp(100), cfg);
        let got = pull(&mut r, 16, 5);
        let want = vec![
            0.0, 1.0, 2.0, 3.0, 4.0, 5.0, // plain
            6.0, 5.5, 5.0, 4.5, // seam: 6*1+0*0, 7*.75+1*.25, 8*.5+2*.5, 9*.25+3*.75
            4.0, 5.0, // second pass starts after the head
            6.0, 5.5, 5.0, 4.5, // seam again
        ];
        assert_eq!(got, want);
    }

    #[test]
    fn block_size_does_not_change_the_output() {
        let cfg = RegionConfig {
            curve: Curve::Linear,
            ..RegionConfig::window(secs(1.5), secs(4.0)).repeating(secs(0.7))
        };
        let mut a = open(ramp(100), cfg);
        let mut b = open(ramp(100), cfg);
        assert_eq!(pull(&mut a, 90, 1), pull(&mut b, 90, 64));
    }

    #[test]
    fn crossfade_is_clamped_to_half_the_window() {
        let cfg = RegionConfig::window(secs(0.0), secs(1.0)).repeating(secs(5.0));
        let r = open(ramp(100), cfg);
        assert_eq!(r.seam, 5);
    }

    #[test]
    fn out_point_past_the_end_ends_at_the_source_end() {
        let cfg = RegionConfig::window(secs(8.0), secs(20.0));
        let mut r = open(ramp(100), cfg);
        let got = pull(&mut r, 100, 8);
        assert_eq!(got.len(), 20);
        assert_eq!(got[0], 80.0);
    }

    #[test]
    fn out_point_past_the_end_still_repeats() {
        let cfg = RegionConfig::window(secs(9.5), secs(20.0)).repeating(Duration::ZERO);
        let mut r = open(ramp(100), cfg);
        let got = pull(&mut r, 12, 5);
        assert_eq!(
            got,
            vec![
                95.0, 96.0, 97.0, 98.0, 99.0, 95.0, 96.0, 97.0, 98.0, 99.0, 95.0, 96.0
            ]
        );
    }

    #[test]
    fn seek_is_measured_from_the_in_point() {
        let cfg = RegionConfig::window(secs(2.0), secs(5.0));
        let mut r = open(ramp(100), cfg);
        assert!(r.seek(secs(1.0)));
        assert_eq!(r.position(), Some(secs(1.0)));
        assert_eq!(
            pull(&mut r, 100, 4),
            (30..50).map(|i| i as f32).collect::<Vec<_>>()
        );
    }

    #[test]
    fn bad_windows_are_rejected() {
        assert!(Region::new(ramp(100), RegionConfig::window(secs(5.0), secs(5.0))).is_err());
        assert!(Region::new(ramp(100), RegionConfig::window(secs(6.0), secs(5.0))).is_err());
        let endless = Box::new(crate::source::Silence::new(1, RATE));
        let cfg = RegionConfig {
            repeat: true,
            ..RegionConfig::default()
        };
        assert!(Region::new(endless, cfg).is_err());
    }

    #[test]
    fn repeat_uses_the_source_duration_when_no_end_is_given() {
        let cfg = RegionConfig {
            start: secs(9.0),
            repeat: true,
            ..RegionConfig::default()
        };
        let mut r = open(ramp(100), cfg);
        assert_eq!(r.window(), Some(secs(1.0)));
        assert_eq!(pull(&mut r, 12, 5)[10], 90.0);
    }
}
