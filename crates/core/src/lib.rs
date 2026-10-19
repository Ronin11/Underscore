//! The underscore engine.
//!
//! Any number of *layers* come in. One stream goes out. A layer is a spoken
//! foreground (a book, a podcast), a musical background, an ambient bed, or
//! anything else the shell wants to stack. Layers duck one another by rule,
//! fade and crossfade on request, and are balanced automatically against
//! playback speed and the noise in the room. A transport on top gives the
//! player what listeners expect: smart rewind on resume, sleep timers, speed
//! that keeps pitch.
//!
//! The engine works on interleaved `f32` samples and has no I/O of its own.
//! Decoding, device output, and library access live in other crates. That
//! keeps this crate testable in isolation and keeps the one hard rule easy
//! to audit:
//!
//! **The mix exists only in memory.** Nothing in this crate, and nothing
//! that calls it, writes the combined signal to disk. See `docs/legal.md`.
//!
//! # Threading
//!
//! [`Player`] is single-owner and meant to be driven from the audio thread.
//! Control from a UI thread goes through a command channel the shell owns,
//! with one exception: [`SpeedControl`] is a lock-free handle a UI thread
//! can write directly.
//!
//! # Time
//!
//! Wall-clock time is never read here. Every call that needs "now" takes a
//! monotonic [`std::time::Duration`] from the caller, so the transport can
//! be tested without sleeping.

pub mod auto;
pub mod duck;
pub mod engine;
pub mod fade;
pub mod layer;
pub mod player;
pub mod source;
pub mod stretch;
pub mod transport;

pub use auto::{AgcConfig, AutoLevel, AutoLevelConfig, NoiseConfig};
pub use duck::{DuckConfig, Ducker};
pub use engine::{DuckRule, DuckTarget, Engine, EngineConfig, LayerId, LayerMeter, RenderReport};
pub use fade::{Crossfade, Curve, Fader};
pub use layer::{Layer, LayerConfig, LayerKind};
pub use player::{Player, PlayerConfig};
pub use source::{Silence, SliceSource, Source};
pub use stretch::{SpeedControl, StretchConfig, TimeStretch};
pub use transport::{Action, RewindPolicy, SleepTimer, Transport, TransportConfig, TransportState};
