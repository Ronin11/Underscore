//! The shared layer: where books and music come from.
//!
//! A [`Library`] is anything that can list what the listener owns, find a
//! track by its tags, hand over one item as a local file, and remember how
//! far through a book they are. Local folders implement it here. Servers
//! (Audiobookshelf, Subsonic, Jellyfin) implement the same trait with
//! HTTP underneath; the player never sees the difference.
//!
//! Two rules from `README.md` are enforced by shape, not by review:
//!
//! - The [`Cache`] stores single items of the listener's own library, one
//!   file each, exactly as the source served them. There is no API here
//!   that takes audio from the engine, so nothing mixed can reach disk.
//! - Nothing here decrypts. A library serves plain files or it does not
//!   serve them.
//!
//! [`conductor`] is the glue between a music map and the player: it turns
//! the book's position into "play this region of this track now".

pub mod abs;
pub mod cache;
pub mod conductor;
pub mod local;

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub use abs::{Audiobookshelf, Credentials};
pub use cache::Cache;
pub use conductor::{Conductor, Direction, Resolution};
pub use local::LocalLibrary;

/// What an item is for: the top layer or the bottom one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// A book or a podcast episode. Goes on top and ducks the rest.
    Speech,
    /// Music. Goes underneath.
    Music,
}

/// An item's identity within one library. Opaque; stable across restarts.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ItemId(pub String);

impl fmt::Display for ItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ItemId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// A chapter mark in a book.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chapter {
    /// Title, when known.
    pub title: Option<String>,
    /// Where it starts.
    pub start: Duration,
}

/// One thing a library holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// Identity within the library.
    pub id: ItemId,
    /// Which layer it belongs on.
    pub role: Role,
    /// Title.
    pub title: String,
    /// Artist for music, author for a book.
    #[serde(default)]
    pub artist: Option<String>,
    /// Album for music, series for a book.
    #[serde(default)]
    pub album: Option<String>,
    /// Length, when known.
    #[serde(default)]
    pub duration: Option<Duration>,
    /// Words that describe it: genre, mood, whatever the library has.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Chapter marks, for books. Empty when unknown.
    #[serde(default)]
    pub chapters: Vec<Chapter>,
    /// File extension or format name, as a decoding hint.
    #[serde(default)]
    pub format: Option<String>,
}

impl Item {
    /// Chapter start times, for building a map timeline.
    #[must_use]
    pub fn chapter_starts(&self) -> Vec<Duration> {
        self.chapters.iter().map(|c| c.start).collect()
    }
}

/// What to look for. Every field is optional; empty matches nothing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Query {
    /// Restrict to one role.
    pub role: Option<Role>,
    /// Artist, matched loosely.
    pub artist: Option<String>,
    /// Title, matched loosely.
    pub title: Option<String>,
    /// Album, matched loosely.
    pub album: Option<String>,
    /// Words to match against tags.
    pub tags: Vec<String>,
    /// Free text, matched against everything.
    pub text: Option<String>,
}

impl Query {
    /// A music track by its tags.
    #[must_use]
    pub fn track(artist: &str, title: &str, album: Option<&str>) -> Self {
        Self {
            role: Some(Role::Music),
            artist: Some(artist.to_string()),
            title: Some(title.to_string()),
            album: album.map(str::to_string),
            ..Self::default()
        }
    }

    /// Music that matches any of `tags`.
    #[must_use]
    pub fn vibe(tags: &[String]) -> Self {
        Self {
            role: Some(Role::Music),
            tags: tags.to_vec(),
            ..Self::default()
        }
    }

