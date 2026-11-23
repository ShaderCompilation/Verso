//! Settings → Library → KOReader → *Import progress*: the sheet that reads
//! KOReader's data for the whole library, shows what it found and imports it
//! after the user confirms (docs/koreader-progress.md §5). The work runs on a
//! [`Job`] thread; nothing is written before *Import*.
use crate::geom::Rect;
use crate::gfx::HAlign;
use crate::koreader::bulk::{Event, Job, Preview, Scan, UNREADABLE_SHOWN};
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
    /// Writing this many books' states.
    Importing(usize),
    /// What went wrong, shown until OK.
    Failed(String),
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
    store: Store,
    waker: Waker,
    stage: Stage,
    job: Option<Job>,
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
        let sheet = Sheet::open("Import progress from KOReader", height, screen, t);
        let job = Job::scan(store.clone(), open, waker.clone());
        let mut this = Self {
            sheet,
            store,
            waker,
            stage: Stage::Scanning(0, 0),
            job: Some(job),
            body: Rect::default(),
            buttons: Vec::new(),
        };
        this.place(t);
        this
    }

    /// Lays out the buttons for the stage, and the text above them.
    fn place(&mut self, t: &Theme) {
        let acts: &[(&str, ButtonKind, Act)] = match &self.stage {
            Stage::Scanning(..) => &[("Cancel", ButtonKind::Secondary, Act::Cancel)],
            Stage::Preview(scan) if scan.preview.changes > 0 => {
                &[("Cancel", ButtonKind::Secondary, Act::Cancel), ("Import", ButtonKind::Primary, Act::Import)]
            }
            Stage::Preview(_) | Stage::Failed(_) => &[("OK", ButtonKind::Primary, Act::Close)],
            Stage::Importing(_) => &[],
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

    /// The main text and the note under it.
    fn text(&self) -> (String, &'static str) {
        match &self.stage {
            Stage::Scanning(_, 0) => ("Reading KOReader data…".into(), ""),
            Stage::Scanning(done, total) => (format!("Reading KOReader data… {done} of {total}"), ""),
            Stage::Preview(scan) => preview_text(&scan.preview),
            Stage::Importing(n) => (format!("Importing {}…", counted(*n, "book")), ""),
            Stage::Failed(message) => (message.clone(), ""),
        }
    }

    pub fn draw(&self, cx: &mut DrawCx) {
        let t = cx.theme;
        self.sheet.draw(cx);
        let (text, note) = self.text();
        // Paragraphs are centred in their rect: give each its own height.
        let b = self.body;
        let (_, h) = cx.text.measure(&text, t.body, Some(b.w as f32));
        let h = (h.ceil() as i32).min(b.h);
        cx.paragraph(&text, t.body, Rect { h, ..b }, HAlign::Left);
        if !note.is_empty() {
            let y = b.y + h + t.mm(3.0);
            let (_, note_h) = cx.text.measure(note, t.small, Some(b.w as f32));
            let note_h = (note_h.ceil() as i32).min(b.bottom() - y).max(0);
            cx.paragraph(note, t.small.gray(INK2), Rect { y, h: note_h, ..b }, HAlign::Left);
        }
        for (b, _) in &self.buttons {
            b.draw(cx);
        }
    }

    /// Touches go here first while the sheet is open. Nothing closes it
    /// while the import writes.
    pub fn on_gesture(&mut self, g: &Gesture, t: &Theme) -> Option<Outcome> {
        if matches!(self.stage, Stage::Importing(_)) {
            return None;
        }
        let act = if self.sheet.on_gesture(g) {
            Some(Act::Cancel)
        } else {
            self.buttons.iter_mut().find_map(|(b, act)| b.on_gesture(g).then_some(*act))
        };
        match act? {
            // Dropping the job cancels the scan; nothing was written.
            Act::Cancel | Act::Close => {
                self.job = None;
                Some(Outcome::Close)
            }
            Act::Import => {
                let Stage::Preview(scan) = std::mem::replace(&mut self.stage, Stage::Importing(0)) else {
                    return None;
                };
                self.stage = Stage::Importing(scan.preview.changes);
                self.job = Some(Job::import(self.store.clone(), *scan, self.waker.clone()));
                self.place(t);
                None
            }
        }
    }

    /// Takes the job's events. Some when the sheet should close; redraws
    /// when the stage or the progress changed (returns true in `changed`).
    pub fn on_background(&mut self, t: &Theme, changed: &mut bool) -> Option<Outcome> {
        let job = self.job.as_ref()?;
        let finished = job.finished();
        let mut outcome = None;
        for event in job.events() {
            *changed = true;
            match event {
                Event::Scanning(done, total) => self.stage = Stage::Scanning(done, total),
                Event::Scanned(scan) => self.stage = Stage::Preview(scan),
                Event::Imported(imported) => match imported.error {
                    None => {
                        outcome = Some(Outcome::Imported(format!(
                            "Imported KOReader progress for {}",
                            counted(imported.written, "book")
                        )))
                    }
                    Some(e) => {
                        log::error!("KOReader import: {e:#}");
                        self.stage = Stage::Failed(format!(
                            "Imported {} of {}, then couldn't save: {e}.\n\nRun the import again to finish the rest.",
                            imported.written,
                            counted(imported.total, "book"),
                        ));
                    }
                },
            }
        }
        if finished {
            self.job = None;
            // The thread ended without its result: it couldn't start.
            match self.stage {
                Stage::Scanning(..) => self.stage = Stage::Failed("Couldn't read KOReader data.".into()),
                Stage::Importing(_) if outcome.is_none() => {
                    self.stage = Stage::Failed("Couldn't import KOReader progress.".into())
                }
                _ => {}
            }
            *changed = true;
        }
        if *changed {
            self.place(t);
        }
        outcome
    }

    #[cfg(test)]
    pub fn button(&self, label: &str) -> Option<Rect> {
        self.buttons.iter().find(|(b, _)| b.label == label).map(|(b, _)| b.rect())
    }

    /// The scan or the import is running (scripted screenshots wait).
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }
}

