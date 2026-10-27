//! Builds zwhisper (the Zig engine) and links it statically.
//!
//! The engine lives in its own repository. Point `ZWHISPER_DIR` at a checkout, or keep
//! it beside this one as `../zwhisper`. `zig` must be on PATH (0.17 or later).
use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    // ZWHISPER_DIR, else the submodule under this crate, else a checkout beside the repo.
    let candidates = [
        env::var_os("ZWHISPER_DIR").map(PathBuf::from),
        Some(manifest.join("zwhisper")),
        Some(manifest.join("../../../zwhisper")),
    ];
    let dir = candidates
        .into_iter()
        .flatten()
        .find(|d| d.join("build.zig").is_file())
        .and_then(|d| d.canonicalize().ok())
        .expect("zwhisper not found: set ZWHISPER_DIR, init the submodule, or check it out beside this repo");
    println!("cargo:rerun-if-env-changed=ZWHISPER_DIR");
    println!("cargo:rerun-if-changed={}", dir.join("src").display());
    println!("cargo:rerun-if-changed={}", dir.join("build.zig").display());

    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("zwhisper");
    let target = env::var("TARGET").unwrap();
    let zig_target = match target.as_str() {
        "aarch64-linux-android" => Some("aarch64-linux-android"),
        "x86_64-linux-android" => Some("x86_64-linux-android"),
        "armv7-linux-androideabi" => Some("arm-linux-androideabi"),
        "aarch64-unknown-linux-gnu" => Some("aarch64-linux-gnu"),
        "x86_64-unknown-linux-gnu" => Some("x86_64-linux-gnu"),
        _ => None,
    };
    let host = env::var("HOST").unwrap();

    let mut cmd = Command::new("zig");
    cmd.current_dir(&dir)
        .arg("build")
        .arg("-Doptimize=ReleaseFast")
        .arg("--prefix")
        .arg(&out);
    if target != host {
        let zt = zig_target.unwrap_or_else(|| panic!("no zig target mapping for {target}"));
        cmd.arg(format!("-Dtarget={zt}"));
        if target.starts_with("aarch64") {
            // every 64-bit Android phone; NEON is baseline
            cmd.arg("-Dcpu=baseline");
        }
    }
    let status = cmd.status().expect("run zig (is it on PATH?)");
    assert!(status.success(), "zig build failed in {}", dir.display());

    println!(
        "cargo:rustc-link-search=native={}",
        out.join("lib").display()
    );
    println!("cargo:rustc-link-lib=static=zwhisper");
}
