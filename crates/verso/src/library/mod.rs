//! The library index is independent of the reading engine.
//!
//! `epub` and `markdown` parse metadata/covers without opening crengine;
//! `worker` scans configured roots incrementally by `(size, mtime)`, generates
//! cover and tile thumbnails (`covers`), builds the folder hierarchy
//! (`collections`) and runs off-thread Home/Library warm-up (`home`,
//! `previews`). Each book keeps a stable `id` (preserved across moves), an
//! `annotation_id` (content-based), and `legacy_id` for state lookup; `search`
//! filters cached metadata in memory and `sort` orders books.

pub mod collections;
pub mod covers;
pub mod discovery;
pub mod epub;
pub mod files;
pub mod format;
pub mod home;
pub mod language;
pub mod previews;
pub mod search;
pub mod sort;
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
    /// Whether the package has a description (synopsis) with text; `None`
    /// in records older than metadata version 1. The text is read when shown.
    pub synopsis: Option<bool>,
    /// The series name from calibre or an EPUB 3 `series` collection.
    pub series: String,
    /// The book's number in its series as written (`"3"`, `"1.5"`,
    /// `"3.0"`); parsed when sorting so `Metadata` stays `Eq`.
    pub series_index: String,
    /// The first creator's `file-as` name (`Sanderson, Brandon`).
    pub author_sort: String,
    /// The `read_metadata` version that produced this record. The scan
    /// re-reads older records in the background while they stay shown.
    pub version: u32,
}
impl Metadata {
    /// Bumped when `read_metadata` learns a field or changes parsing.
    pub const VERSION: u32 = 2;

    /// `Mistborn · Book 3`, `Mistborn` without a number, `None` without a series.
    pub fn series_label(&self) -> Option<String> {
        if self.series.is_empty() {
            return None;
        }
        let number = series_number(&self.series_index);
        Some(if number.is_empty() { self.series.clone() } else { format!("{} · Book {number}", self.series) })
    }
}

/// Reads display metadata for a supported book file, filling an empty title
/// from the filename stem.
pub fn read_metadata(path: &std::path::Path) -> anyhow::Result<Metadata> {
    match format::BookFormat::from_path(path) {
        Some(format::BookFormat::Epub) => epub::read(path),
        Some(format::BookFormat::Markdown) => crate::markdown::read(path),
        None => anyhow::bail!("Unsupported book format: {}", path.display()),
    }
}

/// Reads the book's synopsis text (`description` or `synopsis`), or `None` when absent.
pub fn read_synopsis(path: &std::path::Path) -> anyhow::Result<Option<String>> {
    match format::BookFormat::from_path(path) {
        Some(format::BookFormat::Epub) => epub::synopsis(path),
        Some(format::BookFormat::Markdown) => crate::markdown::synopsis(path),
        None => anyhow::bail!("Unsupported book format: {}", path.display()),
    }
}

/// The number badge on `book`'s cover, in every sort: its series number
/// from metadata. Guessed numbers move a book in Series sort but never label
/// it. Formatted with [`sort::format_number`].
pub fn series_badge(book: &Book) -> Option<String> {
    let m = &book.metadata;
    if m.series.trim().is_empty() {
        return None;
    }
    sort::parse_number(&m.series_index).map(sort::format_number)
}

