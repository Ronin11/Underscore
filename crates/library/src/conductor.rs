//! From a music map and the book's position to "play this now".
//!
//! The [`Conductor`] is pure planning: it resolves every cue's track
//! against a library once, then turns each position update into at most
//! one [`Direction`]. Opening the track, decoding it, and handing it to
//! the player are separate steps ([`open`], [`apply`]) so a shell can run
//! the slow part off the audio thread.

use std::time::Duration;

use underscore_core::{Gain, LayerId, Player, Region, RegionConfig, Silence, Source};
use underscore_decode::FileSource;
use underscore_map::{Change, Map, Schedule, Span, Timeline};

use crate::{Cache, Error, ItemId, Library, Query};

/// How a cue's music was found, or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// The named track is in the library.
    Track(ItemId),
    /// The named track is not, or none was named; this matched the vibe.
    Vibe(ItemId),
    /// The cue asks for silence.
    Silence,
    /// Nothing matched. Plays as silence.
    Missing,
}

impl Resolution {
    /// The item to play, if there is one.
    #[must_use]
    pub fn item(&self) -> Option<&ItemId> {
        match self {
            Self::Track(id) | Self::Vibe(id) => Some(id),
            Self::Silence | Self::Missing => None,
        }
    }
}

/// What the music layer should do next.
#[derive(Debug, Clone, PartialEq)]
pub enum Direction {
    /// Crossfade to this item, windowed and scaled as given.
    Play {
        /// Index of the cue in the map.
        cue: usize,
        /// What to play.
        item: ItemId,
        /// Which part, and whether to repeat it.
        region: RegionConfig,
        /// Linear gain from the cue's level offset.
        gain: f32,
        /// How long the crossfade in takes.
        fade_in: Duration,
    },
    /// Fade the music out over this long.
    Silence {
        /// How long the fade takes.
        fade_out: Duration,
    },
}

/// A map, resolved and ready to follow the book.
#[derive(Debug)]
pub struct Conductor {
    map: Map,
    schedule: Schedule,
    resolved: Vec<Resolution>,
    /// The last direction given was silence, so another would be noise.
    silent: bool,
}

impl Conductor {
    /// Resolve `map` against `timeline` and look every cue up in `music`.
    ///
    /// # Errors
    /// The map does not fit the timeline, or the library failed.
    pub fn new(map: Map, timeline: &Timeline, music: &dyn Library) -> Result<Self, Error> {
        let schedule = Schedule::new(&map, timeline)?;
        let mut resolved = Vec::with_capacity(map.cues.len());
        for cue in &map.cues {
            resolved.push(resolve(cue, music)?);
        }
        Ok(Self {
            map,
            schedule,
            resolved,
            silent: false,
        })
    }

    /// The map being followed.
    #[must_use]
    pub fn map(&self) -> &Map {
        &self.map
    }

    /// How each cue resolved, in map order.
    #[must_use]
    pub fn resolutions(&self) -> &[Resolution] {
        &self.resolved
    }

    /// Every cue's span in the book, in map order.
    #[must_use]
    pub fn spans(&self) -> &[Span] {
        self.schedule.spans()
    }

    /// The cue current at the last update.
    #[must_use]
    pub fn current(&self) -> Option<usize> {
        self.schedule.current()
    }

    /// The book moved to `at`. Returns what the music should do, if that
    /// changed. Silence is only asked for once; a gap after a silent cue
    /// says nothing.
    pub fn update(&mut self, at: Duration) -> Option<Direction> {
        let change = self.schedule.update(at)?;
        let direction = match change {
            Change::Leave => Direction::Silence {
                fade_out: self.map.defaults.fade_out.duration(),
            },
            Change::Enter(n) => {
                let cue = &self.map.cues[n];
                match self.resolved[n].item() {
                    Some(item) => Direction::Play {
                        cue: n,
                        item: item.clone(),
                        region: self.map.region_config(cue),
                        gain: self.map.level_gain(cue),
                        fade_in: self.map.fade_in(cue),
                    },
                    None => Direction::Silence {
                        fade_out: self.map.fade_out(cue),
                    },
                }
            }
        };
        let silence = matches!(direction, Direction::Silence { .. });
        if silence && self.silent {
            return None;
        }
        self.silent = silence;
        Some(direction)
    }