    /// How well `item` matches. Zero is no match. Higher is better.
    #[must_use]
    pub fn score(&self, item: &Item) -> u32 {
        if self.role.is_some_and(|r| r != item.role) {
            return 0;
        }
        let mut score = 0;
        let mut asked = false;
        let mut field = |want: &Option<String>, have: Option<&str>| {
            let Some(want) = want else { return true };
            asked = true;
            let Some(have) = have else { return false };
            match similarity(want, have) {
                Match::Exact => {
                    score += 4;
                    true
                }
                Match::Contains => {
                    score += 2;
                    true
                }
                Match::None => false,
            }
        };
        let title_ok = field(&self.title, Some(&item.title));
        let artist_ok = field(&self.artist, item.artist.as_deref());
        if !title_ok || !artist_ok {
            return 0;
        }
        // Album only refines: a wrong album halves the score, no album is
        // neutral, since many files are not tagged with one.
        if self.album.is_some() && !field(&self.album, item.album.as_deref()) {
            score /= 2;
        }
        if !self.tags.is_empty() {
            asked = true;
            let hits = self
                .tags
                .iter()
                .filter(|want| {
                    item.tags
                        .iter()
                        .any(|have| similarity(want, have) != Match::None)
                })
                .count();
            if hits == 0 && self.title.is_none() && self.artist.is_none() {
                return 0;
            }
            score += u32::try_from(hits).unwrap_or(u32::MAX);
        }
        if let Some(text) = &self.text {
            asked = true;
            let hay = [
                Some(item.title.as_str()),
                item.artist.as_deref(),
                item.album.as_deref(),
            ]
            .into_iter()
            .flatten()
            .chain(item.tags.iter().map(String::as_str));
            let hit = hay
                .map(|h| similarity(text, h))
                .fold(Match::None, Match::max);
            match hit {
                Match::Exact => score += 3,
                Match::Contains => score += 1,
                Match::None => return 0,
            }
        }
        if asked { score } else { 0 }
    }

    /// Rank `items` by score, best first, dropping non-matches. Backends
    /// without their own search use this.
    #[must_use]
    pub fn rank<'a>(&self, items: impl IntoIterator<Item = &'a Item>) -> Vec<Item> {
        let mut scored: Vec<(u32, &Item)> = items
            .into_iter()
            .map(|i| (self.score(i), i))
            .filter(|(s, _)| *s > 0)
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.title.cmp(&b.1.title)));
        scored.into_iter().map(|(_, i)| i.clone()).collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Match {
    None,
    Contains,
    Exact,
}

/// Fold case, drop punctuation, collapse whitespace.
fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = true;
    for ch in s.chars() {
        if ch.is_alphanumeric() {
            out.extend(ch.to_lowercase());
            space = false;
        } else if !space {
            out.push(' ');
            space = true;
        }
    }
    out.trim_end().to_string()
}

fn similarity(want: &str, have: &str) -> Match {
    let (w, h) = (normalize(want), normalize(have));
    if w.is_empty() {
        Match::None
    } else if w == h {
        Match::Exact
    } else if h.contains(&w) || w.contains(&h) && !h.is_empty() {
        Match::Contains
    } else {
        Match::None
    }
}

