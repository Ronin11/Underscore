//! The Android shell's way into the engine: JNI exports for the Kotlin app,
//! each a thin call into [`underscore_bridge::Bridge`]. Everything with
//! behavior lives in `crates/bridge`, shared with any other shell.

pub mod jni;
