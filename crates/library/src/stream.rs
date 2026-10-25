//! Playing a file while it downloads.
//!
//! A [`Download`] fills one cache file from the server, a range at a time,
//! and keeps track of which bytes have arrived. A [`StreamReader`] reads
//! that file like any other, waiting only for bytes that are not there
//! yet. A read past what has arrived tells the downloader to fetch from
//! there next, so a seek, or a container that keeps its index at the end,
//! does not wait for everything before it. When every byte is in, the file
//! moves to its place in the [`crate::Cache`], byte for byte what the
//! server holds, exactly as a whole download would have left it.
//!
//! This moves the server's own file downwards and nothing else. It has no
//! way to take samples from the engine.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// Fetches bytes from `from` to the end, as a reader. The downloader
/// calls it again from another offset when a reader needs bytes elsewhere.
pub type RangeFetch = dyn Fn(u64) -> io::Result<Box<dyn Read + Send>> + Send + Sync;

/// How long a reader waits for missing bytes before giving up.
const PATIENCE: Duration = Duration::from_mins(2);
/// How far ahead of the downloader a wanted offset may be and still be
/// worth waiting for, rather than starting a new request there.
const NEAR: u64 = 1 << 20;

/// Byte ranges that have arrived, sorted and merged.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Ranges(Vec<(u64, u64)>);

impl Ranges {
    /// Mark `from..to` arrived.
    pub fn add(&mut self, from: u64, to: u64) {
        if to <= from {
            return;
        }
        let at = self.0.partition_point(|r| r.1 < from);
        let mut lo = from;
        let mut hi = to;
        let mut end = at;
        while end < self.0.len() && self.0[end].0 <= hi {
            lo = lo.min(self.0[end].0);
            hi = hi.max(self.0[end].1);
            end += 1;
        }
        self.0.splice(at..end, [(lo, hi)]);
    }

    /// The end of the arrived run holding `at`, if `at` has arrived.
    #[must_use]
    pub fn run_end(&self, at: u64) -> Option<u64> {
        self.0.iter().find(|r| r.0 <= at && at < r.1).map(|r| r.1)
    }

    /// The first byte at or after `at` that has not arrived.
    #[must_use]
    pub fn next_gap(&self, at: u64) -> u64 {
        self.run_end(at).unwrap_or(at)
    }

    /// How many bytes have arrived.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.0.iter().map(|r| r.1 - r.0).sum()
    }
}

#[derive(Debug, Default)]
struct State {
    have: Ranges,
    /// Where a reader is waiting.
    want: Option<u64>,
    /// Every byte is in and the file is in its place.
    done: bool,
    error: Option<String>,
    /// Allowed to download the whole file. A paused download fetches only
    /// what a reader is waiting for.
    go: bool,
    /// Starts when this one is done: the next part of a book.
    next: Option<Download>,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    len: u64,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// One file coming down from the server.
#[derive(Clone)]
pub struct Download {
    shared: Arc<Shared>,
    part: PathBuf,
    path: PathBuf,
}

impl std::fmt::Debug for Download {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Download")
            .field("path", &self.path)
            .field("len", &self.shared.len)
            .finish_non_exhaustive()
    }
}

impl Download {
    /// Start filling `path`, `len` bytes, with `fetch`. The bytes go to
    /// `path` with `.part` added until all are in. Returns at once; the
    /// work runs on its own thread.
    ///
    /// # Errors
    /// The partial file could not be created.
    pub fn start(path: PathBuf, len: u64, fetch: Box<RangeFetch>) -> io::Result<Self> {
        Self::begin(path, len, fetch, true)
    }

    /// Like [`Download::start`], but only fetching what a reader waits
    /// for until [`Download::go`], or until the download before it, given
    /// [`Download::then`], is done. For the later parts of a book.
    ///
    /// # Errors
    /// The partial file could not be created.
    pub fn start_paused(path: PathBuf, len: u64, fetch: Box<RangeFetch>) -> io::Result<Self> {
        Self::begin(path, len, fetch, false)
    }