    /// Forget what was last asked for, so the next update says it again.
    /// For when the player dropped the music and needs telling.
    pub fn reset(&mut self) {
        self.schedule.reset();
        self.silent = false;
    }
}

/// Every item `map` plays from `music`, each once, in map order, without
/// fitting it to a book. For fetching a map's music ahead, and for knowing
/// which cached files a book's map uses. Searches only; a server library
/// loads its song list first if it has not.
///
/// # Errors
/// The library failed.
pub fn items(map: &Map, music: &dyn Library) -> Result<Vec<ItemId>, Error> {
    let mut out: Vec<ItemId> = Vec::new();
    for cue in &map.cues {
        if let Some(id) = resolve(cue, music)?.item()
            && !out.contains(id)
        {
            out.push(id.clone());
        }
    }
    Ok(out)
}

fn resolve(cue: &underscore_map::Cue, music: &dyn Library) -> Result<Resolution, Error> {
    if cue.silence {
        return Ok(Resolution::Silence);
    }
    if let Some(t) = &cue.track {
        let q = Query::track(&t.artist, &t.title, t.album.as_deref());
        if let Some(hit) = music.search(&q)?.into_iter().next() {
            return Ok(Resolution::Track(hit.id));
        }
    }
    if !cue.vibe.is_empty()
        && let Some(hit) = music.search(&Query::vibe(&cue.vibe))?.into_iter().next()
    {
        return Ok(Resolution::Vibe(hit.id));
    }
    Ok(Resolution::Missing)
}

/// Fetch, decode, window, and scale the item a direction names, at the
/// engine's format. Slow: does I/O. Run it off the audio thread.
///
/// # Errors
/// The item could not be fetched or decoded, or its region is empty.
pub fn open(
    direction: &Direction,
    music: &dyn Library,
    cache: &Cache,
    sample_rate: u32,
    channels: usize,
) -> Result<Box<dyn Source>, Error> {
    match direction {
        Direction::Silence { .. } => Ok(Box::new(Silence::new(channels, sample_rate))),
        Direction::Play {
            item, region, gain, ..
        } => {
            let path = music.fetch(item, cache)?;
            let file = FileSource::open_path(&path, sample_rate, channels)?;
            let region = Region::new(Box::new(file), *region)
                .map_err(|_| Error::Unsupported(format!("the region of `{item}`")))?;
            let source: Box<dyn Source> = if (gain - 1.0).abs() > f32::EPSILON {
                Box::new(Gain::new(Box::new(region), *gain))
            } else {
                Box::new(region)
            };
            Ok(source)
        }
    }
}

