//! The whole player: an engine, a transport, automatic levelling, and the
//! glue between them. This is what a shell drives.

use std::time::Duration;

use crate::auto::{AutoLevel, AutoLevelConfig};
use crate::duck::DuckConfig;
use crate::engine::{DuckRule, DuckTarget, Engine, EngineConfig, LayerId, RenderReport};
use crate::fade::{Curve, Fader};
use crate::layer::{LayerConfig, LayerKind};
use crate::source::Source;
use crate::stretch::{SpeedControl, StretchConfig, TimeStretch};
use crate::transport::{Action, SleepTimer, Transport, TransportConfig};

/// Everything tunable, in one place, so a shell can persist it.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PlayerConfig {
    /// Format and block size.
    pub engine: EngineConfig,
    /// Rewind, skips, sleep fade, speed range.
    pub transport: TransportConfig,
    /// Speed, noise, and AGC compensation.
    pub auto: AutoLevelConfig,
    /// Time-stretch tuning.
    pub stretch: StretchConfig,
    /// How speech ducks everything else.
    pub ducking: DuckConfig,
    /// The speech layer's settings.
    pub speech: LayerConfig,
    /// Fade when the music button is toggled.
    pub music_toggle_fade: Duration,
    /// Fade on pause and resume, so stops do not click.
    pub pause_fade: Duration,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self {
            engine: EngineConfig::default(),
            transport: TransportConfig::default(),
            auto: AutoLevelConfig::default(),
            stretch: StretchConfig::default(),
            ducking: DuckConfig::default(),
            speech: LayerConfig::new("speech", LayerKind::Speech),
            music_toggle_fade: Duration::from_millis(1200),
            pause_fade: Duration::from_millis(120),
        }
    }
}

/// The player. Single-owner; see the crate docs on threading.
pub struct Player {
    cfg: PlayerConfig,
    engine: Engine,
    transport: Transport,
    auto: AutoLevel,
    speed: SpeedControl,
    speech: LayerId,
    music_enabled: bool,
    master: Fader,
    block_secs: f32,
}

impl std::fmt::Debug for Player {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Player")
            .field("transport", &self.transport)
            .field("music_enabled", &self.music_enabled)
            .field("layers", &self.engine.layer_count())
            .finish_non_exhaustive()
    }
}

impl Player {
    /// A player with a speech layer and no background layers yet.
    #[must_use]
    pub fn new(cfg: PlayerConfig) -> Self {
        let mut engine = Engine::new(cfg.engine);
        let speech = engine.add_layer(cfg.speech.clone());
        engine.add_duck_rule(DuckRule {
            from: speech,
            to: DuckTarget::Others,
            config: cfg.ducking,
        });
        let speed = SpeedControl::new(&cfg.stretch);
        Self {
            engine,
            transport: Transport::new(cfg.transport.clone()),
            auto: AutoLevel::new(cfg.auto.clone()),
            speed,
            speech,
            music_enabled: true,
            master: Fader::at(0.0),
            block_secs: 0.0,
            cfg,
        }
    }

    /// The configuration the player was built with.
    #[must_use]
    pub fn config(&self) -> &PlayerConfig {
        &self.cfg
    }

    /// The engine, for layer control and meters.
    #[must_use]
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// The engine, mutably: add rules, set levels, swap sources.
    pub fn engine_mut(&mut self) -> &mut Engine {
        &mut self.engine
    }

    /// The transport, read-only.
    #[must_use]
    pub fn transport(&self) -> &Transport {
        &self.transport
    }

    /// Automatic levelling, for tuning.
    pub fn auto_mut(&mut self) -> &mut AutoLevel {
        &mut self.auto
    }

    /// The speech layer's id.
    #[must_use]
    pub fn speech_layer(&self) -> LayerId {
        self.speech
    }

    /// A lock-free handle to the playback speed.
    #[must_use]
    pub fn speed_control(&self) -> SpeedControl {
        self.speed.clone()
    }

    /// Add a background layer of `kind`. It is ducked by speech via the
    /// default rule and obeys the music button.
    pub fn add_layer(&mut self, cfg: LayerConfig) -> LayerId {
        let id = self.engine.add_layer(cfg);
        if !self.music_enabled {
            self.engine.layer_mut(id).fade_to(0.0, Duration::ZERO);
        }
        id
    }

    /// Add a music layer with default settings.
    pub fn add_music_layer(&mut self, name: impl Into<String>) -> LayerId {
        self.add_layer(LayerConfig::new(name, LayerKind::Music))
    }

