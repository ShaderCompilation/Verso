//! Versioned annotation records and an atomic per-book manifest. Immutable record
//! files become visible together when the manifest commits; tombstones retain
//! import identities after removal. No KOReader files are written.
pub mod lua;
pub mod worker;
use crate::{
    document::TextRange,
    store::{Store, atomic},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, path::PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Annotation {
    pub version: u32,
    pub id: String,
    pub book: String,
    pub fingerprint: String,
    pub range: TextRange,
    pub quotation: String,
    pub created: String,
    pub chapter: String,
    pub note: Option<String>,
    pub provenance: Option<Provenance>,
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
pub fn normalize(text: &str) -> String {
    text.replace('\u{ad}', "").split_whitespace().collect::<Vec<_>>().join(" ")
}
pub fn created_now() -> String {
    let seconds = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()
        as libc::time_t;
    let mut time = std::mem::MaybeUninit::<libc::tm>::uninit();
    // gmtime_r initializes the structure on success and has no shared state.
    let time = unsafe {
        if libc::gmtime_r(&seconds, time.as_mut_ptr()).is_null() {
            return seconds.to_string();
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

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
fn manifest_path(store: &Store, book: &str) -> Result<PathBuf> {
    ensure!(valid_id(book), "invalid annotation book identity");
    Ok(store.annotation_dir().join(format!("book-{book}.json")))
}
impl Collection {
    pub fn load(store: &Store, book: &str) -> Result<Self> {
        let path = manifest_path(store, book)?;
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self { manifest: Manifest { version: 1, ..Default::default() }, records: vec![] });
            }
            Err(e) => return Err(e.into()),
        };
        let manifest: Manifest =
            serde_json::from_slice(&bytes).context("invalid annotation manifest (left untouched)")?;
        ensure!(manifest.version == 1, "unsupported annotation manifest version (left untouched)");
        let mut records = Vec::new();
        for id in &manifest.records {
            ensure!(valid_id(id), "invalid annotation ID");
            let a: Annotation =
                serde_json::from_slice(&std::fs::read(store.annotation_dir().join(format!("{id}.json")))?)
                    .context("invalid annotation record (left untouched)")?;
            ensure!(
                a.version == 1 && a.id == *id && a.book == book,
                "unsupported or inconsistent annotation record (left untouched)"
            );
            records.push(a);
        }
        Ok(Self { manifest, records })
    }
    pub fn add(&mut self, annotation: Annotation) {
        if let Some(p) = &annotation.provenance {
            self.manifest.sources.insert(p.entry.clone());
        }
        self.manifest.records.push(annotation.id.clone());
        self.records.push(annotation);
    }
    pub fn remove(&mut self, id: &str) {
        self.manifest.records.retain(|i| i != id);
        self.records.retain(|a| a.id != id);
    }
    pub fn commit(&self, store: &Store, book: &str) -> Result<()> {
        // Validate the existing committed set before any mutation. Incompatible
        // records are recoverable errors, never silently replaced.
        Self::load(store, book)?;
        for a in &self.records {
            ensure!(valid_id(&a.id), "invalid annotation ID");
            let path = store.annotation_dir().join(format!("{}.json", a.id));
            if path.exists() {
                ensure!(
                    std::fs::read(&path)? == serde_json::to_vec(a)?,
                    "annotation ID collision; existing record preserved"
                );
            } else {
                atomic(&path, a)?;
            }
        }
        atomic(&manifest_path(store, book)?, &self.manifest)
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
    fn quote_validation_only_folds_whitespace_and_soft_hyphens() {
        assert_eq!(normalize("Hello\n  wo\u{ad}rld!"), "Hello world!");
        assert_ne!(normalize("Hello!"), normalize("hello!"));
        assert_ne!(normalize("Hello!"), normalize("Hello."));
    }
}