/// Hand an opened source to the player's music layer with the fade the
/// direction asks for.
///
/// # Errors
/// The source's format does not match the engine; the source comes back.
pub fn apply(
    player: &mut Player,
    layer: LayerId,
    direction: &Direction,
    source: Box<dyn Source>,
) -> Result<(), Box<dyn Source>> {
    let over = match direction {
        Direction::Play { fade_in, .. } => *fade_in,
        Direction::Silence { fade_out } => *fade_out,
    };
    player.crossfade_layer(layer, source, over)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Item, LocalLibrary, Role};

    fn item(title: &str, artist: &str, tags: &[&str]) -> Item {
        Item {
            id: ItemId(format!("{artist}/{title}")),
            role: Role::Music,
            title: title.into(),
            artist: Some(artist.into()),
            album: None,
            duration: Some(Duration::from_secs(180)),
            tags: tags.iter().map(|t| (*t).to_string()).collect(),
            chapters: Vec::new(),
            format: None,
        }
    }

    fn library() -> LocalLibrary {
        LocalLibrary::from_items(
            "test",
            vec![
                item("Prelude", "Yo-Yo Ma", &["Classical"]),
                item("Clair de Lune", "Debussy", &["Classical", "Piano", "calm"]),
            ],
        )
    }

    const MAP: &str = r#"
        version = 1
        [defaults]
        fade_in = "8"
        fade_out = "6"
        level_db = -6
        [[cue]]
        at = "0:10"
        track = { artist = "Yo-Yo Ma", title = "Prelude" }
        region = { start = "0:40", end = "2:15", repeat = true }
        [[cue]]
        at = "1:00"
        track = { artist = "Nobody", title = "Nothing" }
        vibe = ["piano"]
        [[cue]]
        at = "2:00"
        until = "2:30"
        silence = true
        [[cue]]
        at = "3:00"
        vibe = ["polka"]
    "#;

    #[test]
    fn resolves_tracks_then_vibes_then_gives_up() {
        let c = Conductor::new(Map::parse(MAP).unwrap(), &Timeline::flat(), &library()).unwrap();
        assert_eq!(
            c.resolutions(),
            &[
                Resolution::Track(ItemId::from("Yo-Yo Ma/Prelude")),
                Resolution::Vibe(ItemId::from("Debussy/Clair de Lune")),
                Resolution::Silence,
                Resolution::Missing,
            ]
        );
    }

    #[test]
    fn a_maps_items_are_listed_once_each_without_a_book() {
        let twice = format!(
            "{MAP}\n[[cue]]\nat = \"4:00\"\ntrack = {{ artist = \"Yo-Yo Ma\", title = \"Prelude\" }}\n"
        );
        let map = Map::parse(&twice).unwrap();
        assert_eq!(
            items(&map, &library()).unwrap(),
            vec![
                ItemId::from("Yo-Yo Ma/Prelude"),
                ItemId::from("Debussy/Clair de Lune"),
            ]
        );
    }

    #[test]
    fn directions_follow_the_book() {
        let mut c =
            Conductor::new(Map::parse(MAP).unwrap(), &Timeline::flat(), &library()).unwrap();
        assert_eq!(c.update(Duration::from_secs(0)), None);
        let Some(Direction::Play {
            cue,
            item,
            region,
            gain,
            fade_in,
        }) = c.update(Duration::from_secs(10))
        else {
            panic!("expected play");
        };
        assert_eq!(cue, 0);
        assert_eq!(item, ItemId::from("Yo-Yo Ma/Prelude"));
        assert!(region.repeat);
        assert_eq!(region.start, Duration::from_secs(40));
        assert!((gain - 0.501).abs() < 0.001);
        assert_eq!(fade_in, Duration::from_secs(8));
        assert_eq!(c.update(Duration::from_secs(30)), None);
        assert!(matches!(
            c.update(Duration::from_secs(60)),
            Some(Direction::Play { cue: 1, .. })
        ));
        assert_eq!(
            c.update(Duration::from_secs(120)),
            Some(Direction::Silence {
                fade_out: Duration::from_secs(6)
            })
        );
        // The gap after the silent cue, and the cue nothing matched, are
        // silence too: already there, so nothing to say.
        assert_eq!(c.update(Duration::from_secs(150)), None);
        assert_eq!(c.update(Duration::from_secs(180)), None);
        // Seek back, then forward into the gap again.
        assert!(matches!(
            c.update(Duration::from_secs(15)),
            Some(Direction::Play { cue: 0, .. })
        ));
        assert!(matches!(
            c.update(Duration::from_secs(150)),
            Some(Direction::Silence { .. })
        ));
        // After a reset the current cue is said again.
        c.update(Duration::from_secs(70));
        c.reset();
        assert!(matches!(
            c.update(Duration::from_secs(70)),
            Some(Direction::Play { cue: 1, .. })
        ));
    }

    #[test]
    fn silence_opens_without_a_library_lookup() {
        let cache = Cache::open(std::env::temp_dir().join("underscore-conductor-test")).unwrap();
        let d = Direction::Silence {
            fade_out: Duration::from_secs(1),
        };
        let src = open(&d, &library(), &cache, 48_000, 2).unwrap();
        assert_eq!(src.channels(), 2);
        assert_eq!(src.sample_rate(), 48_000);
    }
}
