//! Following a music map while the book plays.
//!
//! The conductor is asked where the book is on every render block, under
//! the bridge lock. When it answers with a direction, the direction is
//! parked here as a [`Job`] and the worker thread is woken. The worker
//! takes the job, opens the item with the lock released, then takes the
//! lock again for the one crossfade. If the book moved on while the item
//! was opening, the result is dropped and the newer job is served instead.
//!
//! Nothing here touches JNI, so the whole path is tested as plain Rust.

use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::time::Duration;

use underscore_core::{LayerId, Player, Silence, Source};
use underscore_library::{Cache, Conductor, Direction, Library, Resolution, conductor};
use underscore_map::{Map, Timeline};

use crate::Bridge;

/// How long the worker sleeps between checks that the bridge still
/// exists, in case a wake-up was missed.
const IDLE_CHECK: Duration = Duration::from_secs(1);

/// A map, the library it resolves against, and what it is doing now.
pub struct MapPlayback {
    map: Map,
    library: Arc<dyn Library>,
    cache: Cache,
    /// Built once the map fits the loaded book.
    conductor: Option<Conductor>,
    /// Why there is no conductor, when there is none.
    unfit: Option<String>,
    /// The direction waiting for the worker. Only the latest is kept.
    pending: Option<Job>,
    /// Counts directions issued. A job older than this is stale.
    generation: u64,
    /// What went wrong last time the worker opened something.
    last_error: Option<String>,
}

impl std::fmt::Debug for MapPlayback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MapPlayback")
            .field("cues", &self.map.cues.len())
            .field("following", &self.conductor.is_some())
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

/// One direction, with everything needed to open it away from the lock.
pub struct Job {
    generation: u64,
    direction: Direction,
    library: Arc<dyn Library>,
    cache: Cache,
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Job")
            .field("generation", &self.generation)
            .field("direction", &self.direction)
            .finish_non_exhaustive()
    }
}

impl Job {
    /// Fetch, decode, and window the direction's item. Slow: does I/O.
    /// Never call it under the bridge lock.
    ///
    /// # Errors
    /// The item could not be fetched or decoded.
    pub fn open(
        &self,
        sample_rate: u32,
        channels: usize,
    ) -> Result<Box<dyn Source>, underscore_library::Error> {
        conductor::open(
            &self.direction,
            &*self.library,
            &self.cache,
            sample_rate,
            channels,
        )
    }

    /// What the job will do.
    #[must_use]
    pub fn direction(&self) -> &Direction {
        &self.direction
    }
}

impl MapPlayback {
    /// A map over a library, not yet fitted to any book.
    #[must_use]
    pub fn new(map: Map, library: Arc<dyn Library>, cache: Cache) -> Self {
        Self {
            map,
            library,
            cache,
            conductor: None,
            unfit: Some("no book loaded".to_string()),
            pending: None,
            generation: 0,
            last_error: None,
        }
    }

    /// Fit the map to a book with these chapter starts. An empty list is
    /// a book without chapter marks. Returns whether the map now follows.
    /// If music was already under the map's control, it is asked to fade
    /// out, since the new book starts outside every cue.
    pub fn fit(&mut self, chapter_starts: &[Duration]) -> bool {
        let timeline = if chapter_starts.is_empty() {
            Timeline::flat()
        } else {
            Timeline::chapters(chapter_starts.to_vec())
        };
        let was_following = self.conductor.is_some();
        match Conductor::new(self.map.clone(), &timeline, &*self.library) {
            Ok(c) => {
                self.conductor = Some(c);
                self.unfit = None;
            }
            Err(e) => {
                self.conductor = None;
                self.unfit = Some(match e {
                    underscore_library::Error::Map(underscore_map::Error::Chapter { .. })
                        if chapter_starts.is_empty() =>
                    {
                        "the map uses chapters and the book has none".to_string()
                    }
                    other => other.to_string(),
                });
            }
        }
        if was_following {
            self.issue(Direction::Silence {
                fade_out: self.map.defaults.fade_out.duration(),
            });
        } else {
            self.pending = None;
        }
        self.conductor.is_some()
    }

    /// Whether the map fits the loaded book and is being followed.
    #[must_use]
    pub fn following(&self) -> bool {
        self.conductor.is_some()
    }

    /// The book is at `at`. Returns true when a new job is waiting, so the
    /// caller should wake the worker. Cheap; runs on the render thread.
    pub fn update(&mut self, at: Duration) -> bool {
        let Some(direction) = self.conductor.as_mut().and_then(|c| c.update(at)) else {
            return false;
        };
        self.issue(direction);
        true
    }

