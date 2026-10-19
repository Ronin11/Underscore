//! The Android bridge.
//!
//! [`Bridge`] is plain Rust: a [`Player`] with one speech layer and one
//! music layer, and the handful of operations the app exposes. The `jni`
//! module wraps it for Kotlin. The Kotlin side owns the audio device (an
//! `AudioTrack`) and calls `render` from its playback thread; everything
//! else is called from the UI thread. A mutex serialises them, which is
//! fine at this stage: control calls are rare and the lock is held for one
//! block at most.
//!
//! No audio is ever written to storage here. Files come in as descriptors
//! the app opened read-only; the mix leaves through `render` only.

use std::fs::File;
use std::os::fd::FromRawFd;
use std::time::Duration;

use underscore_core::{LayerId, Player, PlayerConfig, SleepTimer, Source};
use underscore_decode::{FileSource, MediaSource};

pub mod jni;

/// A player wired the way the app needs it.
pub struct Bridge {
    player: Player,
    music: LayerId,
    rate: u32,
    channels: usize,
}

impl std::fmt::Debug for Bridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bridge")
            .field("rate", &self.rate)
            .field("channels", &self.channels)
            .finish_non_exhaustive()
    }
}

impl Bridge {
    /// A player at the device format, with one music layer.
    #[must_use]
    pub fn new(sample_rate: u32, channels: usize) -> Self {
        let mut cfg = PlayerConfig::default();
        cfg.engine.sample_rate = sample_rate;
        cfg.engine.channels = channels;
        let mut player = Player::new(cfg);
        let music = player.add_music_layer("music");
        Self {
            player,
            music,
            rate: sample_rate,
            channels,
        }
    }

    fn decode(&self, source: Box<dyn MediaSource>, hint: Option<&str>) -> Option<Box<dyn Source>> {
        FileSource::open(source, hint, self.rate, self.channels)
            .ok()
            .map(|s| Box::new(s) as Box<dyn Source>)
    }

    /// Load the book or podcast from an already-open descriptor. Takes
    /// ownership of the descriptor.
    pub fn load_speech_fd(&mut self, fd: i32, hint: Option<&str>) -> bool {
        // SAFETY: the caller detached this descriptor from its Android
        // owner and hands it to us exactly once; we own it from here.
        let file = unsafe { File::from_raw_fd(fd) };
        self.decode(Box::new(file), hint)
            .is_some_and(|src| self.player.set_speech_source(src).is_ok())
    }

    /// Load the music from an already-open descriptor. Takes ownership.
    pub fn load_music_fd(&mut self, fd: i32, hint: Option<&str>) -> bool {
        // SAFETY: as in `load_speech_fd`.
        let file = unsafe { File::from_raw_fd(fd) };
        let music = self.music;
        self.decode(Box::new(file), hint)
            .is_some_and(|src| self.player.set_layer_source(music, src).is_ok())
    }

    /// Load the book from a path. Used by tests and desktop shells.
    pub fn load_speech_path(&mut self, path: &str) -> bool {
        FileSource::open_path(path, self.rate, self.channels)
            .is_ok_and(|s| self.player.set_speech_source(Box::new(s)).is_ok())
    }

    /// Load music from a path.
    pub fn load_music_path(&mut self, path: &str) -> bool {
        let music = self.music;
        FileSource::open_path(path, self.rate, self.channels)
            .is_ok_and(|s| self.player.set_layer_source(music, Box::new(s)).is_ok())
    }

    /// The player, for anything not wrapped here.
    pub fn player_mut(&mut self) -> &mut Player {
        &mut self.player
    }

    /// Play or resume.
    pub fn play(&mut self, now: Duration) {
        self.player.play(now);
    }

    /// Pause.
    pub fn pause(&mut self, now: Duration) {
        self.player.pause(now);
    }

    /// Play if paused, pause if playing.
    pub fn toggle(&mut self, now: Duration) {
        self.player.toggle(now);
    }

    /// True while playing.
    #[must_use]
    pub fn is_playing(&self) -> bool {
        self.player.transport().is_playing()
    }

    /// Set speed; returns what was set.
    pub fn set_speed(&mut self, speed: f32) -> f32 {
        self.player.set_speed(speed)
    }

