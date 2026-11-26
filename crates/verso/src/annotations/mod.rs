//! Versioned annotation records and an atomic per-book manifest. Immutable record
//! files become visible together when the manifest commits; tombstones retain
//! import identities after removal. No KOReader files are written.
pub mod export;
pub mod import;
pub mod lua;
pub mod worker;
use crate::{
    document::TextRange,
    store::{Store, atomic, atomic_batch},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap},
    path::PathBuf,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Annotation {
    pub version: u32,
    /// Serialized only for bookmarks, so highlight records keep the exact
    /// bytes they were written with: `Collection::commit` compares existing
    /// record files byte for byte.
    #[serde(default, skip_serializing_if = "Kind::is_highlight")]
    pub kind: Kind,
    pub id: String,
    pub book: String,
    /// The book's content fingerprint when the record was checked against
    /// it; empty for a bulk import, which never opens the book.
    pub fingerprint: String,
    pub range: TextRange,
    pub quotation: String,
    pub created: String,
    pub chapter: String,
    pub note: Option<String>,
    pub provenance: Option<Provenance>,
}
/// What an annotation marks. A page bookmark's range runs from the page start
/// to the end of its opening words, which are its `quotation`; on a page with
/// no text both the end and the quotation are empty.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Highlight,
    Bookmark,
}
impl Kind {
    fn is_highlight(&self) -> bool {
        *self == Kind::Highlight
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Provenance {
    pub source: String,
    pub entry: String,
}
#[derive(Clone, Default, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub records: Vec<String>,
    pub sources: BTreeSet<String>,
}
#[derive(Clone, Default)]
pub struct Collection {
    pub manifest: Manifest,
    pub records: Vec<Annotation>,
}
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
/// The text without soft hyphens, with each run of whitespace as one space and
/// none at the ends.
fn normalized(text: &str) -> impl Iterator<Item = char> + '_ {
    text.split_whitespace()
        .filter(|w| w.chars().any(|c| c != '\u{ad}'))
        .enumerate()
        .flat_map(|(i, w)| (i > 0).then_some(' ').into_iter().chain(w.chars().filter(|&c| c != '\u{ad}')))
}
/// Whether a book's text still reads as a saved, non-empty quotation.
pub fn same_quotation(text: &str, quotation: &str) -> bool {
    normalized(quotation).next().is_some() && normalized(text).eq(normalized(quotation))
}
pub fn created_now() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    let Ok(seconds) = libc::time_t::try_from(secs) else {
        return secs.to_string();
    };
    let mut time = std::mem::MaybeUninit::<libc::tm>::uninit();
    // gmtime_r initializes the structure on success and has no shared state.
    let time = unsafe {
        if libc::gmtime_r(&seconds, time.as_mut_ptr()).is_null() {
            return secs.to_string();
        }
        time.assume_init()
    };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
        time.tm_year + 1900,
        time.tm_mon + 1,
        time.tm_mday,
        time.tm_hour,
        time.tm_min,
        time.tm_sec
    )
}
/// Converts a stored UTC timestamp (`YYYY-MM-DD HH:MM:SS UTC`) to local
/// wall-clock time (`YYYY-MM-DD HH:MM:SS`). Anything else (KOReader's
/// zone-less timestamps, empty strings, unparsable values) is kept as is.
pub fn created_local(created: &str) -> String {
    crate::clock::parse_utc(created)
        .and_then(crate::clock::LocalDateTime::at)
        .map(crate::clock::LocalDateTime::timestamp)
        .unwrap_or_else(|| created.to_owned())
}
/// Current local time for export headers, falling back to UTC on failure.
pub fn exported_now() -> String {
    crate::clock::LocalDateTime::now().map(crate::clock::LocalDateTime::timestamp).unwrap_or_else(created_now)
}

