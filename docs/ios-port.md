# Porting underscore to iOS

This is a plan for an iPhone version of the app, written for whoever takes
it on, with or without an AI assistant. It says what can be reused, what
has to be written, in what order, and where the traps are. Read
`README.md` first: its three rules bind the iOS app exactly as they bind
Android.

## What there is today

The Android app is two layers:

- **Rust, shared.** Everything that matters lives in plain Rust crates
  with no Android code in them:
  - `crates/core`: the engine. Layers, ducking, fades, time stretch,
    auto level, transport, sleep timers.
  - `crates/decode`: MP3, AAC in M4A and M4B, FLAC, Ogg Vorbis, WAV.
    Chapters from M4B files.
  - `crates/map`: music maps.
  - `crates/library`: the Audiobookshelf client, the Subsonic client
    (Navidrome), the map server client, the per-stream cache, the
    conductor that follows a map, and the storage planner.
- **Kotlin, Android only.** A shell under `android/` that draws the
  screens, owns the audio output, the media session, notifications, and
  downloads, and calls the Rust side through 71 JNI functions in
  `crates/android/src/jni.rs`.

Between the two sits `Bridge`, in `crates/android/src/lib.rs`, with its
helpers `follow.rs` (the map worker thread) and `transcript.rs` (the
transcriber). `Bridge` is plain Rust. It does not know about Android. It
owns the `Player`, loads books and music, signs in to the servers, fits a
map, and renders audio: `Bridge::render(out, now)` fills a buffer of
interleaved f32 samples. `jni.rs` is only a thin layer that turns Java
calls into `Bridge` calls.

So an iOS app reuses all of the Rust, including `Bridge`, and replaces
the Kotlin shell and the JNI layer with Swift and a C interface.

## What the iOS app needs

| Piece | Android today | iOS |
|---|---|---|
| Screens | Views built in Kotlin code | SwiftUI |
| Calls into Rust | JNI, `jni.rs` | A C interface, `#[no_mangle] extern "C"`, with a header from `cbindgen` |
| Audio out | `AudioTrack` fed by a render thread | `AVAudioEngine` with an `AVAudioSourceNode` that calls `render` |
| Playing in the background | Foreground service | `AVAudioSession` category `.playback` and the `audio` background mode |
| Lock screen and headset controls | `MediaSession` | `MPRemoteCommandCenter` and `MPNowPlayingInfoCenter` |
| Other apps' audio | Audio focus | `AVAudioSession` interruption notifications |
| Headphones pulled out | `ACTION_AUDIO_BECOMING_NOISY` | Route change with reason `oldDeviceUnavailable`: pause |
| Picking a file | Storage Access Framework, file descriptors | `UIDocumentPickerViewController`, security-scoped URLs, paths |
| Single sign-on | Browser plus a return activity | `ASWebAuthenticationSession` with a custom URL scheme |
| Settings | `SharedPreferences` | `UserDefaults` |
| Login tokens | Private preferences | The Keychain |
| Kept books | App cache folder | Application Support, excluded from iCloud backup. Not Caches, which iOS empties when space runs low. |
| Shake to undo | Accelerometer | Core Motion |
| Watch sleep source | Wear OS app | Out of scope at first. See below. |

## Rules that still apply

- **The mix lives in memory only.** No `AVAudioFile` writer, no
  `AVAudioRecorder`, no tap on the main mixer that saves samples. The
  `AVAudioSourceNode` hands samples to the output and nowhere else.
- **No DRM.** Do not use MusicKit, Apple Music, or the media library's
  protected items. Music comes from the listener's files or their own
  Navidrome server, as on Android.
- **No streaming-service SDKs.** Apple Music counts. Its terms allow
  mixing only for approved partners.

## Step 0: tools

- A Mac with a current Xcode.
- Rust from `rustup`, with the iOS targets:
  `rustup target add aarch64-apple-ios aarch64-apple-ios-sim`.
