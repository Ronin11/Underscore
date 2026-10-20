//! A directory of single items from the listener's own library, each one
//! exactly as its source served it.
//!
//! This is the per-stream cache the README allows. It has no way to take
//! audio from the engine: the only way in is a writer the caller fills
//! from a library, and the file lands under a name derived from the
//! library and item it came from.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use crate::ItemId;

/// A cache directory.
#[derive(Debug, Clone)]
pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    /// Open or create a cache at `dir`.
    ///
    /// # Errors
    /// The directory could not be created.
    pub fn open(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    /// Where the files live.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The path an item would have in the cache, present or not.
    #[must_use]
    pub fn path_for(&self, library: &str, id: &ItemId, ext: &str) -> PathBuf {
        let hash = fnv1a(library.as_bytes(), id.0.as_bytes());
        let ext = ext.trim_start_matches('.');
        let name = if ext.is_empty() {
            format!("{hash:016x}")
        } else {
            format!("{hash:016x}.{ext}")
        };
        self.dir.join(name)
    }

    /// The cached file for an item, if it is there.
    #[must_use]
    pub fn get(&self, library: &str, id: &ItemId, ext: &str) -> Option<PathBuf> {
        let path = self.path_for(library, id, ext);
        path.is_file().then_some(path)
    }

    /// The cached file for an item, filling it with `fill` first if it is
    /// missing. `fill` writes the item's bytes, unchanged, to the writer it
    /// is given. A failed fill leaves nothing behind.
    ///
    /// # Errors
    /// The fill failed or the file could not be written.
    pub fn get_or_fill(
        &self,
        library: &str,
        id: &ItemId,
        ext: &str,
        fill: impl FnOnce(&mut File) -> io::Result<()>,
    ) -> io::Result<PathBuf> {
        let path = self.path_for(library, id, ext);
        if path.is_file() {
            return Ok(path);
        }
        let tmp = path.with_extension(format!(
            "{}.part",
            path.extension().and_then(|e| e.to_str()).unwrap_or("")
        ));
        let result = File::create(&tmp).and_then(|mut f| {
            fill(&mut f)?;
            f.sync_all()
        });
        match result {
            Ok(()) => {
                fs::rename(&tmp, &path)?;
                Ok(path)
            }
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                Err(e)
            }
        }
    }

    /// Drop one item from the cache. Not an error if it was not there.
    ///
    /// # Errors
    /// The file exists and could not be removed.
    pub fn evict(&self, library: &str, id: &ItemId, ext: &str) -> io::Result<()> {
        match fs::remove_file(self.path_for(library, id, ext)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }

    /// Bytes on disk across every cached item.
    ///
    /// # Errors
    /// The directory could not be read.
    pub fn size(&self) -> io::Result<u64> {
        let mut total = 0;
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                total += entry.metadata()?.len();
            }
        }
        Ok(total)
    }
}

/// FNV-1a over `library`, a separator, and `id`. Stable across builds,
/// which the standard hasher is not.
fn fnv1a(library: &[u8], id: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in library.iter().chain(&[0u8]).chain(id) {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "underscore-cache-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn fills_once_and_then_hits() {
        let cache = Cache::open(temp()).unwrap();
        let id = ItemId::from("abc");
        assert!(cache.get("lib", &id, "mp3").is_none());
        let mut fills = 0;
        let p1 = cache
            .get_or_fill("lib", &id, "mp3", |f| {
                fills += 1;
                f.write_all(b"bytes")
            })
            .unwrap();
        let p2 = cache
            .get_or_fill("lib", &id, "mp3", |_| {
                fills += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(p1, p2);
        assert_eq!(fills, 1);
        assert_eq!(fs::read(&p1).unwrap(), b"bytes");
        assert_eq!(cache.size().unwrap(), 5);
        cache.evict("lib", &id, "mp3").unwrap();
        cache.evict("lib", &id, "mp3").unwrap();
        assert!(cache.get("lib", &id, "mp3").is_none());
    }

    #[test]
    fn failed_fill_leaves_nothing() {
        let cache = Cache::open(temp()).unwrap();
        let id = ItemId::from("x");
        let err = cache.get_or_fill("lib", &id, "flac", |f| {
            f.write_all(b"partial")?;
            Err(io::Error::other("network went away"))
        });
        assert!(err.is_err());
        assert!(cache.get("lib", &id, "flac").is_none());
        assert_eq!(fs::read_dir(cache.dir()).unwrap().count(), 0);
    }

    #[test]
    fn keys_are_scoped_by_library() {
        let cache = Cache::open(temp()).unwrap();
        let id = ItemId::from("1");
        assert_ne!(
            cache.path_for("a", &id, "mp3"),
            cache.path_for("b", &id, "mp3")
        );
        assert_ne!(
            cache.path_for("a", &id, "mp3"),
            cache.path_for("a", &ItemId::from("2"), "mp3")
        );
        assert!(
            cache
                .path_for("a", &id, ".m4b")
                .to_string_lossy()
                .ends_with(".m4b")
        );
    }
}