/// The most characters a note may hold.
pub const NOTE_LIMIT: usize = 4000;
/// A note as stored: surrounding whitespace trimmed, blank means none.
pub fn clean_note(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}
impl Annotation {
    /// A bulk-imported page bookmark, holding only KOReader's position: the
    /// next resolution reads its opening words there, and removes it when
    /// the position isn't in the book.
    pub fn awaits_opening(&self) -> bool {
        self.kind == Kind::Bookmark && self.fingerprint.is_empty()
    }
    /// The same bookmark at its `opening` words, checked against the book
    /// with `fingerprint`. A new record (records are immutable); the import
    /// provenance is kept.
    pub fn placed(&self, opening: crate::document::Opening, fingerprint: &str) -> Self {
        Self {
            id: digest(format!("{}:placed", self.id).as_bytes()),
            range: opening.range,
            quotation: opening.text,
            fingerprint: fingerprint.to_owned(),
            ..self.clone()
        }
    }
    /// The same passage with a different note. Record files are immutable, so
    /// the edit is a new record with a new ID; everything else is kept,
    /// including the import provenance.
    pub fn with_note(&self, note: Option<String>) -> Self {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        let id = digest(format!("{}:{nanos}:{note:?}", self.id).as_bytes());
        Self { id, note, ..self.clone() }
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
fn manifest_path(store: &Store, book: &str) -> Result<PathBuf> {
    ensure!(valid_id(book), "invalid annotation book identity");
    Ok(store.annotation_dir().join(format!("book-{book}.json")))
}
/// The import identities a book's manifest holds (`Manifest::sources`),
/// without reading its records. Empty when it has no manifest.
pub fn sources(store: &Store, book: &str) -> Result<BTreeSet<String>> {
    let bytes = match std::fs::read(manifest_path(store, book)?) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(e) => return Err(e.into()),
    };
    let manifest: Manifest = serde_json::from_slice(&bytes).context("invalid annotation manifest")?;
    ensure!(manifest.version == 1, "unsupported annotation manifest version");
    Ok(manifest.sources)
}
impl Collection {
    pub fn load(store: &Store, book: &str) -> Result<Self> {
        Self::load_with_bytes(store, book).map(|(c, _)| c)
    }
    fn load_with_bytes(store: &Store, book: &str) -> Result<(Self, HashMap<String, Vec<u8>>)> {
        let path = manifest_path(store, book)?;
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok((
                    Self { manifest: Manifest { version: 1, ..Default::default() }, records: vec![] },
                    HashMap::new(),
                ));
            }
            Err(e) => return Err(e.into()),
        };
        let manifest: Manifest =
            serde_json::from_slice(&bytes).context("invalid annotation manifest (left untouched)")?;
        ensure!(manifest.version == 1, "unsupported annotation manifest version (left untouched)");
        let dir = store.annotation_dir();
        let mut records = Vec::with_capacity(manifest.records.len());
        let mut raw = HashMap::with_capacity(manifest.records.len());
        for id in &manifest.records {
            ensure!(valid_id(id), "invalid annotation ID");
            let file_bytes = std::fs::read(dir.join(format!("{id}.json")))?;
            let a: Annotation =
                serde_json::from_slice(&file_bytes).context("invalid annotation record (left untouched)")?;
            ensure!(
                a.version == 1 && a.id == *id && a.book == book,
                "unsupported or inconsistent annotation record (left untouched)"
            );
            raw.insert(id.clone(), file_bytes);
            records.push(a);
        }
        Ok((Self { manifest, records }, raw))
    }
    pub fn add(&mut self, annotation: Annotation) {
        if let Some(p) = &annotation.provenance {
            self.manifest.sources.insert(p.entry.clone());
        }
        self.manifest.records.push(annotation.id.clone());
        self.records.push(annotation);
    }
    /// Puts `new` where the record `old` was, keeping its place in reading
    /// and saved order. The old record file stays on disk, like a removed
    /// record's, and import identities stay in `sources`. False if `old` is
    /// not in the collection.
    pub fn replace(&mut self, old: &str, new: Annotation) -> bool {
        let Some(i) = self.records.iter().position(|a| a.id == old) else { return false };
        if let Some(p) = &new.provenance {
            self.manifest.sources.insert(p.entry.clone());
        }
        for id in self.manifest.records.iter_mut().filter(|id| *id == old) {
            *id = new.id.clone();
        }
        self.records[i] = new;
        true
    }
    pub fn remove(&mut self, id: &str) {
        self.manifest.records.retain(|i| i != id);
        self.records.retain(|a| a.id != id);
    }
    pub fn commit(&self, store: &Store, book: &str) -> Result<()> {
        // Validate the existing committed set before any mutation. Incompatible
        // records are recoverable errors, never silently replaced.
        let (_, disk_bytes) = Self::load_with_bytes(store, book)?;
        self.commit_records(store, book, &self.records, &disk_bytes)
    }
    /// Commits only `self.records[start..]` and swaps the manifest. Used by
    /// bulk import right after `Collection::load` already validated the
    /// existing on-disk set and no reader has the book open.
    pub(crate) fn commit_from(&self, store: &Store, book: &str, start: usize) -> Result<()> {
        self.commit_records(store, book, &self.records[start..], &HashMap::new())
    }
    fn commit_records(
        &self,
        store: &Store,
        book: &str,
        records: &[Annotation],
        disk_bytes: &HashMap<String, Vec<u8>>,
    ) -> Result<()> {
        let manifest = manifest_path(store, book)?;
        let dir = store.annotation_dir();
        let mut to_write: Vec<(String, Vec<u8>)> = Vec::new();
        let mut queued: HashMap<&str, usize> = HashMap::new();
        for a in records {
            ensure!(valid_id(&a.id), "invalid annotation ID");
            let bytes = serde_json::to_vec(a)?;
            if let Some(existing) = disk_bytes.get(&a.id) {
                ensure!(existing == &bytes, "annotation ID collision; existing record preserved");
                continue;
            }
            if let Some(&idx) = queued.get(a.id.as_str()) {
                ensure!(to_write[idx].1 == bytes, "annotation ID collision; existing record preserved");
                continue;
            }
            let name = format!("{}.json", a.id);
            match std::fs::read(dir.join(&name)) {
                Ok(existing) => {
                    ensure!(existing == bytes, "annotation ID collision; existing record preserved");
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    queued.insert(a.id.as_str(), to_write.len());
                    to_write.push((name, bytes));
                }
                Err(e) => return Err(e.into()),
            }
        }
        if let (_, Some((_, err))) = atomic_batch(&dir, &to_write) {
            return Err(err);
        }
        atomic(&manifest, &self.manifest)
    }
}

