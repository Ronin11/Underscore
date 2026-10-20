# underscore: notes for agents

Read `README.md` first; the three rules there are hard constraints on every
change. In particular:

- Never add code that writes, caches, or exports the mixed signal. Per-stream
  caching of the user's own library is fine. The mix is not.
- Never add a dependency, feature, or doc link that decrypts or strips DRM.
- Never add a Spotify, Apple Music, or YouTube Music SDK or API client.

`docs/legal.md` explains why. `docs/architecture.md` has the layout, where
things stand, and what is next. `docs/android.md` covers building and
installing the app.

## Map

```
crates/core      the engine: layers, ducking, fades, time-stretch, auto-level,
                 transport, Player facade. Pure f32, no I/O, no platform code.
crates/decode    FileSource: symphonia decoding, resampling, channel mapping.
crates/android   Bridge (plain Rust over Player) and jni.rs (JNI exports).
crates/cli       `underscore` binary. Placeholder; no playback yet.
android/         Gradle project. Engine.kt (JNI wrapper), PlaybackService.kt
                 (foreground service, AudioTrack render thread), MainActivity.kt
                 (UI built in code, no layout XML).
.github/         ci.yml (fmt, clippy, test) and android.yml (APK pre-release).
```

## Hard boundaries

- `crates/core` takes `f32` samples and returns `f32` samples. Decoding,
  library clients, and device output live in other crates. Never add I/O,
  threads, or clocks to core; every "now" is a `Duration` the caller passes.
- `crates/core` must stay allocation-free in the render path after setup.
- JNI names are a contract. Every `external fun nativeX` in
  `android/.../Engine.kt` must have a matching
  `Java_io_github_ronin11_underscore_Engine_nativeX` in
  `crates/android/src/jni.rs`, same parameter order and JNI types. A mismatch
  compiles fine and crashes at first call. Change both or neither.
- Audio reaches the device through `PlaybackService`'s render thread only.
  Do not add a second path, a recorder, or a file writer.

## Checks before any commit

```
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo check --target aarch64-linux-android -p underscore-android --release
```

CI runs the first three and fails on any warning. The workspace lints are
`clippy::all` and `clippy::pedantic` at `warn`; fix the warning rather than
widening an `allow`, unless the cast or lint is genuinely intentional and
the `allow` is scoped to the one item. The fourth needs
`rustup target add aarch64-linux-android` once; it does not need the NDK.

The Kotlin side cannot be compiled without the Android SDK. If you have it,
`cd android && ./gradlew assembleDebug` is the check. If you do not, read
your Kotlin changes twice and watch the Android APK workflow after pushing:
it is the only compiler for that code in a cloud session.

## CI and releases

Every push to `main` that touches `crates/`, `android/`, `Cargo.*` or the
workflow builds an arm64 APK and publishes it as a pre-release tagged
`apk-<run number>`. The APK is signed with the debug key; this is for
trying, not shipping. Docs-only pushes do not trigger it.

## Style

Prose in docs and comments: short sentences, no em-dashes. Commit messages
say what changed and why, not which tool wrote them.
