//! Which downloads to keep when space runs short.
//!
//! Each book on the phone lists the cache files it uses: its own audio,
//! and the music its map plays. Music is often shared, so a file goes only
//! when no book that stays still lists it. [`plan`] is pure: it is handed
//! the books, the files on disk, and the space allowed, and says what to
//! drop. The shell does the deleting.
//!
//! The order, while the cache is over its budget:
//!
//! 1. Files no book lists, oldest first: music from a map since changed,
//!    or files from before books kept lists.
//! 2. Books, least recently played first, each with the files only it
//!    used. A book kept for offline listening, or open now, is never
//!    dropped, and nor are its files.
//!
//! A book the listener removes goes at once, with the files only it used,
//! whatever the budget.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// A book on the phone and the cache files it uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Book {
    /// The shell's key for the book, such as `abs:<id>`.
    pub key: String,
    /// The names of the cache files it uses. Some may not be on disk yet.
    #[serde(default)]
    pub files: Vec<String>,
    /// When it was last played, in milliseconds since 1970.
    #[serde(default)]
    pub played: u64,
    /// Kept for offline listening: never dropped to make room.
    #[serde(default)]
    pub pinned: bool,
}

/// A file in the cache.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct File {
    pub name: String,
    pub bytes: u64,
    /// When it was last written, in milliseconds since 1970.
    #[serde(default)]
    pub modified: u64,
}

/// What to drop.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    /// Books to forget, in the order they were dropped.
    pub books: Vec<String>,
    /// Cache files to delete.
    pub files: Vec<String>,
    /// Bytes the cache holds once they are gone.
    pub bytes: u64,
}

/// What the shell asks [`plan`] about.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Ask {
    pub books: Vec<Book>,
    pub files: Vec<File>,
    /// Bytes allowed, or `None` for no limit.
    #[serde(default)]
    pub budget: Option<u64>,
    /// Books open now, which stay.
    #[serde(default)]
    pub open: Vec<String>,
    /// Files in use now, such as a book still loading, which stay.
    #[serde(default)]
    pub busy: Vec<String>,
    /// Books the listener removed, which go whatever the budget.
    #[serde(default)]
    pub remove: Vec<String>,
}

/// Decide which books and files to drop. See the module notes for the order.
#[must_use]
pub fn plan(ask: &Ask) -> Plan {
    let mut out = Plan::default();
    let mut gone: HashSet<&str> = HashSet::new();
    let mut kept: Vec<&Book> = Vec::new();
    for book in &ask.books {
        if ask.remove.contains(&book.key) {
            out.books.push(book.key.clone());
        } else {
            kept.push(book);
        }
    }
    let busy: HashSet<&str> = ask.busy.iter().map(String::as_str).collect();
    let listed =
        |kept: &[&Book], name: &str| kept.iter().any(|b| b.files.iter().any(|f| f == name));
    let mut total: u64 = ask.files.iter().map(|f| f.bytes).sum();

    // Removed books take the files only they used, whatever the budget.
    for file in &ask.files {
        let removed = ask
            .books
            .iter()
            .any(|b| ask.remove.contains(&b.key) && b.files.contains(&file.name));
        if removed && !listed(&kept, &file.name) && !busy.contains(file.name.as_str()) {
            delete(file, &mut total, &mut gone, &mut out);
        }
    }

    if let Some(budget) = ask.budget {
        // Files no book lists, oldest first.
        let mut orphans: Vec<&File> = ask
            .files
            .iter()
            .filter(|f| !gone.contains(f.name.as_str()) && !busy.contains(f.name.as_str()))
            .filter(|f| !listed(&kept, &f.name))
            .collect();
        orphans.sort_by_key(|f| f.modified);
        for file in orphans {
            if total <= budget {
                break;
            }
            delete(file, &mut total, &mut gone, &mut out);
        }

        // Then books, least recently played first.
        let mut candidates: Vec<&Book> = kept
            .iter()
            .copied()
            .filter(|b| !b.pinned && !ask.open.contains(&b.key))
            .collect();
        candidates.sort_by_key(|b| b.played);
        for book in candidates {
            if total <= budget {
                break;
            }
            kept.retain(|b| b.key != book.key);
            out.books.push(book.key.clone());
            for file in &ask.files {
                if gone.contains(file.name.as_str()) || busy.contains(file.name.as_str()) {
                    continue;
                }
                if book.files.contains(&file.name) && !listed(&kept, &file.name) {
                    delete(file, &mut total, &mut gone, &mut out);
                }
            }
        }
    }
    out.bytes = total;
    out
}

