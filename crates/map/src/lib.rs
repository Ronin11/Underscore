//! Music maps.
//!
//! A map is a plan for one book: for each stretch of it, what should play
//! underneath and how. It is data, not audio. It names tracks by artist and
//! title, or by mood when no track is named, and it anchors cues to
//! chapters and offsets so the plan survives a different edition. A map
//! holds nothing from the book beyond a few words per anchor and nothing
//! from the music beyond its tags, so maps can be shared the way playlists
//! are. See `docs/music-map.md` for the format and the reasoning.
//!
//! This crate parses and validates maps and resolves them against a
//! recording's chapter times ([`schedule`]). Finding the named tracks in a
//! library and feeding the player are the shell's job.

pub mod schedule;
pub mod time;

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use underscore_core::RegionConfig;

pub use schedule::{Change, Schedule, Span, Timeline};
pub use time::Time;

/// The map format version this crate reads and writes.
pub const VERSION: u32 = 1;

/// A whole map file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Map {
    /// Format version. Always [`VERSION`].
    pub version: u32,
    /// This map's own name for itself, chosen once and kept through every
    /// edit, so a book can point at a map wherever the file ends up. A map
    /// server gives one to a map that has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// What to call it in a list of maps for the same book.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The `id` of the map this one was copied from and changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub based_on: Option<String>,
    /// False while a map is still being written, chapter by chapter. The
    /// chapters not reached yet have no cues, so they play without music.
    #[serde(default = "yes", skip_serializing_if = "is_yes")]
    pub complete: bool,
    /// Which book this is for.
    #[serde(default, skip_serializing_if = "Book::is_empty")]
    pub book: Book,
    /// Values a cue inherits when it does not say otherwise.
    #[serde(default)]
    pub defaults: Defaults,
    /// The cues, in book order.
    #[serde(default, rename = "cue")]
    pub cues: Vec<Cue>,
}

/// Which book a map is for. Every field is a hint; a shell matches on
/// whatever its library can supply.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Book {
    /// Title as the library shows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Author.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Narrator. Different narrators mean different timings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub narrator: Option<String>,
    /// ISBN of the edition the map was written against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isbn: Option<String>,
    /// Audible ASIN, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asin: Option<String>,
    /// Running time of the recording the map was written against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<Time>,
}

fn yes() -> bool {
    true
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's skip_serializing_if passes a reference
fn is_yes(v: &bool) -> bool {
    *v
}

impl Book {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Values a cue inherits when it does not set its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Defaults {
    /// Fade in when a cue starts.
    pub fade_in: Time,
    /// Fade out when a cue ends into silence.
    pub fade_out: Time,
    /// Crossfade when one cue's music hands over to the next, and at the
    /// seam of a repeating region.
    pub crossfade: Time,
    /// Offset from the listener's music level, in decibels.
    pub level_db: f32,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            fade_in: Time::secs(8.0),
            fade_out: Time::secs(6.0),
            crossfade: Time::secs(4.0),
            level_db: 0.0,
        }
    }
}

/// One stretch of the book and what plays under it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cue {
    /// Where the cue starts.
    pub at: Position,
    /// Where it ends. Defaults to the next cue's `at`, or the end of the
    /// book for the last cue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<Position>,
    /// A few words from the book at `at`, so a tool can re-anchor the cue
    /// to a different recording from its transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    /// Why this music, for whoever reads the map later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// The track to play, if one is named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track: Option<TrackRef>,
    /// What the music should feel like: used to choose a track when
    /// `track` is absent or not in the library.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vibe: Vec<String>,
    /// Play nothing. Excludes `track` and `vibe`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub silence: bool,
    /// Which part of the track, and whether to repeat it. Absent means the
    /// whole track, once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<Region>,
    /// Level offset in decibels. Overrides the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level_db: Option<f32>,
    /// Overrides the default fade in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade_in: Option<Time>,
    /// Overrides the default fade out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade_out: Option<Time>,
}