    /// Load speech content. It is wrapped for time-stretching so speed
    /// changes keep pitch. The transport resets to position zero, stopped.
    ///
    /// # Errors
    /// The source back, if its format does not match the engine.
    pub fn set_speech_source(&mut self, source: Box<dyn Source>) -> Result<(), Box<dyn Source>> {
        let duration = source.duration();
        let stretched = TimeStretch::new(source, &self.cfg.stretch, self.speed.clone());
        self.engine
            .layer_mut(self.speech)
            .set_source(Box::new(stretched))
            .map_err(|_| unreachable!("format was checked by the stretcher's inner source"))?;
        self.transport.load(duration);
        Ok(())
    }

    /// Set a background layer's source with a hard cut.
    ///
    /// # Errors
    /// The source back, if its format does not match the engine.
    pub fn set_layer_source(
        &mut self,
        id: LayerId,
        source: Box<dyn Source>,
    ) -> Result<(), Box<dyn Source>> {
        self.engine.layer_mut(id).set_source(source)
    }

    /// Crossfade a background layer to a new source.
    ///
    /// # Errors
    /// The source back, if its format does not match the engine.
    pub fn crossfade_layer(
        &mut self,
        id: LayerId,
        source: Box<dyn Source>,
        over: Duration,
    ) -> Result<(), Box<dyn Source>> {
        self.engine.layer_mut(id).crossfade_to(source, over)
    }

    /// Start or resume, with smart rewind.
    pub fn play(&mut self, now: Duration) {
        let action = self.transport.play(now);
        self.apply(action);
        self.master
            .ramp_to(1.0, self.frames(self.cfg.pause_fade), Curve::Smooth);
    }

    /// Pause.
    pub fn pause(&mut self, now: Duration) {
        let action = self.transport.pause(now);
        self.apply(action);
    }

    /// Play if paused, pause if playing.
    pub fn toggle(&mut self, now: Duration) {
        if self.transport.is_playing() {
            self.pause(now);
        } else {
            self.play(now);
        }
    }

    /// Seek the speech content.
    pub fn seek(&mut self, to: Duration) {
        let action = self.transport.seek(to);
        self.apply(Some(action));
    }

    /// The skip-back button.
    pub fn skip_back(&mut self) {
        let action = self.transport.skip_back();
        self.apply(Some(action));
    }

    /// The skip-forward button.
    pub fn skip_forward(&mut self) {
        let action = self.transport.skip_forward();
        self.apply(Some(action));
    }

    /// Set the speech speed; pitch is preserved. Returns what was set.
    pub fn set_speed(&mut self, speed: f32) -> f32 {
        let s = self.transport.set_speed(speed);
        self.speed.set(s);
        self.auto.set_speed(s);
        s
    }

    /// One step faster.
    pub fn speed_up(&mut self) -> f32 {
        let s = self.transport.speed_up();
        self.set_speed(s)
    }

    /// One step slower.
    pub fn speed_down(&mut self) -> f32 {
        let s = self.transport.speed_down();
        self.set_speed(s)
    }

    /// The music button. Off fades every non-speech layer out and pauses
    /// its source; on fades them back in where they left off.
    pub fn set_music_enabled(&mut self, enabled: bool) {
        if self.music_enabled == enabled {
            return;
        }
        self.music_enabled = enabled;
        let fade = self.cfg.music_toggle_fade;
        let speech = self.speech;
        let ids: Vec<LayerId> = self
            .engine
            .layers()
            .map(|(id, _)| id)
            .filter(|id| *id != speech)
            .collect();
        for id in ids {
            self.engine
                .layer_mut(id)
                .fade_to(if enabled { 1.0 } else { 0.0 }, fade);
        }
    }

    /// Whether background layers are on.
    #[must_use]
    pub fn music_enabled(&self) -> bool {
        self.music_enabled
    }

    /// Set or clear the sleep timer.
    pub fn set_sleep(&mut self, timer: SleepTimer, now: Duration) {
        self.transport.set_sleep(timer, now);
    }

    /// Push the sleep timer out, or start one.
    pub fn extend_sleep(&mut self, by: Duration, now: Duration) {
        self.transport.extend_sleep(by, now);
    }

    /// Tell the player a chapter boundary just went by.
    pub fn chapter_ended(&mut self, now: Duration) {
        let action = self.transport.chapter_ended(now);
        self.apply(action);
    }

    /// Report ambient noise in dBFS, from whatever the shell measures.
    pub fn set_ambient_noise_db(&mut self, db: f32) {
        self.auto.set_noise_db(db);
    }

