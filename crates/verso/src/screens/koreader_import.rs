//! Settings → Library → KOReader → *Import* (progress and highlights): the
//! sheet that reads KOReader's data for the whole library, shows what it
//! found and imports it after the user confirms. The work runs on a
//! [`Job`] thread; nothing is written before *Import*. [`Flow`] is that
//! work without the sheet, which setup's From KOReader page also shows.
use crate::geom::Rect;
use crate::gfx::HAlign;
use crate::koreader::bulk::{Event, ImportPhase, Imported, Job, Preview, Scan, UNREADABLE_SHOWN};
use crate::platform::Waker;
use crate::store::Store;
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK2, Theme};
use crate::ui::widget::DrawCx;
use crate::ui::widgets::{Button, ButtonKind, Sheet};

enum Stage {
    /// Books read so far, of the books to read (0 of 0 until the list is in).
    Scanning(usize, usize),
    Preview(Box<Scan>),
    /// Writing books' states or highlights: phase, done so far, of the phase's total.
    Importing(ImportPhase, usize, usize),
    /// What was imported.
    Done(String),
    /// What went wrong.
    Failed(String),
}

/// The scan, the preview and the import, without the sheet: Settings'
/// sheet and setup's From KOReader page each drive one.
pub(crate) struct Flow {
    store: Store,
    waker: Waker,
    stage: Stage,
    job: Option<Job>,
}

impl Flow {
    /// Starts the scan. `open` is the book open in the reader: it's left out.
    pub(crate) fn scan(store: Store, waker: Waker, open: Option<String>) -> Self {
        let job = Job::scan(store.clone(), open, waker.clone());
        Self { store, waker, stage: Stage::Scanning(0, 0), job: Some(job) }
    }

    /// A preview with something to import.
    pub(crate) fn importable(&self) -> bool {
        matches!(&self.stage, Stage::Preview(scan) if scan.preview.importable())
    }

    pub(crate) fn importing(&self) -> bool {
        matches!(self.stage, Stage::Importing(..))
    }

    /// Finished: a preview, the import's result or an error.
    fn settled(&self) -> bool {
        matches!(self.stage, Stage::Preview(_) | Stage::Done(_) | Stage::Failed(_))
    }

    /// Imports what the preview shows.
    pub(crate) fn import(&mut self) {
        if !self.importable() {
            return;
        }
        let Stage::Preview(scan) = std::mem::replace(&mut self.stage, Stage::Importing(ImportPhase::Progress, 0, 0))
        else {
            return;
        };
        self.stage = if scan.preview.changes > 0 {
            Stage::Importing(ImportPhase::Progress, 0, scan.preview.changes)
        } else {
            Stage::Importing(ImportPhase::Highlights, 0, scan.preview.highlight_books)
        };
        self.job = Some(Job::import(self.store.clone(), *scan, self.waker.clone()));
    }

    /// Stops a scan; nothing was written.
    fn cancel(&mut self) {
        self.job = None;
    }

    fn apply(&mut self, event: Event) -> Option<String> {
        match event {
            Event::Scanning(done, total) => {
                self.stage = Stage::Scanning(done, total);
                None
            }
            Event::Scanned(scan) => {
                self.stage = Stage::Preview(scan);
                None
            }
            Event::Importing(phase, done, total) => {
                self.stage = Stage::Importing(phase, done, total);
                None
            }
            Event::Imported(result) => match &result.error {
                None => {
                    let text = imported_text(&result);
                    self.stage = Stage::Done(text.clone());
                    Some(text)
                }
                Some(e) => {
                    log::error!("KOReader import: {e:#}");
                    self.stage = Stage::Failed(format!(
                        "{}, then couldn't save: {e}.\n\nRun the import again to finish the rest.",
                        failed_text(&result),
                    ));
                    None
                }
            },
        }
    }

    /// Takes the job's events: whether the stage or the progress changed,
    /// and what was imported once the import finished.
    pub(crate) fn on_background(&mut self) -> (bool, Option<String>) {
        let Some(job) = self.job.as_ref() else { return (false, None) };
        let finished = job.finished();
        let events = job.events();
        let (mut changed, mut imported) = (false, None);
        for event in events {
            changed = true;
            if let Some(text) = self.apply(event) {
                imported = Some(text);
            }
        }
        if finished {
            self.job = None;
            // The thread ended without its result: it couldn't start.
            match self.stage {
                Stage::Scanning(..) => self.stage = Stage::Failed("Couldn't read KOReader data.".into()),
                Stage::Importing(..) => self.stage = Stage::Failed("Couldn't import from KOReader.".into()),
                _ => {}
            }
            changed = true;
        }
        (changed, imported)
    }

