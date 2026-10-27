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
  service, a half-second refresh while in front, the app bar, the four
  tabs along the bottom, and the helpers that build rows and dialogs in
  code. No layout XML, so nothing to keep in sync. Each tab is its own
  activity; switching swaps them without animation, and the player stays
  underneath, so Back from any tab lands on it.
- `MainActivity.kt` is the Player tab: speed, sleep, and music in the
  app bar, then the book and chapter (tap the chapter line for the
  list), a scrub bar across the current chapter with time left, and
  previous and next chapter around the skip and play buttons. It has no
  menu. Opening a file and keeping a book offline live on the Library
  tab, and storage on Settings.
- `BooksActivity.kt` is the Library tab: the server's books, and a file
  from the phone under its menu.
- `ConnectionsActivity.kt` is the Connections tab: the Audiobookshelf
  sign-in, the music server and the music folder maps play from, and the
  sleep sources. An empty section offers to add one, and the + in the
  app bar walks through adding any kind. One server of each kind for
  now; removing a server forgets its sign-in.
- `SettingsActivity.kt` is the Settings tab. It edits `Settings.kt`, one
  preferences file the service watches and pushes into the engine: skip
  lengths, rewind on resume, keep the screen on, shake to undo sleep, the
  music level, and storage. Its last row opens Debug.
- `DebugActivity.kt` holds what belongs on none of them: a music track
  played by hand under the book, a fake room-noise level for the
  auto-level, the transcriber, the sleep log, the live engine state, and
  the "Report a bug" button.
- `BugReport.kt` builds the report text, and `UnderscoreApp.kt` saves
  the stack trace of a crash for it.

Settings persist, and so do the open book, each book's map, and the
position; see "Coming back" below.

## Music maps

Set the music folder under Connections, then load a map from the player's
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

The Navidrome row under Connections, for Navidrome or any other Subsonic-compatible server,
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

The Audiobookshelf row under Connections asks for the address, username, and password of an
Audiobookshelf server and signs in; the tokens are kept in the app's
private preferences and the password is not. The Library tab lists
every book the account can see, with its cover, author, series,
and how far the listener is. A search box matches title, author, and
series; a row of filters picks all, in progress, not started, or
finished; Sort orders by last played, recently added, title, author, or
series in reading order. Covers come from the server,
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

A book split across many files plays as one: the bridge joins the parts
with `Chain` in `crates/core`, using the server's length for each part,
and seeks and positions run across the whole book. The first part
downloads at once and each later part when the one before is done, or
sooner, a piece at a time, when playback or a seek reaches it. The server's
chapter list is the book's chapters. The phone transcriber reads one file,
so it skips these books for now. Podcasts are not listed.

The player shows the book's cover, author, and series. The series line
opens the book screen on that series, in reading order. A book from a file
shows the art and author inside it. The cover and author also go to the
lock screen and the media controls.
A book plays while it downloads. The file comes down into its cache
file a range at a time, and the player reads what is in; a seek, or an
M4B that keeps its index at the end, sends the downloader to that spot
first, and it fills in the rest after. The player's app bar shows how far
the download is. The file only takes its place in the cache once every
byte is in, so the storage screen, coming back, and the transcriber,
which waits for the whole file, see exactly what a full download leaves.
A seek far past what is in can pause while that part arrives. The
decoder is opened and moved to the listener's place outside the bridge
lock, so the first wait never holds up the screen. A plain `http://` address works; the manifest
allows cleartext for exactly this reason.

## Maps and transcripts beside the book

A server book's folder can hold two more files, and the app looks for
both each time it opens the book:

- `underscore.toml`, the book's music map. It loads in place of a
  different map loaded for that book, unless the same map is already
  loaded. It needs a music server or folder to play from, as any map does.