    /// Let a paused download fetch everything.
    pub fn go(&self) {
        self.shared.lock().go = true;
        self.shared.changed.notify_all();
    }

    /// Start `next` when this one is done.
    pub fn then(&self, next: Download) {
        let mut st = self.shared.lock();
        if st.done {
            drop(st);
            next.go();
        } else {
            st.next = Some(next);
        }
    }

    fn begin(path: PathBuf, len: u64, fetch: Box<RangeFetch>, go: bool) -> io::Result<Self> {
        let part = PathBuf::from(format!("{}.part", path.display()));
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .read(true)
            .open(&part)?;
        file.set_len(len)?;
        let this = Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    go,
                    ..State::default()
                }),
                changed: Condvar::new(),
                len,
            }),
            part,
            path,
        };
        let worker = this.clone();
        std::thread::Builder::new()
            .name("underscore-download".into())
            .spawn(move || worker.run(&file, &*fetch))?;
        Ok(this)
    }

    /// The file's full length.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.shared.len
    }

    /// Whether the file is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.shared.len == 0
    }

    /// Where the file will be in the cache once it is whole.
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// The file's extension, such as `m4b`: a hint for the decoder.
    #[must_use]
    pub fn extension(&self) -> Option<String> {
        self.path
            .extension()
            .and_then(|e| e.to_str())
            .map(ToOwned::to_owned)
    }

    /// Bytes in so far.
    #[must_use]
    pub fn received(&self) -> u64 {
        self.shared.lock().have.total()
    }

    /// The cache file, once every byte is in.
    #[must_use]
    pub fn finished(&self) -> Option<PathBuf> {
        self.shared.lock().done.then(|| self.path.clone())
    }

    /// What went wrong, if the download stopped.
    #[must_use]
    pub fn error(&self) -> Option<String> {
        self.shared.lock().error.clone()
    }

    /// A reader over the file, from its start.
    ///
    /// # Errors
    /// The partial file could not be opened.
    pub fn reader(&self) -> io::Result<StreamReader> {
        Ok(StreamReader {
            file: File::open(&self.part)?,
            pos: 0,
            shared: Arc::clone(&self.shared),
        })
    }

    fn run(&self, file: &File, fetch: &RangeFetch) {
        let result = self.fill(file, fetch);
        let mut st = self.shared.lock();
        match result {
            Ok(()) => match file
                .sync_all()
                .and_then(|()| fs::rename(&self.part, &self.path))
            {
                Ok(()) => st.done = true,
                Err(e) => st.error = Some(format!("finishing the download: {e}")),
            },
            Err(e) => st.error = Some(format!("downloading: {e}")),
        }
        let next = st.next.take();
        drop(st);
        self.shared.changed.notify_all();
        if let Some(next) = next {
            next.go();
        }
    }

    fn fill(&self, file: &File, fetch: &RangeFetch) -> io::Result<()> {
        let len = self.shared.len;
        let mut buf = vec![0u8; 1 << 16];
        loop {
            // Where a reader waits, if anywhere; otherwise the first gap.
            // Paused, wait for a reader or for the go.
            let (start, go) = {
                let mut st = self.shared.lock();
                while !st.go && st.want.is_none_or(|w| st.have.run_end(w).is_some()) {
                    st = self
                        .shared
                        .changed
                        .wait(st)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
                let wanted = st.want.take().filter(|w| st.have.run_end(*w).is_none());
                (st.have.next_gap(wanted.unwrap_or(0)), st.go)
            };
            let start = if start >= len {
                let first = self.shared.lock().have.next_gap(0);
                if first >= len {
                    return Ok(());
                }
                first
            } else {
                start
            };
            let mut body = fetch(start)?;
            let mut at = start;
            loop {
                let n = body.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                let n = usize::try_from((len - at).min(n as u64)).unwrap_or(n);
                file.write_all_at(&buf[..n], at)?;
                let from = at;
                at += n as u64;
                let switch = {
                    let mut st = self.shared.lock();
                    st.have.add(from, at);
                    let elsewhere = st
                        .want
                        .is_some_and(|w| st.have.run_end(w).is_none() && (w < at || w > at + NEAR));
                    // Ran into bytes already here: pick the next gap.
                    let overlap = at < len && st.have.run_end(at).is_some();
                    // Paused: a little past what the reader asked for is enough.
                    let enough = !go && !st.go && at >= start + NEAR;
                    elsewhere || overlap || enough || at >= len
                };
                self.shared.changed.notify_all();
                if switch {
                    break;
                }
            }
            if at == start {
                return Err(io::Error::other(format!(
                    "the server sent nothing from byte {start}"
                )));
            }
        }
    }
}

