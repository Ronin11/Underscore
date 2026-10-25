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
- `ServiceActivity.kt` is what every screen shares: the binding to the
  service, a half-second refresh while in front, the app bar, and the
  helpers that build rows and dialogs in code. No layout XML, so nothing
  to keep in sync.
- `MainActivity.kt` is the player: the book and chapter, a scrub bar
  across the current chapter with time left, previous and next chapter
  around the skip and play buttons, and a tool bar along the bottom for
  speed, sleep, chapters, and music. The app bar's menu holds the library
  (a file, the server's books, the server sign-in) and the way to the
  other two screens.
- `SettingsActivity.kt` edits `Settings.kt`, one preferences file the
  service watches and pushes into the engine: skip lengths, rewind on
  resume, keep the screen on, the music level, the music folder maps play
  from, and the Audiobookshelf sign-in.
- `DebugActivity.kt` holds what belongs on neither: a music track played
  by hand under the book, a fake room-noise level for the auto-level, the
  live engine state, and the "Report a bug" button.
- `BugReport.kt` builds the report text, and `UnderscoreApp.kt` saves
  the stack trace of a crash for it.

Settings persist; the picked book, map, and position do not yet, and
there is no library view.

## Music maps

Set the music folder in Settings, then load a map from the player's
Music menu (`docs/music-map.md`). The folder is a document-tree pick
that the settings screen turns into a path on device storage, which is
why the app asks for the audio permission: the engine scans the folder
by path, probing every audio file for tags. The scan runs on its own
thread and the app bar says when it is done; the Music menu shows how
many cues found a track.

From then on the engine asks the conductor where the book is on every
render block. When a cue starts or ends, the direction is parked and a
worker thread in `crates/android/src/follow.rs` opens the track while the
lock is free, then hands it to the music layer with one crossfade. A
seek that lands in another cue before the worker is done makes the
earlier job stale; it is dropped, not applied. A map that places cues by
chapter waits until a book with chapter marks is loaded, and the status
line says so. "Clear the map" fades the music out and forgets the map;
a track picked by hand on the Debug screen still plays underneath
whatever the map says, until the next cue replaces it.

Folders on the Downloads provider or a cloud provider have no path and
cannot be scanned; pick one on the device or an SD card.

## Music server

The Navidrome row in Settings, or any other Subsonic-compatible server,
asks for the address, username, and password and signs in. What is kept
is the user name and a salted token, never the password. Once signed in,
maps play their music from the server instead of the music folder. The
first time a map loads, the app reads the server's whole song list, which
takes a few seconds on a big library; after that every cue is resolved in
memory. Each track is downloaded once, as the server holds it, into the
same per-stream cache the books use.

A map names its tracks by artist, title, and album, so the server needs
those tags. The server's address has to be reachable from the phone: a
`.home` name works on the home network only.

## Audiobookshelf

"Audiobookshelf server" under the library menu, or the Server row in
Settings, asks for the address, username, and password of an
Audiobookshelf server and signs in; the tokens are kept in the app's
private preferences and the password is not. "Server books" opens the book
screen: every book the account can see, with its cover, author, series,
and how far the listener is. A search box matches title, author, and
series; a row of filters picks all, in progress, not started, or
finished; Sort orders by last played, recently added, title, author, or
series in reading order. Books split across many files are shown dimmed,
since only single-file books play for now. Covers come from the server,
which serves them without a login, and are kept in the app's cache.
Picking a book downloads its file into the app's
cache, the same per-stream cache the maps use, loads it, and jumps to
where the server says the listener was. From then on the app reports
the position back every half minute while playing and whenever it
pauses, so other Audiobookshelf clients carry on from the same place.
The chapter list comes from the server when the file has none.

An account that signs in through the server's single sign-on (OpenID,
such as Authentik) uses "Sign in with Authentik" in the same dialog with
only the address filled in. The browser opens the provider's own page,
then returns to the app at `underscore://oauth`, or at the stock
`audiobookshelf://oauth` that servers allow by default, and the app
trades the answer for tokens. This is the server's mobile flow with
PKCE; the password never reaches the app. To use the app's own address,
add `underscore://oauth` to the allowed mobile redirect URIs in the
server's authentication settings. If the official Audiobookshelf app is
also installed, Android may ask which app should open the stock address.