/// A place in the book: an offset into a chapter, or into the whole
/// recording when no chapter is given. Chapters count from 1.
///
/// Written as a table, `{ chapter = 7, offset = "4:10" }`, or as a bare
/// time, `"4:10"`, when there is no chapter.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct Position {
    /// Chapter number, from 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chapter: Option<u32>,
    /// Offset from the chapter start, or from the recording start.
    pub offset: Time,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PositionRepr {
    Bare(Time),
    Full {
        #[serde(default)]
        chapter: Option<u32>,
        offset: Time,
    },
}

impl<'de> Deserialize<'de> for Position {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match PositionRepr::deserialize(deserializer)? {
            PositionRepr::Bare(offset) => Self {
                chapter: None,
                offset,
            },
            PositionRepr::Full { chapter, offset } => Self { chapter, offset },
        })
    }
}

/// A track named by its tags, so it can be found in any library.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackRef {
    /// Artist.
    pub artist: String,
    /// Title.
    pub title: String,
    /// Album, to tell versions apart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    /// The recording ID at musicbrainz.org, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mbid: Option<String>,
}

/// Which part of a track to play.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Region {
    /// In point.
    pub start: Time,
    /// Out point. Absent means the end of the track.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<Time>,
    /// Go back to the in point instead of stopping.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub repeat: bool,
    /// Crossfade at the seam when repeating. Defaults to the map's
    /// crossfade.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crossfade: Option<Time>,
}

/// What is wrong with a map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Not valid TOML, or not the shape of a map.
    Parse(String),
    /// Could not be written out.
    Format(String),
    /// A version this crate does not read.
    Version(u32),
    /// A cue says `silence` and also names music.
    Conflict {
        /// Index into `cues`.
        cue: usize,
    },
    /// A cue names no track, no vibe, and no silence.
    Empty {
        /// Index into `cues`.
        cue: usize,
    },
    /// A cue's region ends at or before it starts.
    Region {
        /// Index into `cues`.
        cue: usize,
    },
    /// A cue names a chapter the recording does not have.
    Chapter {
        /// Index into `cues`.
        cue: usize,
        /// The chapter asked for.
        chapter: u32,
    },
    /// A cue starts before the one before it, or ends after the next.
    Order {
        /// Index into `cues`.
        cue: usize,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "map does not parse: {e}"),
            Self::Format(e) => write!(f, "map cannot be written: {e}"),
            Self::Version(v) => write!(f, "map version {v} is not supported; this reads {VERSION}"),
            Self::Conflict { cue } => {
                write!(f, "cue {} asks for silence and names music", cue + 1)
            }
            Self::Empty { cue } => {
                write!(f, "cue {} names no track, no vibe, and no silence", cue + 1)
            }
            Self::Region { cue } => {
                write!(f, "cue {} has a region that ends before it starts", cue + 1)
            }
            Self::Chapter { cue, chapter } => write!(
                f,
                "cue {} is in chapter {chapter}, which this recording does not have",
                cue + 1
            ),
            Self::Order { cue } => {
                write!(f, "cue {} overlaps or precedes the cue before it", cue + 1)
            }
        }
    }
}

impl std::error::Error for Error {}

impl Map {
    /// An empty map at the current version.
    #[must_use]
    pub fn new() -> Self {
        Self {
            version: VERSION,
            id: None,
            name: None,
            based_on: None,
            complete: true,
            book: Book::default(),
            defaults: Defaults::default(),
            cues: Vec::new(),
        }
    }

    /// Read a map from TOML and check it.
    ///
    /// # Errors
    /// The text is not a map, or a cue is contradictory. Order against a
    /// recording is checked later, by [`Schedule::new`].
    pub fn parse(toml_text: &str) -> Result<Self, Error> {
        let map: Self = toml::from_str(toml_text).map_err(|e| Error::Parse(e.to_string()))?;
        map.validate()?;
        Ok(map)
    }

