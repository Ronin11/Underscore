//! Play, pause, seek, speed, sleep: the player's state machine.
//!
//! The transport never touches audio. It tracks where the listener is in
//! the content and answers questions like "we just resumed after an hour,
//! how far back should we go?" and "is the sleep timer up?". The caller
//! supplies the clock as a monotonic [`Duration`].

use std::time::Duration;

/// How far to rewind on resume, by how long the pause lasted. Audible
/// snaps back a little; listeners who fell asleep want more.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RewindPolicy {
    /// `(paused at least, rewind by)`, ascending by pause length. The
    /// longest matching entry wins. Empty means never rewind.
    pub steps: Vec<(Duration, Duration)>,
}

impl Default for RewindPolicy {
    fn default() -> Self {
        Self {
            steps: vec![
                (Duration::from_secs(5), Duration::from_secs(2)),
                (Duration::from_secs(60), Duration::from_secs(5)),
                (Duration::from_mins(10), Duration::from_secs(10)),
                (Duration::from_hours(1), Duration::from_secs(20)),
                (Duration::from_hours(24), Duration::from_secs(30)),
            ],
        }
    }
}

impl RewindPolicy {
    /// A policy that never rewinds.
    #[must_use]
    pub fn none() -> Self {
        Self { steps: Vec::new() }
    }

    /// The rewind for a pause of `paused`.
    #[must_use]
    pub fn rewind_for(&self, paused: Duration) -> Duration {
        self.steps
            .iter()
            .filter(|(at_least, _)| paused >= *at_least)
            .map(|(_, by)| *by)
            .max()
            .unwrap_or(Duration::ZERO)
    }
}

/// Transport settings a listener can tune.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TransportConfig {
    /// Rewind on resume.
    pub rewind: RewindPolicy,
    /// The skip-back button.
    pub skip_back: Duration,
    /// The skip-forward button.
    pub skip_forward: Duration,
    /// A countdown sleep timer fades out over its last this-long.
    pub sleep_fade: Duration,
    /// Slowest speed the speed buttons reach.
    pub min_speed: f32,
    /// Fastest speed the speed buttons reach.
    pub max_speed: f32,
    /// Increment for the speed buttons.
    pub speed_step: f32,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            rewind: RewindPolicy::default(),
            skip_back: Duration::from_secs(15),
            skip_forward: Duration::from_secs(30),
            sleep_fade: Duration::from_secs(15),
            min_speed: 0.5,
            max_speed: 3.5,
            speed_step: 0.1,
        }
    }
}

/// A sleep timer request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SleepTimer {
    /// No timer.
    Off,
    /// Pause after this long, fading over the last `sleep_fade`.
    In(Duration),
    /// Pause when the shell reports the chapter ended.
    EndOfChapter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sleep {
    Off,
    Until(Duration),
    EndOfChapter,
}

/// Where the transport is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportState {
    /// Nothing loaded, or the content ended.
    Stopped,
    /// Rendering.
    Playing,
    /// Holding position since `since`.
    Paused {
        /// When the pause began, on the caller's clock.
        since: Duration,
    },
}

/// Something the caller must do to the audio in response to a transport
/// call. Returned rather than performed, since the transport has no audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Seek the speech source to this content time.
    Seek(Duration),
    /// Stop rendering speech and the layers that follow it.
    Pause,
}

/// The player's state machine.
#[derive(Debug, Clone)]
pub struct Transport {
    cfg: TransportConfig,
    state: TransportState,
    position: Duration,
    duration: Option<Duration>,
    speed: f32,
    sleep: Sleep,
}

impl Transport {
    /// A stopped transport at position zero, 1.0x, no sleep timer.
    #[must_use]
    pub fn new(cfg: TransportConfig) -> Self {
        Self {
            cfg,
            state: TransportState::Stopped,
            position: Duration::ZERO,
            duration: None,
            speed: 1.0,
            sleep: Sleep::Off,
        }
    }

    /// The configuration.
    #[must_use]
    pub fn config(&self) -> &TransportConfig {
        &self.cfg
    }

    /// Mutable configuration.
    pub fn config_mut(&mut self) -> &mut TransportConfig {
        &mut self.cfg
    }

    /// Current state.
    #[must_use]
    pub fn state(&self) -> TransportState {
        self.state
    }

    /// True while rendering.
    #[must_use]
    pub fn is_playing(&self) -> bool {
        self.state == TransportState::Playing
    }

    /// Content position.
    #[must_use]
    pub fn position(&self) -> Duration {
        self.position
    }

    /// Content length, if known.
    #[must_use]
    pub fn duration(&self) -> Option<Duration> {
        self.duration
    }

    /// Current speed.
    #[must_use]
    pub fn speed(&self) -> f32 {
        self.speed
    }

    /// Load new content: position zero, stopped, known length if any.
    pub fn load(&mut self, duration: Option<Duration>) {
        self.position = Duration::ZERO;
        self.duration = duration;
        self.state = TransportState::Stopped;
    }