/// A series number for display: parsed numbers are formatted with
/// [`sort::format_number`] (`2.50` → `2.5`, `01.0` → `1`, `.5` → `0.5`),
/// non-numeric values stay as trimmed text (`III`), and blank or bare `.`
/// return empty.
pub fn series_number(index: &str) -> String {
    let index = index.trim();
    if index.is_empty() || index == "." {
        String::new()
    } else if let Some(position) = sort::parse_number(index) {
        sort::format_number(position)
    } else {
        index.to_owned()
    }
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
            roots.iter().any(|r| path.parent() == Some(collections::normalize_cow(r).as_ref()))
        };
        let in_roots = |path: &std::path::Path| {
            let path = collections::normalize_cow(path);
            roots.iter().any(|r| path.starts_with(collections::normalize_cow(r).as_ref()))
        };
        let is_book = |path: &std::path::Path| format::BookFormat::is_book(path);
        match self {
            Self::Move(from, to) => {
                for entry in &mut home.books {
                    rewrite_path(&mut entry.book.path, from, to);
                }
                if home.library_root_count == 1 && !is_book(from) {
                    let from_top = is_top_child(from);
                    let to_top = is_top_child(to);
                    if from_top != to_top {
                        if to_top {
                            home.library_folders += 1;
                        } else {
                            home.library_folders = home.library_folders.saturating_sub(1);
                        }
                    }
                }
            }
            Self::Remove(path) => {
                home.books.retain(|e| !e.book.path.starts_with(path));
                if is_book(path) {
                    if in_roots(path) {
                        home.library_books = home.library_books.saturating_sub(1);
                    }
                } else if home.library_root_count == 1 && is_top_child(path) {
                    home.library_folders = home.library_folders.saturating_sub(1);
                }
            }
            Self::AddEmpty(path) => {
                if home.library_root_count == 1 && is_top_child(path) {
                    home.library_folders += 1;
                }
            }
            Self::Add(book) => {
                if in_roots(&book.path) {
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
    #[test]
    fn path_change_apply_home_updates_recursive_book_counts() {
        let roots = vec![PathBuf::from("/books")];
        let mut home = home::Summary::default();
        home.set_library_counts((1, 2, 5));

        // Moving a book between root and a subfolder leaves the total unchanged.
        PathChange::Move("/books/loose.epub".into(), "/books/Sub/loose.epub".into()).apply_home(&mut home, &roots);
        assert_eq!((home.library_folders, home.library_books), (2, 5));

        // Removing or adding a nested book updates the recursive total.
        PathChange::Remove("/books/Sub/nested.epub".into()).apply_home(&mut home, &roots);
        assert_eq!((home.library_folders, home.library_books), (2, 4));
        PathChange::Add(Box::new(book("/books/Sub/Deep/new.epub", "n"))).apply_home(&mut home, &roots);
        assert_eq!((home.library_folders, home.library_books), (2, 5));

        // Markdown files (.md and .markdown) follow the same count rules.
        PathChange::Add(Box::new(book("/books/notes.md", "md"))).apply_home(&mut home, &roots);
        assert_eq!((home.library_folders, home.library_books), (2, 6));
        PathChange::Move("/books/notes.md".into(), "/books/Sub/notes.md".into()).apply_home(&mut home, &roots);
        assert_eq!((home.library_folders, home.library_books), (2, 6));
        PathChange::Remove("/books/Sub/notes.md".into()).apply_home(&mut home, &roots);
        assert_eq!((home.library_folders, home.library_books), (2, 5));
        PathChange::Remove("/books/Sub/essay.markdown".into()).apply_home(&mut home, &roots);
        assert_eq!((home.library_folders, home.library_books), (2, 4));

        // Moving a top-level folder into a subfolder updates the folder count only.
        PathChange::Move("/books/Sub".into(), "/books/Archive/Sub".into()).apply_home(&mut home, &roots);
        assert_eq!((home.library_folders, home.library_books), (1, 4));
    }

    #[test]
    fn read_metadata_and_read_synopsis_dispatch_by_format() -> anyhow::Result<()> {
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let epub_meta = read_metadata(&fixtures.join("library3.epub"))?;
        assert!(!epub_meta.title.is_empty());
        let md_meta = read_metadata(&fixtures.join("rich.md"))?;
        assert_eq!(md_meta.title, "Field Notes on E-Ink Typography");
        assert_eq!(md_meta.synopsis, Some(true));
        let md_syn = read_synopsis(&fixtures.join("rich.md"))?.expect("rich.md synopsis");
        assert!(md_syn.contains("Practical notes on layouts"));
        let prose_meta = read_metadata(&fixtures.join("prose.markdown"))?;
        assert_eq!(prose_meta.title, "Plain Prose Notes");
        assert_eq!(read_synopsis(&fixtures.join("prose.markdown"))?, None);
        assert!(read_metadata(&fixtures.join("markdown-cover.png")).is_err());
        assert!(read_synopsis(&fixtures.join("markdown-cover.png")).is_err());
        Ok(())
    }
}
