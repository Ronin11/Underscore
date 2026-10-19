//! The underscore mixing engine.
//!
//! Two streams come in: a *foreground* (the audiobook or podcast) and a
//! *background* (the listener's own music). One stream goes out. The engine
//! works on plain `f32` samples and has no I/O of its own: decoding, device
//! output and library access live in other crates. That keeps this crate
//! testable in isolation and keeps the one hard rule easy to audit:
//!
//! **The mix exists only in memory.** Nothing in this crate, and nothing
//! that calls it, writes the combined signal to disk. See `docs/legal.md`.

pub mod duck;
pub mod mix;

pub use duck::{DuckConfig, Ducker};
pub use mix::Mixer;