- `underscore.transcript.json`, a finished transcript in the desktop
  transcriber's format: `{"chapters": [{"segments": [{"start", "end",
  "text"}]}]}`, times in seconds from the start of the book. It becomes
  the book's transcript, marked whole, so the phone never transcribes that
  book, and the player shows its captions even with phone transcription
  off.

Audiobookshelf lists every file beside a book, and
`GET /api/items/{id}/file/{ino}` reads one with plain access to the book.
It has no way to add a file to a book's folder, so these are put there
from a computer with the library mounted, and the app only reads them.
After adding them, scan the library in Audiobookshelf: its folder watcher
does not always see new files, especially on a network mount, and until
a scan the book does not list them. The bug report's "beside the book on
the server" line says what the app found.

## Coming back

The app remembers the last book, where the listener was in it, and each
book's map. When it starts again, after the system closed it or the phone
restarted, it signs back in to both servers, opens that book at that
place, and follows that map again. A server book opens from the cache,
at the position the server has, which may be newer if another app
played it since. Without the server it opens the cached file as it is,
at the position this app saved. A book from a file opens again through
the read grant the picker gave, which the app keeps. Each book's map is
kept in the app's private files, under `files/maps`, so opening a book
again brings its map back, offline too. Clearing the map forgets it for
that book.

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

## Falling asleep

The Sleep button offers Auto, Manual, and Off. Manual holds the timers.
Auto lists the ways of noticing sleep that have been set up, each with a
check box, and Add… at the bottom. Add lists every kind of source; picking
one opens its setup, which says what it needs, asks for any permission,
and tests it live. While a setup is open the source's reports go to that
screen, not the player, so a test never pauses the book. Choosing a timer
turns Auto off, and turning Auto on clears a timer.

Each kind is a `SleepSource` in `Sleep.kt`; `SleepSetup.kt` is the Auto
list and the setup screens. When a checked source is sure, Auto is on, and
the book is playing, the service marks where in the book sleep
began, plays a minute more, fades and pauses. The Sleep button then offers
"Back to where I fell asleep". The mark is two minutes before the time the
source gives, or ten minutes back when it gives none, since nothing can
tell the moment someone drifts off. A report while the player is not
running is only logged: nothing can be playing, and starting the player
for it would open the last book for nothing.

Shaking the phone undoes a stop that came while the listener was awake.
During the wind-down it calls the wind-down off; within five minutes of
the stop it plays the book again. During a manual timer it starts the
timer over, as in Audible. The phone buzzes once when a shake lands. The
accelerometer only listens while a shake could do something, and Settings
can turn it off.

The sleep log keeps what each source said and what came of it, across
restarts, so a night can be read the next morning. The watch also sends
every change of state, awake as well as asleep. A shake, a call-off from
the Sleep menu, or playing again soon after marks a stop as too soon,
with the source it came from. That is the data for tuning the sources:
without it there is no telling a stop that came too soon from one that
came right. The Debug screen shows the latest lines; the bug report has
all of it, with about a day of the phone's readings.

- **This phone.** Google's sleep API sends a reading every ten minutes,
  from how still the phone is and how dark the room is. Lying still in the
  dark while listening looks a lot like sleep, so it takes two readings in
  a row at 85 out of 100 or more. The bug report lists about a day of readings,
  which is how to tune that. It needs the physical activity permission and
  Google Play services.
- **Other apps.** Any app can send the broadcast
  `io.github.ronin11.underscore.ASLEEP` to `io.github.ronin11.underscore`.
  Two extras, both optional: `since`, a long, the wall-clock time in
  milliseconds the listener fell asleep, and `source`, a string for the
  bug report. Tasker, Home Assistant's companion app, or a smart bed's app
  can send it. It does nothing unless it is set up and checked under Auto.
  Its setup has a button that sends a test from the phone itself.
- **Watch.** A Wear OS watch, such as a Pixel Watch, running the small
  app in `android/wear`. Health Services on the watch reports when the
  wearer is asleep, from heart rate and movement; the app sends that to
  the phone over the Wearable Data Layer, which `WatchListener` hears.
  The Data Layer only connects apps with the same id and signing key, so
  the watch app shares both with the phone app. Its setup on the phone
  finds the watch, asks it how it stands (on or off, permission, whether
  this watch offers the asleep state at all), and shows the watch's test
  button's report. Outside that screen, with Auto on and a book playing,
  the test button winds the book down for real, to try the whole chain
  before bed.

  ```
  adb shell am broadcast -a io.github.ronin11.underscore.ASLEEP \
    -n io.github.ronin11.underscore/.SleepReceiver --es source test
  ```

A new kind of sensor is a new `SleepSource` added to `Sleep.sources`, with
its setup steps in `SleepSetup`, or simply another app sending the
broadcast.

### Installing the watch app

Each release has `underscore-watch-*.apk` beside the phone APK. The watch
has no browser, so it goes on with adb over Wi-Fi:

1. On the watch: Settings, System, About, tap the build number seven
   times. Then Settings, Developer options: turn on ADB debugging and
   Wireless debugging, and tap "Pair new device".
2. On the computer: `adb pair <ip:port> <code>` with what the watch shows,
   then `adb connect <ip:port>` with the address on the Wireless debugging
   screen itself.
3. `adb -s <ip:port> install -r underscore-watch-N.apk`.

Open "underscore sleep" on the watch, tap Watch for sleep, and allow the
permission. Then on the phone: Sleep, Auto…, Add…, Watch.

## Storage

Server books and map music are cached under the app's cache folder,
`cache/library`, one file per item exactly as the server sent it. Covers
are under `cache/covers`. Android may clear the cache folder when the
phone runs short of space.

The book you open comes down whole in the background while it plays, so
seeking is quick, the map and the transcriber can read ahead, and a gap
in the signal does not stop it. That much is always kept. The rest is up
to Settings, Storage:

- **Keep up to.** A slider from "Only the book I'm on" through 1 to 50
  GB to "No limit", 5 GB to start, with about how many books of the
  usual size on the phone fit. When the cache is over it, files no book
  uses go at once. If that is not enough, the app asks which books to
  remove, with the ones played longest ago that would make room already
  checked, on whatever screen is open, and with a notification when the
  app is closed. "Not now" asks again the next time it fills; "Raise
  the limit" opens Settings.
- **Remove old books without asking.** Off by default. On, the books
  played longest ago go without the question.
- **Keep offline.** A long press on a book in the book list, or "Keep
  this book offline" in the player's menu, downloads the whole
  book, its map and transcript from beside it on the server, its cover,
  and every track its map plays. A book kept offline is never removed
  to make room. When the server is out of reach the book list shows
  the books on the phone, and they open from their files at the place
  this app last saw.
- **Download on Wi-Fi only.** On by default. It holds back downloads for
  books kept offline and their music until the phone is on Wi-Fi or
  another unmetered network. The book you are playing still streams.
- **On this phone.** Each book with what it takes on its own and what it
  shares with other books. Tap one to keep it offline or remove it.

Music is often shared between books, so the app keeps a list of the
files each book uses, in `files/storage.json`: the book's own audio, and
the cached tracks its map plays. A book goes with only the files no
other book still lists. The open book and books kept offline stay, and
so do their files. Which books and files go is decided by `plan` in
`crates/library/src/keep.rs`, which is pure and tested; the app keeps
the list and does the deleting. A book's map and transcript stay when
the book goes: they are small, and a transcript made on the phone takes
hours to make again.

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

The quickest way is the "Report a bug" button on the Debug screen, the
last row of the Settings tab.
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