    fn issue(&mut self, direction: Direction) {
        self.generation += 1;
        self.pending = Some(Job {
            generation: self.generation,
            direction,
            library: Arc::clone(&self.library),
            cache: self.cache.clone(),
        });
    }

    /// The job waiting for the worker, if any.
    pub fn take(&mut self) -> Option<Job> {
        self.pending.take()
    }

    /// The worker opened `job`. Apply it, unless the book has moved on.
    pub fn finish(
        &mut self,
        job: &Job,
        result: Result<Box<dyn Source>, underscore_library::Error>,
        player: &mut Player,
        layer: LayerId,
    ) {
        if job.generation != self.generation {
            return;
        }
        match result {
            Ok(source) => {
                if conductor::apply(player, layer, &job.direction, source).is_err() {
                    self.last_error = Some("the engine refused the source".to_string());
                } else {
                    self.last_error = None;
                }
            }
            Err(e) => self.last_error = Some(e.to_string()),
        }
    }

    /// One line for the UI: how the map resolved and what it is doing.
    #[must_use]
    pub fn status(&self) -> String {
        let Some(c) = &self.conductor else {
            let why = self.unfit.as_deref().unwrap_or("not fitted");
            return format!("Map loaded, not following: {why}");
        };
        let found = c
            .resolutions()
            .iter()
            .filter(|r| matches!(r, Resolution::Track(_) | Resolution::Vibe(_)))
            .count();
        let asked = c
            .resolutions()
            .iter()
            .filter(|r| !matches!(r, Resolution::Silence))
            .count();
        let now = match c.current() {
            None => "nothing yet".to_string(),
            Some(n) => self.describe(c, n),
        };
        let mut s = format!("Map: {found} of {asked} cues found. Now: {now}");
        if let Some(e) = &self.last_error {
            s.push_str(". Last error: ");
            s.push_str(e);
        }
        s
    }

    fn describe(&self, c: &Conductor, n: usize) -> String {
        let what = match &c.resolutions()[n] {
            Resolution::Track(id) | Resolution::Vibe(id) => self.library.item(id).map_or_else(
                |_| id.to_string(),
                |it| {
                    let by = it.artist.map(|a| format!(" by {a}")).unwrap_or_default();
                    format!("{}{by}", it.title)
                },
            ),
            Resolution::Silence => "silence".to_string(),
            Resolution::Missing => "nothing found".to_string(),
        };
        let why = c.map().cues[n]
            .why
            .as_deref()
            .map(|w| format!(" ({w})"))
            .unwrap_or_default();
        format!("cue {}, {what}{why}", n + 1)
    }
}

/// A source of silence at the engine's format, for fading the map's music
/// out when the map is removed.
#[must_use]
pub fn silence(sample_rate: u32, channels: usize) -> Box<dyn Source> {
    Box::new(Silence::new(channels, sample_rate))
}

/// How the render thread wakes the worker.
#[derive(Debug, Default)]
pub struct Wake {
    state: Mutex<WakeState>,
    cv: Condvar,
}

#[derive(Debug, Default)]
struct WakeState {
    pending: bool,
    stopped: bool,
}

impl Wake {
    /// There is work to do.
    pub fn notify(&self) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pending = true;
        self.cv.notify_one();
    }

    /// The bridge is going away; the worker should exit.
    pub fn stop(&self) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stopped = true;
        self.cv.notify_all();
    }

    /// Block until notified, stopped, or `timeout` passes. Returns false
    /// once stopped.
    fn wait(&self, timeout: Duration) -> bool {
        let guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let (mut guard, _) = self
            .cv
            .wait_timeout_while(guard, timeout, |s| !s.pending && !s.stopped)
            .unwrap_or_else(PoisonError::into_inner);
        guard.pending = false;
        !guard.stopped
    }
}

/// Start the worker that serves the bridge's map jobs. It holds the
/// bridge weakly and exits when the bridge is dropped.
///
/// # Panics
/// The operating system refused to start a thread.
pub fn spawn(bridge: Weak<Mutex<Bridge>>, wake: Arc<Wake>) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("underscore-map".to_string())
        .spawn(move || {
            loop {
                let Some(strong) = bridge.upgrade() else {
                    break;
                };
                let (rate, channels, job) = {
                    let mut b = strong.lock().unwrap_or_else(PoisonError::into_inner);
                    (b.sample_rate(), b.channels(), b.take_job())
                };
                if let Some(job) = job {
                    let result = job.open(rate, channels);
                    strong
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .finish_job(&job, result);
                    continue;
                }
                drop(strong);
                if !wake.wait(IDLE_CHECK) {
                    break;
                }
            }
        })
        .expect("spawn the map worker")
}