    /// Start or resume. Resuming after a pause rewinds per policy and asks
    /// for a seek.
    pub fn play(&mut self, now: Duration) -> Option<Action> {
        let action = match self.state {
            TransportState::Playing | TransportState::Stopped => None,
            TransportState::Paused { since } => {
                let back = self.cfg.rewind.rewind_for(now.saturating_sub(since));
                if back.is_zero() {
                    None
                } else {
                    self.position = self.position.saturating_sub(back);
                    Some(Action::Seek(self.position))
                }
            }
        };
        self.state = TransportState::Playing;
        action
    }

    /// Pause, remembering when.
    pub fn pause(&mut self, now: Duration) -> Option<Action> {
        if self.state == TransportState::Playing {
            self.state = TransportState::Paused { since: now };
            Some(Action::Pause)
        } else {
            None
        }
    }

    /// Play if paused or stopped; pause if playing.
    pub fn toggle(&mut self, now: Duration) -> Option<Action> {
        if self.is_playing() {
            self.pause(now)
        } else {
            self.play(now)
        }
    }

    /// Mark the content as finished.
    pub fn stop(&mut self) {
        self.state = TransportState::Stopped;
        self.sleep = Sleep::Off;
    }

    /// Jump to `to`, clamped to the content length.
    pub fn seek(&mut self, to: Duration) -> Action {
        self.position = self.duration.map_or(to, |d| to.min(d));
        Action::Seek(self.position)
    }

    /// The skip-back button.
    pub fn skip_back(&mut self) -> Action {
        self.seek(self.position.saturating_sub(self.cfg.skip_back))
    }

    /// The skip-forward button.
    pub fn skip_forward(&mut self) -> Action {
        self.seek(self.position.saturating_add(self.cfg.skip_forward))
    }

    /// Record how far the speech source actually got, as content time.
    pub fn set_position(&mut self, position: Duration) {
        self.position = position;
    }

    /// Advance by `content` of content time, as rendered.
    pub fn advance(&mut self, content: Duration) {
        self.position = self.position.saturating_add(content);
    }

    /// Set the speed, clamped. Returns what was set.
    pub fn set_speed(&mut self, speed: f32) -> f32 {
        let s = if speed.is_finite() {
            speed.clamp(self.cfg.min_speed, self.cfg.max_speed)
        } else {
            1.0
        };
        self.speed = (s * 100.0).round() / 100.0;
        self.speed
    }

    /// One step faster.
    pub fn speed_up(&mut self) -> f32 {
        self.set_speed(self.speed + self.cfg.speed_step)
    }

    /// One step slower.
    pub fn speed_down(&mut self) -> f32 {
        self.set_speed(self.speed - self.cfg.speed_step)
    }

    /// Set or clear the sleep timer.
    pub fn set_sleep(&mut self, timer: SleepTimer, now: Duration) {
        self.sleep = match timer {
            SleepTimer::Off => Sleep::Off,
            SleepTimer::In(d) => Sleep::Until(now.saturating_add(d)),
            SleepTimer::EndOfChapter => Sleep::EndOfChapter,
        };
    }

    /// Push a countdown timer out by `by`. A finished or absent timer
    /// becomes a fresh countdown of `by`. End-of-chapter is left alone.
    pub fn extend_sleep(&mut self, by: Duration, now: Duration) {
        self.sleep = match self.sleep {
            Sleep::Until(t) if t > now => Sleep::Until(t.saturating_add(by)),
            Sleep::Off | Sleep::Until(_) => Sleep::Until(now.saturating_add(by)),
            Sleep::EndOfChapter => Sleep::EndOfChapter,
        };
    }

    /// Time left on a countdown timer, if one is set.
    #[must_use]
    pub fn sleep_remaining(&self, now: Duration) -> Option<Duration> {
        match self.sleep {
            Sleep::Until(t) => Some(t.saturating_sub(now)),
            Sleep::Off | Sleep::EndOfChapter => None,
        }
    }

    /// True if an end-of-chapter timer is armed.
    #[must_use]
    pub fn sleep_at_chapter_end(&self) -> bool {
        self.sleep == Sleep::EndOfChapter
    }

    /// Master gain for the sleep fade: 1.0 normally, falling to 0.0 over
    /// the countdown's last `sleep_fade`.
    #[must_use]
    pub fn sleep_gain(&self, now: Duration) -> f32 {
        let Sleep::Until(t) = self.sleep else {
            return 1.0;
        };
        let left = t.saturating_sub(now);
        let fade = self.cfg.sleep_fade;
        if left >= fade || fade.is_zero() {
            return if left.is_zero() { 0.0 } else { 1.0 };
        }
        (left.as_secs_f32() / fade.as_secs_f32()).clamp(0.0, 1.0)
    }

    /// Call once per render block. Fires a countdown timer that is up.
    pub fn tick(&mut self, now: Duration) -> Option<Action> {
        if let Sleep::Until(t) = self.sleep
            && now >= t
            && self.is_playing()
        {
            self.sleep = Sleep::Off;
            return self.pause(now);
        }
        None
    }

