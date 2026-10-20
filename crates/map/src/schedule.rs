//! Turning a map into "which cue is current" as the book plays.
//!
//! A [`Timeline`] knows where the chapters of one particular recording
//! start, so a cue's chapter-and-offset position can be turned into one
//! absolute time. A [`Schedule`] holds every cue's resolved span and
//! answers the only question the player asks: did the current cue change?

use std::time::Duration;

use crate::{Error, Map, Position};

/// Where the chapters of one recording start, so positions can be
/// resolved against it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Timeline {
    starts: Vec<Duration>,
}

impl Timeline {
    /// A recording with no chapter data. Positions must be plain offsets.
    #[must_use]
    pub fn flat() -> Self {
        Self::default()
    }

    /// A recording whose chapters start at `starts`, in order, chapter 1
    /// first. Chapter 1 usually starts at zero.
    #[must_use]
    pub fn chapters(starts: Vec<Duration>) -> Self {
        Self { starts }
    }

    /// Number of chapters known.
    #[must_use]
    pub fn chapter_count(&self) -> usize {
        self.starts.len()
    }

    /// Absolute time of `position`, or `None` when it names a chapter this
    /// recording does not have.
    #[must_use]
    pub fn resolve(&self, position: &Position) -> Option<Duration> {
        let base = match position.chapter {
            None => Duration::ZERO,
            Some(n) => *self.starts.get(n.checked_sub(1)? as usize)?,
        };
        Some(base + position.offset.duration())
    }
}

/// A cue's resolved extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// When the cue starts.
    pub start: Duration,
    /// When it ends. `None` runs to the end of the book.
    pub end: Option<Duration>,
}

impl Span {
    fn contains(&self, at: Duration) -> bool {
        at >= self.start && self.end.is_none_or(|end| at < end)
    }
}

/// What changed between two positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Cue `n` of the map became current.
    Enter(usize),
    /// No cue is current any more: a gap between cues, or past the last.
    Leave,
}

/// A map resolved against one recording, tracking the current cue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    spans: Vec<Span>,
    current: Option<usize>,
}

impl Schedule {
    /// Resolve every cue of `map` against `timeline`.
    ///
    /// # Errors
    /// A cue names a chapter the timeline lacks, cues are out of order,
    /// or a cue's `until` is before its `at` or after the next cue.
    pub fn new(map: &Map, timeline: &Timeline) -> Result<Self, Error> {
        let mut spans = Vec::with_capacity(map.cues.len());
        for (i, cue) in map.cues.iter().enumerate() {
            let resolve = |p: &Position| {
                timeline.resolve(p).ok_or(Error::Chapter {
                    cue: i,
                    chapter: p.chapter.unwrap_or(0),
                })
            };
            let start = resolve(&cue.at)?;
            let end = cue.until.as_ref().map(resolve).transpose()?;
            if end.is_some_and(|e| e <= start) {
                return Err(Error::Order { cue: i });
            }
            spans.push(Span { start, end });
        }
        for i in 1..spans.len() {
            let next = spans[i].start;
            let prev = &mut spans[i - 1];
            if prev.start >= next || prev.end.is_some_and(|e| e > next) {
                return Err(Error::Order { cue: i });
            }
            if prev.end.is_none() {
                prev.end = Some(next);
            }
        }
        Ok(Self {
            spans,
            current: None,
        })
    }

    /// Every cue's resolved span, in map order.
    #[must_use]
    pub fn spans(&self) -> &[Span] {
        &self.spans
    }

    /// The cue current at `at`, if any.
    #[must_use]
    pub fn cue_at(&self, at: Duration) -> Option<usize> {
        let i = self
            .spans
            .partition_point(|s| s.start <= at)
            .checked_sub(1)?;
        self.spans[i].contains(at).then_some(i)
    }

    /// The cue last reported by [`Schedule::update`].
    #[must_use]
    pub fn current(&self) -> Option<usize> {
        self.current
    }

    /// Move to `at`. Returns what changed, if anything. Seeks are just a
    /// bigger step; nothing is assumed about the direction of travel.
    pub fn update(&mut self, at: Duration) -> Option<Change> {
        let now = self.cue_at(at);
        if now == self.current {
            return None;
        }
        self.current = now;
        Some(now.map_or(Change::Leave, Change::Enter))
    }

