//! Developer command line for underscore.
//!
//! Playback on the desktop arrives with `cpal`. Until then this binary
//! exercises everything short of the output device: it scans folders the
//! way the app will, and it checks a music map against a music folder and
//! reports how each cue resolved.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use underscore_core::PlayerConfig;
use underscore_library::{Conductor, Library, LocalLibrary, Resolution, Role};
use underscore_map::{Map, Timeline};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match words.as_slice() {
        [] | ["--help" | "-h"] => {
            print_help();
            Ok(())
        }
        ["--version" | "-V"] => {
            println!("underscore {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        ["defaults"] => {
            println!("{:#?}", PlayerConfig::default());
            Ok(())
        }
        ["scan", rest @ ..] => scan(rest),
        ["map", "check", rest @ ..] => map_check(rest),
        [other, ..] => Err(format!("unknown command `{other}`")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("underscore: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `scan <dir> [--speech]`: list a folder as the library sees it.
fn scan(args: &[&str]) -> Result<(), String> {
    let mut dir = None;
    let mut role = Role::Music;
    for a in args {
        match *a {
            "--speech" | "--books" => role = Role::Speech,
            "--music" => role = Role::Music,
            other if dir.is_none() => dir = Some(PathBuf::from(other)),
            other => return Err(format!("unexpected argument `{other}`")),
        }
    }
    let dir = dir.ok_or("scan needs a folder")?;
    let lib = LocalLibrary::new("scan", vec![(role, dir)], None);
    let n = lib.scan().map_err(|e| e.to_string())?;
    for item in lib.items(role).map_err(|e| e.to_string())? {
        println!(
            "{:>9}  {}{}{}{}",
            item.duration.map_or_else(|| "?".to_string(), clock),
            item.title,
            item.artist
                .as_deref()
                .map(|a| format!("  by {a}"))
                .unwrap_or_default(),
            item.album
                .as_deref()
                .map(|a| format!("  on {a}"))
                .unwrap_or_default(),
            if item.tags.is_empty() {
                String::new()
            } else {
                format!("  [{}]", item.tags.join(", "))
            },
        );
        if !item.chapters.is_empty() {
            println!("           {} chapters", item.chapters.len());
        }
    }
    println!("{n} items");
    Ok(())
}

/// `map check <map.toml> --music <dir> [--book <file>]`: resolve every cue.
fn map_check(args: &[&str]) -> Result<(), String> {
    let mut map_path = None;
    let mut music = None;
    let mut book = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match *a {
            "--music" => music = Some(PathBuf::from(it.next().ok_or("--music needs a folder")?)),
            "--book" => book = Some(PathBuf::from(it.next().ok_or("--book needs a file")?)),
            other if map_path.is_none() => map_path = Some(PathBuf::from(other)),
            other => return Err(format!("unexpected argument `{other}`")),
        }
    }
    let map_path = map_path.ok_or("map check needs a map file")?;
    let music = music.ok_or("map check needs --music <folder>")?;

    let text =
        std::fs::read_to_string(&map_path).map_err(|e| format!("{}: {e}", map_path.display()))?;
    let map = Map::parse(&text).map_err(|e| e.to_string())?;

    let timeline = match book {
        None => Timeline::flat(),
        Some(path) => {
            let probe = underscore_decode::probe_path(&path)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            let starts: Vec<Duration> = probe.chapters.iter().map(|c| c.start).collect();
            if starts.is_empty() {
                println!(
                    "{}: no chapter marks; positions are offsets into the file",
                    path.display()
                );
                Timeline::flat()
            } else {
                println!("{}: {} chapters", path.display(), starts.len());
                Timeline::chapters(starts)
            }
        }
    };

    let lib = LocalLibrary::new("music", vec![(Role::Music, music)], None);
    let n = lib.scan().map_err(|e| e.to_string())?;
    println!("{n} tracks in the library");

    let conductor = Conductor::new(map, &timeline, &lib).map_err(|e| e.to_string())?;
    let mut found = 0;
    for (i, (cue, res)) in conductor
        .map()
        .cues
        .iter()
        .zip(conductor.resolutions())
        .enumerate()
    {
        let at = match cue.at.chapter {
            Some(ch) => format!("ch {ch} {}", cue.at.offset),
            None => cue.at.offset.to_string(),
        };
        let describe = |id: &underscore_library::ItemId| {
            lib.item(id).map_or_else(
                |_| id.to_string(),
                |it| {
                    format!(
                        "{}{}",
                        it.title,
                        it.artist.map(|a| format!(" by {a}")).unwrap_or_default()
                    )
                },
            )
        };
        let what = match res {
            Resolution::Track(id) => {
                found += 1;
                format!("track    {}", describe(id))
            }
            Resolution::Vibe(id) => {
                found += 1;
                format!("vibe     {}  (for {})", describe(id), cue.vibe.join(", "))
            }
            Resolution::Silence => "silence".to_string(),
            Resolution::Missing => {
                let mut wanted: Vec<String> = cue
                    .track
                    .iter()
                    .map(|t| format!("{} by {}", t.title, t.artist))
                    .collect();
                if !cue.vibe.is_empty() {
                    wanted.push(format!("anything {}", cue.vibe.join(", ")));
                }
                format!("MISSING  wanted {}", wanted.join(", or "))
            }
        };
        println!("cue {:>3}  {at:>14}  {what}", i + 1);
    }
    let asked = conductor
        .resolutions()
        .iter()
        .filter(|r| !matches!(r, Resolution::Silence))
        .count();
    println!("{found} of {asked} cues resolved");
    Ok(())
}

fn clock(d: Duration) -> String {
    underscore_map::Time::from(d).to_string()
}

fn print_help() {
    println!(
        "underscore: your audiobook or podcast on top, your own music underneath.\n\
         \n\
         Usage: underscore <command>\n\
         \n\
         Commands:\n\
         \x20 scan <dir> [--speech]                      List a folder as the library sees it\n\
         \x20 map check <map.toml> --music <dir> [--book <file>]\n\
         \x20                                            Resolve every cue of a map against a music folder\n\
         \x20 defaults                                   Print every tunable's default value\n\
         \x20 --version                                  Print the version\n\
         \x20 --help                                     Print this help\n\
         \n\
         Playback arrives with the desktop milestone; see docs/architecture.md."
    );
}
