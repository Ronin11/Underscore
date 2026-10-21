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
//! A music map adds one more thread, started by [`Bridge::shared`]: the
//! worker in [`follow`] that opens the next track while the lock is free
//! and hands it to the player with one crossfade.
//!
//! No audio is ever written to storage here. Files come in as descriptors
//! the app opened read-only; the mix leaves through `render` only.

use std::fs::File;
use std::io::{Seek, SeekFrom};
use std::os::fd::FromRawFd;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use underscore_core::{LayerId, Player, PlayerConfig, RewindPolicy, SleepTimer, Source};
use underscore_decode::{Chapter, FileSource, MediaSource, probe, probe_path};
use underscore_library::{Audiobookshelf, Cache, Credentials, ItemId, LocalLibrary, Role};
use underscore_map::Map;

use crate::follow::{Job, MapPlayback, Wake};

/// How far past a chapter start the position may be for the crossing to
/// count as the previous chapter ending, rather than a seek landing there.
const CROSSING_WINDOW: Duration = Duration::from_secs(2);

/// How long the map's music takes to fade when the map is removed.
const CLEAR_FADE: Duration = Duration::from_secs(2);

pub mod follow;
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
    /// The music map being followed, if one is loaded.
    map: Option<MapPlayback>,
    /// Wakes the map worker when a job is waiting.
    wake: Arc<Wake>,
    /// The Audiobookshelf server, once signed in.
    server: Option<Arc<Audiobookshelf>>,
    /// The server book that is loaded, for progress reports.
    book: Option<ItemId>,
}

impl std::fmt::Debug for Bridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bridge")
            .field("rate", &self.rate)
            .field("channels", &self.channels)
            .field("map", &self.map)
            .finish_non_exhaustive()
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.wake.stop();
    }
}

