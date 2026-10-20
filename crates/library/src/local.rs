//! Folders on this machine as a library.
//!
//! One folder per role. A scan walks each folder, probes every audio file
//! for tags and length, and falls back to the file name when the tags are
//! missing. Progress is kept in a small JSON file the caller names.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};
use std::time::Duration;

use underscore_decode::probe_path;

use crate::{Cache, Chapter, Error, Item, ItemId, Library, Query, Role};

/// File extensions the decoder reads.
pub const EXTENSIONS: &[&str] = &["mp3", "m4a", "m4b", "aac", "flac", "ogg", "oga", "wav"];

/// A library over local folders.
pub struct LocalLibrary {
    name: String,
    roots: Vec<(Role, PathBuf)>,
    items: RwLock<Vec<Item>>,
    progress_file: Option<PathBuf>,
    progress: Mutex<HashMap<String, f64>>,
}

impl std::fmt::Debug for LocalLibrary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalLibrary")
            .field("name", &self.name)
            .field("roots", &self.roots)
            .field("items", &self.items.read().map_or(0, |i| i.len()))
            .finish_non_exhaustive()
    }
}

impl LocalLibrary {
    /// A library named `name` over `roots`, empty until [`scan`] runs.
    /// `progress_file`, when given, is where positions are remembered.
    ///
    /// [`scan`]: LocalLibrary::scan
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        roots: Vec<(Role, PathBuf)>,
        progress_file: Option<PathBuf>,
    ) -> Self {
        let progress = progress_file
            .as_ref()
            .and_then(|p| fs::read(p).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            name: name.into(),
            roots,
            items: RwLock::new(Vec::new()),
            progress_file,
            progress: Mutex::new(progress),
        }
    }

    /// A library over items the caller already has, for tests and for
    /// shells that keep their own index.
    #[must_use]
    pub fn from_items(name: impl Into<String>, items: Vec<Item>) -> Self {
        let lib = Self::new(name, Vec::new(), None);
        *lib.items
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = items;
        lib
    }

    /// Walk every root and rebuild the index. Files the decoder rejects
    /// are skipped. Returns how many items were found.
    ///
    /// # Errors
    /// A root could not be read.
    pub fn scan(&self) -> Result<usize, Error> {
        let mut found = Vec::new();
        for (role, root) in &self.roots {
            let mut files = Vec::new();
            walk(root, &mut files)?;
            files.sort();
            for path in files {
                if let Some(item) = Self::describe(*role, root, &path) {
                    found.push(item);
                }
            }
        }
        let n = found.len();
        *self
            .items
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = found;
        Ok(n)
    }

    /// Build an item for one file, from its tags or its name.
    fn describe(role: Role, root: &Path, path: &Path) -> Option<Item> {
        let rel = path.strip_prefix(root).ok()?;
        let id = ItemId(format!(
            "{}/{}",
            root.file_name()?.to_string_lossy(),
            rel.to_string_lossy()
        ));
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        let probe = probe_path(path).ok()?;
        let stem = path.file_stem()?.to_string_lossy().to_string();
        let (name_artist, name_title) = split_name(&stem);
        let parent = path
            .parent()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().to_string());
        let grandparent = path
            .parent()
            .and_then(Path::parent)
            .filter(|p| *p != root)
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().to_string());

        let title = probe.tags.title.unwrap_or(name_title);
        let artist = probe
            .tags
            .artist
            .or(probe.tags.album_artist)
            .or(name_artist)
            .or(grandparent);
        let album = probe
            .tags
            .album
            .or(parent.filter(|_| path.parent() != Some(root)));
        let tags = [probe.tags.genre, probe.tags.mood]
            .into_iter()
            .flatten()
            .flat_map(|t| {
                t.split([';', ',', '/'])
                    .map(|s| s.trim().to_string())
                    .collect::<Vec<_>>()
            })
            .filter(|t| !t.is_empty())
            .collect();
        let chapters = probe
            .chapters
            .into_iter()
            .map(|c| Chapter {
                title: c.title,
                start: c.start,
            })
            .collect();
        Some(Item {
            id,
            role,
            title,
            artist,
            album,
            duration: probe.duration,
            tags,
            chapters,
            format: Some(ext),
        })
    }

    /// The file behind an item id.
    fn path_of(&self, id: &ItemId) -> Option<PathBuf> {
        let (root_name, rel) = id.0.split_once('/')?;
        let (_, root) = self.roots.iter().find(|(_, r)| {
            r.file_name()
                .is_some_and(|n| n.to_string_lossy() == root_name)
        })?;
        let path = root.join(rel);
        path.is_file().then_some(path)
    }

    fn save_progress(&self, progress: &HashMap<String, f64>) -> io::Result<()> {
        let Some(file) = &self.progress_file else {
            return Ok(());
        };
        if let Some(dir) = file.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = file.with_extension("json.part");
        fs::write(&tmp, serde_json::to_vec_pretty(progress)?)?;
        fs::rename(tmp, file)
    }
}

/// `Artist - Title` from a file name, or just the title.
fn split_name(stem: &str) -> (Option<String>, String) {
    // Strip a leading track number: "03 - Title", "03. Title", "03 Title".
    let rest = stem
        .trim_start_matches(|c: char| c.is_ascii_digit())
        .trim_start_matches(['.', '-', ' ', '_']);
    let rest = if rest.is_empty() { stem } else { rest };
    match rest.split_once(" - ") {
        Some((artist, title)) if !artist.trim().is_empty() && !title.trim().is_empty() => {
            (Some(artist.trim().to_string()), title.trim().to_string())
        }
        _ => (None, rest.trim().to_string()),
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            walk(&path, out)?;
        } else if kind.is_file() || kind.is_symlink() {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase);
            if ext.is_some_and(|e| EXTENSIONS.contains(&e.as_str())) {
                out.push(path);
            }
        }
    }
    Ok(())
}

