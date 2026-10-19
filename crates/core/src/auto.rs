//! Automatic levelling: the balance between speech and everything else,
//! adjusted for playback speed, the noise in the room, and the narrator.
//!
//! Three mechanisms, each independently switchable:
//!
//! - **Speed compensation.** Faster speech is denser and needs more room.
//!   A curve maps speed to a scale on every non-speech layer.
//! - **Noise compensation.** The shell measures ambient noise (a microphone
//!   RMS in dBFS, a motion-derived guess, anything) and reports it here.
//!   Louder rooms boost speech and cut the background.
//! - **Speech AGC.** A slow automatic gain on the speech layer toward a
//!   target loudness, so a quiet narrator and a loud one sit at the same
//!   level and the music balance holds between books.
//!
//! All outputs are smoothed per block so nothing steps audibly.

/// Speech automatic gain control.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AgcConfig {
    /// Off means the speech layer is passed at unity.
    pub enabled: bool,
    /// Target RMS of the speech layer, linear. 0.1 is about -20 dBFS.
    pub target_rms: f32,
    /// Never boost more than this.
    pub max_gain: f32,
    /// Never cut below this.
    pub min_gain: f32,
    /// Blocks quieter than this (RMS) are pauses and do not move the gain.
    pub gate_rms: f32,
    /// Seconds for the gain to cover about two thirds of a change.
    pub time_const_secs: f32,
}

impl Default for AgcConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            target_rms: 0.1,
            max_gain: 4.0,
            min_gain: 0.5,
            gate_rms: 0.005,
            time_const_secs: 2.0,
        }
    }
}

/// Ambient noise compensation.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct NoiseConfig {
    /// Off means reported noise is ignored.
    pub enabled: bool,
    /// Noise at or below this (dBFS) is a quiet room: no change.
    pub quiet_db: f32,
    /// Noise at or above this (dBFS) is a loud room: full compensation.
    pub loud_db: f32,
    /// Speech gain in a loud room.
    pub speech_boost_max: f32,
    /// Background scale in a loud room.
    pub background_scale_min: f32,
    /// Seconds for compensation to cover about two thirds of a change.
    pub time_const_secs: f32,
}

impl Default for NoiseConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            quiet_db: -55.0,
            loud_db: -20.0,
            speech_boost_max: 1.8,
            background_scale_min: 0.4,
            time_const_secs: 3.0,
        }
    }
}

/// Everything automatic levelling can be tuned with.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AutoLevelConfig {
    /// `(speed, background scale)` points, sorted by speed, linearly
    /// interpolated and clamped at the ends. Empty disables it.
    pub speed_curve: Vec<(f32, f32)>,
    /// Ambient noise compensation.
    pub noise: NoiseConfig,
    /// Speech automatic gain.
    pub agc: AgcConfig,
}

impl Default for AutoLevelConfig {
    fn default() -> Self {
        Self {
            speed_curve: vec![(1.0, 1.0), (1.5, 0.8), (2.0, 0.6), (3.0, 0.4)],
            noise: NoiseConfig::default(),
            agc: AgcConfig::default(),
        }
    }
}

/// Live automatic levelling state. Feed it the speech block each render
/// and read back the two gains to apply.
#[derive(Debug, Clone)]
pub struct AutoLevel {
    cfg: AutoLevelConfig,
    speed: f32,
    noise_db: f32,
    agc_gain: f32,
    noise_blend: f32,
}

impl AutoLevel {
    /// Fresh state: unity everywhere, 1.0x, a quiet room.
    #[must_use]
    pub fn new(cfg: AutoLevelConfig) -> Self {
        let quiet = cfg.noise.quiet_db;
        Self {
            cfg,
            speed: 1.0,
            noise_db: quiet,
            agc_gain: 1.0,
            noise_blend: 0.0,
        }
    }

    /// The configuration.
    #[must_use]
    pub fn config(&self) -> &AutoLevelConfig {
        &self.cfg
    }

    /// Mutable configuration; changes apply from the next block.
    pub fn config_mut(&mut self) -> &mut AutoLevelConfig {
        &mut self.cfg
    }

    /// Tell the leveller the current playback speed.
    pub fn set_speed(&mut self, speed: f32) {
        self.speed = speed;
    }

    /// Report measured ambient noise in dBFS (negative; 0 is full scale).
    pub fn set_noise_db(&mut self, db: f32) {
        if db.is_finite() {
            self.noise_db = db;
        }
    }

    /// Background scale from the speed curve alone.
    #[must_use]
    pub fn speed_scale(&self) -> f32 {
        let curve = &self.cfg.speed_curve;
        let Some(first) = curve.first() else {
            return 1.0;
        };
        if self.speed <= first.0 {
            return first.1;
        }
        for pair in curve.windows(2) {
            let (s0, g0) = pair[0];
            let (s1, g1) = pair[1];
            if self.speed <= s1 {
                let t = if s1 > s0 {
                    (self.speed - s0) / (s1 - s0)
                } else {
                    1.0
                };
                return g0 + (g1 - g0) * t;
            }
        }
        curve.last().map_or(1.0, |p| p.1)
    }

