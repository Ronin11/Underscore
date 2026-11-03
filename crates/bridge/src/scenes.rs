//! A map from what the phone heard.
//!
//! The transcriber leaves a transcript per book. This rates each minute of
//! it with the scene model, three axes per passage: whether music belongs
//! there at all, how much energy the passage has, and its colour from dark
//! to light. Then it writes a map whose cues ask for music by those
//! coordinates, so the conductor picks the nearest song the listener owns.
//! No catalog is browsed and no song is named: the model reads the book,
//! a plain matcher picks the music, the two stages `docs/music-map.md`
//! keeps apart.
//!
//! Ratings are kept beside the transcript, so a map made again after the
//! transcriber has gone further only rates the new passages.

use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use underscore_map::Map;

use crate::transcript::{self, Covered, Segment};

/// What the scene model said about one passage.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rated {
    pub start: Duration,
    pub end: Duration,
    /// Probability that no music belongs here.
    pub silence: f32,
    /// Expected energy, 1 to 5.
    pub energy: f32,
    /// Expected valence, -1 to 1.
    pub valence: f32,
}

/// How passages become cues.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Options {
    /// A cue shorter than this joins its nearest neighbour.
    pub min_cue: Duration,
    /// A passage this far from the running cue's energy starts a new cue.
    pub energy_step: f32,
    /// Likewise for valence.
    pub valence_step: f32,
    /// A passage whose silence probability passes this plays nothing.
    pub silence_at: f32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            min_cue: Duration::from_secs(120),
            energy_step: 1.0,
            valence_step: 0.6,
            silence_at: 0.5,
        }
    }
}

/// Where book `key`'s ratings live under `dir`: beside its transcript, by
/// the same name.
#[must_use]
pub fn ratings_path(dir: &Path, key: &str) -> PathBuf {
    let store = transcript::store_path(dir, key);
    let name = store
        .file_name()
        .map_or_else(|| "ratings.jsonl".into(), std::ffi::OsStr::to_os_string);
    dir.join("scenes").join(name)
}

/// Read ratings back, in book order.
#[must_use]
#[allow(clippy::cast_possible_truncation)] // f64 from JSON into the f32 it was
pub fn load_ratings(path: &Path) -> Vec<Rated> {
    let mut out = Vec::new();
    let Ok(file) = File::open(path) else {
        return out;
    };
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let ms = |k: &str| {
            v.get(k)
                .and_then(serde_json::Value::as_u64)
                .map(Duration::from_millis)
        };
        let num = |k: &str| v.get(k).and_then(serde_json::Value::as_f64);
        if let (Some(start), Some(end), Some(q), Some(g), Some(v)) =
            (ms("s"), ms("e"), num("q"), num("g"), num("v"))
        {
            out.push(Rated {
                start,
                end,
                silence: q as f32,
                energy: g as f32,
                valence: v as f32,
            });
        }
    }
    out.sort_by_key(|r| r.start);
    out
}

/// Write ratings, replacing the file.
#[cfg_attr(not(feature = "transcribe"), allow(dead_code))] // only `build` and tests write
fn save_ratings(path: &Path, rated: &[Rated]) {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let Ok(mut f) = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
    else {
        return;
    };
    let mut out = String::new();
    for r in rated {
        out.push_str(
            &serde_json::json!({
                "s": ms(r.start), "e": ms(r.end),
                "q": round(r.silence, 3), "g": round(r.energy, 3), "v": round(r.valence, 3),
            })
            .to_string(),
        );
        out.push('\n');
    }
    let _ = f.write_all(out.as_bytes());
}

/// What `build` made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Built {
    /// The map, as TOML.
    pub map: String,
    pub cues: usize,
    pub passages: usize,
    /// How much of the book the transcript covers.
    pub covered: Duration,
    /// Whether the transcript reaches the end of the book.
    pub complete: bool,
}

impl Built {
    /// For a shell: `map`, `cues`, `passages`, `coveredMs`, `complete`.
    #[must_use]
    pub fn json(&self) -> String {
        serde_json::json!({
            "map": self.map,
            "cues": self.cues,
            "passages": self.passages,
            "coveredMs": ms(self.covered),
            "complete": self.complete,
        })
        .to_string()
    }
}

