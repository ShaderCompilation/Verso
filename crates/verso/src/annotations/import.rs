//! KOReader's highlights, notes and page bookmarks as Verso records, shared by
//! the per-book import (Reader Menu → Notes, resolved against the open book)
//! and the bulk import (`koreader::bulk`, every book, none opened). Both name
//! an entry by the same digest of the book's annotation identity and the
//! entry, so neither imports what the other already did, and a removed import
//! stays removed (its identity stays in the manifest's `sources`).
use super::{Annotation, Kind, Provenance, digest, lua};
use crate::document::{Position, TextRange};
use anyhow::{Context, Result, ensure};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// What one KOReader file holds for one book, new to Verso.
#[derive(Clone, Default, Debug)]
pub struct Preview {
    pub records: Vec<Annotation>,
    /// Entries imported before (or removed after importing).
    pub duplicates: usize,
    /// PDF entries and entries missing a position or text.
    pub unsupported: usize,
}

/// The per-book import: reads a whole metadata file, which must have
/// KOReader's modern `annotations` table.
pub fn read(
    book: &str,
    fingerprint: &str,
    path: &Path,
    known: &BTreeSet<String>,
    cancelled: &dyn Fn() -> bool,
) -> Result<Preview> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    let data = lua::parse_cancellable(&bytes, cancelled)?;
    let table = data
        .table()?
        .get("annotations")
        .context("unsupported legacy KOReader format: modern annotations table required")?
        .table()?;
    records(book, fingerprint, &path.display().to_string(), table, known, cancelled)
}

