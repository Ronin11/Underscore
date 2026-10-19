# Architecture

underscore is one Rust workspace. The engine knows nothing about files,
devices, or servers, so it can be tested as arithmetic and reused under
every platform shell.

## Layers

```
sources     local files · podcast RSS · Audiobookshelf API · OpenSubsonic API
   │
decode      symphonia (MP3, AAC, FLAC, Opus, M4B chapters) → f32 PCM, resampled
   │
engine      crates/core: ducking, mixing, later energy matching and transitions
   │
output      cpal on desktop · platform audio shells on mobile
   │
shells      CLI now · mobile and desktop UI later, over the same engine
```

### Sources

Each source yields a stream of decoded samples plus metadata: chapters for
books, episodes for podcasts, tempo and loudness for music once analysis
exists. Local files are the reference implementation. Audiobookshelf and
OpenSubsonic are HTTP clients that stream the user's own media from the
user's own server; they are the only network code in the product.

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

Planned: voice activity detection in place of the peak follower for noisy
narration; music energy and tempo analysis so the background can be chosen
or reordered to suit the chapter; a quiet-gap filler; a soft limiter on the
master.

The engine never produces anything that outlives the output buffer. There
is no render-to-file path, and none will be added.

### Output

Desktop uses `cpal`. Mobile uses the platform audio session through a thin
shell (UniFFI bindings over the core crate), configured for background
playback with the lock screen and headset controls wired to the foreground
stream, since that is what the listener is "listening to".

## Milestones

1. **Local desktop playback.** `underscore play <book> <music>` decodes two
   local files with symphonia, runs them through the engine, and plays on the
   default device via cpal. Pause, seek, chapter skip, background level.
2. **Podcast RSS and M4B chapters.** Subscriptions, episode download, chapter
   navigation as transition events.
3. **Self-hosted sources.** Audiobookshelf and OpenSubsonic clients with
   progress sync back to the server.
4. **Smarter layering.** VAD, music analysis, chapter-aware transitions,
   gap filling.
5. **Linux desktop app, packaged and hosted.** A desktop shell over the
   engine, built in CI and published as downloadable packages: a Flatpak
   for the broadest reach, plus an AppImage and a `.deb` for people who
   avoid Flatpak. Releases are hosted as GitHub Releases on this
   repository, with a Flathub submission once the app is stable.
6. **Android app.** The Rust core compiled for Android with `cargo-ndk`,
   a thin Kotlin shell for the audio session, background playback,
   notification and lock-screen controls, and the microphone level feed for
   noise compensation. Distributed as a signed APK on GitHub Releases
   first, then F-Droid, then Play if it earns it. iOS can follow the same
   pattern later; it is not a target yet.

Desktop milestones exist to get the engine right first. The Android app is
where the product lives for most listeners.

## Release and hosting

Both shells ship from this repository's CI. A tagged commit builds every
package, runs the full test suite against the exact artifacts, and attaches
them to a GitHub Release. No package is hand-built. Package manifests and
signing live in `deploy/` when they arrive.

## Why Rust everywhere

One language for the engine, the decoders, the network clients, and the
shells means one test suite covers the behavior that matters and one
toolchain is pinned in `rust-toolchain.toml`. Mobile shells are the only
place platform code is unavoidable, and UniFFI keeps that to bindings.