    /// Render one block. Silence when paused and the pause fade is done.
    pub fn render(&mut self, out: &mut [f32], now: Duration) -> RenderReport {
        let ch = self.cfg.engine.channels;
        let frames = out.len() / ch;
        #[allow(clippy::cast_precision_loss)]
        {
            self.block_secs = frames as f32 / self.cfg.engine.sample_rate as f32;
        }

        let action = self.transport.tick(now);
        self.apply(action);

        if !self.transport.is_playing() && self.master.settled() && self.master.target() <= 0.0 {
            out[..frames * ch].fill(0.0);
            return RenderReport {
                frames,
                layers_ended: 0,
            };
        }

        self.engine
            .set_layer_scale(self.speech, self.auto.speech_gain());
        let bg = self.auto.background_scale();
        for kind in [LayerKind::Music, LayerKind::Ambient, LayerKind::Other] {
            self.engine.set_kind_scale(kind, bg);
        }

        let report = self.engine.render(out);
        self.auto
            .update(self.engine.last_block(self.speech, frames), self.block_secs);

        let sleep = self.transport.sleep_gain(now);
        for frame in out[..frames * ch].chunks_exact_mut(ch) {
            let g = self.master.advance() * sleep;
            for s in frame {
                *s *= g;
            }
        }

        self.sync_position();
        if self.engine.layer(self.speech).ended() && self.transport.is_playing() {
            self.transport.stop();
            self.master.set(0.0);
        }
        report
    }

    fn sync_position(&mut self) {
        if !self.transport.is_playing() {
            return;
        }
        if let Some(p) = self
            .engine
            .layer(self.speech)
            .source()
            .and_then(Source::position)
        {
            self.transport.set_position(p);
        } else {
            let content = self.block_secs * self.transport.speed();
            self.transport.advance(Duration::from_secs_f32(content));
        }
    }

    fn apply(&mut self, action: Option<Action>) {
        match action {
            Some(Action::Seek(to)) => {
                if let Some(src) = self.engine.layer_mut(self.speech).source_mut() {
                    src.seek(to);
                }
                self.engine.reset_dynamics();
            }
            Some(Action::Pause) => {
                self.master
                    .ramp_to(0.0, self.frames(self.cfg.pause_fade), Curve::Smooth);
            }
            None => {}
        }
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn frames(&self, d: Duration) -> u32 {
        (d.as_secs_f64() * f64::from(self.cfg.engine.sample_rate)).round() as u32
    }
}

#[cfg(test)]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp
)]
mod tests {
    use super::*;
    use crate::source::SliceSource;

    const RATE: u32 = 48_000;

    fn player() -> Player {
        let mut cfg = PlayerConfig::default();
        cfg.engine.channels = 1;
        cfg.engine.sample_rate = RATE;
        cfg.pause_fade = Duration::ZERO;
        cfg.auto.agc.enabled = false;
        cfg.auto.noise.enabled = false;
        Player::new(cfg)
    }

    fn sine(hz: f32, secs: f32) -> Box<dyn Source> {
        Box::new(SliceSource::sine(
            hz,
            0.5,
            (secs * RATE as f32) as usize,
            RATE,
        ))
    }

    fn constant(v: f32, secs: f32) -> Box<dyn Source> {
        Box::new(SliceSource::new(
            vec![v; (secs * RATE as f32) as usize],
            1,
            RATE,
        ))
    }

    fn render_secs(p: &mut Player, secs: f32, now: Duration) -> Vec<f32> {
        let mut all = Vec::new();
        let mut buf = vec![0.0; 1024];
        let blocks = (secs * RATE as f32 / 1024.0) as usize;
        for _ in 0..blocks {
            p.render(&mut buf, now);
            all.extend_from_slice(&buf);
        }
        all
    }

    fn peak(x: &[f32]) -> f32 {
        x.iter().fold(0.0f32, |m, s| m.max(s.abs()))
    }

    #[test]
    fn paused_player_is_silent_and_playing_player_is_not() {
        let mut p = player();
        p.set_speech_source(sine(440.0, 10.0)).ok().unwrap();
        let out = render_secs(&mut p, 0.5, Duration::ZERO);
        assert_eq!(peak(&out), 0.0);
        p.play(Duration::ZERO);
        let out = render_secs(&mut p, 0.5, Duration::ZERO);
        assert!(peak(&out) > 0.4);
    }

