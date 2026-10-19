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
crates/core   underscore-core: the engine. Pure f32 samples, no I/O, fully tested.
crates/cli    underscore: developer command line; first playback milestone lands here.
docs/         architecture.md (design), legal.md (why the rules are the rules)
```

Everything is Rust. The engine is kept free of platform code so the same
crate can sit under a desktop binary now and mobile shells later.

## Development

```
cargo fmt --all
cargo clippy --workspace --all-targets
cargo test --workspace
```

All three must pass before a change lands. The toolchain is pinned in
`rust-toolchain.toml`.

## Status

Initial commit: workspace, ducking engine with tests, placeholder CLI. See
`docs/architecture.md` for the milestones.
