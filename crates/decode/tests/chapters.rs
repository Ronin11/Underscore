//! Chapter marks from real M4B files.
//!
//! The fixtures were written by ffmpeg from a ten-second tone and this
//! metadata, so the expected values below are what ffmpeg was told:
//!
//! ```text
//! One: The Beginning   0.0 s
//! Two – Middle         3.0 s
//! Three                7.5 s
//! ```
//!
//! `both.m4b` has a Nero `chpl` atom and a `QuickTime` chapter track.
//! `track_only.m4b` was written with `-movflags disable_chpl`, so only the
//! chapter track remains. `faststart.m4b` puts `moov` before `mdat`.

use std::time::Duration;

use underscore_decode::{FileSource, probe_path};

fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

fn check(name: &str) {
    let p = probe_path(fixture(name)).unwrap();
    let titles: Vec<&str> = p
        .chapters
        .iter()
        .map(|c| c.title.as_deref().unwrap())
        .collect();
    assert_eq!(
        titles,
        ["One: The Beginning", "Two – Middle", "Three"],
        "{name}"
    );
    let starts: Vec<Duration> = p.chapters.iter().map(|c| c.start).collect();
    assert_eq!(
        starts,
        [
            Duration::ZERO,
            Duration::from_secs(3),
            Duration::from_millis(7500)
        ],
        "{name}"
    );
    assert_eq!(p.tags.title.as_deref(), Some("Test Book"), "{name}");
    assert_eq!(p.tags.artist.as_deref(), Some("Some Author"), "{name}");
    let d = p.duration.expect("duration").as_secs_f64();
    assert!((d - 10.0).abs() < 0.2, "{name}: duration {d}");
}

#[test]
fn nero_and_track() {
    check("both.m4b");
}

#[test]
fn track_only() {
    check("track_only.m4b");
}

#[test]
fn moov_first() {
    check("faststart.m4b");
}

#[test]
fn the_fixtures_still_decode() {
    let mut s = FileSource::open_path(fixture("track_only.m4b"), 48_000, 2).unwrap();
    let mut buf = vec![0.0f32; 4096];
    let mut total = 0;
    loop {
        let got = underscore_core::Source::read(&mut s, &mut buf);
        if got == 0 {
            break;
        }
        total += got;
    }
    let secs = f64::from(u32::try_from(total).unwrap()) / 48_000.0;
    assert!((secs - 10.0).abs() < 0.2, "decoded {secs} s");
}