    /// Update from one block of the speech layer's output. `block_secs` is
    /// the block length in seconds.
    pub fn update(&mut self, speech: &[f32], block_secs: f32) {
        self.update_agc(speech, block_secs);
        self.update_noise(block_secs);
    }

    fn update_agc(&mut self, speech: &[f32], block_secs: f32) {
        let agc = self.cfg.agc;
        if !agc.enabled {
            self.agc_gain = 1.0;
            return;
        }
        if speech.is_empty() {
            return;
        }
        let rms = rms(speech);
        if rms < agc.gate_rms {
            return;
        }
        let desired = (agc.target_rms / rms).clamp(agc.min_gain, agc.max_gain);
        let alpha = alpha(block_secs, agc.time_const_secs);
        self.agc_gain += (desired - self.agc_gain) * alpha;
    }

    fn update_noise(&mut self, block_secs: f32) {
        let n = self.cfg.noise;
        let target = if n.enabled && n.loud_db > n.quiet_db {
            ((self.noise_db - n.quiet_db) / (n.loud_db - n.quiet_db)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let alpha = alpha(block_secs, n.time_const_secs);
        self.noise_blend += (target - self.noise_blend) * alpha;
    }

    /// Gain to apply to the speech layer.
    #[must_use]
    pub fn speech_gain(&self) -> f32 {
        let n = self.cfg.noise;
        let boost = 1.0 + (n.speech_boost_max - 1.0) * self.noise_blend;
        self.agc_gain * boost
    }

    /// Scale to apply to every non-speech layer.
    #[must_use]
    pub fn background_scale(&self) -> f32 {
        let n = self.cfg.noise;
        let noise = 1.0 + (n.background_scale_min - 1.0) * self.noise_blend;
        self.speed_scale() * noise
    }
}

fn alpha(block_secs: f32, time_const: f32) -> f32 {
    if time_const <= 0.0 {
        return 1.0;
    }
    1.0 - (-block_secs / time_const).exp()
}

#[allow(clippy::cast_precision_loss)]
fn rms(x: &[f32]) -> f32 {
    let sum: f32 = x.iter().map(|v| v * v).sum();
    (sum / x.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn speed_curve_interpolates_and_clamps() {
        let mut a = AutoLevel::new(AutoLevelConfig::default());
        a.set_speed(0.8);
        assert!(close(a.speed_scale(), 1.0));
        a.set_speed(1.25);
        assert!(close(a.speed_scale(), 0.9));
        a.set_speed(2.0);
        assert!(close(a.speed_scale(), 0.6));
        a.set_speed(5.0);
        assert!(close(a.speed_scale(), 0.4));
    }

    #[test]
    fn empty_speed_curve_is_unity() {
        let mut cfg = AutoLevelConfig::default();
        cfg.speed_curve.clear();
        let mut a = AutoLevel::new(cfg);
        a.set_speed(3.0);
        assert!(close(a.speed_scale(), 1.0));
    }

    #[test]
    fn loud_room_boosts_speech_and_cuts_background() {
        let mut cfg = AutoLevelConfig::default();
        cfg.agc.enabled = false;
        let mut a = AutoLevel::new(cfg);
        a.set_noise_db(-10.0);
        for _ in 0..1000 {
            a.update(&[0.1; 64], 0.1);
        }
        assert!(close(a.speech_gain(), 1.8), "{}", a.speech_gain());
        assert!(close(a.background_scale(), 0.4), "{}", a.background_scale());
        a.set_noise_db(-70.0);
        for _ in 0..1000 {
            a.update(&[0.1; 64], 0.1);
        }
        assert!(close(a.speech_gain(), 1.0));
        assert!(close(a.background_scale(), 1.0));
    }

    #[test]
    fn agc_pulls_quiet_speech_up_and_respects_limits() {
        let mut cfg = AutoLevelConfig::default();
        cfg.noise.enabled = false;
        let mut a = AutoLevel::new(cfg);
        let quiet = vec![0.02; 256];
        for _ in 0..500 {
            a.update(&quiet, 0.1);
        }
        assert!(
            close(a.speech_gain(), 4.0),
            "capped at max: {}",
            a.speech_gain()
        );
        let loud = vec![0.9; 256];
        for _ in 0..500 {
            a.update(&loud, 0.1);
        }
        assert!(
            close(a.speech_gain(), 0.5),
            "capped at min: {}",
            a.speech_gain()
        );
    }

    #[test]
    fn agc_ignores_pauses() {
        let mut cfg = AutoLevelConfig::default();
        cfg.noise.enabled = false;
        let mut a = AutoLevel::new(cfg);
        for _ in 0..100 {
            a.update(&[0.0; 256], 0.1);
        }
        assert!(close(a.speech_gain(), 1.0));
    }

    #[test]
    fn disabled_agc_is_unity() {
        let mut cfg = AutoLevelConfig::default();
        cfg.agc.enabled = false;
        cfg.noise.enabled = false;
        let mut a = AutoLevel::new(cfg);
        a.update(&[0.01; 256], 1.0);
        assert!(close(a.speech_gain(), 1.0));
    }
}
