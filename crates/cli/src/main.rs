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
use underscore_library::{
    Audiobookshelf, Cache, Conductor, Credentials, Library, LocalLibrary, Resolution, Role,
    Subsonic,
};
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
        ["abs", rest @ ..] => abs(rest),
        ["songs", "features", dir] => song_features(dir),
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

/// `songs features <dir>`: what the phone hears in each song, one JSON line
/// per file, for `tools/song-model/train.py`.
fn song_features(dir: &str) -> Result<(), String> {
    fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
                walk(&path, out)?;
            } else if path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                underscore_library::local::EXTENSIONS.contains(&e.to_lowercase().as_str())
            }) {
                out.push(path);
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(std::path::Path::new(dir), &mut files).map_err(|e| format!("{dir}: {e}"))?;
    files.sort();
    let queue = std::sync::Mutex::new(files.into_iter());
    let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let next = queue
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .next();
                    let Some(path) = next else { break };
                    match underscore_library::hearing::hear(&path) {
                        Ok(features) => {
                            let line = format!(
                                "{{\"path\":{:?},\"features\":{features:?}}}",
                                path.display().to_string(),
                            );
                            println!("{line}");
                        }
                        Err(e) => eprintln!("{}: {e}", path.display()),
                    }
                }
            });
        }
    });
    Ok(())
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

/// `map check <map.toml> --music <dir>|--navidrome [--book <file>]`: resolve
/// every cue against a music folder, or against a Subsonic-compatible server whose
/// address and login come from `NAVIDROME_URL`, `NAVIDROME_USER` and
/// `NAVIDROME_PASS`, from the environment or from
/// `~/.config/underscore/navidrome.env`.
fn map_check(args: &[&str]) -> Result<(), String> {
    let mut map_path = None;
    let mut music = None;
    let mut book = None;
    let mut navidrome = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match *a {
            "--music" => music = Some(PathBuf::from(it.next().ok_or("--music needs a folder")?)),
            "--navidrome" => navidrome = true,
            "--book" => book = Some(PathBuf::from(it.next().ok_or("--book needs a file")?)),
            other if map_path.is_none() => map_path = Some(PathBuf::from(other)),
            other => return Err(format!("unexpected argument `{other}`")),
        }
    }
    let map_path = map_path.ok_or("map check needs a map file")?;
    if music.is_none() && !navidrome {
        return Err("map check needs --music <folder> or --navidrome".into());
    }

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

    let lib = music_source(music, navidrome)?;

    let conductor = Conductor::new(map, &timeline, &*lib).map_err(|e| e.to_string())?;
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
        let (what, hit) = describe_resolution(res, cue, &*lib);
        found += usize::from(hit);
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

/// One line about how a cue resolved, and whether it found music.
fn describe_resolution(
    res: &Resolution,
    cue: &underscore_map::Cue,
    lib: &dyn underscore_library::Library,
) -> (String, bool) {
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
    match res {
        Resolution::Track(id) => (format!("track    {}", describe(id)), true),
        Resolution::Vibe(id) => (
            format!("vibe     {}  (for {})", describe(id), cue.vibe.join(", ")),
            true,
        ),
        Resolution::Axes(id) => {
            let (e, v) = (
                cue.energy.unwrap_or(f32::NAN),
                cue.valence.unwrap_or(f32::NAN),
            );
            (
                format!(
                    "axes     {}  (for energy {e:.1} valence {v:.1})",
                    describe(id)
                ),
                true,
            )
        }
        Resolution::Silence => ("silence".to_string(), false),
        Resolution::Missing => {
            let mut wanted: Vec<String> = cue
                .track
                .iter()
                .map(|t| format!("{} by {}", t.title, t.artist))
                .collect();
            if !cue.vibe.is_empty() {
                wanted.push(format!("anything {}", cue.vibe.join(", ")));
            }
            if cue.has_axes() {
                wanted.push("anything near its energy and valence".to_string());
            }
            (format!("MISSING  wanted {}", wanted.join(", or ")), false)
        }
    }
}

fn clock(d: Duration) -> String {
    underscore_map::Time::from(d).to_string()
}