- `cargo install cbindgen`.
- An Apple account. A free one can install on the developer's own phone,
  but the app expires after 7 days and has to be reinstalled from Xcode.
  The paid program ($99 a year) lasts a year per install and allows
  TestFlight for family.

## Step 1: share the bridge

`Bridge` lives in `crates/android`, next to `jni.rs`. Move it out first,
so iOS does not depend on a crate named android:

1. Make `crates/bridge` with `lib.rs` (the `Bridge`), `follow.rs`, and
   `transcript.rs` moved from `crates/android`.
2. Leave `jni.rs` in `crates/android`, depending on `crates/bridge`.
3. Two Android-only things in `Bridge` need a home:
   `load_speech_fd` and `load_music_fd` open a file descriptor. Keep
   them, behind `#[cfg(unix)]`; iOS will use the path versions,
   `load_speech_path` and `load_music_path`.
4. The transcriber pulls in a speech engine that is the heaviest thing
   to cross-compile. Put `transcript.rs` behind a cargo feature,
   `transcribe`, on by default for Android, off for iOS at first.
   Captions still work on iOS from a transcript the map server made,
   since importing one does not need the engine.

Run the checks in `CLAUDE.md` after the move. Android must build and
behave exactly as before. This step changes no behavior.

## Step 2: the C interface

Make `crates/ios`, a `staticlib`, that exposes `Bridge` the way `jni.rs`
does, one function per JNI function. Same rules as `jni.rs`:

- A handle is an `Arc<Mutex<Bridge>>` leaked as a pointer, made by
  `us_create(sample_rate, channels)` and freed by `us_destroy(handle)`.
- Strings in are `*const c_char`, UTF-8. Strings out are allocated by
  Rust and freed by Rust: `us_string_free(ptr)`.
- Anything that needs the network runs outside the bridge lock. Copy
  the pattern `with(handle, None, |b| b.server())` from `jni.rs`: take the
  client out, release the lock, then call it.
- JSON in and out for anything structured, as `jni.rs` does, so the
  Swift side parses with `JSONDecoder`.

Generate `underscore.h` with `cbindgen`. Build for both targets and wrap
them in an XCFramework:

```
cargo build --release -p underscore-ios --target aarch64-apple-ios
cargo build --release -p underscore-ios --target aarch64-apple-ios-sim
xcodebuild -create-xcframework \
  -library target/aarch64-apple-ios/release/libunderscore_ios.a -headers include \
  -library target/aarch64-apple-ios-sim/release/libunderscore_ios.a -headers include \
  -output ios/Underscore.xcframework
```

A build script under `ios/` should do this, so nobody types it twice.

Do not try to port all 71 functions at once. Start with the dozen that
step 3 needs, and add the rest as each feature arrives. `Engine.kt` is
the list of what exists, with a one-line description of each.

## Step 3: sound from a file

The smallest app that proves the whole stack: pick a book file and a
music file, press play, hear the music duck under the narrator.

- An `ios/` Xcode project, SwiftUI, linking the XCFramework.
- An `Engine` class in Swift wrapping the C functions, like `Engine.kt`.
- Audio: an `AVAudioEngine` with an `AVAudioSourceNode` whose render
  block calls `us_render(handle, buffer, frames, now_ms)` and copies the
  interleaved samples into the node's non-interleaved buffers.
- `now` is a monotonic clock in milliseconds, as on Android
  (`SystemClock.elapsedRealtime`). Use `mach_continuous_time` or
  `ProcessInfo.systemUptime`. The engine never reads a clock itself.

The render block runs on a real-time thread. It must not allocate, take
a lock that something slow may hold, or touch Swift objects that retain
and release. `Bridge::render` takes the bridge lock, as on Android; the
rule that keeps this safe is the one above, that nothing slow runs
under that lock. Keep it that way.

## Step 4: a real player