impl Library for LocalLibrary {
    fn name(&self) -> &str {
        &self.name
    }

    fn items(&self, role: Role) -> Result<Vec<Item>, Error> {
        let items = self
            .items
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(items.iter().filter(|i| i.role == role).cloned().collect())
    }

    fn item(&self, id: &ItemId) -> Result<Item, Error> {
        let items = self
            .items
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        items
            .iter()
            .find(|i| &i.id == id)
            .cloned()
            .ok_or_else(|| Error::NotFound(id.clone()))
    }

    fn search(&self, query: &Query) -> Result<Vec<Item>, Error> {
        let items = self
            .items
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(query.rank(items.iter()))
    }

    fn fetch(&self, id: &ItemId, _cache: &Cache) -> Result<PathBuf, Error> {
        self.path_of(id).ok_or_else(|| Error::NotFound(id.clone()))
    }

    fn progress(&self, id: &ItemId) -> Result<Option<Duration>, Error> {
        let progress = self
            .progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(progress.get(&id.0).map(|s| Duration::from_secs_f64(*s)))
    }

    fn report_progress(&self, id: &ItemId, at: Duration) -> Result<(), Error> {
        let mut progress = self
            .progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        progress.insert(id.0.clone(), at.as_secs_f64());
        self.save_progress(&progress)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny valid WAV: `secs` of silence, mono, 8 kHz.
    fn wav(secs: u32) -> Vec<u8> {
        let rate = 8_000u32;
        let data_len = rate * secs * 2;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        out.resize(out.len() + data_len as usize, 0);
        out
    }

    fn temp(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("underscore-local-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn splits_file_names() {
        assert_eq!(
            split_name("Yo-Yo Ma - Prelude"),
            (Some("Yo-Yo Ma".into()), "Prelude".into())
        );
        assert_eq!(split_name("03 - Prelude"), (None, "Prelude".into()));
        assert_eq!(
            split_name("03. Massive Attack - Teardrop"),
            (Some("Massive Attack".into()), "Teardrop".into())
        );
        assert_eq!(split_name("Prelude"), (None, "Prelude".into()));
        assert_eq!(split_name("1999"), (None, "1999".into()));
    }

    #[test]
    fn scans_folders_and_describes_files_by_name_and_layout() {
        let dir = temp("scan");
        let music = dir.join("music");
        let books = dir.join("books");
        fs::create_dir_all(music.join("Yo-Yo Ma/Cello Suites")).unwrap();
        fs::create_dir_all(&books).unwrap();
        fs::write(music.join("Yo-Yo Ma/Cello Suites/01 - Prelude.wav"), wav(2)).unwrap();
        fs::write(music.join("Massive Attack - Teardrop.wav"), wav(1)).unwrap();
        fs::write(music.join("notes.txt"), b"not audio").unwrap();
        fs::write(music.join("broken.mp3"), b"nope").unwrap();
        fs::write(books.join("The Hobbit.wav"), wav(3)).unwrap();

        let lib = LocalLibrary::new(
            "home",
            vec![(Role::Music, music.clone()), (Role::Speech, books)],
            Some(dir.join("state/progress.json")),
        );
        assert_eq!(lib.scan().unwrap(), 3);

        let tracks = lib.items(Role::Music).unwrap();
        assert_eq!(tracks.len(), 2);
        let prelude = tracks.iter().find(|t| t.title == "Prelude").unwrap();
        assert_eq!(prelude.artist.as_deref(), Some("Yo-Yo Ma"));
        assert_eq!(prelude.album.as_deref(), Some("Cello Suites"));
        assert_eq!(prelude.duration, Some(Duration::from_secs(2)));
        assert_eq!(prelude.format.as_deref(), Some("wav"));
        let teardrop = tracks.iter().find(|t| t.title == "Teardrop").unwrap();
        assert_eq!(teardrop.artist.as_deref(), Some("Massive Attack"));
        assert_eq!(teardrop.album, None);

        let books = lib.items(Role::Speech).unwrap();
        assert_eq!(books[0].title, "The Hobbit");
        assert_eq!(books[0].role, Role::Speech);

        let found = lib
            .search(&Query::track("yo-yo ma", "prelude", None))
            .unwrap();
        assert_eq!(found.len(), 1);
        let cache = Cache::open(dir.join("cache")).unwrap();
        let path = lib.fetch(&found[0].id, &cache).unwrap();
        assert!(path.ends_with("Yo-Yo Ma/Cello Suites/01 - Prelude.wav"));
        assert!(matches!(
            lib.fetch(&ItemId::from("music/missing.wav"), &cache),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            lib.item(&ItemId::from("nope")),
            Err(Error::NotFound(_))
        ));

        // Progress survives a new instance.
        let id = books[0].id.clone();
        assert_eq!(lib.progress(&id).unwrap(), None);
        lib.report_progress(&id, Duration::from_secs(90)).unwrap();
        let again = LocalLibrary::new("home", Vec::new(), Some(dir.join("state/progress.json")));
        assert_eq!(again.progress(&id).unwrap(), Some(Duration::from_secs(90)));
    }
}
