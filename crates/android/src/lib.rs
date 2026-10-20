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
use std::io::{Seek, SeekFrom};
use std::os::fd::FromRawFd;
use std::time::Duration;

use underscore_core::{LayerId, Player, PlayerConfig, SleepTimer, Source};
use underscore_decode::{Chapter, FileSource, MediaSource, probe, probe_path};

/// How far past a chapter start the position may be for the crossing to
/// count as the previous chapter ending, rather than a seek landing there.
const CROSSING_WINDOW: Duration = Duration::from_secs(2);

pub mod jni;

/// A player wired the way the app needs it.
pub struct Bridge {
    player: Player,
    music: LayerId,
    rate: u32,
    channels: usize,
    /// Chapter marks of the loaded book, in order.
    chapters: Vec<Chapter>,
    /// The chapter the position was in at the last render.
    last_chapter: Option<usize>,
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
            chapters: Vec::new(),
            last_chapter: None,
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
        let mut file = unsafe { File::from_raw_fd(fd) };
        // Chapter marks first, through a second handle. The handles share
        // an offset, so rewind before decoding.
        let chapters = file
            .try_clone()
            .ok()
            .and_then(|f| probe(Box::new(f), hint).ok())
            .map(|p| p.chapters)
            .unwrap_or_default();
        if file.seek(SeekFrom::Start(0)).is_err() {
            return false;
        }
        let ok = self
            .decode(Box::new(file), hint)
            .is_some_and(|src| self.player.set_speech_source(src).is_ok());
        if ok {
            self.set_chapters(chapters);
        }
        ok
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
        let chapters = probe_path(path).map(|p| p.chapters).unwrap_or_default();
        let ok = FileSource::open_path(path, self.rate, self.channels)
            .is_ok_and(|s| self.player.set_speech_source(Box::new(s)).is_ok());
        if ok {
            self.set_chapters(chapters);
        }
        ok
    }

    fn set_chapters(&mut self, chapters: Vec<Chapter>) {
        self.chapters = chapters;
        self.last_chapter = None;
    }

    /// The loaded book's chapter marks, in order. Empty when it has none.
    #[must_use]
    pub fn chapters(&self) -> &[Chapter] {
        &self.chapters
    }

    /// Which chapter `at` falls in.
    #[must_use]
    pub fn chapter_at(&self, at: Duration) -> Option<usize> {
        self.chapters
            .partition_point(|c| c.start <= at)
            .checked_sub(1)
    }

    /// The chapter the book is in now.
    #[must_use]
    pub fn current_chapter(&self) -> Option<usize> {
        self.chapter_at(self.position())
    }

    /// Jump to the start of chapter `index`. False if there is no such
    /// chapter.
    pub fn seek_chapter(&mut self, index: usize) -> bool {
        let Some(start) = self.chapters.get(index).map(|c| c.start) else {
            return false;
        };
        self.player.seek(start);
        self.last_chapter = Some(index);
        true
    }

    /// Pause when the current chapter ends.
    pub fn set_sleep_end_of_chapter(&mut self, now: Duration) {
        self.player.set_sleep(SleepTimer::EndOfChapter, now);
    }

    /// Whether a sleep at the end of the chapter is set.
    #[must_use]
    pub fn sleep_at_chapter_end(&self) -> bool {
        self.player.transport().sleep_at_chapter_end()
    }

    /// Tell the player when playback carries it over a chapter boundary.
    /// A seek that lands just past one does not count.
    fn watch_chapters(&mut self, now: Duration) {
        let at = self.position();
        let current = self.chapter_at(at);
        if let (Some(prev), Some(cur)) = (self.last_chapter, current)
            && cur == prev + 1
            && at.saturating_sub(self.chapters[cur].start) < CROSSING_WINDOW
        {
            self.player.chapter_ended(now);
        }
        self.last_chapter = current;
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
        self.last_chapter = self.chapter_at(to);
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
        let frames = self.player.render(out, now).frames;
        if !self.chapters.is_empty() {
            self.watch_chapters(now);
        }
        frames
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

    fn fixture() -> String {
        format!(
            "{}/../decode/tests/fixtures/both.m4b",
            env!("CARGO_MANIFEST_DIR")
        )
    }

    #[test]
    fn loads_chapters_and_seeks_by_them() {
        let mut b = Bridge::new(48_000, 2);
        assert!(b.load_speech_path(&fixture()));
        assert_eq!(b.chapters().len(), 3);
        assert_eq!(b.chapters()[1].title.as_deref(), Some("Two – Middle"));
        assert_eq!(b.current_chapter(), Some(0));
        assert!(b.seek_chapter(2));
        assert_eq!(b.position(), Duration::from_millis(7500));
        assert_eq!(b.current_chapter(), Some(2));
        assert!(!b.seek_chapter(3));
        b.seek(Duration::from_secs(4));
        assert_eq!(b.current_chapter(), Some(1));
    }

    #[test]
    fn sleep_at_chapter_end_pauses_at_the_boundary() {
        let mut b = Bridge::new(48_000, 2);
        assert!(b.load_speech_path(&fixture()));
        b.seek(Duration::from_secs(6));
        let mut now = Duration::ZERO;
        b.play(now);
        b.set_sleep_end_of_chapter(now);
        assert!(b.sleep_at_chapter_end());
        let mut out = vec![0.0; 1024 * 2];
        let block = Duration::from_secs_f64(1024.0 / 48_000.0);
        for _ in 0..(48_000 * 3 / 1024) {
            b.render(&mut out, now);
            now += block;
            if !b.is_playing() {
                break;
            }
        }
        assert!(!b.is_playing(), "did not pause at the chapter end");
        assert!(b.position() >= Duration::from_millis(7500));
        assert!(b.position() < Duration::from_millis(9000));
        assert!(!b.sleep_at_chapter_end());
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