    /// Forget the current cue, so the next update reports it again. For
    /// when the player dropped the music and needs to be told what to play.
    pub fn reset(&mut self) {
        self.current = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    fn map(src: &str) -> Map {
        Map::parse(src).unwrap()
    }

    const BOOK: &str = r#"
        version = 1
        [[cue]]
        at = { chapter = 1, offset = "0:10" }
        vibe = ["calm"]
        [[cue]]
        at = { chapter = 2, offset = "0:00" }
        until = { chapter = 2, offset = "0:30" }
        silence = true
        [[cue]]
        at = { chapter = 3, offset = "1:00" }
        track = { artist = "A", title = "B" }
    "#;

    fn timeline() -> Timeline {
        Timeline::chapters(vec![secs(0), secs(100), secs(200)])
    }

    #[test]
    fn resolves_chapter_positions() {
        let t = timeline();
        let p = |chapter, offset| Position {
            chapter,
            offset: crate::Time::secs(offset),
        };
        assert_eq!(t.resolve(&p(Some(1), 10.0)), Some(secs(10)));
        assert_eq!(t.resolve(&p(Some(3), 5.0)), Some(secs(205)));
        assert_eq!(t.resolve(&p(None, 5.0)), Some(secs(5)));
        assert_eq!(t.resolve(&p(Some(4), 0.0)), None);
        assert_eq!(t.resolve(&p(Some(0), 0.0)), None);
        assert_eq!(Timeline::flat().resolve(&p(Some(1), 0.0)), None);
    }

    #[test]
    fn spans_close_at_the_next_cue_or_stay_open() {
        let s = Schedule::new(&map(BOOK), &timeline()).unwrap();
        assert_eq!(
            s.spans(),
            &[
                Span {
                    start: secs(10),
                    end: Some(secs(100))
                },
                Span {
                    start: secs(100),
                    end: Some(secs(130))
                },
                Span {
                    start: secs(260),
                    end: None
                },
            ]
        );
    }

    #[test]
    fn update_reports_enters_leaves_and_seeks() {
        let mut s = Schedule::new(&map(BOOK), &timeline()).unwrap();
        assert_eq!(s.update(secs(0)), None);
        assert_eq!(s.update(secs(5)), None);
        assert_eq!(s.update(secs(10)), Some(Change::Enter(0)));
        assert_eq!(s.update(secs(50)), None);
        assert_eq!(s.update(secs(100)), Some(Change::Enter(1)));
        assert_eq!(s.update(secs(130)), Some(Change::Leave));
        assert_eq!(s.update(secs(200)), None);
        assert_eq!(s.update(secs(1000)), Some(Change::Enter(2)));
        // Seek backwards into the first cue.
        assert_eq!(s.update(secs(20)), Some(Change::Enter(0)));
        s.reset();
        assert_eq!(s.update(secs(21)), Some(Change::Enter(0)));
    }

    #[test]
    fn rejects_out_of_order_and_overlapping_cues() {
        let backwards = r#"
            version = 1
            [[cue]]
            at = "1:00"
            vibe = ["a"]
            [[cue]]
            at = "0:30"
            vibe = ["b"]
        "#;
        assert!(matches!(
            Schedule::new(&map(backwards), &Timeline::flat()),
            Err(Error::Order { cue: 1 })
        ));
        let overlap = r#"
            version = 1
            [[cue]]
            at = "0:00"
            until = "2:00"
            vibe = ["a"]
            [[cue]]
            at = "1:00"
            vibe = ["b"]
        "#;
        assert!(matches!(
            Schedule::new(&map(overlap), &Timeline::flat()),
            Err(Error::Order { cue: 1 })
        ));
        let inverted = r#"
            version = 1
            [[cue]]
            at = "1:00"
            until = "0:30"
            vibe = ["a"]
        "#;
        assert!(matches!(
            Schedule::new(&map(inverted), &Timeline::flat()),
            Err(Error::Order { cue: 0 })
        ));
    }

    #[test]
    fn rejects_unknown_chapters() {
        let err = Schedule::new(&map(BOOK), &Timeline::chapters(vec![secs(0)])).unwrap_err();
        assert!(matches!(err, Error::Chapter { cue: 1, chapter: 2 }));
    }
}
