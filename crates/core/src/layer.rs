//! One stackable stream with its own level, fades, and crossfades.

use std::time::Duration;

use crate::fade::{Crossfade, Curve, Fader};
use crate::source::Source;

/// What a layer carries. Ducking rules and automatic levelling key off it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum LayerKind {
    /// The thing the listener is listening to: a book, a podcast.
    Speech,
    /// Music underneath.
    Music,
    /// Rain, a fire, a café: a bed that sits under both.
    Ambient,
    /// Anything else the shell wants to stack.
    Other,
}

/// Per-layer settings a listener can tune.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LayerConfig {
    /// Shown in the UI; also useful in logs.
    pub name: String,
    /// Drives ducking and auto-level behaviour.
    pub kind: LayerKind,
    /// The listener's level for this layer, `0.0..=1.0`.
    pub level: f32,
    /// Ramp when the layer is enabled or unmuted.
    pub fade_in: Duration,
    /// Ramp when the layer is disabled or muted.
    pub fade_out: Duration,
    /// Shape of those ramps and of crossfades within the layer.
    pub curve: Curve,
    /// When faded fully out, stop pulling from the source so it keeps its
    /// place. Right for music; wrong for a live stream.
    pub pause_when_silent: bool,
}

impl LayerConfig {
    /// Sensible defaults for a layer of `kind` called `name`.
    #[must_use]
    pub fn new(name: impl Into<String>, kind: LayerKind) -> Self {
        let (level, fade_in, fade_out) = match kind {
            LayerKind::Speech => (1.0, Duration::from_millis(50), Duration::from_millis(50)),
            LayerKind::Music => (0.6, Duration::from_millis(1500), Duration::from_millis(800)),
            LayerKind::Ambient => (0.4, Duration::from_secs(3), Duration::from_secs(2)),
            LayerKind::Other => (0.8, Duration::from_millis(500), Duration::from_millis(500)),
        };
        Self {
            name: name.into(),
            kind,
            level,
            fade_in,
            fade_out,
            curve: Curve::EqualPower,
            pause_when_silent: kind != LayerKind::Speech,
        }
    }
}

/// A layer: a current source, an optional incoming source mid-crossfade,
/// and the gain ramps around them. Renders post-fade, pre-duck audio.
pub struct Layer {
    cfg: LayerConfig,
    channels: usize,
    sample_rate: u32,
    current: Option<Box<dyn Source>>,
    next: Option<(Box<dyn Source>, Crossfade)>,
    fader: Fader,
    scratch: Vec<f32>,
    ended: bool,
}

impl std::fmt::Debug for Layer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Layer")
            .field("cfg", &self.cfg)
            .field("has_source", &self.current.is_some())
            .field("crossfading", &self.next.is_some())
            .field("gain", &self.fader.gain())
            .field("ended", &self.ended)
            .finish_non_exhaustive()
    }
}

impl Layer {
    /// An empty layer at the engine's format, sitting at full gain.
    #[must_use]
    pub fn new(
        cfg: LayerConfig,
        channels: usize,
        sample_rate: u32,
        max_block_frames: usize,
    ) -> Self {
        Self {
            cfg,
            channels,
            sample_rate,
            current: None,
            next: None,
            fader: Fader::at(1.0),
            scratch: vec![0.0; max_block_frames * channels],
            ended: false,
        }
    }

    /// The layer's settings.
    #[must_use]
    pub fn config(&self) -> &LayerConfig {
        &self.cfg
    }

    /// Mutable settings. Changing `level` takes effect on the next block.
    pub fn config_mut(&mut self) -> &mut LayerConfig {
        &mut self.cfg
    }

    /// The layer's kind.
    #[must_use]
    pub fn kind(&self) -> LayerKind {
        self.cfg.kind
    }

    /// Set the listener's level, clamped to `0.0..=1.0`.
    pub fn set_level(&mut self, level: f32) {
        self.cfg.level = level.clamp(0.0, 1.0);
    }

    /// Replace the source with a hard cut. Any crossfade in progress is
    /// dropped. Returns the source unchanged if its format does not match.
    ///
    /// # Errors
    /// The rejected source, when its channel count or rate differ.
    pub fn set_source(&mut self, source: Box<dyn Source>) -> Result<(), Box<dyn Source>> {
        if !self.accepts(&*source) {
            return Err(source);
        }
        self.current = Some(source);
        self.next = None;
        self.ended = false;
        Ok(())
    }

