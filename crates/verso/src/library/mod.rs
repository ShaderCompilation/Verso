//! The library index is independent of the reading engine.

pub mod collections;
pub mod covers;
pub mod epub;
pub mod files;
pub mod home;
pub mod language;
pub mod previews;
pub mod search;
pub mod worker;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
    /// Inserts one newly received book using the same identity rules as a scan
    /// (`assign_ids`), without running a filesystem scan pass.
    pub fn add_book(&mut self, mut book: Book) -> Book {
        book.path = collections::normalize(&book.path);
        book.available = true;
        book.metadata_pending = false;
        book.error = None;
        if !book.fingerprint.is_empty() {
            book.fingerprint_attempted = true;
        }
        // A destination receiving a book is no longer empty.
        self.empty_folders.retain(|p| !book.path.starts_with(p));
        let previous = self.books.clone();
        let old_ids: std::collections::HashMap<PathBuf, String> =
            previous.iter().map(|b| (b.path.clone(), b.id.clone())).collect();
        let path = book.path.clone();
        self.books.retain(|b| b.path != path);
        self.books.push(book);
        assign_ids(&mut self.books, &previous);
        // Drop any unavailable tombstone whose identity moved onto an available book.
        let available_ids: std::collections::HashSet<String> =
            self.books.iter().filter(|b| b.available).map(|b| b.id.clone()).collect();
        self.books.retain(|b| b.available || old_ids.get(&b.path).is_none_or(|id| !available_ids.contains(id)));
        self.books.iter().find(|b| b.path == path).cloned().expect("added book is in index")
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
    /// A newly added book with its metadata and fingerprint already read.
    Add(Box<Book>),
}
impl PathChange {
    pub fn apply(&self, index: &mut LibraryIndex) {
        match self {
            Self::Move(from, to) => index.move_path(from, to),
            Self::Remove(path) => index.remove_path(path),
            Self::AddEmpty(path) => index.add_empty_folder(path.clone()),
            Self::Add(book) => {
                index.add_book((**book).clone());
            }
        }
    }
    pub fn apply_home(&self, home: &mut home::Summary, roots: &[PathBuf]) {
        let is_top_child = |path: &std::path::Path| {
            let path = collections::normalize_cow(path);
            roots.iter().any(|r| path.parent() == Some(r.as_path()))
        };
        let is_epub = |path: &std::path::Path| path.extension().is_some_and(|e| e.eq_ignore_ascii_case("epub"));
        match self {
            Self::Move(from, to) => {
                for entry in &mut home.books {
                    rewrite_path(&mut entry.book.path, from, to);
                }
                if home.library_root_count == 1 {
                    let from_top = is_top_child(from);
                    let to_top = is_top_child(to);
                    if from_top != to_top {
                        let count = if is_epub(from) { &mut home.library_books } else { &mut home.library_folders };
                        if to_top {
                            *count += 1;
                        } else {
                            *count = count.saturating_sub(1);
                        }
                    }
                }
            }
            Self::Remove(path) => {
                home.books.retain(|e| !e.book.path.starts_with(path));
                if home.library_root_count == 1 && is_top_child(path) {
                    if is_epub(path) {
                        home.library_books = home.library_books.saturating_sub(1);
                    } else {
                        home.library_folders = home.library_folders.saturating_sub(1);
                    }
                }
            }
            Self::AddEmpty(path) => {
                if home.library_root_count == 1 && is_top_child(path) {
                    home.library_folders += 1;
                }
            }
            Self::Add(book) => {
                if home.library_root_count == 1 && is_top_child(&book.path) {
                    home.library_books += 1;
                }
            }
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
    // Claim unique content before paths: an edited edition at the old path
    // must not steal the moved original's reading state. Reserved IDs prevent
    // fresh books from reusing old state; claims prevent duplicate transfers.
    let mut claimed = HashSet::new();
    let mut assigned = vec![None; books.len()];
    for (slot, b) in assigned.iter_mut().zip(books.iter()) {
        // A tombstone must not out-claim the available book of the same content.
        if b.available
            && let Some(Some(old)) = previous_fingerprints.get(&b.fingerprint)
            && fingerprints.get(&b.fingerprint) == Some(&1)
            && claimed.insert(old.id.clone())
        {
            *slot = Some(old.id.clone());
        }
    }
    for (slot, b) in assigned.iter_mut().zip(books.iter()) {
        if slot.is_none()
            && let Some(old) = previous_paths.get(&b.path)
            && ((!b.fingerprint.is_empty() && b.fingerprint == old.fingerprint)
                || (b.fingerprint.is_empty() && old.fingerprint.is_empty() && b.stamp == old.stamp)
                || (!b.metadata.identifier.is_empty() && b.metadata.identifier == old.metadata.identifier))
            && claimed.insert(old.id.clone())
        {
            *slot = Some(old.id.clone());
        }
    }
    for (assigned, b) in assigned.into_iter().zip(books.iter_mut()) {
        if let Some(id) = assigned {
            b.id = id;
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
        if b.available
            && fingerprints.get(&b.fingerprint) == Some(&1)
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

fn cmp_ci(a: &str, b: &str) -> std::cmp::Ordering {
    if a.is_ascii() && b.is_ascii() {
        a.bytes().map(|c| c.to_ascii_lowercase()).cmp(b.bytes().map(|c| c.to_ascii_lowercase()))
    } else {
        a.to_lowercase().cmp(&b.to_lowercase())
    }
}

/// Total order for books: title (case-insensitive), then author, then path.
pub fn compare_books(a: &Book, b: &Book) -> std::cmp::Ordering {
    cmp_ci(&a.metadata.title, &b.metadata.title)
        .then_with(|| cmp_ci(&a.metadata.authors, &b.metadata.authors))
        .then_with(|| a.path.cmp(&b.path))
}

/// The slice `start..end` of `indices` once ordered by title, without cloning
/// books or sorting items outside the page.
pub fn select_page_indices(books: &[Book], mut indices: Vec<usize>, start: usize, end: usize) -> Vec<usize> {
    if start >= indices.len() || start >= end {
        return Vec::new();
    }
    let end = end.min(indices.len());
    let cmp = |&a: &usize, &b: &usize| compare_books(&books[a], &books[b]);
    if end < indices.len() {
        indices.select_nth_unstable_by(end - 1, cmp);
        indices.truncate(end);
    }
    if start > 0 {
        indices.select_nth_unstable_by(start, cmp);
    }
    indices[start..end].sort_by(cmp);
    indices[start..end].to_vec()
}

pub fn sort_books(books: &mut [Book]) {
    books.sort_by_cached_key(|b| (b.metadata.title.to_lowercase(), b.metadata.authors.to_lowercase(), b.path.clone()));
}

#[cfg(test)]
pub(crate) mod selection_tests {
    use super::*;
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
        sort_books(&mut books);
        assert_eq!(books.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(), ["2", "1", "0", "3"]);
    }
    #[test]
    fn select_page_indices_matches_full_sort() {
        let mut books: Vec<_> = (0..25).map(book).collect();
        books[0].metadata.title = "beta".into();
        books[1].metadata.title = "Alpha".into();
        books[2].metadata.title = "alpha".into();
        books[3].metadata.title = "Война и мир".into();
        books[4].metadata.title = "война и мир".into();
        books[1].metadata.authors = "Zed".into();
        books[2].metadata.authors = "ann".into();
        books[3].metadata.authors = "Толстой".into();
        books[5].stamp = books[6].stamp.clone();
        books[5].metadata.title = books[6].metadata.title.clone();
        let mut full = books.clone();
        sort_books(&mut full);
        let full_ids: Vec<&str> = full.iter().map(|b| b.id.as_str()).collect();
        for (start, end) in [(0, 8), (8, 16), (16, 24), (24, 32), (3, 11), (0, 25), (30, 38)] {
            let picked = select_page_indices(&books, (0..books.len()).collect(), start, end);
            let picked_ids: Vec<&str> = picked.iter().map(|&i| books[i].id.as_str()).collect();
            let expected = &full_ids[start.min(full_ids.len())..end.min(full_ids.len())];
            assert_eq!(picked_ids, expected, "{start}..{end}");
        }
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
    fn received_book_beats_tombstone_of_same_content() {
        let mut tomb = book("/a/old.epub", "one");
        tomb.id = "x".into();
        tomb.available = false;
        let mut index = LibraryIndex { books: vec![tomb], ..Default::default() };
        let added = index.add_book(book("/b/new.epub", "one"));
        assert_eq!(added.id, "x");
        assert_eq!(added.annotation_id, "x");
        assert!(index.books.iter().all(|b| b.available));
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
    fn moved_original_keeps_reading_identity_before_same_path_edition_in_either_order() {
        let mut original = vec![book("/a/title.epub", "one")];
        assign_ids(&mut original, &[]);
        for reverse in [false, true] {
            let mut current = vec![book("/a/title.epub", "new edition"), book("/b/moved.epub", "one")];
            if reverse {
                current.reverse();
            }
            assign_ids(&mut current, &original);
            let moved = current.iter().find(|b| b.path == std::path::Path::new("/b/moved.epub")).unwrap();
            let edited = current.iter().find(|b| b.path == std::path::Path::new("/a/title.epub")).unwrap();
            assert_eq!(moved.id, original[0].id);
            assert_ne!(edited.id, moved.id);
            // Subsequent scans preserve both independently assigned identities.
            let previous = current.clone();
            assign_ids(&mut current, &previous);
            assert_eq!(current, previous);
        }
    }
    #[test]
    fn reading_identity_transfers_repair_duplicate_previous_ids() {
        let mut previous = vec![book("/a/title.epub", "one"), book("/b/title.epub", "two")];
        previous[0].id = "shared".into();
        previous[1].id = "shared".into();
        let mut current = previous.clone();
        assign_ids(&mut current, &previous);
        assert_ne!(current[0].id, current[1].id);
        assert_eq!(current.iter().filter(|b| b.id == "shared").count(), 1);
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
        // Also when A's edit shares the original EPUB identifier.
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