/// Disjoint horizontal bands for the union of all visible annotation areas.
/// Per-entry rectangles remain available for tapping overlapping entries.
pub fn paint_union(rectangles: impl Iterator<Item = crate::geom::Rect>) -> Vec<crate::geom::Rect> {
    use std::collections::BTreeMap;
    let mut events = BTreeMap::<i32, Vec<(i32, i32, i32)>>::new();
    for r in rectangles.filter(|r| r.w > 0 && r.h > 0) {
        events.entry(r.y).or_default().push((r.x, r.right(), 1));
        events.entry(r.bottom()).or_default().push((r.x, r.right(), -1));
    }
    let mut active = BTreeMap::<(i32, i32), i32>::new();
    let mut result = Vec::new();
    let mut last = 0;
    for (y, changes) in events {
        if y > last {
            let mut span: Option<(i32, i32)> = None;
            for &(left, right) in active.keys() {
                if let Some((x, end)) = span {
                    if left <= end {
                        span = Some((x, end.max(right)));
                    } else {
                        result.push(crate::geom::Rect::new(x, last, end - x, y - last));
                        span = Some((left, right));
                    }
                } else {
                    span = Some((left, right));
                }
            }
            if let Some((x, end)) = span {
                result.push(crate::geom::Rect::new(x, last, end - x, y - last));
            }
        }
        for (left, right, delta) in changes {
            let n = active.entry((left, right)).or_default();
            *n += delta;
            if *n == 0 {
                active.remove(&(left, right));
            }
        }
        last = y;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Position;
    pub(super) fn record(id: &str) -> Annotation {
        Annotation {
            version: 1,
            kind: crate::annotations::Kind::Highlight,
            id: id.into(),
            book: "book".into(),
            fingerprint: "content".into(),
            range: TextRange { start: Position("a".into()), end: Position("b".into()) },
            quotation: "a quote".into(),
            created: "2026-11-15".into(),
            chapter: "Chapter".into(),
            note: Some("note".into()),
            provenance: Some(Provenance { source: "sample".into(), entry: id.into() }),
        }
    }

    #[test]
    fn painted_union_covers_each_pixel_once_even_for_dense_nested_ranges() {
        use crate::geom::Rect;
        let source: Vec<_> = (0..1000).map(|i| Rect::new((i % 17) - 2, (i % 13) - 2, 3 + i % 9, 2 + i % 11)).collect();
        let union = paint_union(source.iter().copied());
        for y in 0..30 {
            for x in 0..30 {
                let point = crate::geom::Point::new(x, y);
                let count = union.iter().filter(|r| r.contains(point)).count();
                assert_eq!(count, usize::from(source.iter().any(|r| r.contains(point))));
            }
        }
    }
    #[test]
    fn atomic_batches_removal_tombstones_and_failed_retry() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-annotation-{}", std::process::id()));
        let store = Store::at(root.clone());
        let mut c = Collection::load(&store, "book")?;
        c.add(record("first"));
        c.commit(&store, "book")?;
        std::fs::create_dir(store.annotation_dir().join("book-book.json.tmp"))?;
        c.add(record("second"));
        c.add(record("third"));
        assert!(c.commit(&store, "book").is_err());
        assert_eq!(Collection::load(&store, "book")?.records.len(), 1, "uncommitted files invisible");
        std::fs::remove_dir(store.annotation_dir().join("book-book.json.tmp"))?;
        c.commit(&store, "book")?;
        assert_eq!(Collection::load(&store, "book")?.records.len(), 3);
        c.remove("second");
        c.commit(&store, "book")?;
        let restored = Collection::load(&store, "book")?;
        assert_eq!(restored.records.len(), 2);
        assert!(restored.manifest.sources.contains("second"));
        let path = store.annotation_dir().join("first.json");
        let valid = std::fs::read(&path)?;
        for bad in [b"{broken".as_slice(), br#"{"version":99}"#] {
            std::fs::write(&path, bad)?;
            assert!(Collection::load(&store, "book").is_err());
            assert!(c.commit(&store, "book").is_err());
            assert_eq!(std::fs::read(&path)?, bad);
        }
        std::fs::write(&path, valid)?;
        let mut unsupported = record("first");
        unsupported.version = 99;
        std::fs::write(&path, serde_json::to_vec(&unsupported)?)?;
        assert!(Collection::load(&store, "book").is_err());
        assert!(Collection::load(&store, "../escape").is_err());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn commit_batches_new_records_rejects_byte_mismatches_and_keeps_single_record_path() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::at(dir.path().to_path_buf());
        let tmp_files = || -> Vec<String> {
            std::fs::read_dir(store.annotation_dir())
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|name| name.ends_with(".tmp"))
                .collect()
        };

        // Single new record uses the single-file sync_all() path, not a batch sync.
        let before = crate::store::batch_sync_count();
        let mut c = Collection::load(&store, "book")?;
        c.add(record("one"));
        c.commit(&store, "book")?;
        assert_eq!(crate::store::batch_sync_count(), before, "single record avoids batch sync");

        // Multiple new records commit in one batch; retry after failed manifest swap
        // accepts identical on-disk records without re-running a batch sync.
        std::fs::create_dir(store.annotation_dir().join("book-book.json.tmp"))?;
        c.add(record("two"));
        c.add(record("three"));
        assert!(c.commit(&store, "book").is_err());
        assert_eq!(crate::store::batch_sync_count(), before + 1);
        assert!(tmp_files().iter().all(|name| name == "book-book.json.tmp"));
        std::fs::remove_dir(store.annotation_dir().join("book-book.json.tmp"))?;
        c.commit(&store, "book")?;
        assert_eq!(crate::store::batch_sync_count(), before + 1, "retry found identical files already on disk");
        assert_eq!(Collection::load(&store, "book")?.records.len(), 3);

        // Existing committed record mutated in memory is rejected by byte comparison
        // before any sibling new record is written.
        let mut mutated = Collection::load(&store, "book")?;
        mutated.records[0].quotation = "altered".into();
        mutated.add(record("never-written"));
        let err = mutated.commit(&store, "book").unwrap_err();
        assert!(err.to_string().contains("annotation ID collision; existing record preserved"), "{err:#}");
        assert!(!store.annotation_dir().join("never-written.json").exists());

        // Bulk path (`commit_from`) also rejects colliding unreferenced on-disk records
        // without touching the existing file or writing sibling records, and batches >1 new records.
        let mut loaded = Collection::load(&store, "book")?;
        let start = loaded.records.len();
        let orphan_path = store.annotation_dir().join("orphan.json");
        std::fs::write(&orphan_path, b"{\"existing\":true}")?;
        loaded.add(record("sibling"));
        loaded.add(record("orphan"));
        let err = loaded.commit_from(&store, "book", start).unwrap_err();
        assert!(err.to_string().contains("annotation ID collision; existing record preserved"), "{err:#}");
        assert_eq!(std::fs::read(&orphan_path)?, b"{\"existing\":true}");
        assert!(!store.annotation_dir().join("sibling.json").exists());

        // After removing the conflicting orphan, commit_from batches both new records.
        std::fs::remove_file(&orphan_path)?;
        let before = crate::store::batch_sync_count();
        loaded.commit_from(&store, "book", start)?;
        assert_eq!(crate::store::batch_sync_count(), before + 1);
        assert_eq!(Collection::load(&store, "book")?.records.len(), 5);

        // A mid-batch .tmp failure cleans up earlier staged .tmp files and retries cleanly.
        let mut loaded = Collection::load(&store, "book")?;
        let start = loaded.records.len();
        loaded.add(record("four"));
        loaded.add(record("five"));
        let blocked = store.annotation_dir().join("five.json.tmp");
        std::fs::create_dir(&blocked)?;
        assert!(loaded.commit_from(&store, "book", start).is_err());
        assert!(!store.annotation_dir().join("four.json.tmp").exists());
        assert!(!store.annotation_dir().join("four.json").exists());
        assert_eq!(Collection::load(&store, "book")?.records.len(), 5);
        std::fs::remove_dir(&blocked)?;
        loaded.commit_from(&store, "book", start)?;
        assert_eq!(Collection::load(&store, "book")?.records.len(), 7);
        assert!(tmp_files().is_empty());
        Ok(())
    }
    #[test]
    fn replacing_a_note_keeps_position_and_sources_and_commits_a_new_record() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-annotation-replace-{}", std::process::id()));
        let store = Store::at(root.clone());
        let mut c = Collection::load(&store, "book")?;
        for id in ["first", "second", "third"] {
            c.add(record(id));
        }
        c.commit(&store, "book")?;
        let edited = c.records[1].with_note(clean_note("  new words "));
        assert_ne!(edited.id, "second");
        assert_eq!(edited.note.as_deref(), Some("new words"));
        assert!(c.replace("second", edited.clone()));
        assert!(!c.replace("missing", record("x")));
        assert_eq!(c.manifest.records, ["first", edited.id.as_str(), "third"]);
        assert_eq!(c.records[1].id, edited.id);
        assert!(c.manifest.sources.contains("second"), "import identities stay");
        c.commit(&store, "book")?;
        let restored = Collection::load(&store, "book")?;
        assert_eq!(restored.records[1].note.as_deref(), Some("new words"));
        assert_eq!(restored.records[1].quotation, "a quote");
        assert!(store.annotation_dir().join("second.json").exists(), "the old file is kept");
        let cleared = restored.records[1].with_note(clean_note(" \n "));
        assert!(cleared.note.is_none());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn highlight_records_keep_their_bytes_and_bookmarks_round_trip() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-annotation-kind-{}", std::process::id()));
        let store = Store::at(root.clone());
        // A highlight written before kinds existed, byte for byte.
        let old = br#"{"version":1,"id":"first","book":"book","fingerprint":"content","range":{"start":"a","end":"b"},"quotation":"a quote","created":"2026-11-15","chapter":"Chapter","note":"note","provenance":{"source":"sample","entry":"first"}}"#;
        std::fs::create_dir_all(store.annotation_dir())?;
        std::fs::write(store.annotation_dir().join("first.json"), old)?;
        let manifest = Manifest { version: 1, records: vec!["first".into()], sources: BTreeSet::new() };
        atomic(&store.annotation_dir().join("book-book.json"), &manifest)?;
        let mut c = Collection::load(&store, "book")?;
        assert_eq!(c.records[0].kind, Kind::Highlight);
        assert_eq!(serde_json::to_vec(&c.records[0])?, old, "a highlight serializes without a kind");
        let mut bookmark = record("mark");
        bookmark.kind = Kind::Bookmark;
        bookmark.range.end = crate::document::Position(String::new());
        bookmark.quotation.clear();
        c.add(bookmark);
        c.commit(&store, "book")?;
        assert_eq!(std::fs::read(store.annotation_dir().join("first.json"))?, old, "existing file untouched");
        assert!(
            String::from_utf8(std::fs::read(store.annotation_dir().join("mark.json"))?)?
                .contains(r#""kind":"bookmark""#)
        );
        let restored = Collection::load(&store, "book")?;
        assert_eq!(restored.records.iter().map(|a| a.kind).collect::<Vec<_>>(), [Kind::Highlight, Kind::Bookmark]);
        assert!(restored.records[1].quotation.is_empty() && restored.records[1].range.end.0.is_empty());
        restored.commit(&store, "book")?;
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn quote_validation_only_folds_whitespace_and_soft_hyphens() {
        assert!(!same_quotation("Hello!", "hello!"));
        assert!(!same_quotation("Hello!", "Hello."));
        assert_eq!(normalized(" \u{ad} a \u{ad}\u{ad} b\u{ad} ").collect::<String>(), "a b");
        assert!(same_quotation("Hello\n  wo\u{ad}rld!", "Hello world!"));
        assert!(!same_quotation("Hello world", "Hello world!"));
        assert!(!same_quotation("", " \u{ad} "));
    }
    #[test]
    fn created_local_converts_utc_and_leaves_everything_else() {
        let local = |unix| crate::clock::LocalDateTime::at(unix).unwrap().timestamp();
        for (input, expected) in [
            ("2000-02-29 23:59:59 UTC", local(951_868_799)),
            ("2026-11-14 23:08:05 UTC", local(1_794_697_685)),
            ("2026-11-20 08:00:00", "2026-11-20 08:00:00".into()),
            ("2026-11-15", "2026-11-15".into()),
            ("", String::new()),
            ("not a date UTC", "not a date UTC".into()),
            ("2026-02-29 12:00:00 UTC", "2026-02-29 12:00:00 UTC".into()),
            ("2026-02-31 12:00:00 UTC", "2026-02-31 12:00:00 UTC".into()),
            ("2026-04-31 12:00:00 UTC", "2026-04-31 12:00:00 UTC".into()),
            ("2026-11-15 24:00:00 UTC", "2026-11-15 24:00:00 UTC".into()),
            ("+026-11-15 12:00:00 UTC", "+026-11-15 12:00:00 UTC".into()),
        ] {
            assert_eq!(created_local(input), expected, "{input}");
        }
    }
}