    /// Crossfade from the current source to `source` over `over`. With no
    /// current source this is a plain cut.
    ///
    /// # Errors
    /// The rejected source, when its channel count or rate differ.
    pub fn crossfade_to(
        &mut self,
        source: Box<dyn Source>,
        over: Duration,
    ) -> Result<(), Box<dyn Source>> {
        if !self.accepts(&*source) {
            return Err(source);
        }
        if self.current.is_none() {
            return self.set_source(source);
        }
        if let Some((pending, _)) = self.next.take() {
            // Two crossfades in flight: the pending one becomes current.
            self.current = Some(pending);
        }
        let frames = self.frames(over);
        self.next = Some((source, Crossfade::new(frames, self.cfg.curve)));
        self.ended = false;
        Ok(())
    }

    /// Drop the source. The layer renders silence until a new one is set.
    pub fn clear_source(&mut self) {
        self.current = None;
        self.next = None;
        self.ended = false;
    }

    /// The current source, if any.
    #[must_use]
    pub fn source(&self) -> Option<&dyn Source> {
        self.current.as_deref()
    }

    /// The current source, mutably: seek it, ask its position.
    pub fn source_mut(&mut self) -> Option<&mut (dyn Source + 'static)> {
        self.current.as_deref_mut()
    }

    /// True once the current source ran dry with nothing queued after it.
    #[must_use]
    pub fn ended(&self) -> bool {
        self.ended
    }

    /// Ramp to `gain` (`0.0..=1.0`) over `over`, on top of `level`.
    pub fn fade_to(&mut self, gain: f32, over: Duration) {
        let frames = self.frames(over);
        self.fader
            .ramp_to(gain.clamp(0.0, 1.0), frames, self.cfg.curve);
    }

    /// Fade out over `fade_out` or back in over `fade_in`.
    pub fn set_muted(&mut self, muted: bool) {
        if muted {
            self.fade_to(0.0, self.cfg.fade_out);
        } else {
            self.fade_to(1.0, self.cfg.fade_in);
        }
    }

    /// True when the fade has settled at zero: nothing to hear.
    #[must_use]
    pub fn is_silent(&self) -> bool {
        self.fader.settled() && self.fader.target() <= 0.0
    }

    /// The fade gain at this instant, before `level`.
    #[must_use]
    pub fn fade_gain(&self) -> f32 {
        self.fader.gain()
    }

    /// Reset any transition state, as after a seek.
    pub fn reset_transitions(&mut self) {
        if let Some((pending, _)) = self.next.take() {
            self.current = Some(pending);
        }
        self.fader.set(self.fader.target());
    }

    /// Render one block of post-fade audio into `out` (interleaved, whole
    /// frames). Returns the number of frames written, always `out.len() /
    /// channels`; silence is written where no source is playing.
    pub fn render(&mut self, out: &mut [f32]) -> usize {
        let ch = self.channels;
        let frames = out.len() / ch;
        let out = &mut out[..frames * ch];

        if self.current.is_none() || (self.cfg.pause_when_silent && self.is_silent()) {
            out.fill(0.0);
            return frames;
        }

        self.pull(out);
        self.apply_fade(out);
        frames
    }

    /// Fill `out` from the current source, blending in the next one if a
    /// crossfade is running. Pads with silence when a source runs dry.
    fn pull(&mut self, out: &mut [f32]) {
        let ch = self.channels;
        let frames = out.len() / ch;

        let got = self.current.as_mut().map_or(0, |s| s.read(out));
        out[got * ch..].fill(0.0);

        if let Some((incoming, fade)) = self.next.as_mut() {
            let scratch = &mut self.scratch[..out.len()];
            let got_in = incoming.read(scratch);
            scratch[got_in * ch..].fill(0.0);
            for f in 0..frames {
                let (g_out, g_in) = fade.advance();
                for c in 0..ch {
                    let i = f * ch + c;
                    out[i] = out[i].mul_add(g_out, scratch[i] * g_in);
                }
            }
            if fade.done() || got < frames {
                let (incoming, _) = self.next.take().expect("checked above");
                self.current = Some(incoming);
            }
        } else if got < frames {
            self.ended = true;
        }
    }