/// Rate what the transcriber has heard of book `key` and write a map for
/// it. `dir` is the transcriber's directory: the transcript is read from
/// there, the scene model is kept there (fetched once, about 60 MB) and
/// the ratings go beside the transcript. Slow: a thread of its own.
///
/// # Errors
/// Nothing has been heard yet, the model could not be fetched or loaded,
/// or the map it wrote does not parse.
#[cfg(feature = "transcribe")]
pub fn build(
    dir: &Path,
    key: &str,
    length: Duration,
    threads: usize,
    stop: &std::sync::atomic::AtomicBool,
) -> Result<Built, String> {
    use underscore_transcribe::{SceneRater, download_scene_model, passages, scene_model_ready};

    let (segments, covered) = transcript::load(&transcript::store_path(dir, key));
    if segments.is_empty() {
        return Err("nothing heard yet".into());
    }
    let wanted = passages(&segments, Duration::from_secs(60));
    let path = ratings_path(dir, key);
    let known = load_ratings(&path);
    let mut rated = Vec::with_capacity(wanted.len());
    let mut model = None;
    for p in &wanted {
        if let Some(r) = known.iter().find(|r| r.start == p.start && r.end == p.end) {
            rated.push(*r);
            continue;
        }
        if model.is_none() {
            if !scene_model_ready(dir) {
                download_scene_model(dir, stop, |_, _| {})
                    .map_err(|e| format!("scene model: {e}"))?;
            }
            let dir = underscore_transcribe::scene_model_path(dir);
            model = Some(SceneRater::open(&dir, threads).map_err(|e| format!("scene model: {e}"))?);
        }
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("stopped".into());
        }
        let axes = model
            .as_ref()
            .ok_or("no scene model")?
            .rate(&p.text)
            .map_err(|e| format!("scene model: {e}"))?;
        rated.push(Rated {
            start: p.start,
            end: p.end,
            silence: axes.silence,
            energy: axes.energy_mean,
            valence: axes.valence_mean,
        });
    }
    save_ratings(&path, &rated);
    let complete = covered.next_gap(Duration::ZERO) + Duration::from_secs(60) >= length;
    let map = compose(&rated, &segments, length, complete, &Options::default())?;
    Ok(Built {
        cues: Map::parse(&map).map_or(0, |m| m.cues.len()),
        map,
        passages: rated.len(),
        covered: covered.total(),
        complete,
    })
}

/// Without the speech engine there is no scene model either.
///
/// # Errors
/// Always: this build cannot rate scenes.
#[cfg(not(feature = "transcribe"))]
pub fn build(
    _dir: &Path,
    _key: &str,
    _length: Duration,
    _threads: usize,
    _stop: &std::sync::atomic::AtomicBool,
) -> Result<Built, String> {
    Err("this build cannot rate scenes".into())
}

/// One cue in the making.
#[derive(Debug, Clone, Copy)]
struct Cue {
    start: Duration,
    end: Duration,
    silent: bool,
    energy: f32,
    valence: f32,
    /// Passages summed into `energy` and `valence`.
    n: u32,
}

impl Cue {
    #[allow(clippy::cast_precision_loss)] // a count of passages
    fn mean(&self) -> (f32, f32) {
        let n = self.n.max(1) as f32;
        (self.energy / n, self.valence / n)
    }

    fn len(&self) -> Duration {
        self.end.saturating_sub(self.start)
    }

    /// How unlike two cues are, for choosing which neighbour a short one joins.
    fn distance(&self, other: &Self) -> f32 {
        if self.silent != other.silent {
            return 100.0;
        }
        if self.silent {
            return 0.0;
        }
        let (e1, v1) = self.mean();
        let (e2, v2) = other.mean();
        ((e1 - e2) / 2.0).powi(2) + (v1 - v2).powi(2)
    }

    fn absorb(&mut self, other: &Self) {
        self.start = self.start.min(other.start);
        self.end = self.end.max(other.end);
        if !self.silent && !other.silent {
            self.energy += other.energy;
            self.valence += other.valence;
            self.n += other.n;
        }
    }
}