/// The preview: what the import will do, and the note under it.
fn preview_text(p: &Preview) -> (String, &'static str) {
    let mut lines = Vec::new();
    if p.found() == 0 && p.open_skipped == 0 {
        if p.unreadable == 0 {
            return (
                "No KOReader progress found.".into(),
                "KOReader keeps each book's progress in a .sdr folder beside the book.",
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
    if p.unreadable > 0 {
        let names: Vec<_> =
            p.unreadable_books.iter().map(|b| b.file_name().unwrap_or_default().to_string_lossy()).collect();
        let more = p.unreadable - names.len().min(UNREADABLE_SHOWN);
        let more = if more > 0 { format!(" and {more} more") } else { String::new() };
        lines.push(format!("Couldn't read {}: {}{more}", counted(p.unreadable, "book"), names.join(", ")));
    }
    let note = if p.changes == 0 {
        "Nothing to import: Verso already has this progress."
    } else {
        "Positions are imported one way; KOReader's files aren't changed."
    };
    (lines.join("\n"), note)
}

fn counted(count: usize, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::{Canvas, IconRenderer, TextRenderer};

    #[test]
    fn the_fullest_preview_fits_every_screen() {
        let mut full = Preview {
            not_started: 118,
            newer_in_koreader: 15,
            newer_in_verso: 8,
            up_to_date: 3,
            finished: 23,
            open_skipped: 1,
            unreadable: 5,
            unreadable_books: (0..3).map(|i| format!("/mnt/us/documents/A long book title {i}.epub").into()).collect(),
            changes: 133,
        };
        let (text, note) = preview_text(&full);
        assert!(text.starts_with("KOReader progress found for 145 books:\n    118 not started in Verso\n"), "{text}");
        assert!(text.contains("A long book title 2.epub and 2 more"), "{text}");
        assert!(note.contains("one way"));
        full.changes = 0;
        assert!(preview_text(&full).1.starts_with("Nothing to import"));
        assert_eq!(preview_text(&Preview::default()).0, "No KOReader progress found.");
        full.changes = 133;
        let dir = tempfile::tempdir().unwrap();
        for (dpi, w, h) in [(300, 1272, 1696), (300, 1696, 1272), (167, 600, 800), (167, 800, 600)] {
            let t = Theme::for_dpi(dpi);
            let screen = Rect::new(0, 0, w, h);
            let mut sheet = ImportSheet::open(&t, screen, Store::at(dir.path().into()), Waker::new(|| {}), None);
            sheet.job = None;
            let mut scan = Scan::default();
            scan.preview = full.clone();
            sheet.stage = Stage::Preview(Box::new(scan));
            sheet.place(&t);
            let mut text = TextRenderer::new();
            let (main, note) = sheet.text();
            let (_, main_h) = text.measure(&main, t.body, Some(sheet.body.w as f32));
            let (_, note_h) = text.measure(note, t.small, Some(sheet.body.w as f32));
            let needed = main_h.ceil() as i32 + t.mm(3.0) + note_h.ceil() as i32;
            assert!(needed <= sheet.body.h, "{w}x{h}: {needed} > {}", sheet.body.h);
            assert_eq!(sheet.buttons.len(), 2);
            assert!(sheet.sheet.rect().y >= t.top_bar, "{w}x{h}: the top bar stays visible");
            let (mut canvas, mut icons) = (Canvas::new(w as u32, h as u32), IconRenderer::default());
            sheet.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut vec![]));
        }
    }
}
