//! The mixer: any number of layers, ducking rules between them, one output.

use crate::duck::{DuckConfig, Ducker};
use crate::layer::{Layer, LayerConfig, LayerKind};

/// Engine-wide settings fixed at construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct EngineConfig {
    /// Interleaved channel count every source must match.
    pub channels: usize,
    /// Sample rate every source must match.
    pub sample_rate: u32,
    /// The largest block `render` will be asked for, in frames.
    pub max_block_frames: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            channels: 2,
            sample_rate: 48_000,
            max_block_frames: 4096,
        }
    }
}

/// Handle to a layer inside an [`Engine`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LayerId(pub usize);

/// Which layers a ducking rule lowers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum DuckTarget {
    /// One layer.
    Layer(LayerId),
    /// Every layer of a kind, including ones added after the rule.
    Kind(LayerKind),
    /// Every layer except the rule's source.
    Others,
}

/// "While `from` is audible, lower `to`."
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DuckRule {
    /// The layer whose level drives the duck.
    pub from: LayerId,
    /// The layers lowered.
    pub to: DuckTarget,
    /// Threshold, floor, attack, hold, release.
    pub config: DuckConfig,
}

impl DuckTarget {
    fn matches(self, id: LayerId, kind: LayerKind, from: LayerId) -> bool {
        match self {
            Self::Layer(target) => target == id,
            Self::Kind(k) => k == kind && id != from,
            Self::Others => id != from,
        }
    }
}

/// What a layer did in the last block, for meters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayerMeter {
    /// Peak absolute sample of the layer after fades, before ducking.
    pub peak: f32,
    /// The smallest combined duck and auto gain applied in the block.
    pub gain: f32,
}

/// What a render produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderReport {
    /// Frames written.
    pub frames: usize,
    /// Layers whose source ran dry during or before this block.
    pub layers_ended: usize,
}

/// The mixer.
pub struct Engine {
    cfg: EngineConfig,
    layers: Vec<Layer>,
    rules: Vec<(DuckRule, Ducker)>,
    /// Per-layer scratch, interleaved.
    bufs: Vec<Vec<f32>>,
    /// Per-layer, per-frame gain.
    gains: Vec<Vec<f32>>,
    /// Per-layer scalar set by automatic levelling.
    auto: Vec<f32>,
    /// Per-kind scalar set by automatic levelling.
    kind_scale: [(LayerKind, f32); 4],
    meters: Vec<LayerMeter>,
    master: f32,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("cfg", &self.cfg)
            .field("layers", &self.layers)
            .field(
                "rules",
                &self.rules.iter().map(|(r, _)| r).collect::<Vec<_>>(),
            )
            .field("master", &self.master)
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// An engine with no layers.
    #[must_use]
    pub fn new(cfg: EngineConfig) -> Self {
        Self {
            cfg,
            layers: Vec::new(),
            rules: Vec::new(),
            bufs: Vec::new(),
            gains: Vec::new(),
            auto: Vec::new(),
            kind_scale: [
                (LayerKind::Speech, 1.0),
                (LayerKind::Music, 1.0),
                (LayerKind::Ambient, 1.0),
                (LayerKind::Other, 1.0),
            ],
            meters: Vec::new(),
            master: 1.0,
        }
    }

    /// The configuration.
    #[must_use]
    pub fn config(&self) -> &EngineConfig {
        &self.cfg
    }

    /// Add a layer. Layers are never removed; clear a layer's source to
    /// silence it.
    pub fn add_layer(&mut self, cfg: LayerConfig) -> LayerId {
        let id = LayerId(self.layers.len());
        self.layers.push(Layer::new(
            cfg,
            self.cfg.channels,
            self.cfg.sample_rate,
            self.cfg.max_block_frames,
        ));
        let len = self.cfg.max_block_frames;
        self.bufs.push(vec![0.0; len * self.cfg.channels]);
        self.gains.push(vec![1.0; len]);
        self.auto.push(1.0);
        self.meters.push(LayerMeter {
            peak: 0.0,
            gain: 1.0,
        });
        id
    }

