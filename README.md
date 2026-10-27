# underscore

Your audiobook or podcast on top. Your own music underneath.

underscore is a layered audio player. It plays a spoken foreground (a book,
a podcast) and a musical background at the same time, and it is smart about
the layering: the music ducks while someone is speaking, holds through the
pauses between sentences, and comes back up when the narration stops. Both
streams come from things the listener already owns, either as files on the
device or from a library they host themselves.

```
foreground (book, podcast)  ──┐
                              ├── mix (in memory, never on disk) ── output
background (music)  ── duck ──┘
```

## Where the audio comes from

| Mode        | Books and podcasts                 | Music                                      |
|-------------|------------------------------------|--------------------------------------------|
| Local       | M4B, MP3 files; podcast RSS feeds  | MP3, AAC, FLAC, Opus files                 |
| Self-hosted | [Audiobookshelf](https://www.audiobookshelf.org/) | Any [OpenSubsonic](https://opensubsonic.netlify.app/) server (Navidrome, Gonic, Airsonic) |

Jellyfin and Plex can follow. Streaming services cannot; see below.

## Three rules

These are not preferences. They are what keeps the product legal, and
`docs/legal.md` explains why.

1. **The mix exists only in memory.** The combined signal is never written
   to disk, cached, exported, or shared. Individual streams from the user's
   own library may be cached. The mix may not.
2. **No DRM, anywhere.** underscore reads plain files and the two APIs
   above. It never decrypts anything, never bundles or links to anything
   that does, and its documentation describes where to buy DRM-free audio,
   not how to unlock locked audio.
3. **No streaming-service SDKs.** Spotify's developer terms forbid mixing
   its content with other audio. Apple Music permits real-time mixing only
   for approved partners. YouTube Music forbids audio-only playback. Adding
   any of them is a contract negotiation, not a feature branch.

## Layout

```
crates/core     underscore-core: the engine. Pure f32 samples, no I/O, fully tested.
crates/decode   underscore-decode: files in (MP3, M4A/M4B, FLAC, Ogg, WAV), engine frames out.
crates/map      underscore-map: music maps, which music under which chapter. Data only.
crates/library  underscore-library: the Library trait, local folders, the per-stream
                cache, and the conductor that plays a map.
crates/mapserver underscore-maps: keeps maps for an Audiobookshelf server's books,
                and the queue of maps for the worker to make.
crates/android  underscore-android: the JNI bridge the Kotlin app calls.
crates/cli      underscore: developer command line. Scans folders, checks maps.
android/        the Android app: a Kotlin shell over the engine.
worker/         the map worker (Python): transcript, scenes, and music by sound, on
                the listener's own GPU.
deploy/         how to run the map server and the worker.
docs/           architecture.md (design), legal.md (why the rules are the rules),
                music-map.md (the map format), map-server.md (how maps get made),
                ios-port.md (a plan for an iPhone app)
```

Everything is Rust except the map worker, which runs speech and music
models that live in Python, on the listener's server. The engine is kept free of platform code so the same
crate can sit under a desktop binary now and mobile shells later.

## Development

```
cargo fmt --all
cargo clippy --workspace --all-targets
cargo test --workspace
```

All three must pass before a change lands. The toolchain is pinned in
`rust-toolchain.toml`.

## Try it on Android

Every push to `main` that touches the code builds an arm64 APK in GitHub
Actions and publishes it as a pre-release under
[Releases](https://github.com/Ronin11/Underscore/releases). It is signed
with a try-it key, so Android asks you to allow installs from your browser.
Pick a book or podcast file and a music file from your phone, press Play.

## Status

The engine is in: N-layer mixing with chained ducking rules, crossfades,
pitch-preserving speed, automatic levelling against speed and room noise,
a music button, smart rewind, and sleep timers. Decoding covers the common
formats. The Android app is a plain try-it shell: file pickers, transport,
speed, chapters, a music map over a folder of your own music, music
button and level, sleep timers, lock-screen and headset controls, a
foreground service so it keeps playing with the screen off. See
`docs/architecture.md` for what is next: self-hosted sources, remembered
position, a real UI.