/// The library `map check` resolves against: a folder, or the Navidrome
/// server named by the environment.
fn music_source(music: Option<PathBuf>, navidrome: bool) -> Result<Box<dyn Library>, String> {
    if let Some(music) = music.filter(|_| !navidrome) {
        let lib = LocalLibrary::new("music", vec![(Role::Music, music)], None);
        let n = lib.scan().map_err(|e| e.to_string())?;
        println!("{n} tracks in the library");
        return Ok(Box::new(lib));
    }
    let env = env_file("navidrome.env");
    let var = |k: &str| std::env::var(k).ok().or_else(|| env.get(k).cloned());
    let (Some(url), Some(user), Some(pass)) = (
        var("NAVIDROME_URL"),
        var("NAVIDROME_USER"),
        var("NAVIDROME_PASS"),
    ) else {
        return Err("set NAVIDROME_URL, NAVIDROME_USER and NAVIDROME_PASS".into());
    };
    let server = Subsonic::login("navidrome", &url, &user, &pass).map_err(|e| e.to_string())?;
    let n = server.refresh_items().map_err(|e| e.to_string())?;
    println!("{n} tracks on {}", server.base());
    Ok(Box::new(server))
}

/// `abs list|fetch <id> --cache <dir>|progress <id>`: talk to an
/// Audiobookshelf server. The address and login come from `ABS_URL` with
/// `ABS_USER` and `ABS_PASS` or `ABS_TOKEN`, from the environment or from
/// `~/.config/underscore/abs.env`.
fn abs(args: &[&str]) -> Result<(), String> {
    let env = abs_env();
    let var = |k: &str| std::env::var(k).ok().or_else(|| env.get(k).cloned());
    let url = var("ABS_URL").ok_or("ABS_URL is not set")?;
    let server = match (var("ABS_TOKEN"), var("ABS_USER"), var("ABS_PASS")) {
        (Some(token), _, _) => Audiobookshelf::with_credentials(
            "abs",
            &url,
            Credentials {
                access: token,
                refresh: None,
            },
        ),
        (None, Some(user), Some(pass)) => {
            Audiobookshelf::login("abs", &url, &user, &pass).map_err(|e| e.to_string())?
        }
        _ => return Err("set ABS_TOKEN, or ABS_USER and ABS_PASS".into()),
    };
    match args {
        ["list"] => {
            println!(
                "signed in as {}",
                server.whoami().map_err(|e| e.to_string())?
            );
            let books = server.items(Role::Speech).map_err(|e| e.to_string())?;
            for b in &books {
                println!(
                    "{}  {}  by {}  {}  [{}]",
                    b.id,
                    b.title,
                    b.artist.as_deref().unwrap_or("?"),
                    b.duration.map_or_else(|| "?".into(), clock),
                    b.tags.join(", "),
                );
            }
            println!("{} books", books.len());
            Ok(())
        }
        ["fetch", id, "--cache", dir] => {
            let cache = Cache::open(PathBuf::from(dir)).map_err(|e| e.to_string())?;
            let item = server.item(&(*id).into()).map_err(|e| e.to_string())?;
            println!("{}: {} chapters", item.title, item.chapters.len());
            let started = std::time::Instant::now();
            let path = server
                .fetch(&(*id).into(), &cache)
                .map_err(|e| e.to_string())?;
            let size = std::fs::metadata(&path).map_or(0, |m| m.len());
            println!(
                "{} ({} MB) in {:.1} s",
                path.display(),
                size / 1_048_576,
                started.elapsed().as_secs_f64()
            );
            Ok(())
        }
        ["progress", id] => {
            let at = server.progress(&(*id).into()).map_err(|e| e.to_string())?;
            println!("{}", at.map_or_else(|| "not started".into(), clock));
            Ok(())
        }
        _ => Err("abs takes `list`, `fetch <id> --cache <dir>`, or `progress <id>`".into()),
    }
}

/// `KEY=VALUE` lines from `~/.config/underscore/abs.env`, if it exists.
fn abs_env() -> std::collections::HashMap<String, String> {
    env_file("abs.env")
}

/// `KEY=VALUE` lines from a file under `~/.config/underscore/`, if it exists.
fn env_file(name: &str) -> std::collections::HashMap<String, String> {
    let Some(home) = std::env::var_os("HOME") else {
        return std::collections::HashMap::new();
    };
    let path = PathBuf::from(home).join(".config/underscore").join(name);
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
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
         \x20 songs features <dir>   what the phone hears in each song, for training\n\
         \x20                                            Resolve every cue of a map against a music folder\n\
         \x20 defaults                                   Print every tunable's default value\n\
         \x20 --version                                  Print the version\n\
         \x20 --help                                     Print this help\n\
         \n\
         Playback arrives with the desktop milestone; see docs/architecture.md."
    );
}