    /// The main text and the note under it.
    pub(crate) fn text(&self) -> (String, &'static str) {
        match &self.stage {
            Stage::Scanning(_, 0) => ("Reading KOReader data…".into(), ""),
            Stage::Scanning(done, total) => (format!("Reading KOReader data… {done} of {total}"), ""),
            Stage::Preview(scan) => preview_text(&scan.preview),
            Stage::Importing(phase, done, total) => {
                let what = match phase {
                    ImportPhase::Progress => "progress",
                    ImportPhase::Highlights => "highlights",
                };
                (format!("Importing {what}… {done} of {total}"), "")
            }
            Stage::Done(text) => (format!("{text}."), ""),
            Stage::Failed(message) => (message.clone(), ""),
        }
    }

    /// The scan or the import is running.
    pub(crate) fn busy(&self) -> bool {
        self.job.is_some()
    }

    #[cfg(test)]
    pub(crate) fn preview(&self) -> Option<&Preview> {
        match &self.stage {
            Stage::Preview(scan) => Some(&scan.preview),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Act {
    /// Cancel the scan, or leave the preview without importing.
    Cancel,
    Import,
    Close,
}

/// What Settings does after a touch or a job event.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Close the sheet.
    Close,
    /// Close it and say how many books were imported.
    Imported(String),
}

pub struct ImportSheet {
    sheet: Sheet,
    flow: Flow,
    /// The text area above the buttons.
    body: Rect,
    buttons: Vec<(Button, Act)>,
}

impl ImportSheet {
    /// Opens the sheet and starts the scan. `open` is the book open in the
    /// reader under Settings: it's left out.
    pub fn open(t: &Theme, screen: Rect, store: Store, waker: Waker, open: Option<String>) -> Self {
        // Below the top bar, or (when it would cut through the tabs) the full
        // height under it.
        let full = screen.h - t.top_bar;
        let height = if full - t.mm(80.0) < t.mm(19.0) { full } else { t.mm(80.0) };
        let sheet = Sheet::open("Import from KOReader", height, screen, t);
        let mut this = Self { sheet, flow: Flow::scan(store, waker, open), body: Rect::default(), buttons: Vec::new() };
        this.place(t);
        this
    }

    /// Lays out the buttons for the stage, and the text above them.
    fn place(&mut self, t: &Theme) {
        let acts: &[(&str, ButtonKind, Act)] = if self.flow.importable() {
            &[("Cancel", ButtonKind::Secondary, Act::Cancel), ("Import", ButtonKind::Primary, Act::Import)]
        } else if self.flow.importing() {
            &[]
        } else if self.flow.settled() {
            &[("OK", ButtonKind::Primary, Act::Close)]
        } else {
            &[("Cancel", ButtonKind::Secondary, Act::Cancel)]
        };
        let c = self.sheet.content(t);
        let y = c.bottom() - t.margin - t.button_h;
        let gap = t.mm(3.0);
        let n = acts.len().max(1) as i32;
        let w = (c.w - gap * (n - 1)) / n;
        self.buttons = acts
            .iter()
            .enumerate()
            .map(|(i, &(label, kind, act))| {
                let mut b = Button::new(label, kind);
                b.place(Rect::new(c.x + i as i32 * (w + gap), y, w, t.button_h), t);
                (b, act)
            })
            .collect();
        self.body = Rect { h: (y - c.y - t.mm(3.0)).max(0), ..c };
    }

    pub fn draw(&self, cx: &mut DrawCx) {
        self.sheet.draw(cx);
        let (text, note) = self.flow.text();
        draw_text(cx, &text, note, self.body);
        for (b, _) in &self.buttons {
            b.draw(cx);
        }
    }

    /// Touches go here first while the sheet is open. Nothing closes it
    /// while the import writes.
    pub fn on_gesture(&mut self, g: &Gesture, t: &Theme) -> Option<Outcome> {
        if self.flow.importing() {
            return None;
        }
        let act = if self.sheet.on_gesture(g) {
            Some(Act::Cancel)
        } else {
            self.buttons.iter_mut().find_map(|(b, act)| b.on_gesture(g).then_some(*act))
        };
        match act? {
            Act::Cancel | Act::Close => {
                self.flow.cancel();
                Some(Outcome::Close)
            }
            Act::Import => {
                self.flow.import();
                self.place(t);
                None
            }
        }
    }

    /// Takes the job's events. Some when the sheet should close; redraws
    /// when the stage or the progress changed (returns true in `changed`).
    pub fn on_background(&mut self, t: &Theme, changed: &mut bool) -> Option<Outcome> {
        let (moved, imported) = self.flow.on_background();
        *changed |= moved;
        if moved {
            self.place(t);
        }
        imported.map(Outcome::Imported)
    }

    #[cfg(test)]
    pub fn button(&self, label: &str) -> Option<Rect> {
        self.buttons.iter().find(|(b, _)| b.label == label).map(|(b, _)| b.rect())
    }

    /// The scan or the import is running (scripted screenshots wait).
    pub fn busy(&self) -> bool {
        self.flow.busy()
    }
}

/// A flow's text in `body`, with its note under it in small gray.
pub(crate) fn draw_text(cx: &mut DrawCx, text: &str, note: &str, b: Rect) {
    let t = cx.theme;
    // Paragraphs are centred in their rect: give each its own height.
    let (_, h) = cx.text.measure(text, t.body, Some(b.w as f32));
    let h = (h.ceil() as i32).min(b.h);
    cx.paragraph(text, t.body, Rect { h, ..b }, HAlign::Left);
    if !note.is_empty() {
        let y = b.y + h + t.mm(3.0);
        let (_, note_h) = cx.text.measure(note, t.small, Some(b.w as f32));
        let note_h = (note_h.ceil() as i32).min(b.bottom() - y).max(0);
        cx.paragraph(note, t.small.gray(INK2), Rect { y, h: note_h, ..b }, HAlign::Left);
    }
}

/// What a finished import says: "Imported KOReader progress for 3 books
/// and highlights for 2".
fn imported_text(i: &Imported) -> String {
    match (i.written, i.highlights_written) {
        (0, 0) => "Nothing imported from KOReader".into(),
        (p, 0) => format!("Imported KOReader progress for {}", counted(p, "book")),
        (0, h) => format!("Imported KOReader highlights for {}", counted(h, "book")),
        (p, h) => format!("Imported KOReader progress for {} and highlights for {h}", counted(p, "book")),
    }
}

/// What a stopped import got through: "Imported 3 of 5 books' progress".
fn failed_text(i: &Imported) -> String {
    let mut parts = Vec::new();
    if i.total > 0 {
        parts.push(format!("{} of {}' progress", i.written, counted(i.total, "book")));
    }
    if i.highlights_total > 0 {
        parts.push(format!("{} of {}' highlights", i.highlights_written, counted(i.highlights_total, "book")));
    }
    format!("Imported {}", parts.join(" and "))
}

/// The preview: what the import will do, and the note under it.
fn preview_text(p: &Preview) -> (String, &'static str) {
    let mut lines = Vec::new();
    if p.found() == 0 && p.open_skipped == 0 {
        if p.unreadable == 0 && p.highlight_books == 0 {
            return (
                "No KOReader progress or highlights found.".into(),
                "KOReader keeps each book's progress and highlights in a .sdr folder beside the book.",
            );
        }
    } else {
        lines.push(format!("KOReader progress found for {}:", counted(p.found() + p.open_skipped, "book")));
    }
    for (count, what) in [
        (p.not_started, "not started in Verso"),
        (p.newer_in_koreader, "newer in KOReader"),
        (p.newer_in_verso, "newer in Verso (kept)"),
        (p.up_to_date, "already up to date"),
        (p.open_skipped, "open in the reader (skipped)"),
        (p.finished, "marked finished"),
    ] {
        if count > 0 {
            lines.push(format!("    {count} {what}"));
        }
    }
    if p.highlight_books > 0 {
        let kinds: Vec<_> = [(p.highlights, "highlight"), (p.notes, "note"), (p.bookmarks, "bookmark")]
            .into_iter()
            .filter(|&(n, _)| n > 0)
            .map(|(n, noun)| counted(n, noun))
            .collect();
        lines.push(format!("{} with new highlights: {}", counted(p.highlight_books, "book"), kinds.join(", ")));
    }
    if p.unreadable > 0 {
        let names: Vec<_> =
            p.unreadable_books.iter().map(|b| b.file_name().unwrap_or_default().to_string_lossy()).collect();
        let more = p.unreadable - names.len().min(UNREADABLE_SHOWN);
        let more = if more > 0 { format!(" and {more} more") } else { String::new() };
        lines.push(format!("Couldn't read {}: {}{more}", counted(p.unreadable, "book"), names.join(", ")));
    }
    let note = if !p.importable() {
        "Nothing to import: Verso already has all of it."
    } else {
        "Imported one way; KOReader's files aren't changed."
    };
    (lines.join("\n"), note)
}

fn counted(count: usize, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}

#[cfg(test)]
fn fullest_preview() -> Preview {
    Preview {
        not_started: 118,
        newer_in_koreader: 15,
        newer_in_verso: 8,
        up_to_date: 3,
        finished: 23,
        complete_only: 2,
        open_skipped: 1,
        unreadable: 5,
        unreadable_books: (0..3).map(|i| format!("/mnt/us/documents/A long book title {i}.epub").into()).collect(),
        changes: 133,
        highlight_books: 37,
        highlights: 412,
        notes: 58,
        bookmarks: 20,
    }
}

/// The fullest preview's text and note (setup's page checks it fits).
#[cfg(test)]
pub(crate) fn fullest_text() -> (String, &'static str) {
    preview_text(&fullest_preview())
}

/// The preview with every line, for `screens/fit_tests.rs`.
#[cfg(test)]
pub(super) fn fit_samples(t: &Theme, screen: Rect, text: &mut crate::gfx::TextRenderer) -> Vec<crate::ui::fit::Parts> {
    let dir = std::env::temp_dir().join("verso-import-fit");
    let mut sheet = ImportSheet::open(t, screen, Store::at(dir), Waker::new(|| {}), None);
    sheet.flow.job = None;
    let mut scan = Scan::default();
    scan.preview = fullest_preview();
    sheet.flow.stage = Stage::Preview(Box::new(scan));
    sheet.place(t);
    let (main, note) = sheet.flow.text();
    let w = sheet.body.w;
    let needed =
        crate::ui::fit::text_h(text, &main, t.body, w) + t.mm(3.0) + crate::ui::fit::text_h(text, note, t.small, w);
    let panel = sheet.sheet.rect();
    assert!(panel.y >= t.top_bar, "ImportSheet at {}x{}: the top bar stays visible", screen.w, screen.h);
    let buttons = sheet.buttons.iter().map(|(b, _)| b.rect());
    vec![crate::ui::fit::Parts::new("ImportSheet", panel, buttons).body(sheet.body, needed)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_text_names_counts_and_unreadable_books() {
        let mut full = fullest_preview();
        let (text, note) = preview_text(&full);
        assert!(text.starts_with("KOReader progress found for 147 books:\n    118 not started in Verso\n"), "{text}");
        assert!(text.contains("A long book title 2.epub and 2 more"), "{text}");
        assert!(text.contains("37 books with new highlights: 412 highlights, 58 notes, 20 bookmarks"), "{text}");
        assert!(note.contains("one way"));
        full.changes = 0;
        assert!(preview_text(&full).1.contains("one way"), "highlights alone import");
        full.highlight_books = 0;
        assert!(preview_text(&full).1.starts_with("Nothing to import"));
        assert_eq!(preview_text(&Preview::default()).0, "No KOReader progress or highlights found.");
        let only = Preview { highlight_books: 1, notes: 1, ..Default::default() };
        assert_eq!(preview_text(&only).0, "1 book with new highlights: 1 note");
    }

    #[test]
    fn importing_progress_uses_phase_specific_wording_and_preview_denominators() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().to_path_buf());
        let mut scan = Scan::default();
        scan.preview = Preview { changes: 26, highlight_books: 22, ..Default::default() };
        let mut flow =
            Flow { store: store.clone(), waker: Waker::new(|| {}), stage: Stage::Preview(Box::new(scan)), job: None };

        // Starting import from progress + highlights begins in the progress phase with the preview's changes count.
        flow.import();
        assert!(flow.importing());
        assert_eq!(flow.text(), ("Importing progress… 0 of 26".into(), ""));
        flow.job = None;

        // Progress updates reflect each phase and its own denominator.
        assert_eq!(flow.apply(Event::Importing(ImportPhase::Progress, 12, 26)), None);
        assert_eq!(flow.text(), ("Importing progress… 12 of 26".into(), ""));
        assert_eq!(flow.apply(Event::Importing(ImportPhase::Highlights, 0, 22)), None);
        assert_eq!(flow.text(), ("Importing highlights… 0 of 22".into(), ""));
        assert_eq!(flow.apply(Event::Importing(ImportPhase::Highlights, 22, 22)), None);
        assert_eq!(flow.text(), ("Importing highlights… 22 of 22".into(), ""));

        let done = flow.apply(Event::Imported(Imported {
            written: 26,
            total: 26,
            highlights_written: 22,
            highlights_total: 22,
            error: None,
        }));
        assert!(!flow.importing());
        assert_eq!(done.as_deref(), Some("Imported KOReader progress for 26 books and highlights for 22"));

        // Highlights-only preview starts directly in the highlights phase.
        let mut hl_scan = Scan::default();
        hl_scan.preview = Preview { highlight_books: 4, highlights: 9, ..Default::default() };
        let mut hl_flow = Flow { store, waker: Waker::new(|| {}), stage: Stage::Preview(Box::new(hl_scan)), job: None };
        hl_flow.import();
        assert_eq!(hl_flow.text(), ("Importing highlights… 0 of 4".into(), ""));
        hl_flow.job = None;
    }
}