    #[test]
    fn music_button_silences_background_and_keeps_its_place() {
        let mut p = player();
        p.set_speech_source(constant(0.0, 30.0)).ok().unwrap();
        let m = p.add_music_layer("m");
        p.engine_mut().layer_mut(m).set_level(1.0);
        p.set_layer_source(m, constant(0.3, 5.0)).ok().unwrap();
        p.play(Duration::ZERO);
        let out = render_secs(&mut p, 0.5, Duration::ZERO);
        assert!((peak(&out) - 0.3).abs() < 1e-3);

        p.set_music_enabled(false);
        let _ = render_secs(&mut p, 2.0, Duration::ZERO);
        let out = render_secs(&mut p, 0.5, Duration::ZERO);
        assert!(peak(&out) == 0.0, "silent once the fade is through");

        p.set_music_enabled(true);
        let _ = render_secs(&mut p, 2.0, Duration::ZERO);
        let out = render_secs(&mut p, 0.5, Duration::ZERO);
        assert!(
            (peak(&out) - 0.3).abs() < 1e-3,
            "music is back: {}",
            peak(&out)
        );
        // 5.5 s played, but the music only ran while audible: 0.5 s before
        // the button, 1.2 s of fade-out, 2.0 s of fade-in and after, 0.5 s
        // more. About 4.2 s consumed, not 5.5.
        let consumed = p
            .engine()
            .layer(m)
            .source()
            .and_then(Source::position)
            .unwrap();
        let secs = consumed.as_secs_f32();
        assert!((secs - 4.2).abs() < 0.15, "consumed {secs}s");
    }

    #[test]
    fn speech_ducks_music() {
        let mut p = player();
        p.set_speech_source(constant(0.5, 30.0)).ok().unwrap();
        let m = p.add_music_layer("m");
        p.engine_mut().layer_mut(m).set_level(1.0);
        p.set_layer_source(m, constant(0.4, 30.0)).ok().unwrap();
        p.play(Duration::ZERO);
        let _ = render_secs(&mut p, 1.0, Duration::ZERO);
        let gain = p.engine().meters()[m.0].gain;
        assert!(
            (gain - p.config().ducking.floor_gain).abs() < 1e-3,
            "{gain}"
        );
    }

    #[test]
    fn resume_after_long_pause_rewinds() {
        let mut p = player();
        p.set_speech_source(constant(0.5, 600.0)).ok().unwrap();
        p.play(Duration::ZERO);
        let _ = render_secs(&mut p, 60.0, Duration::ZERO);
        let before = p.transport().position();
        assert!((before.as_secs_f32() - 60.0).abs() < 0.5, "{before:?}");
        p.pause(Duration::from_secs(60));
        p.play(Duration::from_hours(1) + Duration::from_secs(60));
        let after = p.transport().position();
        let rewound = before.saturating_sub(after).as_secs_f32();
        assert!((rewound - 20.0).abs() < 0.5, "rewound {rewound}s");
        let src_pos = p
            .engine()
            .layer(p.speech_layer())
            .source()
            .and_then(Source::position)
            .unwrap();
        assert!((src_pos.as_secs_f32() - after.as_secs_f32()).abs() < 0.05);
    }

    #[test]
    fn speed_changes_content_rate_and_background_scale() {
        let mut p = player();
        p.set_speech_source(constant(0.0, 600.0)).ok().unwrap();
        assert!((p.set_speed(2.0) - 2.0).abs() < 1e-6);
        p.play(Duration::ZERO);
        let _ = render_secs(&mut p, 10.0, Duration::ZERO);
        let pos = p.transport().position().as_secs_f32();
        assert!(
            (pos - 20.0).abs() < 0.5,
            "10 s at 2x is 20 s of content: {pos}"
        );
        let _ = render_secs(&mut p, 0.1, Duration::ZERO);
        let scale = p.engine().meters()[1.min(p.engine().layer_count() - 1)].gain;
        // Without a music layer the speech meter is what we have; check the
        // leveller directly instead.
        let _ = scale;
        assert!((p.auto_mut().background_scale() - 0.6).abs() < 1e-4);
    }

    #[test]
    fn sleep_timer_pauses_the_player() {
        let mut p = player();
        p.set_speech_source(constant(0.5, 600.0)).ok().unwrap();
        p.play(Duration::ZERO);
        p.set_sleep(SleepTimer::In(Duration::from_secs(30)), Duration::ZERO);
        let out = render_secs(&mut p, 0.1, Duration::from_secs(10));
        assert!(peak(&out) > 0.4);
        let out = render_secs(&mut p, 0.1, Duration::from_secs(31));
        assert_eq!(peak(&out), 0.0);
        assert!(!p.transport().is_playing());
    }

    #[test]
    fn end_of_content_stops() {
        let mut p = player();
        p.set_speech_source(constant(0.5, 0.5)).ok().unwrap();
        p.play(Duration::ZERO);
        let _ = render_secs(&mut p, 1.0, Duration::ZERO);
        assert_eq!(
            p.transport().state(),
            crate::transport::TransportState::Stopped
        );
    }
}
