# underscore: notes for agents

Read `README.md` first; the three rules there are hard constraints on every
change. In particular:

- Never add code that writes, caches, or exports the mixed signal. Per-stream
  caching of the user's own library is fine. The mix is not.
- Never add a dependency, feature, or doc link that decrypts or strips DRM.
- Never add a Spotify, Apple Music, or YouTube Music SDK or API client.

`docs/legal.md` explains why. The map worker decodes the listener's own
files only to analyse them; it must never write audio, and songs it
downloads to hear are deleted once embedded. `docs/architecture.md` has the layout, where
things stand, and what is next. `docs/android.md` covers building and
installing the app. `docs/music-map.md` is the map format: what plays under
which stretch of a book, as data, never as audio.

## Map

```
crates/core      the engine: layers, ducking, fades, time-stretch, auto-level,
                 transport, Player facade. Pure f32, no I/O, no platform code.
crates/decode    FileSource: symphonia decoding, resampling, channel mapping.
                 probe(): tags, length, chapters without decoding; mp4.rs reads
                 the M4B chapter atoms symphonia does not.
crates/map       Music maps: the TOML format, validation, and the schedule that
                 resolves cues against chapter times. Data only, no I/O.
crates/library   The Library trait (list, search, fetch, progress), LocalLibrary
                 over folders, Audiobookshelf over HTTP (abs.rs), a
                 Subsonic-compatible music server such as Navidrome
                 (subsonic.rs), the per-stream Cache, the Conductor
                 that turns a map plus the book position into player
                 directions, and keep.rs, which decides which downloads
                 go when space runs short without taking music another
                 book shares.
crates/transcribe  Speech to text on the device with whisper.cpp (whisper-rs):
                 model download, 16 kHz mono in, timed segments out.
crates/bridge    Bridge (plain Rust over Player, no platform code), follow.rs
                 (the map worker thread), transcript.rs (the transcriber that
                 keeps a lead on playback and writes a transcript per book;
                 its speech engine is the `transcribe` feature, on by
                 default). What every app shell calls.
crates/android   jni.rs only: JNI exports, each a thin call into the bridge.
crates/cli       `underscore` binary. `scan` and `map check`; no playback yet.
crates/mapserver `underscore-maps`: maps for an Audiobookshelf server's books,
                 each book's default, the queue of maps to make, and the
                 worker's transcripts. Signs phones in through
                 Audiobookshelf. Data only; it never sees audio.
worker/          The map worker, Python, on a GPU machine: whisper, a local
                 model through Ollama for scenes, CLAP to match moods to the
                 music library. `docs/map-server.md`. Its test writes
                 `worker/tests/sample-map.toml`, which a Rust test parses.
deploy/          Dockerfile and compose for the map server; conda setup and
                 a systemd user unit for the worker.
android/         Gradle project. Engine.kt (JNI wrapper), PlaybackService.kt
                 (foreground service, AudioTrack render thread, media session,
                 audio focus), Settings.kt (the preferences the service applies
                 to the engine), ServiceActivity.kt (what the screens share,
                 and the tabs), four tabs: MainActivity.kt (the player),
                 BooksActivity.kt (the library; holding a book opens BookActivity.kt), ConnectionsActivity.kt
                 (servers, music folder, sleep sources), SettingsActivity.kt;
                 DebugActivity.kt, BugReport.kt, Sleep.kt (the sources that
                 notice the listener fell asleep), SleepSetup.kt (their
                 setup and test screens), Shake.kt, Subtitles.kt and SubtitlesActivity.kt
                 (the line being read, speakers in color), Storage.kt (each
                 book's files and map, for keeping books offline). UI
                 built in code, no layout XML; icons are vector
                 drawables under res/drawable.
android/wear/    The Wear OS app: Health Services says the wearer is asleep,
                 and it tells the phone app over the Wearable Data Layer.
                 Same application id and signing key as the phone app.
.github/         ci.yml (fmt, clippy, test) and android.yml (APK pre-release,
                 phone and watch).
```

## Hard boundaries

- `crates/core` takes `f32` samples and returns `f32` samples. Decoding,
  library clients, and device output live in other crates. Never add I/O,
  threads, or clocks to core; every "now" is a `Duration` the caller passes.
- `crates/core` must stay allocation-free in the render path after setup.
- JNI names are a contract. Every `external fun nativeX` in
  `android/.../Engine.kt` must have a matching
  `Java_io_github_ronin11_underscore_Engine_nativeX` in
  `crates/android/src/jni.rs`, same parameter order and JNI types. A mismatch
  compiles fine and crashes at first call. Change both or neither.
- Audio reaches the device through `PlaybackService`'s render thread only.
  Do not add a second path, a recorder, or a file writer.
- `Cache` in `crates/library` stores single items fetched from a library,
  byte for byte. Never give it, or anything else, a way to take samples
  from the engine.
- `Conductor::update` is cheap and may run anywhere. `conductor::open`
  does I/O and must not run on the audio thread or under the bridge lock.
  On Android that means the worker in `crates/bridge/src/follow.rs`;
  `Bridge::prepare_map` scans a folder and is called before the lock is
  taken, from a Kotlin background thread. So is
  `Bridge::prepare_map_from_server`, which loads the music server's whole
  song list. After that `Subsonic::search` answers from memory, which is
  why fitting a map under the lock does no I/O.

## Checks before any commit

```
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo check --target aarch64-linux-android -p underscore-android --release
```

CI runs the first three, and `python -m unittest discover worker/tests`,
and fails on any warning. The workspace lints are
`clippy::all` and `clippy::pedantic` at `warn`; fix the warning rather than
widening an `allow`, unless the cast or lint is genuinely intentional and
the `allow` is scoped to the one item. The fourth needs
`rustup target add aarch64-linux-android` once, and since the library crate
gained TLS (`ring` compiles C) it needs the NDK's clang for the target:

```
NDK=/path/to/ndk/toolchains/llvm/prebuilt/linux-x86_64/bin
CC_aarch64_linux_android=$NDK/aarch64-linux-android26-clang \
AR_aarch64_linux_android=$NDK/llvm-ar \
cargo check --target aarch64-linux-android -p underscore-android --release
```

Without an NDK, skip the fourth check and watch the APK workflow.

The Kotlin side cannot be compiled without the Android SDK. If you have it,
`cd android && ./gradlew assembleDebug` is the check. If you do not, read
your Kotlin changes twice and watch the Android APK workflow after pushing:
it is the only compiler for that code in a cloud session.

## CI and releases

Every push to `main` that touches `crates/`, `android/`, `Cargo.*` or the
workflow builds an arm64 APK and publishes it as a pre-release tagged
`apk-<run number>`. The APK is signed with `android/app/try-it.keystore`,
a public key checked into the repo so builds install over each other; this
is for trying, not shipping. Docs-only pushes do not trigger it.

## Style

Prose in docs and comments: short sentences, no em-dashes. Commit messages
say what changed and why, not which tool wrote them.
