//! The library index is independent of the reading engine.

pub mod collections;
pub mod covers;
pub mod epub;
pub mod files;
pub mod home;
pub mod search;
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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Book {
    pub path: PathBuf,
    pub id: String,
    #[serde(default)]
    pub annotation_id: String,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryIndex {
    pub version: u32,
    pub books: Vec<Book>,
    #[serde(default)]
    pub empty_folders: Vec<PathBuf>,
}

impl LibraryIndex {
    pub const VERSION: u32 = 1;

    pub fn move_path(&mut self, from: &std::path::Path, to: &std::path::Path) {
        // A destination receiving a book or folder is no longer empty.
        self.empty_folders.retain(|p| !to.starts_with(p));
        for book in &mut self.books {
            rewrite_path(&mut book.path, from, to);
        }
        for folder in &mut self.empty_folders {
            rewrite_path(folder, from, to);
        }
    }
    pub fn remove_path(&mut self, path: &std::path::Path) {
        self.books.retain(|b| !b.path.starts_with(path));
        self.empty_folders.retain(|p| !p.starts_with(path));
    }
    pub fn add_empty_folder(&mut self, path: PathBuf) {
        // A new child means its parent is no longer literally empty.
        self.empty_folders.retain(|p| !path.starts_with(p));
        if !self.empty_folders.contains(&path) {
            self.empty_folders.push(path);
            self.empty_folders.sort();
        }
    }
}
impl Default for LibraryIndex {
    fn default() -> Self {
        Self { version: Self::VERSION, books: Vec::new(), empty_folders: Vec::new() }
    }
}

pub(crate) fn rewrite_path(path: &mut PathBuf, from: &std::path::Path, to: &std::path::Path) {
    if let Ok(tail) = path.strip_prefix(from) {
        *path = if tail.as_os_str().is_empty() { to.to_owned() } else { to.join(tail) };
    }
}

/// Ordered edits shared by the worker and the screen's older publications.
#[derive(Clone, Debug)]
pub enum PathChange {
    Move(PathBuf, PathBuf),
    Remove(PathBuf),
    AddEmpty(PathBuf),
}
impl PathChange {
    pub fn apply(&self, index: &mut LibraryIndex) {
        match self {
            Self::Move(from, to) => index.move_path(from, to),
            Self::Remove(path) => index.remove_path(path),
            Self::AddEmpty(path) => index.add_empty_folder(path.clone()),
        }
    }
    pub fn apply_home(&self, home: &mut home::Summary) {
        match self {
            Self::Move(from, to) => {
                for entry in &mut home.books {
                    rewrite_path(&mut entry.book.path, from, to);
                }
            }
            Self::Remove(path) => home.books.retain(|e| !e.book.path.starts_with(path)),
            Self::AddEmpty(_) => {}
        }
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
    let mut previous_paths = HashMap::new();
    let mut previous_fingerprints = HashMap::new();
    let previous_legacy: HashSet<_> = previous.iter().map(|b| &b.legacy_id).collect();
    for old in previous {
        previous_paths.entry(&old.path).or_insert(old);
        if !old.fingerprint.is_empty() {
            // Only an unambiguous content match may transfer an identity.
            previous_fingerprints.entry(&old.fingerprint).and_modify(|entry| *entry = None).or_insert(Some(old));
        }
    }
    for b in books.iter_mut() {
        if let Some(old) = previous_paths.get(&b.path)
            && ((!b.fingerprint.is_empty() && b.fingerprint == old.fingerprint)
                || (b.fingerprint.is_empty() && old.fingerprint.is_empty() && b.stamp == old.stamp)
                || (!b.metadata.identifier.is_empty() && b.metadata.identifier == old.metadata.identifier))
        {
            b.id = old.id.clone();
            continue;
        }
        if let Some(Some(old)) = previous_fingerprints.get(&b.fingerprint)
            && fingerprints.get(&b.fingerprint) == Some(&1)
        {
            b.id = old.id.clone();
        } else if legacy_counts.get(&b.legacy_id) == Some(&1)
            && !used.contains(&b.legacy_id)
            && !previous_legacy.contains(&b.legacy_id)
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
    // Annotation identity is separate from reading identity. An unambiguous
    // content match wins over the path, because records were made against that
    // content; a same-path replacement keeps the identity only when no moved
    // copy claimed it. Each previous identity goes to at most one book, and a
    // fresh identity never reuses any previous one (its records may still exist).
    let annotation_identity =
        |o: &Book| if o.annotation_id.is_empty() { o.id.clone() } else { o.annotation_id.clone() };
    let reserved: HashSet<String> = previous.iter().map(annotation_identity).collect();
    let mut claimed = HashSet::new();
    let mut assigned: Vec<Option<String>> = vec![None; books.len()];
    for (slot, b) in assigned.iter_mut().zip(books.iter()) {
        if fingerprints.get(&b.fingerprint) == Some(&1)
            && let Some(Some(old)) = previous_fingerprints.get(&b.fingerprint)
            && claimed.insert(annotation_identity(old))
        {
            *slot = Some(annotation_identity(old));
        }
    }
    for (slot, b) in assigned.iter_mut().zip(books.iter()) {
        if slot.is_none()
            && let Some(old) = previous_paths.get(&b.path)
            && claimed.insert(annotation_identity(old))
        {
            *slot = Some(annotation_identity(old));
        }
    }
    for (slot, b) in assigned.into_iter().zip(books.iter_mut()) {
        b.annotation_id = slot.unwrap_or_else(|| {
            let mut id = b.id.clone();
            let mut suffix = 0;
            while reserved.contains(&id) || !claimed.insert(id.clone()) {
                suffix += 1;
                id = format!("{}-n{suffix}", b.id);
            }
            id
        });
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
            annotation_id: String::new(),
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
    fn deterministic_sorting() {
        let mut books: Vec<_> = (0..4).map(book).collect();
        books[0].metadata.title = "beta".into();
        books[1].metadata.title = "Alpha".into();
        books[2].metadata.title = "alpha".into();
        books[1].metadata.authors = "Zed".into();
        books[2].metadata.authors = "ann".into();
        let states = HashMap::from([(
            "0".into(),
            BookState { position: Position("saved".into()), last_opened: 30, ..Default::default() },
        )]);
        sort_books(&mut books, &states, Sort::Recent);
        assert_eq!(books.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(), ["0", "3", "2", "1"]);
        sort_books(&mut books, &states, Sort::Title);
        assert_eq!(books.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(), ["2", "1", "0", "3"]);
        sort_books(&mut books, &states, Sort::Author);
        assert_eq!(books.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(), ["2", "1", "0", "3"]);
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
    use std::collections::HashSet;
    fn book(path: &str, fingerprint: &str) -> Book {
        Book {
            path: path.into(),
            id: String::new(),
            annotation_id: String::new(),
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
        assert_eq!(replacement[0].annotation_id, "legacy");
        assert_eq!(moved[0].annotation_id, "legacy");
        let mut moved_replacement = vec![book("/moved/replacement.epub", "unrelated bytes")];
        assign_ids(&mut moved_replacement, &replacement);
        assert_eq!(moved_replacement[0].annotation_id, "legacy");
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
    #[test]
    fn annotation_identities_stay_unambiguous() {
        let mut original = vec![book("/a/title.epub", "one")];
        assign_ids(&mut original, &[]);
        // Replace the content at A while its old content moves to B in the same
        // rescan: the moved content keeps the records, A gets a fresh identity.
        let mut swapped = vec![book("/a/title.epub", "unrelated bytes"), book("/b/moved.epub", "one")];
        swapped[0].metadata.identifier = "unrelated book".into();
        assign_ids(&mut swapped, &original);
        assert_eq!(swapped[1].annotation_id, "legacy");
        assert_ne!(swapped[0].annotation_id, "legacy");
        // Also when A's edit keeps its reading identity through the identifier.
        let mut edited = vec![book("/a/title.epub", "new edition"), book("/b/moved.epub", "one")];
        assign_ids(&mut edited, &original);
        assert_eq!(edited[1].annotation_id, "legacy");
        assert_ne!(edited[0].annotation_id, "legacy");
        // Ambiguous copies fall back to the path; nobody else gets the identity.
        let mut copies = vec![book("/a/title.epub", "one"), book("/b/copy.epub", "one")];
        assign_ids(&mut copies, &original);
        assert_eq!(copies[0].annotation_id, "legacy");
        assert_ne!(copies[1].annotation_id, "legacy");
        // A previously reserved identity is never handed out fresh, even after
        // its book disappeared and a new book derives the same reading ID.
        let mut kept = vec![book("/a/title.epub", "unrelated bytes")];
        kept[0].metadata.identifier = "unrelated book".into();
        assign_ids(&mut kept, &original);
        assert_eq!(kept[0].annotation_id, "legacy");
        let mut fresh = vec![book("/c/other.epub", "three")];
        fresh[0].metadata.identifier = "other".into();
        assign_ids(&mut fresh, &kept);
        assert_ne!(fresh[0].annotation_id, "legacy");
        for books in [&swapped, &edited, &copies] {
            let ids: HashSet<_> = books.iter().map(|b| &b.annotation_id).collect();
            assert_eq!(ids.len(), books.len());
        }
    }
}