    /// A layer by id.
    ///
    /// # Panics
    /// If `id` did not come from this engine.
    #[must_use]
    pub fn layer(&self, id: LayerId) -> &Layer {
        &self.layers[id.0]
    }

    /// A layer by id, mutably.
    ///
    /// # Panics
    /// If `id` did not come from this engine.
    pub fn layer_mut(&mut self, id: LayerId) -> &mut Layer {
        &mut self.layers[id.0]
    }

    /// All layers, in id order.
    pub fn layers(&self) -> impl Iterator<Item = (LayerId, &Layer)> {
        self.layers.iter().enumerate().map(|(i, l)| (LayerId(i), l))
    }

    /// Ids of every layer of `kind`.
    pub fn layers_of(&self, kind: LayerKind) -> impl Iterator<Item = LayerId> + '_ {
        self.layers()
            .filter(move |(_, l)| l.kind() == kind)
            .map(|(id, _)| id)
    }

    /// Number of layers.
    #[must_use]
    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }

    /// Add a ducking rule.
    pub fn add_duck_rule(&mut self, rule: DuckRule) {
        let ducker = Ducker::new(rule.config, self.cfg.sample_rate);
        self.rules.push((rule, ducker));
    }

    /// Replace every ducking rule.
    pub fn set_duck_rules(&mut self, rules: impl IntoIterator<Item = DuckRule>) {
        self.rules.clear();
        for r in rules {
            self.add_duck_rule(r);
        }
    }

    /// The ducking rules.
    pub fn duck_rules(&self) -> impl Iterator<Item = &DuckRule> {
        self.rules.iter().map(|(r, _)| r)
    }

    /// Scalar applied to one layer on top of its own level, for automatic
    /// levelling.
    pub fn set_layer_scale(&mut self, id: LayerId, scale: f32) {
        self.auto[id.0] = scale.max(0.0);
    }

    /// Scalar applied to every layer of `kind`, for automatic levelling.
    pub fn set_kind_scale(&mut self, kind: LayerKind, scale: f32) {
        for slot in &mut self.kind_scale {
            if slot.0 == kind {
                slot.1 = scale.max(0.0);
            }
        }
    }

    /// Output gain after the sum.
    pub fn set_master(&mut self, gain: f32) {
        self.master = gain.clamp(0.0, 4.0);
    }

    /// Output gain after the sum.
    #[must_use]
    pub fn master(&self) -> f32 {
        self.master
    }

    /// Meters from the last render, in layer order.
    #[must_use]
    pub fn meters(&self) -> &[LayerMeter] {
        &self.meters
    }

    /// Reset every ducker and transition, as after a seek.
    pub fn reset_dynamics(&mut self) {
        for (_, d) in &mut self.rules {
            d.reset();
        }
        for l in &mut self.layers {
            l.reset_transitions();
        }
    }

    /// The last rendered block of one layer, post-fade and pre-duck, as
    /// left by the most recent `render`. Useful for analysis.
    ///
    /// # Panics
    /// If `id` did not come from this engine.
    #[must_use]
    pub fn last_block(&self, id: LayerId, frames: usize) -> &[f32] {
        &self.bufs[id.0][..frames * self.cfg.channels]
    }

    /// Render one block into `out` (interleaved, whole frames, at most
    /// `max_block_frames`). Output is clamped to `-1.0..=1.0`.
    ///
    /// # Panics
    /// If `out` holds more than `max_block_frames` frames.
    pub fn render(&mut self, out: &mut [f32]) -> RenderReport {
        let ch = self.cfg.channels;
        let frames = out.len() / ch;
        assert!(
            frames <= self.cfg.max_block_frames,
            "block of {frames} frames exceeds max_block_frames"
        );
        let out = &mut out[..frames * ch];

        self.render_layers(frames);
        self.apply_ducking(frames);
        self.sum(out, frames);

        RenderReport {
            frames,
            layers_ended: self.layers.iter().filter(|l| l.ended()).count(),
        }
    }

    fn render_layers(&mut self, frames: usize) {
        let ch = self.cfg.channels;
        for (i, layer) in self.layers.iter_mut().enumerate() {
            let buf = &mut self.bufs[i][..frames * ch];
            layer.render(buf);
            let scale = self.auto[i] * kind_scale(&self.kind_scale, layer.kind());
            self.gains[i][..frames].fill(scale);
            self.meters[i].peak = buf.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        }
    }

    fn apply_ducking(&mut self, frames: usize) {
        let ch = self.cfg.channels;
        let n_layers = self.layers.len();
        for (rule, ducker) in &mut self.rules {
            let from = rule.from.0;
            if from >= n_layers {
                continue;
            }
            let targets: Vec<usize> = (0..n_layers)
                .filter(|&j| {
                    rule.to
                        .matches(LayerId(j), self.layers[j].kind(), rule.from)
                })
                .collect();
            if targets.is_empty() {
                continue;
            }
            let src = &self.bufs[from];
            for f in 0..frames {
                let level = src[f * ch..(f + 1) * ch]
                    .iter()
                    .fold(0.0f32, |m, s| m.max(s.abs()));
                let g = ducker.process(level);
                for &j in &targets {
                    self.gains[j][f] *= g;
                }
            }
        }
        for i in 0..n_layers {
            self.meters[i].gain = self.gains[i][..frames]
                .iter()
                .fold(f32::INFINITY, |m, g| m.min(*g));
        }
    }

    fn sum(&self, out: &mut [f32], frames: usize) {
        let ch = self.cfg.channels;
        out.fill(0.0);
        for (buf, gains) in self.bufs.iter().zip(&self.gains) {
            let frames_in = buf[..frames * ch].chunks_exact(ch);
            let frames_out = out.chunks_exact_mut(ch);
            for ((src, dst), &g) in frames_in.zip(frames_out).zip(&gains[..frames]) {
                if g == 0.0 {
                    continue;
                }
                for (s, d) in src.iter().zip(dst) {
                    *d = s.mul_add(g, *d);
                }
            }
        }
        let m = self.master;
        for s in out.iter_mut() {
            *s = (*s * m).clamp(-1.0, 1.0);
        }
    }
}

