# The Android app

A Kotlin shell over the Rust engine. The files that matter:

- `Engine.kt` wraps the JNI exports from `crates/android/src/jni.rs`. One
  instance owns one native player. Times are milliseconds on
  `SystemClock.elapsedRealtime()`; the engine only needs them monotonic.
- `PlaybackService.kt` is a foreground service. It owns the `Engine` and an
  `AudioTrack` (48 kHz, stereo, float). A render thread asks the engine for
  1024-frame blocks and writes them straight to the track. Files are opened
  read-only through the content resolver and the descriptor is detached and
  handed to Rust, which owns it from then on. It also owns the media
  session, audio focus, and the notification; transport goes through it.
- `MainActivity.kt` binds to the service and builds its controls in code:
  file pickers, scrub bar, transport, speed, chapters, map, music button
  and level, sleep timers. No layout XML, so nothing to keep in sync.
- `BugReport.kt` builds the text behind the "Report a bug" button, and
  `UnderscoreApp.kt` saves the stack trace of a crash for it.

The app has no state of its own: picked files and folders are not
remembered across launches, and there is no library view yet.

## Music maps

Pick a music folder, then pick a map (`docs/music-map.md`). The folder
is a document-tree pick that the activity turns into a path on device
storage, which is why the app asks for the audio permission: the engine
scans the folder by path, probing every audio file for tags. The scan
runs on its own thread and the status line says when it is done and how
many cues found a track.

From then on the engine asks the conductor where the book is on every
render block. When a cue starts or ends, the direction is parked and a
worker thread in `crates/android/src/follow.rs` opens the track while the
lock is free, then hands it to the music layer with one crossfade. A
seek that lands in another cue before the worker is done makes the
earlier job stale; it is dropped, not applied. A map that places cues by
chapter waits until a book with chapter marks is loaded, and the status
line says so. "Clear map" fades the music out and forgets the map; a
music file picked by hand still plays underneath whatever the map says,
until the next cue replaces it.

Folders on the Downloads provider or a cloud provider have no path and
cannot be scanned; pick one on the device or an SD card.

## Lock screen, headset, and other apps

The service holds a `MediaSessionCompat` and a media-style notification
with back, play/pause, and forward. The session carries the book's name
and the current chapter, with position and speed, so the lock screen
and a watch show the book and a scrub bar. Headset buttons arrive
through the session callback. Nothing is shown for the music: the
listener is listening to the book.

Play requests audio focus and does nothing if it is refused. Another
app taking focus pauses the book; a transient loss (a call, a
navigation prompt) pauses and resumes afterwards. Spoken word is never
ducked under another app's audio, only paused. Unplugging headphones
pauses. The service polls the engine once a second for changes it made
on its own, such as a sleep timer running out, and republishes the
state only when something the lock screen shows has moved.

## Install the pre-built APK

Each push to `main` publishes one under
[Releases](https://github.com/Ronin11/Underscore/releases). Download it on
the phone, allow installs from the browser when asked, open it. arm64 only.

## Build it yourself

Needs the Android SDK and NDK, Java 17, and the Rust Android target.

```
rustup target add aarch64-linux-android
cargo install cargo-ndk
export ANDROID_NDK_HOME=/path/to/ndk          # the NDK inside your SDK
cargo ndk --target arm64-v8a --platform 26 \
  --output-dir android/app/src/main/jniLibs \
  build --release -p underscore-android
cd android && ./gradlew assembleDebug
adb install -r app/build/outputs/apk/debug/app-debug.apk
```

`jniLibs/` is ignored by git; the `.so` is always a build output. Build the
Rust side in release even for a debug APK: WSOLA in a debug build is too
slow to keep up on a phone.

## Debugging

The quickest way is the "Report a bug" button at the bottom of the app.
It writes one text file with the build and commit, the device and Android
version, whether notifications are allowed and which ones are posted (and
whether the system saw them as media notifications), the audio outputs,
the service's view of the session, the engine's transport state, the last
crash if there was one, and this app's own recent logcat lines. The share
sheet then opens; nothing leaves the phone until a target is picked. Share
it to wherever gets it to a computer and paste it into the chat. The
report has no audio and no file contents in it. The build and commit it
came from are also shown under the button.

`adb logcat -s AndroidRuntime underscore` shows crashes. A JNI name
mismatch shows as `UnsatisfiedLinkError` on first use of that function;
compare `Engine.kt` and `jni.rs`. A file that will not open is reported by a
toast; the common causes are an unsupported codec (AAX, WMA, Opus in MP4)
or a URI the app has no read grant for.

## What to add next, in rough order

1. Remember the last book, music folder, map, and position across
   launches, with a persisted URI grant for the map.
2. Keep the music folder's index between map loads instead of rescanning.
3. Microphone level for noise compensation (`RECORD_AUDIO`, a short RMS
   window on a low-priority thread, `Engine.setNoiseDb`).
4. A music queue: a folder or playlist, crossfaded track to track.
5. A real UI.

Chapters are in: the bridge reads M4B chapter marks when a book loads,
the activity shows the current chapter and lists them in a dialog that
jumps on tap, and "Sleep at chapter end" pauses when the boundary passes.
The bridge detects the crossing during render; a seek that lands just past
a boundary does not count as one.