    /// Write the map as TOML.
    ///
    /// # Errors
    /// Serialization failed, which only happens with non-finite floats.
    pub fn to_toml(&self) -> Result<String, Error> {
        toml::to_string_pretty(self).map_err(|e| Error::Format(e.to_string()))
    }

    /// Check every cue for contradictions.
    ///
    /// # Errors
    /// See [`Error`].
    pub fn validate(&self) -> Result<(), Error> {
        if self.version != VERSION {
            return Err(Error::Version(self.version));
        }
        for (cue, c) in self.cues.iter().enumerate() {
            let names_music = c.track.is_some() || !c.vibe.is_empty();
            if c.silence && names_music {
                return Err(Error::Conflict { cue });
            }
            if !c.silence && !names_music {
                return Err(Error::Empty { cue });
            }
            if let Some(r) = &c.region
                && r.end.is_some_and(|e| e <= r.start)
            {
                return Err(Error::Region { cue });
            }
        }
        Ok(())
    }

    /// The fade in for `cue`, after defaults.
    #[must_use]
    pub fn fade_in(&self, cue: &Cue) -> Duration {
        cue.fade_in.unwrap_or(self.defaults.fade_in).duration()
    }

    /// The fade out for `cue`, after defaults.
    #[must_use]
    pub fn fade_out(&self, cue: &Cue) -> Duration {
        cue.fade_out.unwrap_or(self.defaults.fade_out).duration()
    }

    /// The crossfade between this cue's music and the next, after defaults.
    #[must_use]
    pub fn crossfade(&self) -> Duration {
        self.defaults.crossfade.duration()
    }

    /// The level offset for `cue` in decibels, after defaults.
    #[must_use]
    pub fn level_db(&self, cue: &Cue) -> f32 {
        cue.level_db.unwrap_or(self.defaults.level_db)
    }

    /// The level offset for `cue` as a linear gain.
    #[must_use]
    pub fn level_gain(&self, cue: &Cue) -> f32 {
        10f32.powf(self.level_db(cue) / 20.0)
    }

    /// How to window the track for `cue`, ready for the engine. The whole
    /// track once when the cue has no region.
    #[must_use]
    pub fn region_config(&self, cue: &Cue) -> RegionConfig {
        let Some(r) = &cue.region else {
            return RegionConfig::default();
        };
        RegionConfig {
            start: r.start.duration(),
            end: r.end.map(Time::duration),
            repeat: r.repeat,
            crossfade: r.crossfade.unwrap_or(self.defaults.crossfade).duration(),
            ..RegionConfig::default()
        }
    }
}

impl Default for Map {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
version = 1

[book]
title = "The Name of the Wind"
author = "Patrick Rothfuss"
duration = "27:55:00"

[defaults]
fade_in = "8"
level_db = -6

[[cue]]
at = { chapter = 7, offset = "4:10" }
until = { chapter = 7, offset = "11:30" }
anchor = "the lute was warm in his hands"
why = "first time at the Eolian"
track = { artist = "Yo-Yo Ma", title = "Prelude", album = "Bach: Cello Suites" }
vibe = ["solo strings", "warm", "no vocals"]
region = { start = "0:40", end = "2:15", repeat = true, crossfade = "3" }
level_db = -9

[[cue]]
at = { chapter = 7, offset = "11:30" }
silence = true
why = "Denna speaks"

[[cue]]
at = { chapter = 8, offset = "0:00" }
vibe = ["sparse", "slow", "piano"]
fade_in = "15"
"#;