fn kind_scale(table: &[(LayerKind, f32)], kind: LayerKind) -> f32 {
    table
        .iter()
        .find(|(k, _)| *k == kind)
        .map_or(1.0, |(_, s)| *s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{SliceSource, Source};

    const RATE: u32 = 48_000;

    fn engine() -> Engine {
        Engine::new(EngineConfig {
            channels: 1,
            sample_rate: RATE,
            max_block_frames: 4096,
        })
    }

    fn constant(v: f32, frames: usize) -> Box<dyn Source> {
        Box::new(SliceSource::new(vec![v; frames], 1, RATE))
    }

    fn music_layer(e: &mut Engine, name: &str) -> LayerId {
        let mut cfg = LayerConfig::new(name, LayerKind::Music);
        cfg.level = 1.0;
        e.add_layer(cfg)
    }

    #[test]
    fn empty_engine_renders_silence() {
        let mut e = engine();
        let mut out = [1.0; 16];
        let r = e.render(&mut out);
        assert_eq!(r.frames, 16);
        assert!(out.iter().all(|v| *v == 0.0));
    }

    #[test]
    fn layers_sum_and_clamp() {
        let mut e = engine();
        let a = music_layer(&mut e, "a");
        let b = music_layer(&mut e, "b");
        e.layer_mut(a).set_source(constant(0.4, 100)).ok().unwrap();
        e.layer_mut(b).set_source(constant(0.3, 100)).ok().unwrap();
        let mut out = [0.0; 8];
        e.render(&mut out);
        assert!(out.iter().all(|v| (*v - 0.7).abs() < 1e-6), "{out:?}");
        let c = music_layer(&mut e, "c");
        e.layer_mut(c).set_source(constant(0.9, 100)).ok().unwrap();
        e.render(&mut out);
        assert!(out.iter().all(|v| (*v - 1.0).abs() < 1e-6));
    }

    #[test]
    fn speech_ducks_every_music_layer_but_not_itself() {
        let mut e = engine();
        let speech = e.add_layer(LayerConfig::new("book", LayerKind::Speech));
        let m1 = music_layer(&mut e, "m1");
        let m2 = music_layer(&mut e, "m2");
        let cfg = DuckConfig {
            attack_secs: 0.0,
            ..DuckConfig::default()
        };
        e.add_duck_rule(DuckRule {
            from: speech,
            to: DuckTarget::Kind(LayerKind::Music),
            config: cfg,
        });
        e.layer_mut(speech)
            .set_source(constant(0.5, 1000))
            .ok()
            .unwrap();
        e.layer_mut(m1)
            .set_source(constant(0.2, 1000))
            .ok()
            .unwrap();
        e.layer_mut(m2)
            .set_source(constant(0.2, 1000))
            .ok()
            .unwrap();
        let mut out = [0.0; 64];
        e.render(&mut out);
        let expected = 0.5 + 2.0 * 0.2 * cfg.floor_gain;
        assert!((out[63] - expected).abs() < 1e-3, "{}", out[63]);
        assert!((e.meters()[m1.0].gain - cfg.floor_gain).abs() < 1e-3);
        assert!((e.meters()[speech.0].gain - 1.0).abs() < 1e-6);
    }

    #[test]
    fn rules_chain_three_deep() {
        // Speech ducks music; music ducks ambient. With speech on, ambient
        // sees only music's ducked level, which is below music's threshold,
        // so ambient is not ducked.
        let mut e = engine();
        let speech = e.add_layer(LayerConfig::new("book", LayerKind::Speech));
        let music = music_layer(&mut e, "music");
        let mut amb_cfg = LayerConfig::new("rain", LayerKind::Ambient);
        amb_cfg.level = 1.0;
        let ambient = e.add_layer(amb_cfg);
        let fast = DuckConfig {
            attack_secs: 0.0,
            threshold: 0.3,
            ..DuckConfig::default()
        };
        e.add_duck_rule(DuckRule {
            from: speech,
            to: DuckTarget::Kind(LayerKind::Music),
            config: fast,
        });
        e.add_duck_rule(DuckRule {
            from: music,
            to: DuckTarget::Layer(ambient),
            config: fast,
        });
        e.layer_mut(music)
            .set_source(constant(0.8, 1000))
            .ok()
            .unwrap();
        e.layer_mut(ambient)
            .set_source(constant(0.1, 1000))
            .ok()
            .unwrap();
        let mut out = [0.0; 64];
        e.render(&mut out);
        // No speech: music (0.8) ducks ambient to its floor.
        assert!((e.meters()[ambient.0].gain - fast.floor_gain).abs() < 1e-3);
        assert!((e.meters()[music.0].gain - 1.0).abs() < 1e-6);
    }

    #[test]
    fn kind_and_layer_scales_multiply_in() {
        let mut e = engine();
        let m = music_layer(&mut e, "m");
        e.layer_mut(m).set_source(constant(0.5, 100)).ok().unwrap();
        e.set_kind_scale(LayerKind::Music, 0.5);
        e.set_layer_scale(m, 0.5);
        e.set_master(0.5);
        let mut out = [0.0; 4];
        e.render(&mut out);
        assert!((out[0] - 0.0625).abs() < 1e-6, "{}", out[0]);
    }

    #[test]
    fn ended_layers_are_counted() {
        let mut e = engine();
        let m = music_layer(&mut e, "m");
        e.layer_mut(m).set_source(constant(0.5, 3)).ok().unwrap();
        let mut out = [0.0; 8];
        assert_eq!(e.render(&mut out).layers_ended, 1);
    }
}