fn delete<'a>(file: &'a File, total: &mut u64, gone: &mut HashSet<&'a str>, out: &mut Plan) {
    *total = total.saturating_sub(file.bytes);
    gone.insert(file.name.as_str());
    out.files.push(file.name.clone());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book(key: &str, files: &[&str], played: u64, pinned: bool) -> Book {
        Book {
            key: key.into(),
            files: files.iter().map(|&f| f.into()).collect(),
            played,
            pinned,
        }
    }

    fn file(name: &str, bytes: u64, modified: u64) -> File {
        File {
            name: name.into(),
            bytes,
            modified,
        }
    }

    fn files() -> Vec<File> {
        vec![
            file("a.m4b", 500, 1),
            file("b.m4b", 400, 2),
            file("shared.flac", 30, 3),
            file("only-a.flac", 20, 4),
        ]
    }

    #[test]
    fn under_budget_nothing_goes() {
        let ask = Ask {
            books: vec![book("a", &["a.m4b"], 1, false)],
            files: files(),
            budget: Some(10_000),
            ..Ask::default()
        };
        let p = plan(&ask);
        assert!(p.books.is_empty() && p.files.is_empty());
        assert_eq!(p.bytes, 950);
    }

    #[test]
    fn no_limit_keeps_even_orphans() {
        let ask = Ask {
            books: vec![],
            files: files(),
            budget: None,
            ..Ask::default()
        };
        assert!(plan(&ask).files.is_empty());
    }

    #[test]
    fn removing_a_book_keeps_music_another_book_uses() {
        let ask = Ask {
            books: vec![
                book("a", &["a.m4b", "shared.flac", "only-a.flac"], 1, false),
                book("b", &["b.m4b", "shared.flac"], 2, false),
            ],
            files: files(),
            budget: None,
            remove: vec!["a".into()],
            ..Ask::default()
        };
        let p = plan(&ask);
        assert_eq!(p.books, vec!["a"]);
        assert_eq!(p.files, vec!["a.m4b", "only-a.flac"]);
        assert_eq!(p.bytes, 430);
    }

    #[test]
    fn orphans_go_before_any_book() {
        let ask = Ask {
            books: vec![
                book("a", &["a.m4b"], 1, false),
                book("b", &["b.m4b"], 2, false),
            ],
            files: files(),
            budget: Some(900),
            ..Ask::default()
        };
        let p = plan(&ask);
        assert!(p.books.is_empty());
        assert_eq!(p.files, vec!["shared.flac", "only-a.flac"]);
    }

    #[test]
    fn least_recently_played_goes_first_with_only_its_own_files() {
        let ask = Ask {
            books: vec![
                book("a", &["a.m4b", "shared.flac", "only-a.flac"], 1, false),
                book("b", &["b.m4b", "shared.flac"], 2, false),
            ],
            files: files(),
            budget: Some(600),
            ..Ask::default()
        };
        let p = plan(&ask);
        assert_eq!(p.books, vec!["a"]);
        assert_eq!(p.files, vec!["a.m4b", "only-a.flac"]);
        assert_eq!(p.bytes, 430);
    }

    #[test]
    fn pinned_and_open_books_stay_over_budget() {
        let ask = Ask {
            books: vec![
                book("a", &["a.m4b", "only-a.flac"], 1, true),
                book("b", &["b.m4b", "shared.flac"], 2, false),
            ],
            files: files(),
            budget: Some(0),
            open: vec!["b".into()],
            ..Ask::default()
        };
        let p = plan(&ask);
        assert!(p.books.is_empty());
        assert!(p.files.is_empty());
    }

    #[test]
    fn a_file_in_use_is_never_deleted() {
        let ask = Ask {
            books: vec![],
            files: files(),
            budget: Some(0),
            busy: vec!["a.m4b".into()],
            ..Ask::default()
        };
        let p = plan(&ask);
        assert!(!p.files.contains(&"a.m4b".to_string()));
        assert_eq!(p.bytes, 500);
    }

    #[test]
    fn music_a_pinned_book_shares_stays_when_another_book_goes() {
        let ask = Ask {
            books: vec![
                book("a", &["a.m4b", "shared.flac"], 1, false),
                book("b", &["b.m4b", "shared.flac"], 2, true),
            ],
            files: files(),
            budget: Some(0),
            ..Ask::default()
        };
        let p = plan(&ask);
        assert_eq!(p.books, vec!["a"]);
        assert!(p.files.contains(&"a.m4b".to_string()));
        assert!(!p.files.contains(&"shared.flac".to_string()));
    }
}
