//! The library index is independent of the reading engine.

pub mod collections;
pub mod covers;
pub mod epub;
pub mod worker;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    #[default]
    Recent,
    Title,
    Author,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    pub size: u64,
    pub modified_secs: u64,
    pub modified_nanos: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Metadata {
    pub identifier: String,
    pub title: String,
    pub authors: String,
    pub language: String,
    pub cover: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Cover {
    #[default]
    Pending,
    Ready(String),
    Unavailable(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Book {
    pub path: PathBuf,
    pub id: String,
    pub legacy_id: String,
    pub fingerprint: String,
    #[serde(default)]
    pub fingerprint_attempted: bool,
    pub stamp: Stamp,
    pub metadata: Metadata,
    pub error: Option<String>,
    pub cover: Cover,
    pub available: bool,
    #[serde(default)]
    pub metadata_pending: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LibraryIndex {
    pub version: u32,
    pub books: Vec<Book>,
}

impl LibraryIndex {
    pub const VERSION: u32 = 1;
}
impl Default for LibraryIndex {
    fn default() -> Self {
        Self { version: Self::VERSION, books: Vec::new() }
    }
}

/// Keep path identity on an edit; reuse a moved identity only with an unambiguous
/// content match. Legacy state is imported only for a unique filename/size pair.
fn assign_ids(books: &mut [Book], previous: &[Book]) {
    use std::collections::{HashMap, HashSet};
    let mut legacy_counts = HashMap::new();
    let mut fingerprints = HashMap::new();
    for b in books.iter().filter(|b| b.available) {
        *legacy_counts.entry(b.legacy_id.clone()).or_insert(0) += 1;
        *fingerprints.entry(b.fingerprint.clone()).or_insert(0) += 1;
    }
    let mut used: HashSet<String> = previous.iter().map(|b| b.id.clone()).collect();
    for b in books {
        if let Some(old) = previous.iter().find(|old| old.path == b.path)
            && ((!b.fingerprint.is_empty() && b.fingerprint == old.fingerprint)
                || (b.fingerprint.is_empty() && old.fingerprint.is_empty() && b.stamp == old.stamp)
                || (!b.metadata.identifier.is_empty() && b.metadata.identifier == old.metadata.identifier))
        {
            b.id = old.id.clone();
            continue;
        }
        let matches: Vec<_> =
            previous.iter().filter(|old| !b.fingerprint.is_empty() && old.fingerprint == b.fingerprint).collect();
        if matches.len() == 1 && fingerprints.get(&b.fingerprint) == Some(&1) {
            b.id = matches[0].id.clone();
        } else if legacy_counts.get(&b.legacy_id) == Some(&1)
            && !used.contains(&b.legacy_id)
            && !previous.iter().any(|old| old.legacy_id == b.legacy_id)
        {
            b.id = b.legacy_id.clone();
        } else {
            // The source path is only a disambiguator, never the normal identity.
            let mut hash = 0xcbf29ce484222325u64;
            for &byte in b.path.as_os_str().as_encoded_bytes() {
                hash = (hash ^ byte as u64).wrapping_mul(0x100000001b3);
            }
            let base = format!("{}-{hash:016x}", b.legacy_id);
            b.id = base.clone();
            let mut suffix = 0;
            while used.contains(&b.id) {
                suffix += 1;
                b.id = format!("{base}-{suffix}");
            }
        }
        used.insert(b.id.clone());
    }
}

pub fn sort_books(books: &mut [Book], states: &std::collections::HashMap<String, crate::store::BookState>, sort: Sort) {
    let recent = |b: &Book| {
        let opened = states.get(&b.id).map_or(0, |s| s.last_opened);
        if opened > 0 { (opened, 0) } else { (b.stamp.modified_secs, b.stamp.modified_nanos) }
    };
    books.sort_by_cached_key(|b| {
        let author = b.metadata.authors.to_lowercase();
        let title = b.metadata.title.to_lowercase();
        let recency = std::cmp::Reverse(recent(b));
        match sort {
            Sort::Recent => (recency, false, String::new(), title, b.path.clone()),
            Sort::Title => (std::cmp::Reverse((0, 0)), false, title, author, b.path.clone()),
            Sort::Author => (std::cmp::Reverse((0, 0)), author.trim().is_empty(), author, title, b.path.clone()),
        }
    });
}
pub fn continue_book<'a>(
    books: &'a [Book],
    states: &std::collections::HashMap<String, crate::store::BookState>,
) -> Option<&'a Book> {
    books
        .iter()
        .filter(|b| {
            b.available
                && states.get(&b.id).is_some_and(|s| !s.position.0.is_empty() && !s.finished && !s.hidden_from_continue)
        })
        .max_by_key(|b| (states[&b.id].last_opened, &b.id))
}

#[cfg(test)]
pub(crate) mod selection_tests {
    use super::*;
    use crate::document::Position;
    use crate::store::BookState;
    use std::collections::HashMap;
    pub fn book(id: usize) -> Book {
        Book {
            path: format!("/books/{id}.epub").into(),
            id: id.to_string(),
            legacy_id: id.to_string(),
            fingerprint: id.to_string(),
            fingerprint_attempted: true,
            stamp: Stamp { modified_secs: 10 + id as u64, ..Default::default() },
            metadata: Metadata { title: format!("Title {id}"), authors: String::new(), ..Default::default() },
            error: None,
            cover: Cover::Pending,
            available: true,
            metadata_pending: false,
        }
    }
    #[test]
    fn deterministic_sorting_and_continue_flags() {
        let mut books: Vec<_> = (0..4).map(book).collect();
        books[0].metadata.title = "beta".into();
        books[1].metadata.title = "Alpha".into();
        books[2].metadata.title = "alpha".into();
        books[1].metadata.authors = "Zed".into();
        books[2].metadata.authors = "ann".into();
        let mut states = HashMap::from([(
            "0".into(),
            BookState { position: Position("saved".into()), last_opened: 30, ..Default::default() },
        )]);
        sort_books(&mut books, &states, Sort::Recent);
        assert_eq!(books.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(), ["0", "3", "2", "1"]);
        sort_books(&mut books, &states, Sort::Title);
        assert_eq!(books.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(), ["2", "1", "0", "3"]);
        sort_books(&mut books, &states, Sort::Author);
        assert_eq!(books.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(), ["2", "1", "0", "3"]);
        assert_eq!(continue_book(&books, &states).unwrap().id, "0");
        states.get_mut("0").unwrap().finished = true;
        assert!(continue_book(&books, &states).is_none());
        states.get_mut("0").unwrap().finished = false;
        states.get_mut("0").unwrap().hidden_from_continue = true;
        assert!(continue_book(&books, &states).is_none());
        states.get_mut("0").unwrap().hidden_from_continue = false;
        books.iter_mut().find(|b| b.id == "0").unwrap().available = false;
        assert!(continue_book(&books, &states).is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{BookState, Store};

    #[test]
    fn old_state_defaults_flags_and_broken_index_preserves_state() -> anyhow::Result<()> {
        let root = std::env::temp_dir().join(format!("verso-library-store-{}", std::process::id()));
        std::fs::create_dir_all(root.join("books"))?;
        let store = Store::at(root.clone());
        std::fs::write(root.join("books/legacy.json"), br#"{"position":"anchor","progress":2500}"#)?;
        let old = store.book("legacy");
        assert_eq!(old.position.0, "anchor");
        assert!(!old.finished && !old.hidden_from_continue);
        store.save_book("legacy", &BookState { finished: true, ..old })?;
        store.save_library(&LibraryIndex::default())?;
        std::fs::write(root.join("library.json.tmp"), b"interrupted")?;
        assert_eq!(store.library().version, LibraryIndex::VERSION);
        let mut book = selection_tests::book(0);
        book.path = "/library/nested/../book.epub".into();
        store.save_library(&LibraryIndex { books: vec![book], ..Default::default() })?;
        assert_eq!(store.library().books[0].path, std::path::Path::new("/library/book.epub"));
        assert_eq!(store.library().books[0].id, "0", "root spelling changes preserve assigned identities");
        std::fs::write(root.join("library.json"), b"invalid")?;
        assert!(store.library().books.is_empty());
        assert!(store.book("legacy").finished);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;
    fn book(path: &str, fingerprint: &str) -> Book {
        Book {
            path: path.into(),
            id: String::new(),
            legacy_id: "legacy".into(),
            fingerprint: fingerprint.into(),
            fingerprint_attempted: true,
            stamp: Stamp::default(),
            metadata: Metadata { identifier: "same-edition".into(), ..Default::default() },
            error: None,
            cover: Cover::Pending,
            available: true,
            metadata_pending: false,
        }
    }
    #[test]
    fn unique_legacy_moves_edits_and_collisions() {
        let mut original = vec![book("/a/title.epub", "one")];
        assign_ids(&mut original, &[]);
        assert_eq!(original[0].id, "legacy");
        let mut moved = vec![book("/b/renamed.epub", "one")];
        assign_ids(&mut moved, &original);
        assert_eq!(moved[0].id, "legacy");
        let mut edited = vec![book("/a/title.epub", "new edition")];
        assign_ids(&mut edited, &original);
        assert_eq!(edited[0].id, "legacy");
        let mut replacement = vec![book("/a/title.epub", "unrelated bytes")];
        replacement[0].metadata.identifier = "unrelated book".into();
        assign_ids(&mut replacement, &original);
        assert_ne!(replacement[0].id, "legacy");
        let mut collisions = vec![book("/a/title.epub", "one"), book("/b/title.epub", "two")];
        assign_ids(&mut collisions, &[]);
        assert_ne!(collisions[0].id, collisions[1].id);
        assert!(collisions.iter().all(|b| b.id != "legacy"));
        assign_ids(&mut collisions, &original);
        assert_eq!(collisions[0].id, "legacy");
        assert_ne!(collisions[1].id, "legacy");
        let mut ambiguous = vec![book("/a/copy.epub", "one"), book("/b/copy.epub", "one")];
        assign_ids(&mut ambiguous, &original);
        assert!(ambiguous.iter().all(|b| b.id != "legacy"));
        assert_ne!(ambiguous[0].id, ambiguous[1].id);
    }
}
