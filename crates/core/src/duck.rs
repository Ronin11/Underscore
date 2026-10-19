//! Speech-aware ducking: lower the background while the foreground speaks.
//!
//! A peak envelope follower tracks the foreground. When the envelope rises
//! above a threshold the background gain slides toward a floor over the
//! attack time. When the foreground falls quiet the gain holds for a moment,
//! so pauses between sentences do not pump, then recovers over the release
//! time. Everything is per-sample, mono, and allocation-free.

/// Tunable parameters. Gains are linear (1.0 is unity), times are seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DuckConfig {
    /// Foreground peak level above which ducking engages.
    pub threshold: f32,
    /// Background gain while fully ducked.
    pub floor_gain: f32,
    /// Seconds for the gain to fall from unity to the floor.
    pub attack_secs: f32,
    /// Seconds the floor is held after the foreground goes quiet.
    pub hold_secs: f32,
    /// Seconds for the gain to rise from the floor back to unity.
    pub release_secs: f32,
    /// Seconds for the envelope follower to decay to 37% of its peak.
    pub envelope_decay_secs: f32,
}

impl Default for DuckConfig {
    fn default() -> Self {
        Self {
            threshold: 0.02,
            floor_gain: 0.25,
            attack_secs: 0.05,
            hold_secs: 0.6,
            release_secs: 1.2,
            envelope_decay_secs: 0.05,
        }
    }
}

/// Per-sample ducking state for one mono foreground stream.
#[derive(Debug, Clone)]
pub struct Ducker {
    cfg: DuckConfig,
    envelope: f32,
    envelope_decay: f32,
    gain: f32,
    attack_step: f32,
    release_step: f32,
    hold_samples: u32,
    hold_remaining: u32,
}

impl Ducker {
    /// Build a ducker for the given sample rate.
    ///
    /// # Panics
    /// If `sample_rate` is zero.
    #[must_use]
    pub fn new(cfg: DuckConfig, sample_rate: u32) -> Self {
        assert!(sample_rate > 0, "sample rate must be positive");
        #[allow(clippy::cast_precision_loss)]
        let rate = sample_rate as f32;
        let span = 1.0 - cfg.floor_gain;
        Self {
            cfg,
            envelope: 0.0,
            envelope_decay: (-1.0 / (cfg.envelope_decay_secs.max(1e-4) * rate)).exp(),
            gain: 1.0,
            attack_step: span / (cfg.attack_secs.max(1e-4) * rate),
            release_step: span / (cfg.release_secs.max(1e-4) * rate),
            hold_samples: seconds_to_samples(cfg.hold_secs, rate),
            hold_remaining: 0,
        }
    }

    /// Feed one foreground sample; get the gain to apply to the background
    /// at the same instant.
    pub fn process(&mut self, foreground: f32) -> f32 {
        let level = foreground.abs();
        self.envelope = if level > self.envelope {
            level
        } else {
            self.envelope * self.envelope_decay
        };

        if self.envelope > self.cfg.threshold {
            self.hold_remaining = self.hold_samples;
            self.gain = (self.gain - self.attack_step).max(self.cfg.floor_gain);
        } else if self.hold_remaining > 0 {
            self.hold_remaining -= 1;
        } else {
            self.gain = (self.gain + self.release_step).min(1.0);
        }
        self.gain
    }

    /// The gain most recently returned by [`Ducker::process`].
    #[must_use]
    pub fn gain(&self) -> f32 {
        self.gain
    }

    /// Reset envelope, gain and hold, as after a seek.
    pub fn reset(&mut self) {
        self.envelope = 0.0;
        self.gain = 1.0;
        self.hold_remaining = 0;
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn seconds_to_samples(secs: f32, rate: f32) -> u32 {
    (secs.max(0.0) * rate).round() as u32
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn samples(secs: f32) -> usize {
        (secs * RATE as f32) as usize
    }

    fn run(d: &mut Ducker, sample: f32, n: usize) -> f32 {
        let mut g = d.gain();
        for _ in 0..n {
            g = d.process(sample);
        }
        g
    }

    #[test]
    fn silence_leaves_gain_at_unity() {
        let mut d = Ducker::new(DuckConfig::default(), RATE);
        assert!((run(&mut d, 0.0, samples(1.0)) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn speech_reaches_floor_within_attack_time() {
        let cfg = DuckConfig::default();
        let mut d = Ducker::new(cfg, RATE);
        let g = run(&mut d, 0.5, samples(cfg.attack_secs) + 1);
        assert!((g - cfg.floor_gain).abs() < 1e-3, "gain {g}");
    }

    #[test]
    fn gain_holds_then_recovers_after_speech_stops() {
        let cfg = DuckConfig::default();
        let mut d = Ducker::new(cfg, RATE);
        run(&mut d, 0.5, samples(0.5));
        // Within the hold window the floor is kept.
        let g = run(&mut d, 0.0, samples(cfg.hold_secs) - 1);
        assert!((g - cfg.floor_gain).abs() < 1e-3, "held gain {g}");
        // The hold starts once the envelope has decayed below threshold,
        // which takes a few decay constants; allow a generous margin.
        let settle = samples(cfg.envelope_decay_secs) * 8;
        let g = run(&mut d, 0.0, settle + samples(cfg.release_secs) + 1);
        assert!((g - 1.0).abs() < 1e-3, "released gain {g}");
    }

    #[test]
    fn brief_pause_between_words_does_not_pump() {
        let cfg = DuckConfig::default();
        let mut d = Ducker::new(cfg, RATE);
        run(&mut d, 0.5, samples(0.25));
        // A 200 ms gap is shorter than the hold.
        let g = run(&mut d, 0.0, samples(0.2));
        assert!((g - cfg.floor_gain).abs() < 1e-3, "gain after gap {g}");
    }

    #[test]
    fn reset_returns_to_unity() {
        let mut d = Ducker::new(DuckConfig::default(), RATE);
        run(&mut d, 0.5, samples(1.0));
        d.reset();
        assert!((d.gain() - 1.0).abs() < f32::EPSILON);
    }
}