    /// Current speed.
    #[must_use]
    pub fn speed(&self) -> f32 {
        self.player.transport().speed()
    }

    /// The music button.
    pub fn set_music_enabled(&mut self, enabled: bool) {
        self.player.set_music_enabled(enabled);
    }

    /// Whether the music button is on.
    #[must_use]
    pub fn music_enabled(&self) -> bool {
        self.player.music_enabled()
    }

    /// Music level before ducking, `0.0..=1.0`.
    pub fn set_music_level(&mut self, level: f32) {
        let music = self.music;
        self.player.engine_mut().layer_mut(music).set_level(level);
    }

    /// Skip back by the configured amount.
    pub fn skip_back(&mut self) {
        self.player.skip_back();
    }

    /// Skip forward by the configured amount.
    pub fn skip_forward(&mut self) {
        self.player.skip_forward();
    }

    /// Seek the book.
    pub fn seek(&mut self, to: Duration) {
        self.player.seek(to);
    }

    /// Set a countdown sleep timer, or clear it with zero.
    pub fn set_sleep(&mut self, after: Duration, now: Duration) {
        let timer = if after.is_zero() {
            SleepTimer::Off
        } else {
            SleepTimer::In(after)
        };
        self.player.set_sleep(timer, now);
    }

    /// Push the sleep timer out.
    pub fn extend_sleep(&mut self, by: Duration, now: Duration) {
        self.player.extend_sleep(by, now);
    }

    /// Time left on the sleep timer.
    #[must_use]
    pub fn sleep_remaining(&self, now: Duration) -> Option<Duration> {
        self.player.transport().sleep_remaining(now)
    }

    /// Report room noise in dBFS.
    pub fn set_noise_db(&mut self, db: f32) {
        self.player.set_ambient_noise_db(db);
    }

    /// Book position.
    #[must_use]
    pub fn position(&self) -> Duration {
        self.player.transport().position()
    }

    /// Book length, if known.
    #[must_use]
    pub fn duration(&self) -> Option<Duration> {
        self.player.transport().duration()
    }

    /// Render one block of interleaved samples.
    pub fn render(&mut self, out: &mut [f32], now: Duration) -> usize {
        self.player.render(out, now).frames
    }
}

/// Map an Android MIME type to a file-extension hint for the prober.
#[must_use]
pub fn hint_for_mime(mime: &str) -> Option<&'static str> {
    Some(match mime {
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/mp4" | "audio/x-m4a" | "audio/m4a" => "m4a",
        "audio/x-m4b" | "audio/m4b" => "m4b",
        "audio/flac" | "audio/x-flac" => "flac",
        "audio/ogg" | "application/ogg" => "ogg",
        "audio/wav" | "audio/x-wav" | "audio/vnd.wave" => "wav",
        "audio/aac" => "aac",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_starts_stopped_and_silent() {
        let mut b = Bridge::new(48_000, 2);
        let mut out = vec![1.0; 512];
        b.render(&mut out, Duration::ZERO);
        assert!(out.iter().all(|v| *v == 0.0));
        assert!(!b.is_playing());
        assert!(b.music_enabled());
    }

    #[test]
    fn controls_round_trip() {
        let mut b = Bridge::new(48_000, 2);
        assert!((b.set_speed(1.5) - 1.5).abs() < 1e-6);
        assert!((b.speed() - 1.5).abs() < 1e-6);
        b.set_music_enabled(false);
        assert!(!b.music_enabled());
        b.set_sleep(Duration::from_secs(600), Duration::ZERO);
        assert_eq!(
            b.sleep_remaining(Duration::from_secs(60)),
            Some(Duration::from_secs(540))
        );
        b.set_sleep(Duration::ZERO, Duration::ZERO);
        assert_eq!(b.sleep_remaining(Duration::ZERO), None);
    }

    #[test]
    fn mime_hints() {
        assert_eq!(hint_for_mime("audio/mpeg"), Some("mp3"));
        assert_eq!(hint_for_mime("audio/x-m4b"), Some("m4b"));
        assert_eq!(hint_for_mime("text/plain"), None);
    }

    #[test]
    fn missing_file_fails_cleanly() {
        let mut b = Bridge::new(48_000, 2);
        assert!(!b.load_speech_path("/nonexistent/book.m4b"));
    }
}
