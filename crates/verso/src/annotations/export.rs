//! One book's highlights, page bookmarks and notes as UTF-8 Markdown, written beneath the app
//! data directory. Exporting reads a snapshot and never changes annotations.
use super::{Annotation, Kind};
use crate::store::{Store, atomic_bytes};
use anyhow::{Context, Result, ensure};
use std::path::PathBuf;

/// What the I/O thread needs to write one book's export. `records` are in
/// reading order (unresolved entries last, in saved order); `pages` holds
/// each one's page in the current layout, None when unresolved.
#[derive(Clone, Debug)]
pub struct Export {
    pub title: String,
    pub author: String,
    pub records: Vec<Annotation>,
    pub pages: Vec<Option<usize>>,
}

pub fn markdown(export: &Export, exported: &str) -> String {
    let mut out = String::new();
    let title = one_line(&export.title);
    out.push_str(&format!("# {}\n\n", if title.is_empty() { "Untitled" } else { &title }));
    let author = one_line(&export.author);
    if !author.is_empty() {
        out.push_str(&format!("{author}\n\n"));
    }
    let notes = export.records.iter().filter(|a| a.note.as_deref().is_some_and(|n| !n.trim().is_empty())).count();
    let bookmarks = export.records.iter().filter(|a| a.kind == Kind::Bookmark).count();
    let mut counts = vec![count(export.records.len() - bookmarks, "highlight", "highlights")];
    if bookmarks > 0 {
        counts.push(count(bookmarks, "bookmark", "bookmarks"));
    }
    counts.push(count(notes, "note", "notes"));
    out.push_str(&format!("*{} · exported from Verso on {exported}*\n", counts.join(" · ")));
    let mut chapter: Option<String> = None;
    for (i, a) in export.records.iter().enumerate() {
        let c = one_line(&a.chapter);
        if chapter.as_ref() != Some(&c) {
            out.push_str(&format!("\n## {}\n", if c.is_empty() { "Untitled chapter" } else { &c }));
            chapter = Some(c);
        }
        out.push('\n');
        if a.kind == Kind::Bookmark {
            // One line: the page in the current layout (none when unresolved)
            // and the opening words, or nothing more for a page without text.
            out.push_str("**Bookmark**");
            if let Some(page) = export.pages.get(i).copied().flatten() {
                out.push_str(&format!(" · p. {}", page + 1));
            }
            let words = one_line(&a.quotation);
            if !words.is_empty() {
                out.push_str(&format!(" — \"{words}…\""));
            }
            out.push('\n');
        }
        for line in a.quotation.trim().lines().filter(|_| a.kind == Kind::Highlight) {
            let line = line.trim_end();
            out.push_str(if line.is_empty() { ">\n" } else { "> " });
            if !line.is_empty() {
                out.push_str(line);
                out.push('\n');
            }
        }
        if let Some(note) = a.note.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            out.push_str(&format!("\n**Note:** {note}\n"));
        }
        if !a.created.is_empty() {
            out.push_str(&format!("\n*{}*\n", a.created));
        }
    }
    out
}