    /// The shell saw a chapter boundary go by. Fires an end-of-chapter
    /// timer.
    pub fn chapter_ended(&mut self, now: Duration) -> Option<Action> {
        if self.sleep == Sleep::EndOfChapter && self.is_playing() {
            self.sleep = Sleep::Off;
            return self.pause(now);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn rewind_policy_scales_with_pause_length() {
        let p = RewindPolicy::default();
        assert_eq!(p.rewind_for(secs(1)), Duration::ZERO);
        assert_eq!(p.rewind_for(secs(30)), secs(2));
        assert_eq!(p.rewind_for(secs(300)), secs(5));
        assert_eq!(p.rewind_for(secs(3600)), secs(20));
        assert_eq!(p.rewind_for(secs(100_000)), secs(30));
        assert_eq!(
            RewindPolicy::none().rewind_for(secs(100_000)),
            Duration::ZERO
        );
    }

    #[test]
    fn resume_after_long_pause_rewinds_and_seeks() {
        let mut t = Transport::new(TransportConfig::default());
        t.load(Some(secs(3600)));
        assert_eq!(t.play(secs(0)), None);
        t.advance(secs(600));
        assert_eq!(t.pause(secs(600)), Some(Action::Pause));
        assert_eq!(t.play(secs(600 + 120)), Some(Action::Seek(secs(595))));
        assert!(t.is_playing());
    }

    #[test]
    fn short_pause_does_not_rewind() {
        let mut t = Transport::new(TransportConfig::default());
        t.play(secs(0));
        t.advance(secs(100));
        t.pause(secs(100));
        assert_eq!(t.play(secs(102)), None);
        assert_eq!(t.position(), secs(100));
    }

    #[test]
    fn rewind_never_goes_negative() {
        let mut t = Transport::new(TransportConfig::default());
        t.play(secs(0));
        t.advance(secs(3));
        t.pause(secs(3));
        assert_eq!(t.play(secs(10_000)), Some(Action::Seek(Duration::ZERO)));
    }

    #[test]
    fn skips_clamp_to_content() {
        let mut t = Transport::new(TransportConfig::default());
        t.load(Some(secs(100)));
        assert_eq!(t.skip_back(), Action::Seek(Duration::ZERO));
        t.set_position(secs(90));
        assert_eq!(t.skip_forward(), Action::Seek(secs(100)));
        t.set_position(secs(50));
        assert_eq!(t.skip_back(), Action::Seek(secs(35)));
    }

    #[test]
    fn speed_steps_and_clamps() {
        let mut t = Transport::new(TransportConfig::default());
        assert!((t.speed_up() - 1.1).abs() < 1e-6);
        assert!((t.set_speed(9.0) - 3.5).abs() < 1e-6);
        assert!((t.speed_down() - 3.4).abs() < 1e-6);
        assert!((t.set_speed(0.0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn countdown_timer_fades_then_pauses() {
        let mut t = Transport::new(TransportConfig::default());
        t.play(secs(0));
        t.set_sleep(SleepTimer::In(secs(60)), secs(0));
        assert_eq!(t.sleep_remaining(secs(10)), Some(secs(50)));
        assert!((t.sleep_gain(secs(10)) - 1.0).abs() < 1e-6);
        // Half way through the 15 s fade.
        assert!((t.sleep_gain(Duration::from_millis(52_500)) - 0.5).abs() < 1e-3);
        assert_eq!(t.tick(secs(59)), None);
        assert_eq!(t.tick(secs(60)), Some(Action::Pause));
        assert!(!t.is_playing());
        assert_eq!(t.sleep_remaining(secs(61)), None);
        assert!(
            (t.sleep_gain(secs(61)) - 1.0).abs() < 1e-6,
            "cleared after firing"
        );
    }

    #[test]
    fn extend_pushes_a_live_timer_and_restarts_a_dead_one() {
        let mut t = Transport::new(TransportConfig::default());
        t.play(secs(0));
        t.set_sleep(SleepTimer::In(secs(60)), secs(0));
        t.extend_sleep(secs(30), secs(10));
        assert_eq!(t.sleep_remaining(secs(10)), Some(secs(80)));
        t.set_sleep(SleepTimer::Off, secs(10));
        t.extend_sleep(secs(30), secs(10));
        assert_eq!(t.sleep_remaining(secs(10)), Some(secs(30)));
    }

    #[test]
    fn end_of_chapter_timer_fires_on_boundary() {
        let mut t = Transport::new(TransportConfig::default());
        t.play(secs(0));
        t.set_sleep(SleepTimer::EndOfChapter, secs(0));
        assert!(t.sleep_at_chapter_end());
        assert_eq!(t.tick(secs(10_000)), None, "countdown logic does not apply");
        assert_eq!(t.chapter_ended(secs(100)), Some(Action::Pause));
        assert!(!t.sleep_at_chapter_end());
        assert_eq!(t.chapter_ended(secs(200)), None);
    }

    #[test]
    fn toggle_round_trips() {
        let mut t = Transport::new(TransportConfig::default());
        assert_eq!(t.toggle(secs(0)), None);
        assert!(t.is_playing());
        assert_eq!(t.toggle(secs(1)), Some(Action::Pause));
        assert!(!t.is_playing());
    }
}