/// Reads a downloading file, waiting for bytes that are not in yet.
pub struct StreamReader {
    file: File,
    pos: u64,
    shared: Arc<Shared>,
}

impl StreamReader {
    /// The file's full length.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.shared.len
    }

    /// Whether the file is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.shared.len == 0
    }
}

impl Read for StreamReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let len = self.shared.len;
        if self.pos >= len || out.is_empty() {
            return Ok(0);
        }
        let mut st = self.shared.lock();
        let end = loop {
            if st.done {
                break len;
            }
            if let Some(end) = st.have.run_end(self.pos) {
                break end;
            }
            if let Some(e) = &st.error {
                return Err(io::Error::other(e.clone()));
            }
            st.want = Some(self.pos);
            self.shared.changed.notify_all();
            let (next, timeout) = self
                .shared
                .changed
                .wait_timeout(st, PATIENCE)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            st = next;
            if timeout.timed_out() && st.have.run_end(self.pos).is_none() && !st.done {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the download stalled",
                ));
            }
        };
        drop(st);
        let n = usize::try_from(end - self.pos)
            .unwrap_or(usize::MAX)
            .min(out.len());
        let got = self.file.read_at(&mut out[..n], self.pos)?;
        self.pos += got as u64;
        Ok(got)
    }
}

