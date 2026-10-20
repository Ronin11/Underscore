# Architecture

underscore is one Rust workspace. The engine knows nothing about files,
devices, or servers, so it can be tested as arithmetic and reused under
every platform shell.

## Layers

```
sources     crates/library: one Library trait over local folders now, and
            Audiobookshelf, OpenSubsonic, Jellyfin next; a per-stream cache;
            the conductor that follows a music map
   │
map         crates/map: which music under which stretch of the book, as data
   │
decode      symphonia (MP3, AAC, FLAC, Opus, M4B chapters) → f32 PCM, resampled
   │
engine      crates/core: ducking, mixing, later energy matching and transitions
   │
output      cpal on desktop · platform audio shells on mobile
   │
shells      CLI now · mobile and desktop UI later, over the same engine
```

### Sources (`crates/library`)

One `Library` trait: list items of a role, look one up, search by tags,
`fetch` an item as a local file, and read or report progress. Every
backend answers the same questions, so the player never knows which one
it is talking to. An item carries a role, speech or music, so a server
that holds both (Jellyfin) and servers that hold one (Audiobookshelf,
Navidrome) fit the same shape.

`LocalLibrary` is the reference backend: one folder per role, a scan that
probes every file for tags, length, and chapter marks, with the file name
and folder layout as the fallback when tags are missing. Progress lives in
a JSON file.

Server backends will fetch an item into the `Cache` first and play it from
there, exactly as a local file. That reuses the whole local path and keeps
network code out of the audio thread. Streaming over HTTP range requests
can replace the download later, behind the same `fetch`, when first-play
latency on a large book matters. The cache holds single items of the
listener's own library and has no API that takes audio from the engine.

The conductor is the glue between a music map and the player. It resolves
every cue's track against the music library once (named track first, vibe
tags as the fallback, silence when nothing matches), then turns each
position update into at most one direction: crossfade to this region of
this item at this gain, or fade to silence. Opening the item is a separate,
slow call a shell runs off the audio thread; applying the result is one
crossfade on the music layer. On Android that shell is the bridge's
`follow` module: a worker thread woken by the render thread.

### Decode

Everything is decoded to mono or stereo `f32` at one working sample rate.
Chapter and episode boundaries are passed through as events so the engine
can treat them as transition points.

### Engine (`crates/core`)

Pure sample-domain code. The hot path allocates nothing after setup.

```
Player
 ├─ Transport      play / pause / seek / speed / sleep; smart rewind on resume
 ├─ AutoLevel      speed curve, ambient-noise compensation, speech AGC
 └─ Engine
     ├─ Layer[0]   speech, wrapped in TimeStretch (WSOLA, pitch preserved)
     ├─ Layer[1]   music       ─┐  each: source, optional crossfade to the next
     ├─ Layer[2]   ambient     ─┤  source, level, fade in/out, pause-when-silent
     ├─ Layer[n]   anything    ─┘
     └─ DuckRule[] "while A is audible, lower B"; targets a layer, a kind, or everyone else
```

- `source`: the [`Source`] trait every decoder and client implements, plus
  `Silence` and `SliceSource` for tests and demos.
- `layer`: one stackable stream with level, fades, and crossfades, and the
  option to pause its source while faded out so music keeps its place.
- `duck`: a peak envelope follower driving gain with attack, hold, and
  release. The hold keeps music down through the gaps between sentences.
- `engine`: any number of layers and any number of ducking rules, so rules
  can chain (speech ducks music, music ducks ambient) or fan out (speech
  ducks everyone else). Per-layer meters for the UI.
- `fade`: linear, equal-power, smoothstep, and exponential curves; a fader
  for ramps; a crossfade for pairs.
- `stretch`: WSOLA time-stretch so 0.5x to 4x keeps the narrator's pitch.
  Speed is a lock-free `SpeedControl` a UI thread can write directly.
- `auto`: three independently switchable mechanisms. A speed curve lowers
  the background as speech gets denser. Ambient noise reported by the
  shell (in dBFS, from a microphone or anything else) boosts speech and
  cuts the background. A slow AGC holds speech at a target loudness so the
  music balance survives a change of narrator.
- `transport`: the state machine. Rewind on resume scales with how long
  the pause lasted (2 s after a short break, 30 s after a day). Countdown
  sleep timers fade out over their last seconds; end-of-chapter timers
  fire when the shell reports a boundary; either can be extended. Skip
  buttons, speed steps with clamps, and position tracking.
- `player`: the facade a shell drives. Owns all of the above, wires the
  default speech-ducks-everything rule, and offers the music button, which
  fades every non-speech layer out and pauses it in place.
- `region`: a `Source` that plays a window of another source and can
  repeat it with a crossfade at the seam, so a short passage can sit under
  a long chapter. The head of the window is read once at open.

