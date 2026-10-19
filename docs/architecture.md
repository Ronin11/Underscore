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

Pure sample-domain code, allocation-free in the hot path.

- `duck`: peak envelope follower on the foreground drives the background
  gain with attack, hold, and release. The hold keeps the music down through
  the gaps between sentences so it does not pump. Shipped in the initial
  commit, with tests.
- `mix`: sums the foreground with the ducked background and clamps. Shipped.
- Planned: voice activity detection in place of the peak follower for
  noisy narration; music energy and tempo analysis so the background can be
  chosen or reordered to suit the chapter; crossfades at chapter and track
  boundaries; a quiet-gap filler for long silences.

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
5. **Mobile shells.** Android and iOS over the same core, with background
   audio and media controls. This is where the product lives for most
   listeners; the desktop milestones exist to get the engine right first.

## Why Rust everywhere

One language for the engine, the decoders, the network clients, and the
shells means one test suite covers the behavior that matters and one
toolchain is pinned in `rust-toolchain.toml`. Mobile shells are the only
place platform code is unavoidable, and UniFFI keeps that to bindings.