impl Seek for StreamReader {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let len = i128::from(self.shared.len);
        let next = match to {
            SeekFrom::Start(n) => i128::from(n),
            SeekFrom::End(d) => len + i128::from(d),
            SeekFrom::Current(d) => i128::from(self.pos) + i128::from(d),
        };
        self.pos = u64::try_from(next)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "seek before the start"))?;
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn ranges_merge_and_find_gaps() {
        let mut r = Ranges::default();
        r.add(10, 20);
        r.add(30, 40);
        r.add(18, 31);
        assert_eq!(r.0, vec![(10, 40)]);
        r.add(0, 5);
        assert_eq!(r.next_gap(0), 5);
        assert_eq!(r.next_gap(12), 40);
        assert_eq!(r.next_gap(7), 7);
        assert_eq!(r.run_end(39), Some(40));
        assert_eq!(r.run_end(40), None);
        assert_eq!(r.total(), 35);
    }

    /// A slow server: hands out its bytes a few at a time, from wherever it
    /// is asked, and counts the requests.
    fn slow(data: Vec<u8>, calls: Arc<AtomicUsize>) -> Box<RangeFetch> {
        struct Drip {
            data: Arc<Vec<u8>>,
            at: usize,
        }
        impl Read for Drip {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                std::thread::sleep(Duration::from_millis(2));
                let n = (self.data.len() - self.at).min(out.len()).min(16 << 10);
                out[..n].copy_from_slice(&self.data[self.at..self.at + n]);
                self.at += n;
                Ok(n)
            }
        }
        let data = Arc::new(data);
        Box::new(move |from| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(Drip {
                data: Arc::clone(&data),
                at: usize::try_from(from).unwrap(),
            }) as Box<dyn Read + Send>)
        })
    }

    #[test]
    fn reads_while_downloading_and_jumps_to_the_end() {
        // Bigger than NEAR, so the tail is worth a request of its own.
        let data: Vec<u8> = (0..(4u32 << 20)).map(|i| (i % 251) as u8).collect();
        let dir = std::env::temp_dir().join(format!("underscore-stream-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("book.m4b");
        let _ = fs::remove_file(&path);
        let calls = Arc::new(AtomicUsize::new(0));
        let dl = Download::start(
            path.clone(),
            data.len() as u64,
            slow(data.clone(), Arc::clone(&calls)),
        )
        .unwrap();
        let mut r = dl.reader().unwrap();

        // The tail, long before a front-to-back download would reach it.
        r.seek(SeekFrom::End(-1000)).unwrap();
        let mut tail = vec![0u8; 1000];
        r.read_exact(&mut tail).unwrap();
        assert_eq!(tail, data[data.len() - 1000..]);
        assert!(dl.finished().is_none(), "should still be downloading");

        // Then the start, and everything in order.
        r.seek(SeekFrom::Start(0)).unwrap();
        let mut all = Vec::new();
        r.read_to_end(&mut all).unwrap();
        assert_eq!(all, data);
        assert!(
            calls.load(Ordering::SeqCst) >= 2,
            "the jump should start a second request"
        );

        // It lands in the cache whole.
        for _ in 0..500 {
            if dl.finished().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(dl.finished(), Some(path.clone()));
        assert_eq!(fs::read(&path).unwrap(), data);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_paused_part_fetches_only_what_is_read_until_its_turn() {
        let data: Vec<u8> = (0..(3 << 20) as u32).map(|i| (i % 249) as u8).collect();
        let dir =
            std::env::temp_dir().join(format!("underscore-stream-parts-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let (one, two) = (dir.join("1.mp3"), dir.join("2.mp3"));
        let _ = fs::remove_file(&one);
        let _ = fs::remove_file(&two);
        let calls = Arc::new(AtomicUsize::new(0));
        let small = data[..1000].to_vec();
        let first = Download::start(one.clone(), 1000, slow(small, Arc::clone(&calls))).unwrap();
        let second =
            Download::start_paused(two.clone(), data.len() as u64, fast(data.clone())).unwrap();
        // A reader of the paused part gets its bytes, and no more comes down than it needs.
        let mut r = second.reader().unwrap();
        let mut head = vec![0u8; 100];
        r.read_exact(&mut head).unwrap();
        assert_eq!(head, data[..100]);
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            second.received() < 2 << 20,
            "paused, but fetched {}",
            second.received()
        );
        // Its turn comes when the part before it is done.
        first.then(second.clone());
        for _ in 0..500 {
            if second.finished().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(fs::read(&two).unwrap(), data);
        assert!(first.finished().is_some());
        let _ = fs::remove_dir_all(dir);
    }

    /// A quick server, for sizes the slow one would take too long over.
    fn fast(data: Vec<u8>) -> Box<RangeFetch> {
        let data = Arc::new(data);
        Box::new(move |from| {
            let from = usize::try_from(from).unwrap();
            Ok(Box::new(io::Cursor::new(data[from..].to_vec())) as Box<dyn Read + Send>)
        })
    }

    #[test]
    fn a_failed_download_fails_the_reader_instead_of_hanging() {
        let dir =
            std::env::temp_dir().join(format!("underscore-stream-bad-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let fetch: Box<RangeFetch> = Box::new(|_| Err(io::Error::other("no route")));
        let dl = Download::start(dir.join("x.mp3"), 1000, fetch).unwrap();
        let mut r = dl.reader().unwrap();
        let err = r.read(&mut [0u8; 10]).unwrap_err();
        assert!(err.to_string().contains("no route"), "{err}");
        let _ = fs::remove_dir_all(dir);
    }
}