/// What went wrong.
#[derive(Debug)]
pub enum Error {
    /// Reading or writing a file.
    Io(std::io::Error),
    /// A file the decoder could not read.
    Decode(underscore_decode::DecodeError),
    /// A map that does not fit the recording.
    Map(underscore_map::Error),
    /// No such item.
    NotFound(ItemId),
    /// Something a backend cannot do.
    Unsupported(String),
    /// A backend's own failure, in its own words.
    Backend(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Decode(e) => write!(f, "{e}"),
            Self::Map(e) => write!(f, "{e}"),
            Self::NotFound(id) => write!(f, "no item `{id}`"),
            Self::Unsupported(what) => write!(f, "{what} is not supported here"),
            Self::Backend(what) => f.write_str(what),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<underscore_decode::DecodeError> for Error {
    fn from(e: underscore_decode::DecodeError) -> Self {
        Self::Decode(e)
    }
}

impl From<underscore_map::Error> for Error {
    fn from(e: underscore_map::Error) -> Self {
        Self::Map(e)
    }
}

/// Somewhere books and music come from.
///
/// Every method takes `&self` so one library can be shared between the
/// thread that plans and the thread that plays. Implementations keep their
/// own locks.
pub trait Library: Send + Sync {
    /// A short name, unique among the libraries in use. Cache keys and
    /// progress records are scoped by it.
    fn name(&self) -> &str;

    /// Everything of one role.
    ///
    /// # Errors
    /// The backend could not be reached or read.
    fn items(&self, role: Role) -> Result<Vec<Item>, Error>;

    /// One item by id.
    ///
    /// # Errors
    /// [`Error::NotFound`], or the backend failed.
    fn item(&self, id: &ItemId) -> Result<Item, Error>;

    /// Items matching `query`, best first.
    ///
    /// # Errors
    /// The backend failed.
    fn search(&self, query: &Query) -> Result<Vec<Item>, Error>;

    /// Make the item available as a local file and return its path. Local
    /// libraries return the file itself; others fill the cache first.
    ///
    /// # Errors
    /// [`Error::NotFound`], or the fetch failed.
    fn fetch(&self, id: &ItemId, cache: &Cache) -> Result<PathBuf, Error>;

    /// How far through an item the listener is, if the library knows.
    ///
    /// # Errors
    /// The backend failed.
    fn progress(&self, id: &ItemId) -> Result<Option<Duration>, Error>;

    /// Record how far through an item the listener is.
    ///
    /// # Errors
    /// The backend failed. Callers should log and carry on; progress is
    /// not worth stopping playback over.
    fn report_progress(&self, id: &ItemId, at: Duration) -> Result<(), Error>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(title: &str, artist: &str, album: Option<&str>, tags: &[&str]) -> Item {
        Item {
            id: ItemId(format!("{artist}/{title}")),
            role: Role::Music,
            title: title.into(),
            artist: Some(artist.into()),
            album: album.map(Into::into),
            duration: None,
            tags: tags.iter().map(|t| (*t).to_string()).collect(),
            chapters: Vec::new(),
            format: None,
        }
    }

    fn shelf() -> Vec<Item> {
        vec![
            item(
                "Prelude",
                "Yo-Yo Ma",
                Some("Bach: Cello Suites"),
                &["Classical"],
            ),
            item(
                "Prelude (Live)",
                "Yo-Yo Ma",
                Some("Live at Tanglewood"),
                &["Classical", "Live"],
            ),
            item(
                "Clair de Lune",
                "Debussy",
                None,
                &["Classical", "Piano", "calm"],
            ),
            item(
                "Teardrop",
                "Massive Attack",
                Some("Mezzanine"),
                &["Trip Hop"],
            ),
        ]
    }

    #[test]
    fn normalizes_loosely() {
        assert_eq!(normalize("  Bach: Cello  Suites! "), "bach cello suites");
        assert_eq!(similarity("prelude", "Prelude (Live)"), Match::Contains);
        assert_eq!(similarity("PRELUDE", "Prelude"), Match::Exact);
        assert_eq!(similarity("", "x"), Match::None);
    }

    #[test]
    fn track_query_prefers_the_exact_album() {
        let got = Query::track("Yo-Yo Ma", "Prelude", Some("Bach: Cello Suites")).rank(&shelf());
        assert_eq!(got[0].album.as_deref(), Some("Bach: Cello Suites"));
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn wrong_artist_is_no_match() {
        assert!(
            Query::track("Debussy", "Prelude", None)
                .rank(&shelf())
                .is_empty()
        );
    }

    #[test]
    fn vibe_query_counts_tag_hits() {
        let got = Query::vibe(&["piano".into(), "calm".into()]).rank(&shelf());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].title, "Clair de Lune");
        let got = Query::vibe(&["classical".into()]).rank(&shelf());
        assert_eq!(got.len(), 3);
    }

    #[test]
    fn text_query_searches_everything() {
        let q = Query {
            text: Some("mezzanine".into()),
            ..Query::default()
        };
        assert_eq!(q.rank(&shelf())[0].title, "Teardrop");
    }

    #[test]
    fn empty_query_matches_nothing() {
        assert!(Query::default().rank(&shelf()).is_empty());
    }

    #[test]
    fn role_filters() {
        let mut book = item("Prelude", "Someone", None, &[]);
        book.role = Role::Speech;
        let q = Query {
            role: Some(Role::Music),
            title: Some("Prelude".into()),
            ..Query::default()
        };
        assert_eq!(q.score(&book), 0);
    }
}