Only books that are one audio file play for now; a book split across
many files is listed, and picking it says so. Podcasts are not listed.
The whole file comes down before playback starts, which on a home
network is seconds. A plain `http://` address works; the manifest
allows cleartext for exactly this reason.

## Coming back

The app remembers the last book, where the listener was in it, and the
last map. When it starts again, after the system closed it or the phone
restarted, it signs back in to both servers, opens that book at that
place, and follows that map again. A server book opens from the cache,
at the position the server has, which may be newer if another app
played it since. Without the server it opens the cached file as it is,
at the position this app saved. A book from a file opens again through
the read grant the picker gave, which the app keeps. The map's text is
kept in the app's private files. Clearing the map forgets it.

## Transcribing on the phone

Debug, Transcribe on this phone, turns on a transcriber that follows the
book as it plays. It runs whisper.cpp on the phone with a model picked
there (`tiny.en`, `base.en` by default, or `small.en`), downloaded once
from the whisper.cpp project into the app's files. It keeps about a
quarter hour ahead of the listener, a minute of audio at a time, then
waits; a seek past what is done starts it from there. It decodes the
book with a reader of its own at 16 kHz mono, on a thread at background
priority, so playback never waits on it. What it hears is appended to
one file per book under `files/whisper/transcripts`, a JSON line per
segment and per stretch done, and read back when the book opens again,
so no stretch is done twice. The player shows the line being read; the
Debug screen shows the lead, the pace against real time, and the lines
around the listener. The words stay on the phone. They are for placing
music, like the desktop transcription that maps are made from.

whisper.cpp is C++ and links the NDK's shared C++ runtime, so the APK
carries `libc++_shared.so`. The APK workflow copies it in with cargo-ndk's
`--link-libcxx-shared`; a hand build copies it from the NDK's sysroot.
Note that whisper-rs 0.16's `set_abort_callback_safe` passes whisper.cpp a
pointer of the wrong type, which reads as "abort" and fails the encoder
with -6; `crates/transcribe` sets the raw callback instead.

## Storage

Server books and map music are cached under the app's cache folder,
`cache/library`, one file per item exactly as the server sent it. Covers
are under `cache/covers`. Nothing removes them on its own, though
Android may clear the cache folder when the phone runs short of space.
Settings, Storage, Downloads shows what they take and removes one book,
the map music, the covers, or all of it. The open book is kept, because
it is playing from its file. Anything removed downloads again when it is
next needed.

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

Every build, from CI or a laptop, is signed with `android/app/try-it.keystore`,
a key checked into the repo the way Android's own debug key is shared. That
is what lets one build install over the previous one. It is not a release
key and must not become one. A build installed before that key existed has
to be uninstalled once; after that updates install in place.

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

With wireless debugging paired, `adb connect <ip:port>` from the phone's
developer settings and then `adb install -r` puts a build on the phone
without going through the browser.

## Debugging

The quickest way is the "Report a bug" button on the Debug screen, under
the app bar's menu.
It writes one text file with the build and commit, the device and Android
version, whether notifications are allowed and which ones are posted (and
whether the system saw them as media notifications), the audio outputs,
the service's view of the session, the engine's transport state, the last
crash if there was one, and this app's own recent logcat lines. The share
sheet then opens; nothing leaves the phone until a target is picked. Share
it to wherever gets it to a computer and paste it into the chat. The
report has no audio and no file contents in it. The build and commit it
came from are also shown under the button and in Settings.

`adb logcat -s AndroidRuntime underscore` shows crashes. A JNI name
mismatch shows as `UnsatisfiedLinkError` on first use of that function;
compare `Engine.kt` and `jni.rs`. A file that will not open is reported by a
toast; the common causes are an unsupported codec (AAX, WMA, Opus in MP4)
or a URI the app has no read grant for.

## What to add next, in rough order

1. Keep the music folder's index between map loads instead of rescanning.
2. Microphone level for noise compensation (`RECORD_AUDIO`, a short RMS
   window on a low-priority thread, `Engine.setNoiseDb`).
3. A music queue: a folder or playlist, crossfaded track to track.
4. Cover art on the player, from the server or the file's tags. The
   book screen already shows the server's covers.

Chapters are in: the bridge reads M4B chapter marks when a book loads,
the activity shows the current chapter and lists them in a dialog that
jumps on tap, and "Sleep at chapter end" pauses when the boundary passes.
The bridge detects the crossing during render; a seek that lands just past
a boundary does not count as one.