Planned: voice activity detection in place of the peak follower for noisy
narration; music energy and tempo analysis so the background can be chosen
or reordered to suit the chapter; a quiet-gap filler; a soft limiter on the
master.

The engine never produces anything that outlives the output buffer. There
is no render-to-file path, and none will be added.

### Music maps (`crates/map`)

A map says what music plays under which stretch of a book: cues anchored
to chapter and offset, each naming a track by its tags or a mood, a region
of it, and levels and fades. The crate parses and validates maps and
resolves them against a recording's chapter times into a `Schedule` that
reports when the current cue changes. It is data only; finding tracks and
feeding the player is the shell's job. `docs/music-map.md` has the format
and the plan for the tool that writes maps.

### Output

Android uses an `AudioTrack` owned by a foreground service, fed by a render
thread that calls the engine over JNI; see `docs/android.md`. Lock-screen
and headset controls drive the speech stream, since that is what the
listener is "listening to". Desktop will use `cpal` when it arrives.

## Where things stand

Done:

- The engine, with tests: layers, chained ducking, fades and crossfades,
  WSOLA speed, auto-level, transport with smart rewind and sleep timers.
- Decoding: MP3, AAC in M4A and M4B, FLAC, Ogg Vorbis, WAV. Linear
  resampling and channel mapping. Exact seeks. A probe that reads tags,
  length, and chapter marks without decoding; M4B chapters come from the
  Nero `chpl` atom or the QuickTime chapter track, whichever the file has.
- Chapters on Android: a selector, the current chapter in the status, and
  sleep at the end of the chapter, with the boundary detected in render.
- The music map format, its parser, the schedule that resolves it against
  chapter times, and the engine's region source for partial and repeated
  tracks. All tested.
- The library layer: the `Library` trait, local folders with tag probing,
  the per-stream cache, and the conductor that follows a map. The CLI's
  `map check` resolves a map against a music folder end to end.
- Map playback on Android: the app loads a map and a music folder, the
  bridge asks the conductor on every block, and a worker thread opens
  each track off the audio thread and crossfades it in.
- An Android try-it app over a JNI bridge, published from CI as an arm64
  pre-release on every code push. Lock-screen, notification, and headset
  controls through a media session; audio focus; pause when headphones
  come out.

Not done:

- The desktop CLI does nothing yet; `cpal` output is not wired.
- No podcast feeds, no self-hosted sources, no persistence.
- The CLI does not play maps; only the Android app does. No server
  backends. No tool writes maps.
- No microphone feed, no persistence, no real UI.
- The resampler is linear interpolation. Fine for speech; music under
  speech loses a little top end at 44.1 kHz. Replace with `rubato` when
  quality starts to matter.
- No soft limiter on the master; the sum is hard-clamped.

## Milestones

The order has changed since the first draft: the Android shell came before
desktop playback because that is where listening happens.

1. **Android try-it build.** Done. See `docs/android.md`.
2. **Android, usable daily.** Chapters, maps, and lock-screen controls
   are in. Next: remembered position and files, microphone noise feed, a
   music queue. Then a UI.
3. **Library layer and map playback.** The trait, local folders, the
   cache, the conductor, M4B chapters, and the Android app following a
   map are in. Next: Audiobookshelf, OpenSubsonic, and Jellyfin behind
   the same trait.
4. **Local desktop playback.** `underscore play <book> <music>` via `cpal`.
   Mostly a developer tool for tuning the engine with real material.
5. **Map authoring.** `underscore map`: transcript in, map out, with the
   language model kept to scene segments and candidate picks. See
   `docs/music-map.md`.
6. **Podcast RSS.** Subscriptions, episode download, episodes as transition
   events.
7. **Smarter layering.** VAD, music energy analysis, chapter-aware
   transitions, gap filling, a master limiter, a better resampler.
8. **Linux desktop app, packaged and hosted.** Flatpak, AppImage and `.deb`
   on GitHub Releases, Flathub once stable.
9. **Android, released.** A release signing key, F-Droid, then Play if it
   earns it. iOS can follow the same shape later; it is not a target yet.

## Release and hosting

Both shells ship from this repository's CI. Today `android.yml` builds the
Rust library with `cargo-ndk`, assembles the APK with Gradle, and attaches it
to a pre-release tagged `apk-<run number>` on every code push to `main`,
signed with the debug key. A release key, version tags, and the desktop
packages come with milestones 7 and 8. No package is hand-built.

## Why Rust everywhere

One language for the engine, the decoders, the network clients, and the
shells means one test suite covers the behavior that matters and one
toolchain is pinned in `rust-toolchain.toml`. The Android shell is the one
place platform code is unavoidable; it is a thin Kotlin layer over a
hand-written JNI surface in `crates/android`, small enough to read in one
sitting.