/// A stable, FAT-safe file name: the title plus the full book identity, so the
/// same book overwrites its previous export and two same-titled books don't.
pub fn file_name(title: &str, book: &str) -> String {
    let mut name: String =
        one_line(title).chars().map(|c| if c.is_control() || r#"\/:*?"<>|"#.contains(c) { '_' } else { c }).collect();
    name = name.trim_matches(|c: char| c == '.' || c.is_whitespace()).to_owned();
    if let Some((i, _)) = name.char_indices().nth(80) {
        name.truncate(i);
        name = name.trim_end().to_owned();
    }
    // Duplicate copies share the first 16 hex digits; their path-derived
    // disambiguator follows a dash. Keep it and any annotation-id suffix.
    let id: String = book.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    let suffix = format!(" ({id}).md");
    // FAT filenames are limited to 255 bytes, including the identity suffix.
    while name.len() > 255usize.saturating_sub(suffix.len()) {
        name.pop();
    }
    name = name.trim_end().to_owned();
    if name.is_empty() {
        name = "Untitled".into();
    }
    format!("{name}{suffix}")
}

/// Writes atomically (temp file, fsync, rename) and returns the destination.
pub fn write(store: &Store, book: &str, export: &Export) -> Result<PathBuf> {
    ensure!(!export.records.is_empty(), "this book has no highlights or bookmarks to export");
    let path = store.export_dir().join(file_name(&export.title, book));
    atomic_bytes(&path, markdown(export, &super::created_now()).as_bytes())
        .with_context(|| format!("couldn't write {}", path.display()))?;
    Ok(path)
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn export() -> Export {
        let mut first = super::super::tests::record("first");
        first.quotation = "Первая строка\n\nsecond paragraph".into();
        first.chapter = "Глава 1".into();
        first.note = Some("  My *note*  ".into());
        let mut second = super::super::tests::record("second");
        second.note = None;
        second.chapter = "Глава 1".into();
        let mut third = super::super::tests::record("third");
        third.chapter = String::new();
        third.note = Some(" ".into());
        Export {
            title: "Война и мир".into(),
            author: "Лев Толстой".into(),
            records: vec![first, second, third],
            pages: vec![Some(2), Some(4), None],
        }
    }
    #[test]
    fn markdown_has_title_author_chapters_quotes_and_notes() {
        let md = markdown(&export(), "2026-11-15 12:00:00 UTC");
        assert!(md.starts_with("# Война и мир\n\nЛев Толстой\n\n*3 highlights · 1 note · exported"));
        assert_eq!(md.matches("## Глава 1").count(), 1, "consecutive entries share a chapter heading");
        assert!(md.contains("> Первая строка\n>\n> second paragraph\n"));
        assert!(md.contains("**Note:** My *note*\n"));
        assert!(md.contains("## Untitled chapter"));
        assert_eq!(md.matches("**Note:**").count(), 1, "blank notes are left out");
        assert!(!md.contains("bookmark") && !md.contains("p. "), "highlights alone read as before");
    }
    #[test]
    fn bookmarks_are_listed_in_reading_order_with_page_opening_words_and_note() {
        let mut e = export();
        let mut mark = super::super::tests::record("mark");
        mark.kind = Kind::Bookmark;
        mark.chapter = "Глава 1".into();
        mark.quotation = "It was a bright\ncold day".into();
        mark.note = Some("Come back".into());
        let mut blank = super::super::tests::record("blank");
        blank.kind = Kind::Bookmark;
        blank.chapter = "Глава 1".into();
        blank.quotation.clear();
        blank.note = None;
        let mut lost = blank.clone();
        lost.quotation = "Gone".into();
        lost.chapter = String::new();
        e.records.insert(1, mark);
        e.records.insert(2, blank);
        e.records.push(lost);
        e.pages = vec![Some(2), Some(213), Some(214), Some(4), None, None];
        let md = markdown(&e, "2026-11-22 12:00:00 UTC");
        assert!(md.contains("*3 highlights · 3 bookmarks · 2 notes · exported"), "{md}");
        assert_eq!(md.matches("## Глава 1").count(), 1, "bookmarks share the chapter heading");
        assert!(
            md.contains("\n**Bookmark** · p. 214 — \"It was a bright cold day…\"\n\n**Note:** Come back\n"),
            "{md}"
        );
        assert!(md.contains("\n**Bookmark** · p. 215\n"), "a page without text");
        assert!(md.contains("\n**Bookmark** — \"Gone…\"\n"), "an unresolved bookmark has no page");
        let first = md.find("Первая строка").unwrap();
        let mark = md.find("p. 214").unwrap();
        let second = md.find("> a quote").unwrap();
        assert!(first < mark && mark < second, "reading order is kept");
        assert!(!md.contains("> It was"), "a bookmark isn't quoted as a highlight");
    }
    #[test]
    fn file_names_are_fat_safe_stable_and_distinct() {
        assert_eq!(file_name("A: B/C?", "0123456789abcdef"), "A_ B_C_ (0123456789abcdef).md");
        assert_eq!(file_name(" .. ", "x"), "Untitled (x).md");
        assert_ne!(file_name("Same", "aaaaaaaa"), file_name("Same", "bbbbbbbb"));
        let copy_a = "0123456789abcdef-1111111111111111";
        let copy_b = "0123456789abcdef-2222222222222222";
        assert_ne!(file_name("Same", copy_a), file_name("Same", copy_b));
        assert_ne!(file_name("Same", copy_a), file_name("Same", &format!("{copy_a}-n1")));
        let long = file_name(&"Ж".repeat(200), "id");
        assert_eq!(long.chars().count(), 80 + " (id).md".len());
        let long_cjk = file_name(&"漢".repeat(80), copy_a);
        assert!(long_cjk.len() <= 255);
    }
    #[test]
    fn duplicate_book_exports_keep_both_files() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-export-copies-{}", std::process::id()));
        let store = Store::at(root.clone());
        let first = export();
        let mut second = export();
        second.records[0].note = Some("from the second copy".into());
        let path_a = write(&store, "0123456789abcdef-1111111111111111", &first)?;
        let path_b = write(&store, "0123456789abcdef-2222222222222222", &second)?;
        assert_ne!(path_a, path_b);
        assert!(!std::fs::read_to_string(&path_a)?.contains("from the second copy"));
        assert!(std::fs::read_to_string(&path_b)?.contains("from the second copy"));
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn failed_export_reports_an_error_and_leaves_annotations_alone() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-export-{}", std::process::id()));
        let store = Store::at(root.clone());
        let mut c = super::super::Collection::load(&store, "book")?;
        c.add(super::super::tests::record("first"));
        c.commit(&store, "book")?;
        let before = std::fs::read(store.annotation_dir().join("book-book.json"))?;
        // A file where the export folder should be makes the write fail.
        std::fs::write(store.export_dir(), b"not a folder")?;
        assert!(write(&store, "book", &export()).is_err());
        assert_eq!(std::fs::read(store.annotation_dir().join("book-book.json"))?, before);
        std::fs::remove_file(store.export_dir())?;
        let path = write(&store, "book", &export())?;
        assert!(path.starts_with(store.export_dir()));
        assert!(std::fs::read_to_string(&path)?.contains("Лев Толстой"));
        assert!(write(&store, "book", &Export { records: vec![], ..export() }).is_err());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