- `AVAudioSession`: category `.playback`, mode `.spokenAudio`. Turn on the
  `audio` background mode in the target's capabilities.
- Interruptions: pause on a call, resume if the session says so.
- Route changes: pause when headphones come out.
- Lock screen: `MPRemoteCommandCenter` for play, pause, skip back and
  forward (the listener's skip lengths), and chapter next and previous.
  `MPNowPlayingInfoCenter` with title, author, cover, position, length,
  and speed.
- The Player tab: cover, title, chapter line, scrub bar across the
  current chapter, skip and play buttons, speed, sleep, and music in the
  header. `docs/android.md` and `MainActivity.kt` describe what each
  does. Copy the behavior, not the look.

## Step 5: the servers

- Audiobookshelf sign-in with a password, and single sign-on through
  `ASWebAuthenticationSession`. The Rust side already does the OpenID
  exchange; Swift only opens the page and passes back the URL it returns
  to. Register a custom URL scheme and add it to the redirect addresses
  Audiobookshelf allows.
- Keep tokens in the Keychain, never in `UserDefaults`.
- The Library tab, the book screen (`BookActivity.kt`), and streaming a
  book while it downloads (`prepare_stream` and `load_server_stream`).
- Navidrome sign-in and maps playing from it. The map worker thread in
  `follow.rs` runs as it does on Android; Swift only needs to call
  `us_map_*` from a background queue.
- The map server: its address, the book's default map, asking for a new
  one, reading a map. All of it is already in Rust behind
  `MapServer` in `crates/library`.

## Step 6: keeping books offline

`Storage.kt` keeps a list of what each book uses, and
`crates/library/src/keep.rs` decides what goes when space runs short.
The Swift side keeps the same list and calls the same planner, through
`us_storage_plan`. Put kept books under Application Support, marked
`isExcludedFromBackup`, so iOS neither purges them nor copies gigabytes
of audio into iCloud. Download on Wi-Fi only by default, with
`NWPathMonitor`.

## Step 7: sleep

- Timers and "end of chapter" are in the engine already.
- Shake to undo: Core Motion, same thresholds as `Shake.kt`.
- Automatic sleep detection is the hard part. Android uses the watch
  (Health Services) and the phone's own motion. An Apple Watch can tell
  HealthKit about sleep, but only after the fact, not the moment it
  starts, so it cannot stop the book in time. Leave this out of the
  first version, or try the phone's motion and the "other apps" source
  through Shortcuts.

## Order of work, and how to know each step is done

1. The bridge is shared. Android builds and plays as before; CI is green.
2. The XCFramework builds for device and simulator from one script.
3. A file plays with music ducking under it, on a real iPhone.
4. It keeps playing with the screen locked, and the lock screen
   controls work.
5. A book from Audiobookshelf streams and plays with its map from
   Navidrome.
6. A kept book plays in airplane mode.
7. Sleep timers and shake to undo work.

Stop at any step and the app is still useful for that much.

## Notes for an AI assistant doing the work

- Read `CLAUDE.md`, `README.md`, `docs/architecture.md`, and
  `docs/android.md` before writing code. The Android shell is the
  reference for what the app does; the docs say why.
- Never change the Rust behavior to suit iOS. If iOS needs something
  new from Rust, add it to `Bridge` so Android can use it too, with
  tests.
- `crates/core` takes f32 samples and returns f32 samples. No I/O, no
  threads, no clocks, nothing allocated while rendering. That holds on
  iOS.
- Test on a real phone, not only the simulator. Background audio, the
  lock screen, and Bluetooth behave differently on a device.
- Keep the style: short sentences in docs and comments, no em-dashes,
  commit messages that say what changed and why.

## Rough size

A developer who knows Swift and is new to Rust: a few weekends for steps
1 to 4, the same again for 5 and 6. With an AI assistant writing most of
the code, the slow parts are still the human ones: Xcode signing,
provisioning profiles, and testing on the phone.
