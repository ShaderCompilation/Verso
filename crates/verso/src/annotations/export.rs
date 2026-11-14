//! One book's highlights and notes as UTF-8 Markdown, written beneath the app
//! data directory. Exporting reads a snapshot and never changes annotations.
use super::Annotation;
use crate::store::{Store, atomic_bytes};
use anyhow::{Context, Result, ensure};
use std::path::PathBuf;

/// What the I/O thread needs to write one book's export. `records` are in
/// reading order (unresolved entries last, in saved order).
#[derive(Clone, Debug)]
pub struct Export {
    pub title: String,
    pub author: String,
    pub records: Vec<Annotation>,
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
    out.push_str(&format!(
        "*{} · {} · exported from Verso on {exported}*\n",
        count(export.records.len(), "highlight", "highlights"),
        count(notes, "note", "notes"),
    ));
    let mut chapter: Option<String> = None;
    for a in &export.records {
        let c = one_line(&a.chapter);
        if chapter.as_ref() != Some(&c) {
            out.push_str(&format!("\n## {}\n", if c.is_empty() { "Untitled chapter" } else { &c }));
            chapter = Some(c);
        }
        out.push('\n');
        for line in a.quotation.trim().lines() {
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

/// A stable, FAT-safe file name: the title plus a short book identity, so the
/// same book overwrites its previous export and two same-titled books don't.
pub fn file_name(title: &str, book: &str) -> String {
    let mut name: String =
        one_line(title).chars().map(|c| if c.is_control() || r#"\/:*?"<>|"#.contains(c) { '_' } else { c }).collect();
    name = name.trim_matches(|c: char| c == '.' || c.is_whitespace()).to_owned();
    if let Some((i, _)) = name.char_indices().nth(80) {
        name.truncate(i);
        name = name.trim_end().to_owned();
    }
    if name.is_empty() {
        name = "Untitled".into();
    }
    let id: String = book.chars().filter(char::is_ascii_alphanumeric).take(8).collect();
    format!("{name} ({id}).md")
}

/// Writes atomically (temp file, fsync, rename) and returns the destination.
pub fn write(store: &Store, book: &str, export: &Export) -> Result<PathBuf> {
    ensure!(!export.records.is_empty(), "this book has no highlights to export");
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
            title: "Война и мир".into(), author: "Лев Толстой".into(), records: vec![first, second, third]
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
    }
    #[test]
    fn file_names_are_fat_safe_stable_and_distinct() {
        assert_eq!(file_name("A: B/C?", "0123456789abcdef"), "A_ B_C_ (01234567).md");
        assert_eq!(file_name(" .. ", "x"), "Untitled (x).md");
        assert_ne!(file_name("Same", "aaaaaaaa"), file_name("Same", "bbbbbbbb"));
        let long = file_name(&"Ж".repeat(200), "id");
        assert_eq!(long.chars().count(), 80 + " (id).md".len());
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