    #[test]
    fn parses_the_sample() {
        let m = Map::parse(SAMPLE).unwrap();
        assert_eq!(m.book.title.as_deref(), Some("The Name of the Wind"));
        assert_eq!(m.cues.len(), 3);
        let c = &m.cues[0];
        assert_eq!(c.at.chapter, Some(7));
        assert_eq!(c.at.offset, Time::secs(250.0));
        assert_eq!(c.track.as_ref().unwrap().artist, "Yo-Yo Ma");
        assert_eq!(c.vibe.len(), 3);
        assert_eq!(m.level_db(c), -9.0);
        assert_eq!(m.level_db(&m.cues[1]), -6.0);
        assert_eq!(m.fade_in(c), Duration::from_secs(8));
        assert_eq!(m.fade_in(&m.cues[2]), Duration::from_secs(15));
        assert_eq!(m.fade_out(c), Duration::from_secs(6));
        assert!(m.cues[1].silence);
    }

    #[test]
    fn region_config_fills_in_defaults() {
        let m = Map::parse(SAMPLE).unwrap();
        let r = m.region_config(&m.cues[0]);
        assert_eq!(r.start, Duration::from_secs(40));
        assert_eq!(r.end, Some(Duration::from_secs(135)));
        assert!(r.repeat);
        assert_eq!(r.crossfade, Duration::from_secs(3));
        assert_eq!(m.region_config(&m.cues[2]), RegionConfig::default());
    }

    #[test]
    fn level_gain_is_decibels() {
        let mut m = Map::new();
        m.defaults.level_db = -6.0;
        let c = Cue::default();
        assert!((m.level_gain(&c) - 0.501).abs() < 0.001);
    }

    #[test]
    fn bare_positions_are_offsets() {
        let m = Map::parse("version = 1\n[[cue]]\nat = \"4:10\"\nvibe = [\"x\"]").unwrap();
        assert_eq!(
            m.cues[0].at,
            Position {
                chapter: None,
                offset: Time::secs(250.0)
            }
        );
    }

    #[test]
    fn identity_is_optional_and_complete_by_default() {
        let m = Map::parse("version = 1").unwrap();
        assert_eq!(m.id, None);
        assert!(m.complete);
        let m = Map::parse(
            "version = 1\nid = \"8f3c\"\nname = \"Score\"\nbased_on = \"a91d\"\ncomplete = false",
        )
        .unwrap();
        assert_eq!(m.id.as_deref(), Some("8f3c"));
        assert_eq!(m.name.as_deref(), Some("Score"));
        assert_eq!(m.based_on.as_deref(), Some("a91d"));
        assert!(!m.complete);
        let back = m.to_toml().unwrap();
        assert!(back.contains("complete = false"));
        assert!(!Map::new().to_toml().unwrap().contains("complete"));
    }

    #[test]
    fn round_trips_through_toml() {
        let m = Map::parse(SAMPLE).unwrap();
        let text = m.to_toml().unwrap();
        assert_eq!(Map::parse(&text).unwrap(), m);
    }

    #[test]
    fn rejects_contradictions() {
        let silent_and_named = "version = 1\n[[cue]]\nat = \"0\"\nsilence = true\nvibe = [\"x\"]";
        assert_eq!(
            Map::parse(silent_and_named).unwrap_err(),
            Error::Conflict { cue: 0 }
        );
        let nothing = "version = 1\n[[cue]]\nat = \"0\"";
        assert_eq!(Map::parse(nothing).unwrap_err(), Error::Empty { cue: 0 });
        let region = "version = 1\n[[cue]]\nat = \"0\"\nvibe = [\"x\"]\nregion = { start = \"1:00\", end = \"0:30\" }";
        assert_eq!(Map::parse(region).unwrap_err(), Error::Region { cue: 0 });
        assert_eq!(Map::parse("version = 2").unwrap_err(), Error::Version(2));
        assert!(matches!(
            Map::parse("version = 1\n[[cue]]\nat = \"0\"\nvibe = [\"x\"]\ncolour = 1"),
            Err(Error::Parse(_))
        ));
    }

    #[test]
    fn errors_read_well() {
        assert_eq!(
            Error::Chapter { cue: 2, chapter: 9 }.to_string(),
            "cue 3 is in chapter 9, which this recording does not have"
        );
    }
}
