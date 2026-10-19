//! Combine a foreground and a ducked background into one output buffer.

use crate::duck::{DuckConfig, Ducker};

/// Mixes two mono `f32` streams at a shared sample rate.
#[derive(Debug, Clone)]
pub struct Mixer {
    ducker: Ducker,
    background_level: f32,
}

impl Mixer {
    /// Build a mixer. `background_level` is the music's gain before ducking.
    #[must_use]
    pub fn new(cfg: DuckConfig, sample_rate: u32, background_level: f32) -> Self {
        Self {
            ducker: Ducker::new(cfg, sample_rate),
            background_level: background_level.clamp(0.0, 1.0),
        }
    }

    /// Set the music's gain before ducking, clamped to `0.0..=1.0`.
    pub fn set_background_level(&mut self, level: f32) {
        self.background_level = level.clamp(0.0, 1.0);
    }

    /// Reset ducking state, as after a seek in either stream.
    pub fn reset(&mut self) {
        self.ducker.reset();
    }

    /// Write `out.len()` mixed samples. A shorter input is treated as
    /// trailing silence. Output is clamped to `-1.0..=1.0`.
    pub fn mix_into(&mut self, foreground: &[f32], background: &[f32], out: &mut [f32]) {
        for (i, slot) in out.iter_mut().enumerate() {
            let fg = foreground.get(i).copied().unwrap_or(0.0);
            let bg = background.get(i).copied().unwrap_or(0.0);
            let gain = self.ducker.process(fg) * self.background_level;
            *slot = bg.mul_add(gain, fg).clamp(-1.0, 1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    #[test]
    fn silent_foreground_passes_background_at_level() {
        let mut m = Mixer::new(DuckConfig::default(), RATE, 0.5);
        let bg = vec![0.4; 1000];
        let fg = vec![0.0; 1000];
        let mut out = vec![0.0; 1000];
        m.mix_into(&fg, &bg, &mut out);
        assert!(out.iter().all(|s| (s - 0.2).abs() < 1e-6));
    }

    #[test]
    fn loud_foreground_ducks_background() {
        let cfg = DuckConfig::default();
        let mut m = Mixer::new(cfg, RATE, 1.0);
        let n = RATE as usize;
        let fg = vec![0.5; n];
        let bg = vec![0.4; n];
        let mut out = vec![0.0; n];
        m.mix_into(&fg, &bg, &mut out);
        let expected = 0.5 + 0.4 * cfg.floor_gain;
        assert!(
            (out[n - 1] - expected).abs() < 1e-3,
            "last sample {}",
            out[n - 1]
        );
    }

    #[test]
    fn short_inputs_are_padded_with_silence() {
        let mut m = Mixer::new(DuckConfig::default(), RATE, 1.0);
        let fg = vec![0.0; 2];
        let bg = vec![0.3; 2];
        let mut out = vec![9.0; 4];
        m.mix_into(&fg, &bg, &mut out);
        assert!((out[1] - 0.3).abs() < 1e-6);
        assert!(out[2].abs() < 1e-6 && out[3].abs() < 1e-6);
    }

    #[test]
    fn output_is_clamped() {
        let mut m = Mixer::new(DuckConfig::default(), RATE, 1.0);
        let fg = vec![0.9; 4];
        let bg = vec![0.9; 4];
        let mut out = vec![0.0; 4];
        m.mix_into(&fg, &bg, &mut out);
        assert!(out.iter().all(|s| *s <= 1.0));
    }
}