/// Builds the records of KOReader's `annotations` table for the book with
/// annotation identity `book`, skipping the identities in `known`.
/// `fingerprint` is the book's content fingerprint, or empty when the book
/// wasn't read (the bulk import): a page bookmark then waits for its opening
/// words until the book next opens (`Annotation::awaits_opening`).
pub fn records(
    book: &str,
    fingerprint: &str,
    source: &str,
    table: &BTreeMap<String, lua::Value>,
    known: &BTreeSet<String>,
    cancelled: &dyn Fn() -> bool,
) -> Result<Preview> {
    let mut result = Preview::default();
    let mut seen = known.clone();
    for value in table.values() {
        ensure!(!cancelled(), "import cancelled");
        let Ok(entry) = value.table() else {
            result.unsupported += 1;
            continue;
        };
        let string = |key| entry.get(key).and_then(lua::Value::string).unwrap_or("").to_owned();
        // KOReader's own test (ReaderBookmark:getBookmarkType): an entry with
        // a drawer is a highlight or note, one without is a page bookmark.
        // A bookmark's `text` is KOReader's generated "in <chapter>", so it is
        // ignored; its opening words are read at its position from the book
        // (the quotation stays empty until then). A PDF bookmark's numeric
        // page stays unsupported.
        let kind = if entry.contains_key("drawer") { Kind::Highlight } else { Kind::Bookmark };
        let (start, end, quotation) = match kind {
            Kind::Highlight => (string("pos0"), string("pos1"), string("text")),
            Kind::Bookmark => (string("page"), String::new(), String::new()),
        };
        let complete = match kind {
            Kind::Highlight => !start.is_empty() && !end.is_empty() && !quotation.is_empty(),
            Kind::Bookmark => !start.is_empty(),
        };
        if !complete {
            result.unsupported += 1;
            continue;
        }
        // Stable semantic source identity; formatting, path and row order don't
        // affect it. Changed source entries produce new independent copies.
        let identity = digest(&serde_json::to_vec(&(book, entry))?);
        if !seen.insert(identity.clone()) {
            result.duplicates += 1;
            continue;
        }
        let note = string("note");
        result.records.push(Annotation {
            version: 1,
            kind,
            id: format!("import-{identity}"),
            book: book.to_owned(),
            fingerprint: fingerprint.to_owned(),
            range: TextRange { start: Position(start), end: Position(end) },
            quotation,
            created: string("datetime"),
            chapter: string("chapter"),
            note: (!note.is_empty()).then_some(note),
            provenance: Some(Provenance { source: source.to_owned(), entry: identity }),
        });
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::Book;
    fn preview(book: &Book, path: &Path, known: &BTreeSet<String>) -> Result<Preview> {
        read(&book.annotation_id, &book.fingerprint, path, known, &|| false)
    }
    #[test]
    fn imports_page_bookmarks_and_reimports_only_them_after_a_highlights_only_import() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-import-bookmarks-{}", std::process::id()));
        std::fs::create_dir_all(&root)?;
        let path = root.join("metadata.epub.lua");
        let mut book = crate::library::selection_tests::book(1);
        book.annotation_id = "book".into();
        book.fingerprint = "content".into();
        let highlight =
            "{ drawer='lighten',pos0='/body/p/text().0',pos1='/body/p/text().9',text='Quote',chapter='One' }";
        let bookmark = "{ page='/body/DocFragment[2]/body/p[3]/text().0',text='in Two',chapter='Two',datetime='2026-11-20 08:00:00',note='Come back',pageno=12 }";
        // What an earlier import saw and kept: the highlight only.
        std::fs::write(&path, format!("return {{ annotations = {{ {highlight} }} }}"))?;
        let earlier = preview(&book, &path, &BTreeSet::new())?;
        let known = BTreeSet::from([earlier.records[0].provenance.as_ref().unwrap().entry.clone()]);
        std::fs::write(&path, format!("return {{ annotations = {{ {highlight}, {bookmark} }} }}"))?;
        let again = preview(&book, &path, &known)?;
        assert_eq!((again.records.len(), again.duplicates, again.unsupported), (1, 1, 0));
        let b = &again.records[0];
        assert_eq!(b.kind, Kind::Bookmark);
        assert_eq!(b.range.start.0, "/body/DocFragment[2]/body/p[3]/text().0");
        assert!(b.range.end.0.is_empty() && b.quotation.is_empty(), "opening words come from the book thread");
        assert_eq!((b.chapter.as_str(), b.created.as_str()), ("Two", "2026-11-20 08:00:00"));
        assert_eq!(b.note.as_deref(), Some("Come back"));
        assert_eq!(b.fingerprint, "content");
        assert!(b.id.starts_with("import-"));
        // Importing the same file again finds nothing new.
        let mut all = known.clone();
        all.insert(b.provenance.as_ref().unwrap().entry.clone());
        assert_eq!(preview(&book, &path, &all)?.duplicates, 2);
        // A bookmark without a position is unsupported.
        std::fs::write(&path, "return { annotations = { { text='in Two' } } }")?;
        assert_eq!(preview(&book, &path, &BTreeSet::new())?.unsupported, 1);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn imports_unicode_notes_duplicates_changes_and_legacy() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-import-{}", std::process::id()));
        std::fs::create_dir_all(&root)?;
        let path = root.join("metadata.epub.lua");
        let mut book = crate::library::selection_tests::book(1);
        book.annotation_id = "book".into();
        let data = "return { annotations = { [1] = { drawer='lighten',pos0='/body/p/text().0',pos1='/body/p/text().9',text='Привет!',note='My \\\"note\\\"',datetime='2026-11-15 12:00:00',chapter='Chapter',text_edited=true }, [2]={drawer='lighten',pos0={x=1},pos1={x=2},text='PDF'},[3]={page=12,text='in PDF'} } }";
        std::fs::write(&path, data)?;
        let first = preview(&book, &path, &BTreeSet::new())?;
        assert_eq!(first.records.len(), 1);
        assert_eq!(first.unsupported, 2, "PDF highlights and PDF bookmarks");
        assert_eq!(first.records[0].quotation, "Привет!");
        assert_eq!(first.records[0].note.as_deref(), Some("My \"note\""));
        let known = BTreeSet::from([first.records[0].provenance.as_ref().unwrap().entry.clone()]);
        assert_eq!(preview(&book, &path, &known)?.duplicates, 1);
        std::fs::write(&path, data.replace("Привет!", "Привет?"))?;
        assert_eq!(preview(&book, &path, &known)?.records.len(), 1);
        std::fs::write(&path, "return {highlight={}}")?;
        assert!(preview(&book, &path, &known).unwrap_err().to_string().contains("legacy"));
        // Optional local-only sample copied read-only from the installed version;
        // no personal quotations become repository fixtures.
        if let Ok(sample) = std::fs::read("/tmp/verso-koreader-sample.lua") {
            lua::parse(&sample)?;
        }
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