    fn apply_fade(&mut self, out: &mut [f32]) {
        let ch = self.channels;
        let level = self.cfg.level;
        if self.fader.settled() {
            let g = self.fader.gain() * level;
            if (g - 1.0).abs() > f32::EPSILON {
                for s in out.iter_mut() {
                    *s *= g;
                }
            }
            return;
        }
        for frame in out.chunks_exact_mut(ch) {
            let g = self.fader.advance() * level;
            for s in frame {
                *s *= g;
            }
        }
    }

    fn accepts(&self, source: &dyn Source) -> bool {
        source.channels() == self.channels && source.sample_rate() == self.sample_rate
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn frames(&self, d: Duration) -> u32 {
        (d.as_secs_f64() * f64::from(self.sample_rate)).round() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::SliceSource;

    const RATE: u32 = 1_000;

    fn layer(kind: LayerKind) -> Layer {
        Layer::new(LayerConfig::new("t", kind), 1, RATE, 64)
    }

    fn constant(v: f32, frames: usize) -> Box<dyn Source> {
        Box::new(SliceSource::new(vec![v; frames], 1, RATE))
    }

    #[test]
    fn empty_layer_renders_silence() {
        let mut l = layer(LayerKind::Music);
        let mut out = [1.0; 8];
        assert_eq!(l.render(&mut out), 8);
        assert!(out.iter().all(|v| *v == 0.0));
        assert!(!l.ended());
    }

    #[test]
    fn level_scales_output_and_end_is_reported() {
        let mut l = layer(LayerKind::Speech);
        l.set_level(0.5);
        l.set_source(constant(1.0, 6)).ok().unwrap();
        let mut out = [0.0; 8];
        l.render(&mut out);
        assert!(out[..6].iter().all(|v| (*v - 0.5).abs() < 1e-6));
        assert!(out[6..].iter().all(|v| *v == 0.0));
        assert!(l.ended());
    }

    #[test]
    fn mismatched_format_is_rejected() {
        let mut l = layer(LayerKind::Music);
        let bad: Box<dyn Source> = Box::new(SliceSource::new(vec![0.0; 4], 2, RATE));
        assert!(l.set_source(bad).is_err());
    }

    #[test]
    fn crossfade_moves_from_old_to_new() {
        let mut l = layer(LayerKind::Music);
        l.config_mut().curve = Curve::Linear;
        l.set_level(1.0);
        l.set_source(constant(1.0, 100)).ok().unwrap();
        l.crossfade_to(constant(-1.0, 100), Duration::from_millis(10))
            .ok()
            .unwrap();
        let mut out = [0.0; 20];
        l.render(&mut out);
        assert!((out[0] - 1.0).abs() < 1e-6, "starts on old: {}", out[0]);
        assert!(out[5].abs() < 0.2, "middle is a blend: {}", out[5]);
        assert!((out[15] + 1.0).abs() < 1e-6, "ends on new: {}", out[15]);
        assert!(!l.ended());
    }

    #[test]
    fn muted_music_pauses_its_source() {
        let mut l = layer(LayerKind::Music);
        l.config_mut().fade_out = Duration::ZERO;
        l.set_source(constant(1.0, 10)).ok().unwrap();
        l.set_muted(true);
        let mut out = [1.0; 4];
        l.render(&mut out);
        assert!(out.iter().all(|v| *v == 0.0));
        // Unmuting resumes from where it stopped: all 10 frames still there.
        l.config_mut().fade_in = Duration::ZERO;
        l.set_muted(false);
        let mut out = [0.0; 12];
        l.render(&mut out);
        assert!(out[..10].iter().all(|v| (*v - 0.6).abs() < 1e-6));
    }

    #[test]
    fn fade_out_ramps_down() {
        let mut l = layer(LayerKind::Speech);
        l.config_mut().curve = Curve::Linear;
        l.set_source(constant(1.0, 100)).ok().unwrap();
        l.fade_to(0.0, Duration::from_millis(10));
        let mut out = [0.0; 12];
        l.render(&mut out);
        assert!(out[0] > out[5] && out[5] > out[9], "{out:?}");
        assert!(out[10].abs() < 1e-6);
    }
}