/// Passages into cues: a passage joins the cue before it while it is
/// contiguous, agrees on silence, and stays within a step of the cue's
/// running energy and valence; then cues shorter than `min_cue` join the
/// neighbour they are most like, and neighbours left alike by that join too.
fn cues(rated: &[Rated], o: &Options) -> Vec<Cue> {
    let mut out: Vec<Cue> = Vec::new();
    for r in rated {
        let silent = r.silence > o.silence_at;
        let fresh = Cue {
            start: r.start,
            end: r.end,
            silent,
            energy: r.energy,
            valence: r.valence,
            n: 1,
        };
        if let Some(last) = out.last_mut()
            && last.end == r.start
            && last.silent == silent
        {
            let (e, v) = last.mean();
            if silent
                || ((r.energy - e).abs() < o.energy_step && (r.valence - v).abs() < o.valence_step)
            {
                last.absorb(&fresh);
                continue;
            }
        }
        out.push(fresh);
    }
    // The shortest cue under the minimum that has a neighbour to join.
    let shortest = |out: &[Cue]| {
        (0..out.len())
            .filter(|&i| out[i].len() < o.min_cue)
            .filter(|&i| {
                (i > 0 && out[i - 1].end == out[i].start)
                    || (i + 1 < out.len() && out[i].end == out[i + 1].start)
            })
            .min_by_key(|&i| out[i].len())
    };
    while let Some(i) = shortest(&out) {
        let prev = (i > 0 && out[i - 1].end == out[i].start).then(|| out[i].distance(&out[i - 1]));
        let next = (i + 1 < out.len() && out[i].end == out[i + 1].start)
            .then(|| out[i].distance(&out[i + 1]));
        let into = match (prev, next) {
            (Some(p), Some(n)) if n < p => i + 1,
            (Some(_), _) => i - 1,
            _ => i + 1,
        };
        let short = out[i];
        out[into].absorb(&short);
        out.remove(i);
        coalesce(&mut out, o);
    }
    out
}

/// Join contiguous neighbours that agree on silence and now sit within a
/// step of each other, as they can once a passage between them is gone.
fn coalesce(out: &mut Vec<Cue>, o: &Options) {
    let mut i = 0;
    while i + 1 < out.len() {
        let (a, b) = (out[i], out[i + 1]);
        let (e1, v1) = a.mean();
        let (e2, v2) = b.mean();
        let alike = a.silent == b.silent
            && (a.silent || ((e1 - e2).abs() < o.energy_step && (v1 - v2).abs() < o.valence_step));
        if a.end == b.start && alike {
            out[i].absorb(&b);
            out.remove(i + 1);
        } else {
            i += 1;
        }
    }
}

/// Write the map. `segments` supply each cue's anchor words.
///
/// # Errors
/// No passages, or the map written does not parse (a bug, not bad input).
pub fn compose(
    rated: &[Rated],
    segments: &[Segment],
    length: Duration,
    complete: bool,
    o: &Options,
) -> Result<String, String> {
    if rated.is_empty() {
        return Err("nothing heard yet".into());
    }
    let mut out = String::from("version = 1\nname = \"Heard on this phone\"\n");
    if !complete {
        out.push_str("complete = false\n");
    }
    out.push_str(
        "\n[defaults]\nfade_in = \"6\"\nfade_out = \"6\"\ncrossfade = \"5\"\nlevel_db = -6\n",
    );
    for c in cues(rated, o) {
        let end = c.end.min(length.max(c.end));
        let _ = writeln!(
            out,
            "\n[[cue]]\nat = \"{}\"\nuntil = \"{}\"",
            clock(c.start),
            clock(end)
        );
        if let Some(words) = anchor(segments, c.start) {
            let _ = writeln!(out, "anchor = {}", quote(&words));
        }
        if c.silent {
            out.push_str("silence = true\nwhy = \"no music belongs here, the phone judged\"\n");
            continue;
        }
        let (e, v) = c.mean();
        let energy = round(e.clamp(1.0, 5.0), 1);
        let valence = round(v.clamp(-1.0, 1.0), 2);
        let _ = writeln!(out, "energy = {energy}\nvalence = {valence}");
        let _ = writeln!(
            out,
            "level_db = {}\nwhy = \"energy {energy}, valence {valence}, heard on this phone\"",
            level_db(e)
        );
    }
    Map::parse(&out).map_err(|e| format!("the map written does not parse: {e}"))?;
    Ok(out)
}