impl Bridge {
    /// A player at the device format, with one music layer. No map worker
    /// runs; map jobs must be served by hand. See [`Bridge::shared`].
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
            map: None,
            wake: Arc::new(Wake::default()),
            server: None,
            book: None,
        }
    }

    /// A bridge behind a lock, with the map worker running. The worker
    /// exits when the last reference is dropped.
    #[must_use]
    pub fn shared(sample_rate: u32, channels: usize) -> Arc<Mutex<Self>> {
        let bridge = Self::new(sample_rate, channels);
        let wake = Arc::clone(&bridge.wake);
        let shared = Arc::new(Mutex::new(bridge));
        follow::spawn(Arc::downgrade(&shared), wake);
        shared
    }

    /// The engine's sample rate.
    #[must_use]
    pub fn sample_rate(&self) -> u32 {
        self.rate
    }

    /// The engine's channel count.
    #[must_use]
    pub fn channels(&self) -> usize {
        self.channels
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
            self.book = None;
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

    /// Load a book fetched from the server, by its cached path. The
    /// server's chapter list stands in when the file carries none.
    pub fn load_server_book(&mut self, path: &str, id: ItemId, chapters: Vec<Chapter>) -> bool {
        let ok = self.load_speech_path(path);
        if ok {
            if self.chapters.is_empty() && !chapters.is_empty() {
                self.set_chapters(chapters);
            }
            self.book = Some(id);
        }
        ok
    }

    /// Sign in to an Audiobookshelf server. Does network I/O: call it off
    /// the audio thread and outside the lock, then hand the result to
    /// [`Bridge::set_server`].
    ///
    /// # Errors
    /// The server could not be reached or refused the login.
    pub fn sign_in(
        url: &str,
        username: &str,
        password: &str,
    ) -> Result<Arc<Audiobookshelf>, String> {
        Audiobookshelf::login("abs", url, username, password)
            .map(Arc::new)
            .map_err(|e| e.to_string())
    }

    /// Come back to a server with saved credentials. Checks them with one
    /// request, so it does network I/O like [`Bridge::sign_in`].
    ///
    /// # Errors
    /// The server could not be reached or no longer accepts the login.
    pub fn resume_server(url: &str, creds: Credentials) -> Result<Arc<Audiobookshelf>, String> {
        let server = Audiobookshelf::with_credentials("abs", url, creds);
        server.whoami().map_err(|e| e.to_string())?;
        Ok(Arc::new(server))
    }

    /// Use, or forget, a server.
    pub fn set_server(&mut self, server: Option<Arc<Audiobookshelf>>) {
        if server.is_none() {
            self.book = None;
        }
        self.server = server;
    }

    /// The server in use, shared so callers can talk to it off the lock.
    #[must_use]
    pub fn server(&self) -> Option<Arc<Audiobookshelf>> {
        self.server.clone()
    }

    /// The server book that is loaded, if the book came from the server.
    #[must_use]
    pub fn server_book(&self) -> Option<ItemId> {
        self.book.clone()
    }

    /// A new book is loaded: keep its chapters and refit the map to it.
    fn set_chapters(&mut self, chapters: Vec<Chapter>) {
        self.chapters = chapters;
        self.last_chapter = None;
        let starts = self.chapter_starts();
        if let Some(map) = &mut self.map {
            map.fit(&starts);
            self.wake.notify();
        }
    }

    fn chapter_starts(&self) -> Vec<Duration> {
        self.chapters.iter().map(|c| c.start).collect()
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

    /// Parse a map and scan the music folder it will play from. Slow: it
    /// probes every file under the folder. Run it on a worker thread and
    /// never under the bridge lock, then hand the result to
    /// [`Bridge::install_map`].
    ///
    /// # Errors
    /// The map does not parse, the folder cannot be read or holds no
    /// music, or the cache directory cannot be created.
    pub fn prepare_map(
        toml: &str,
        music_dir: &Path,
        cache_dir: &Path,
    ) -> Result<MapPlayback, String> {
        let map = Map::parse(toml).map_err(|e| e.to_string())?;
        let library =
            LocalLibrary::new("music", vec![(Role::Music, music_dir.to_path_buf())], None);
        let n = library
            .scan()
            .map_err(|e| format!("{}: {e}", music_dir.display()))?;
        if n == 0 {
            return Err(format!("no music found under {}", music_dir.display()));
        }
        let cache = Cache::open(cache_dir).map_err(|e| format!("cache: {e}"))?;
        Ok(MapPlayback::new(map, Arc::new(library), cache))
    }

    /// Follow a prepared map. Fits it to the loaded book, if there is one;
    /// otherwise it is fitted when a book loads. Returns whether it is
    /// following now.
    pub fn install_map(&mut self, mut map: MapPlayback) -> bool {
        let following = map.fit(&self.chapter_starts());
        self.map = Some(map);
        self.wake.notify();
        following
    }

    /// Stop following the map and fade its music out.
    pub fn clear_map(&mut self) {
        if self.map.take().is_some() {
            let music = self.music;
            let _ = self.player.crossfade_layer(
                music,
                follow::silence(self.rate, self.channels),
                CLEAR_FADE,
            );
        }
    }

    /// Whether a map is loaded.
    #[must_use]
    pub fn has_map(&self) -> bool {
        self.map.is_some()
    }

    /// One line on the map for the UI.
    #[must_use]
    pub fn map_status(&self) -> String {
        self.map
            .as_ref()
            .map_or_else(|| "No map".to_string(), MapPlayback::status)
    }

    /// The map job waiting to be opened, for the worker.
    pub fn take_job(&mut self) -> Option<Job> {
        self.map.as_mut().and_then(MapPlayback::take)
    }

    /// The worker opened `job`; apply it unless it is stale.
    pub fn finish_job(
        &mut self,
        job: &Job,
        result: Result<Box<dyn Source>, underscore_library::Error>,
    ) {
        let music = self.music;
        if let Some(map) = &mut self.map {
            map.finish(job, result, &mut self.player, music);
        }
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

    /// How far the skip buttons jump.
    pub fn set_skips(&mut self, back: Duration, forward: Duration) {
        let cfg = self.player.transport_mut().config_mut();
        cfg.skip_back = back;
        cfg.skip_forward = forward;
    }

    /// Rewind a little on resume, scaled by the length of the pause, or not.
    pub fn set_rewind(&mut self, enabled: bool) {
        self.player.transport_mut().config_mut().rewind = if enabled {
            RewindPolicy::default()
        } else {
            RewindPolicy::none()
        };
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

    /// Whether the music layer has something to play.
    #[must_use]
    pub fn music_loaded(&self) -> bool {
        self.player.engine().layer(self.music).source().is_some()
    }

    /// Render one block of interleaved samples.
    pub fn render(&mut self, out: &mut [f32], now: Duration) -> usize {
        let frames = self.player.render(out, now).frames;
        if !self.chapters.is_empty() {
            self.watch_chapters(now);
        }
        let at = self.position();
        if self.map.as_mut().is_some_and(|m| m.update(at)) {
            self.wake.notify();
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
    use std::path::PathBuf;
    use underscore_library::Direction;

    #[test]
    fn bridge_starts_stopped_and_silent() {
        let mut b = Bridge::new(48_000, 2);
        let mut out = vec![1.0; 512];
        b.render(&mut out, Duration::ZERO);
        assert!(out.iter().all(|v| *v == 0.0));
        assert!(!b.is_playing());
        assert!(b.music_enabled());
        assert_eq!(b.map_status(), "No map");
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

    /// A folder with one short WAV named so the scan reads the artist and
    /// title off the file name, and a cache dir beside it.
    fn music_folder(tag: &str) -> (PathBuf, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("underscore-bridge-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let music = dir.join("music");
        std::fs::create_dir_all(&music).unwrap();
        let rate: u32 = 48_000;
        let frames = rate * 2;
        let mut bytes = Vec::with_capacity(44 + frames as usize * 2);
        let data_len = frames * 2;
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&rate.to_le_bytes());
        bytes.extend_from_slice(&(rate * 2).to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        for i in 0..frames {
            #[allow(clippy::cast_possible_truncation)]
            let v = ((f64::from(i) * 0.05).sin() * 8000.0) as i16;
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        std::fs::write(music.join("Yo-Yo Ma - Prelude.wav"), bytes).unwrap();
        (music, dir.join("cache"))
    }

    const MAP: &str = r#"
        version = 1
        [defaults]
        fade_in = "0.1"
        fade_out = "0.1"
        [[cue]]
        at = { chapter = 2, offset = "0" }
        until = { chapter = 3, offset = "0" }
        track = { artist = "Yo-Yo Ma", title = "Prelude" }
    "#;

    #[test]
    fn map_follows_the_book_when_jobs_are_served_by_hand() {
        let (music, cache) = music_folder("by-hand");
        let mut b = Bridge::new(48_000, 2);
        assert!(b.load_speech_path(&fixture()));
        let prepared = Bridge::prepare_map(MAP, &music, &cache).unwrap();
        assert!(b.install_map(prepared));
        assert!(b.map_status().starts_with("Map: 1 of 1 cues found"));
        assert!(!b.music_loaded());

        let mut out = vec![0.0; 2048];
        b.render(&mut out, Duration::ZERO);
        assert!(b.take_job().is_none(), "no cue at the start of the book");

        // Into chapter two: the cue starts.
        b.seek(Duration::from_secs(4));
        b.render(&mut out, Duration::ZERO);
        let job = b.take_job().expect("a play job");
        assert!(matches!(job.direction(), Direction::Play { cue: 0, .. }));
        let opened = job.open(48_000, 2);
        assert!(opened.is_ok(), "{:?}", opened.err());
        b.finish_job(&job, opened);
        assert!(b.music_loaded());
        assert!(b.map_status().contains("Now: cue 1, Prelude by Yo-Yo Ma"));

        // Into chapter three: the cue ends. Then back into two before the
        // worker gets to it: the silence job is stale and must be dropped.
        b.seek(Duration::from_secs(8));
        b.render(&mut out, Duration::ZERO);
        let stale = b.take_job().expect("a silence job");
        assert!(matches!(stale.direction(), Direction::Silence { .. }));
        b.seek(Duration::from_secs(4));
        b.render(&mut out, Duration::ZERO);
        let fresh = b.take_job().expect("a play job again");
        let opened = stale.open(48_000, 2);
        b.finish_job(&stale, opened);
        assert!(matches!(fresh.direction(), Direction::Play { .. }));
        assert!(b.map_status().contains("Now: cue 1"));

        b.clear_map();
        assert_eq!(b.map_status(), "No map");
        assert!(!b.has_map());
    }

    #[test]
    fn a_map_with_chapters_waits_for_a_book_that_has_them() {
        let (music, cache) = music_folder("waits");
        let mut b = Bridge::new(48_000, 2);
        let prepared = Bridge::prepare_map(MAP, &music, &cache).unwrap();
        assert!(!b.install_map(prepared));
        assert!(b.map_status().contains("not following"));
        assert!(b.load_speech_path(&fixture()));
        assert!(b.map_status().starts_with("Map: 1 of 1"));
    }

    #[test]
    fn prepare_map_reports_what_is_wrong() {
        let (music, cache) = music_folder("errors");
        assert!(Bridge::prepare_map("version = 1\n[[cue]]\nat = 0", &music, &cache).is_err());
        let empty = music.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let err = Bridge::prepare_map(MAP, &empty, &cache).unwrap_err();
        assert!(err.contains("no music found"), "{err}");
    }

    #[test]
    fn the_worker_thread_applies_directions() {
        let (music, cache) = music_folder("worker");
        // Built by hand rather than with `shared` so the worker can be
        // joined at the end.
        let bridge = Bridge::new(48_000, 2);
        let wake = Arc::clone(&bridge.wake);
        let shared = Arc::new(Mutex::new(bridge));
        let worker = follow::spawn(Arc::downgrade(&shared), wake);
        {
            let mut b = shared.lock().unwrap();
            assert!(b.load_speech_path(&fixture()));
            let prepared = Bridge::prepare_map(MAP, &music, &cache).unwrap();
            assert!(b.install_map(prepared));
            b.seek(Duration::from_secs(4));
        }
        let mut out = vec![0.0; 2048];
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let loaded = {
                let mut b = shared.lock().unwrap();
                b.render(&mut out, Duration::ZERO);
                b.music_loaded()
            };
            if loaded {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the worker never applied the cue"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        // Dropping the last reference stops the worker.
        drop(shared);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(worker.join().is_ok());
        });
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)),
            Ok(true),
            "the worker did not exit after the bridge was dropped"
        );
    }
}
