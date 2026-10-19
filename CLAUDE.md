# underscore: notes for agents

Read `README.md` first; the three rules there are hard constraints on every
change. In particular:

- Never add code that writes, caches, or exports the mixed signal. Per-stream
  caching of the user's own library is fine. The mix is not.
- Never add a dependency, feature, or doc link that decrypts or strips DRM.
- Never add a Spotify, Apple Music, or YouTube Music SDK or API client.

`crates/core` must stay free of I/O, platform code, and audio-device
dependencies. It takes `f32` samples and returns `f32` samples. Put decoding,
library clients, and device output in other crates.

Before any commit: `cargo fmt --all`, `cargo clippy --workspace --all-targets`
with zero warnings, `cargo test --workspace`. The workspace lints are
`clippy::all` and `clippy::pedantic` at `warn`; fix the warning rather than
widening an `allow`, unless the cast or lint is genuinely intentional and
the `allow` is scoped to the one item.

Prose in docs and comments: short sentences, no em-dashes.