/// The worker's levels: quiet scenes sit further under the voice.
#[allow(clippy::cast_possible_truncation)] // energy is 1 to 5
fn level_db(energy: f32) -> i32 {
    match energy.round() as i32 {
        i32::MIN..=1 => -6,
        2 => -4,
        3 => -2,
        _ => 0,
    }
}

/// The first few words heard at or after `at`.
fn anchor(segments: &[Segment], at: Duration) -> Option<String> {
    let i = segments.partition_point(|g| g.start < at);
    let words: Vec<&str> = segments
        .get(i..)?
        .iter()
        .flat_map(|g| g.text.split_whitespace())
        .take(8)
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

/// A time as the map writes it, floored to a tenth.
fn clock(t: Duration) -> String {
    let tenths = t.as_millis() / 100;
    let (hours, rest) = (tenths / 36_000, tenths % 36_000);
    let (mins, rest) = (rest / 600, rest % 600);
    let (secs, tenth) = (rest / 10, rest % 10);
    if hours > 0 {
        format!("{hours}:{mins:02}:{secs:02}.{tenth}")
    } else {
        format!("{mins}:{secs:02}.{tenth}")
    }
}

fn quote(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

fn round(x: f32, places: i32) -> f32 {
    let k = 10f32.powi(places);
    (x * k).round() / k
}

/// How much of the book the ratings cover, for a shell's progress line.
#[must_use]
pub fn covered(rated: &[Rated]) -> Covered {
    let mut c = Covered::default();
    for r in rated {
        c.add(r.start, r.end);
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(secs: u64) -> Duration {
        Duration::from_secs(secs)
    }

    fn r(start: u64, end: u64, silence: f32, energy: f32, valence: f32) -> Rated {
        Rated {
            start: s(start),
            end: s(end),
            silence,
            energy,
            valence,
        }
    }

    fn segs() -> Vec<Segment> {
        (0..20)
            .map(|i| Segment {
                start: s(i * 30),
                end: s(i * 30 + 25),
                text: format!("words of minute {i} and a few more"),
            })
            .collect()
    }

    #[test]
    fn alike_passages_make_one_cue_and_a_jump_makes_another() {
        let rated = [
            r(0, 60, 0.1, 2.0, 0.0),
            r(60, 120, 0.1, 2.3, 0.1),
            r(120, 180, 0.1, 2.1, -0.1),
            r(180, 240, 0.1, 4.2, -0.5),
            r(240, 300, 0.1, 4.0, -0.4),
        ];
        let c = cues(&rated, &Options::default());
        assert_eq!(c.len(), 2, "{c:?}");
        assert_eq!((c[0].start, c[0].end), (s(0), s(180)));
        assert_eq!((c[1].start, c[1].end), (s(180), s(300)));
        let (e, _) = c[1].mean();
        assert!((e - 4.1).abs() < 0.01);
    }

    #[test]
    fn a_short_cue_joins_the_neighbour_it_is_most_like() {
        let rated = [
            r(0, 60, 0.1, 2.0, 0.0),
            r(60, 120, 0.1, 2.0, 0.0),
            r(120, 180, 0.1, 2.0, 0.0),
            r(180, 240, 0.1, 4.5, 0.0), // one loud minute
            r(240, 300, 0.1, 2.2, 0.0),
            r(300, 360, 0.1, 2.1, 0.0),
        ];
        let c = cues(&rated, &Options::default());
        assert_eq!(c.len(), 1, "{c:?}");
        assert_eq!((c[0].start, c[0].end), (s(0), s(360)));
    }

    #[test]
    fn silence_stays_its_own_cue_and_coverage_gaps_are_kept() {
        let rated = [
            r(0, 60, 0.9, 1.0, 0.0),
            r(60, 120, 0.9, 1.0, 0.0),
            r(120, 180, 0.1, 3.0, 0.5),
            r(180, 240, 0.1, 3.0, 0.5),
            // a gap: 240..600 not heard yet
            r(600, 660, 0.1, 3.0, 0.5),
            r(660, 720, 0.1, 3.0, 0.5),
        ];
        let c = cues(&rated, &Options::default());
        assert_eq!(c.len(), 3, "{c:?}");
        assert!(c[0].silent && !c[1].silent && !c[2].silent);
        assert_eq!((c[2].start, c[2].end), (s(600), s(720)));
    }

    #[test]
    fn the_map_written_parses_and_asks_by_coordinates() {
        let rated = [
            r(0, 60, 0.9, 1.0, 0.0),
            r(60, 120, 0.9, 1.0, 0.0),
            r(120, 180, 0.1, 3.4, -0.26),
            r(180, 240, 0.1, 3.4, -0.26),
        ];
        let toml = compose(&rated, &segs(), s(3600), false, &Options::default()).unwrap();
        let map = Map::parse(&toml).unwrap();
        assert!(!map.complete);
        assert_eq!(map.cues.len(), 2);
        assert!(map.cues[0].silence);
        assert_eq!(map.cues[1].energy, Some(3.4));
        assert_eq!(map.cues[1].valence, Some(-0.26));
        assert_eq!(map.cues[1].level_db, Some(-2.0));
        assert_eq!(
            map.cues[1].anchor.as_deref(),
            Some("words of minute 4 and a few more")
        );
        assert!(toml.contains("at = \"2:00.0\""), "{toml}");
    }

    #[test]
    fn times_are_written_as_people_write_them() {
        assert_eq!(clock(Duration::from_millis(4_100)), "0:04.1");
        assert_eq!(clock(s(250)), "4:10.0");
        assert_eq!(clock(Duration::from_millis(3_723_570)), "1:02:03.5");
    }

    #[test]
    fn ratings_read_back_what_was_written() {
        let dir = std::env::temp_dir().join(format!("underscore-ratings-{}", std::process::id()));
        let path = ratings_path(&dir, "abs:book");
        let rated = [r(0, 60, 0.9, 1.0, 0.0), r(60, 125, 0.05, 3.25, -0.5)];
        save_ratings(&path, &rated);
        let back = load_ratings(&path);
        assert_eq!(back.len(), 2);
        assert_eq!((back[1].start, back[1].end), (s(60), s(125)));
        assert!((back[1].energy - 3.25).abs() < 1e-3);
        assert_eq!(covered(&back).total(), s(125));
        let _ = fs::remove_dir_all(&dir);
    }

    /// Set `UNDERSCORE_SCENE_MODEL` to an indecis model directory and
    /// `UNDERSCORE_TRANSCRIPT` to a desktop transcript to see a real map.
    #[test]
    #[ignore = "needs the scene model and a transcript"]
    #[cfg(feature = "transcribe")]
    fn builds_a_map_from_a_real_transcript() {
        let model = std::env::var("UNDERSCORE_SCENE_MODEL").unwrap();
        let json = fs::read(std::env::var("UNDERSCORE_TRANSCRIPT").unwrap()).unwrap();
        let dir = std::env::temp_dir().join(format!("underscore-hear-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(model, underscore_transcribe::scene_model_path(&dir)).unwrap();
        transcript::import(&dir, "test", &json).unwrap();
        let stop = std::sync::atomic::AtomicBool::new(false);
        let built = build(&dir, "test", Duration::from_mins(15), 4, &stop).unwrap();
        println!(
            "{} passages, {} cues\n{}",
            built.passages, built.cues, built.map
        );
        assert!(built.cues >= 1);
        // The second time, every passage is already rated.
        let again = build(&dir, "test", Duration::from_mins(15), 4, &stop).unwrap();
        assert_eq!(again.map, built.map);
        let _ = fs::remove_dir_all(&dir);
    }
}
