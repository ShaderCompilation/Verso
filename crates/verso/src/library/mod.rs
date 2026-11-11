//! The library index is independent of the reading engine.
#![allow(dead_code)] // Consumed by the worker and screen in the following Phase 5 steps.

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
pub struct Metadata {
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
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Book {
    pub path: PathBuf,
    pub id: String,
    pub legacy_id: String,
    pub fingerprint: String,
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
        std::fs::write(root.join("library.json"), b"invalid")?;
        assert!(store.library().books.is_empty());
        assert!(store.book("legacy").finished);
        std::fs::remove_dir_all(root)?;
        Ok(())
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
        if let Some(old) = previous.iter().find(|old| old.path == b.path) {
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

#[cfg(test)]
mod identity_tests {
    use super::*;
    fn book(path: &str, fingerprint: &str) -> Book {
        Book {
            path: path.into(),
            id: String::new(),
            legacy_id: "legacy".into(),
            fingerprint: fingerprint.into(),
            stamp: Stamp::default(),
            metadata: Metadata::default(),
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
