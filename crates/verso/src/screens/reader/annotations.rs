use crate::{
    annotations::{
        Annotation, Collection, NOTE_LIMIT,
        export::Export,
        worker::{self, Preview},
    },
    document::thread::{Command, Reply, Resolved, Worker},
    geom::Rect,
    gfx::HAlign,
    platform::TextInput,
    screens::EventCx,
    ui::{
        gesture::Gesture,
        theme::{INK2, PAPER, Theme},
        widget::DrawCx,
        widgets::{Button, ButtonKind, Keyboard, List, ListEvent, Pager, Row, TextArea, TextField, TopBar},
    },
};
use std::path::PathBuf;
#[derive(Default)]
pub(super) struct State {
    pub service: Option<worker::Worker>,
    pub collection: Collection,
    pub revision: u64,
    pub resolved: Vec<Resolved>,
    pub ready: bool,
    pub resolving: bool,
    pub paint_pending: bool,
    pub pending: bool,
    /// Loading the committed collection failed; nothing can be saved.
    pub load_error: Option<String>,
    /// The latest save failed and its snapshot is waiting for a retry.
    pub save_error: Option<String>,
    /// Reading, parsing or validating the chosen KOReader file failed, or its
    /// preview became stale. Shown only in Import, separate from save retries.
    pub import_error: Option<String>,
    pub preview: Option<Preview>,
    pub preview_resolved: Vec<Resolved>,
    pub import_id: u64,
    pub importing: bool,
    pub exporting: bool,
    /// The latest export's destination, or why it failed. Separate from the
    /// other errors: exporting never changes annotations or their saving.
    pub export_result: Option<Result<PathBuf, String>>,
}
/// Why a highlight or note can't be saved right now.
pub(super) const NOT_READY: &str = "Highlights aren't ready to save yet. Try again in a moment.";
impl State {
    pub fn resolve(&mut self, generation: u64, worker: &Worker) {
        self.resolve_with(generation, &mut |c| worker.send(c));
    }
    fn resolve_with(&mut self, generation: u64, send: &mut dyn FnMut(Command)) {
        if !self.ready {
            return;
        }
        self.invalidate_import("Highlights changed. Preview the import again.");
        self.resolving = true;
        self.revision += 1;
        self.resolved.clear();
        send(Command::Resolve {
            generation,
            revision: self.revision,
            records: self.collection.records.clone(),
            preview: false,
        });
    }
    /// True when a new highlight or note can be saved now.
    pub fn can_save(&self) -> bool {
        self.ready && !self.pending
    }
    /// Adds a highlight (with any note) and starts saving it; the caller
    /// starts a fresh resolution. Nothing changes when saving can't start.
    pub fn add(&mut self, annotation: Annotation) -> Result<(), &'static str> {
        if !self.can_save() {
            return Err(NOT_READY);
        }
        self.collection.add(annotation);
        self.save();
        Ok(())
    }
    /// Gives entry `index` a new note (`None` deletes it, keeping the
    /// highlight) and starts saving. The edit is a new record in the same
    /// place, so the current resolution stays valid. Ok(false): unchanged.
    pub fn set_note(&mut self, index: usize, note: Option<String>) -> Result<bool, &'static str> {
        if !self.can_save() {
            return Err(NOT_READY);
        }
        let Some(old) = self.collection.records.get(index) else { return Err("This highlight is no longer here.") };
        if old.note == note {
            return Ok(false);
        }
        let (id, new) = (old.id.clone(), old.with_note(note));
        self.collection.replace(&id, new);
        self.save();
        Ok(true)
    }
    /// Entry indices in reading order; unresolved entries (and all entries,
    /// until a resolution completes) follow in saved order.
    pub fn ordered(&self) -> Vec<usize> {
        if self.resolved.len() == self.collection.records.len() {
            self.resolved.iter().map(|e| e.index).collect()
        } else {
            (0..self.collection.records.len()).collect()
        }
    }
    /// True when there is something to export and no export is running.
    pub fn can_export(&self) -> bool {
        self.ready && !self.exporting && !self.collection.records.is_empty()
    }
    /// Writes the in-memory collection (including a snapshot still waiting
    /// to be saved) as Markdown on the I/O thread.
    pub fn export(&mut self, title: String, author: String) -> bool {
        if !self.can_export() {
            return false;
        }
        let records = self.ordered().into_iter().filter_map(|i| self.collection.records.get(i).cloned()).collect();
        self.exporting = true;
        self.export_result = None;
        if let Some(w) = &self.service {
            w.send(worker::Command::Export(Export { title, author, records }));
        }
        true
    }
    pub fn save(&mut self) {
        self.pending = true;
        if let Some(w) = &self.service {
            w.send(worker::Command::Save(self.collection.clone()));
        }
    }
    pub fn flush(&self) -> anyhow::Result<()> {
        if let Some(w) = &self.service {
            w.flush()?;
        }
        Ok(())
    }
    pub fn drain(&mut self, generation: u64, worker: &Worker, cx: &mut EventCx) -> bool {
        let replies: Vec<_> = self.service.as_ref().map(|w| w.drain().collect()).unwrap_or_default();
        let changed = !replies.is_empty();
        for reply in replies {
            if let Some(message) = self.apply(reply, generation, &mut |c| worker.send(c)) {
                cx.toast(message, None);
            }
        }
        changed
    }
    /// Applies one ordered service reply; returns a message to show briefly.
    /// Each error lives in its own field, so a later successful save (including
    /// a retry completed by a flush) clears only the save error.
    fn apply(&mut self, reply: worker::Reply, generation: u64, send: &mut dyn FnMut(Command)) -> Option<String> {
        match reply {
            worker::Reply::Loaded(Ok((c, fingerprint))) => {
                if let Some(w) = &mut self.service {
                    w.book.fingerprint = fingerprint;
                }
                self.collection = c;
                self.ready = true;
                self.load_error = None;
                self.resolve_with(generation, send);
                None
            }
            worker::Reply::Loaded(Err(e)) => {
                self.load_error = Some(e.clone());
                Some(format!("Couldn't load highlights: {e}"))
            }
            worker::Reply::Saved(result) => {
                self.pending = false;
                match result {
                    Ok(()) => {
                        let retried = self.save_error.take().is_some();
                        Some(if retried { "Saved after retry" } else { "Saved" }.into())
                    }
                    Err(e) => {
                        self.save_error = Some(e.clone());
                        Some(format!("Couldn't save highlights and notes: {e}"))
                    }
                }
            }
            worker::Reply::Preview(id, result) if id == self.import_id && self.importing => {
                match result {
                    Ok(p) => {
                        send(Command::Resolve { generation, revision: id, records: p.records.clone(), preview: true });
                        self.preview = Some(p);
                    }
                    Err(e) => {
                        self.importing = false;
                        self.import_error = Some(e);
                    }
                }
                None
            }
            worker::Reply::Preview(..) => None,
            worker::Reply::Exported(result) => {
                // Shown in the list's status line only: a toast would cover the
                // details buttons, and export takes well under a second.
                self.exporting = false;
                self.export_result = Some(result);
                None
            }
        }
    }
    pub fn receive(&mut self, reply: Reply) -> bool {
        if let Reply::Resolved { revision, entries, preview, .. } = reply {
            if preview {
                if revision == self.import_id && self.importing {
                    self.preview_resolved = entries;
                    self.importing = false;
                    return true;
                }
            } else if revision == self.revision {
                self.resolved = entries;
                self.resolving = false;
                return true;
            }
        }
        false
    }
    pub fn start_import(&mut self, path: PathBuf) {
        if !self.ready || self.pending || self.resolving {
            return;
        }
        self.import_id += 1;
        self.importing = true;
        self.import_error = None;
        self.preview = None;
        self.preview_resolved.clear();
        if let Some(w) = &self.service {
            w.send(worker::Command::Preview(path, self.collection.manifest.sources.clone(), self.import_id));
        }
    }
    pub fn cancel_import(&mut self) {
        if let Some(w) = &self.service {
            w.cancel_preview();
        }
        self.import_id += 1;
        self.importing = false;
        self.import_error = None;
        self.preview = None;
        self.preview_resolved.clear();
    }
    /// Discards both I/O still running and a finished preview, whose resolved
    /// positions cannot be reused after layout or committed highlights change.
    pub fn invalidate_import(&mut self, reason: &str) {
        if self.importing || self.preview.is_some() {
            self.cancel_import();
            self.import_error = Some(reason.into());
        }
    }
    pub fn confirm(&mut self, generation: u64, worker: &Worker) {
        if self.importing {
            return;
        }
        if let Some(p) = self.preview.take() {
            for a in p.records {
                self.collection.add(a);
            }
            self.save();
            self.resolve(generation, worker);
        }
    }
}
pub(super) enum Event {
    Close,
    Jump(usize),
    Remove(usize),
    Preview(PathBuf),
    Confirm,
    CancelImport,
    Retry,
    /// Save the editor's note (raw text; blank means no note).
    SaveNote(NoteTarget, String),
    DeleteNote(usize),
    Export,
}
/// What the note editor writes to.
#[derive(Clone)]
pub(super) enum NoteTarget {
    /// A selection that isn't saved yet; saving the note saves it too.
    New(Box<Annotation>),
    /// A saved entry, by index.
    Existing(usize),
}
enum Mode {
    List,
    Detail(usize),
    Import,
    Note(NoteTarget),
}
pub(super) struct View {
    top: TopBar,
    list: List,
    pager: Pager,
    ids: Vec<usize>,
    mode: Mode,
    import: Button,
    export: Button,
    primary: Button,
    remove: Button,
    retry: Button,
    edit_note: Button,
    delete_note: Button,
    save_note: Button,
    cancel_note: Button,
    field: TextField,
    note: TextArea,
    keyboard: Keyboard,
    screen: Rect,
    body: Rect,
    detail_body: Rect,
    detail_pages: Vec<String>,
    preview_path: Option<PathBuf>,
}
impl View {
    pub fn new(t: &Theme, screen: Rect, state: &State, ids: Option<Vec<usize>>) -> Self {
        let mut top = TopBar::new("Highlights & notes", true);
        top.place(screen, t);
        let mut pager = Pager::new(1);
        pager.place(Rect::new(t.margin, screen.bottom() - t.mm(11.0), screen.w - 2 * t.margin, t.mm(11.0)), t);
        let gap = t.mm(3.0);
        let width = (screen.w - 2 * t.margin - gap) / 2;
        let left = |y| Rect::new(t.margin, y, width, t.target);
        let right = |y| Rect::new(screen.w - t.margin - width, y, width, t.target);
        let mut import = Button::new("Import from KOReader", ButtonKind::Secondary);
        import.place(left(t.top_bar + t.mm(2.0)), t);
        let mut export = Button::new("Export", ButtonKind::Secondary);
        export.place(right(t.top_bar + t.mm(2.0)), t);
        let mut primary = Button::new("Jump to passage", ButtonKind::Primary);
        let mut remove = Button::new("Remove", ButtonKind::Secondary);
        let y = pager.rect().y - t.target - gap;
        primary.place(left(y), t);
        remove.place(right(y), t);
        let mut edit_note = Button::new("Add note", ButtonKind::Secondary);
        let mut delete_note = Button::new("Delete note", ButtonKind::Secondary);
        let note_y = y - t.target - gap;
        edit_note.place(left(note_y), t);
        delete_note.place(right(note_y), t);
        let mut save_note = Button::new("Save", ButtonKind::Primary);
        let mut cancel_note = Button::new("Cancel", ButtonKind::Secondary);
        save_note.place(left(y), t);
        cancel_note.place(right(y), t);
        let mut retry = Button::new("Retry saving", ButtonKind::Secondary);
        retry.place(import.rect(), t);
        let body = Rect::new(
            t.margin,
            import.rect().bottom() + gap,
            screen.w - 2 * t.margin,
            y - import.rect().bottom() - t.mm(5.0),
        );
        let detail_body = Rect { h: note_y - body.y - t.mm(2.0), ..body };
        let mut field = TextField::new("KOReader metadata path");
        field.place(Rect::new(t.margin, body.y, body.w, t.target));
        if let Some(w) = &state.service {
            field.edit(&TextInput::Insert(
                w.book.path.with_extension("sdr").join("metadata.epub.lua").display().to_string(),
            ));
        }
        let mut keyboard = Keyboard::default();
        keyboard.place(screen, t);
        let mut v = Self {
            top,
            list: List::new(vec![]),
            pager,
            ids: vec![],
            mode: Mode::List,
            import,
            export,
            primary,
            remove,
            retry,
            edit_note,
            delete_note,
            save_note,
            cancel_note,
            field,
            note: TextArea::new("Write a note", NOTE_LIMIT),
            keyboard,
            screen,
            body,
            detail_body,
            detail_pages: vec![],
            preview_path: None,
        };
        v.refresh(state, ids, t);
        v
    }
    pub fn detail(&mut self, index: usize) {
        self.mode = Mode::Detail(index);
        self.detail_pages.clear();
        self.pager.set(0, 1);
    }
    /// Opens the note editor with the keyboard up.
    pub fn edit_note(&mut self, target: NoteTarget, state: &State, t: &Theme) {
        let current = match &target {
            NoteTarget::New(a) => a.note.clone(),
            NoteTarget::Existing(i) => state.collection.records.get(*i).and_then(|a| a.note.clone()),
        };
        self.note.set_value(current.as_deref().unwrap_or(""));
        self.mode = Mode::Note(target);
        self.field.focused = false;
        self.focus_note(true, t);
    }
    fn focus_note(&mut self, focused: bool, t: &Theme) {
        self.note.focused = focused;
        self.keyboard.set_submit(if focused { "Save" } else { "" });
        self.keyboard.place(self.screen, t);
        self.keyboard.visible = focused;
    }
    /// The note editor is open; it survives sleep so typed text isn't lost.
    pub fn editing_note(&self) -> bool {
        matches!(self.mode, Mode::Note(_))
    }
    pub fn refresh(&mut self, state: &State, ids: Option<Vec<usize>>, t: &Theme) {
        self.ids = ids.unwrap_or_else(|| state.ordered());
        self.list.rows = self
            .ids
            .iter()
            .filter_map(|i| {
                state.collection.records.get(*i).map(|a| {
                    // A fresh resolution batch clears earlier results; until it
                    // finishes, no row is known to be unresolved.
                    let status = if state.resolving {
                        " · Locating…"
                    } else if state.resolved.iter().any(|e| e.index == *i && e.page.is_some()) {
                        ""
                    } else {
                        " · Unresolved"
                    };
                    let one_line = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
                    let sub = match a.note.as_deref() {
                        // Keep a long chapter title from hiding the note.
                        Some(note) => {
                            let mut chapter: String = a.chapter.chars().take(24).collect();
                            if chapter.len() < a.chapter.len() {
                                chapter = format!("{}…", chapter.trim_end());
                            }
                            format!("{chapter}{status} · Note: {}", one_line(note))
                        }
                        None => format!("{}{status}", a.chapter),
                    };
                    Row::new(one_line(&a.quotation)).sub(sub)
                })
            })
            .collect();
        self.list.place(self.body, t.row + t.mm(9.0), 0);
        self.pager.set(self.pager.page, self.list.paging().pages());
    }
    /// The editor's quotation line, note box and (with the keyboard hidden)
    /// Save · Cancel row. A short screen with the keyboard up drops the
    /// quotation to keep room for typing; the keyboard's Save key saves.
    fn note_layout(&self, t: &Theme) -> (Option<Rect>, Rect) {
        let top = self.top.rect().bottom() + t.mm(3.0);
        let bottom = if self.keyboard.visible { self.keyboard.rect().y } else { self.save_note.rect().y } - t.mm(3.0);
        let excerpt_h = 2 * t.small.line_height() as i32;
        let w = self.screen.w - 2 * t.margin;
        if !self.keyboard.visible || bottom - top >= excerpt_h + t.mm(28.0) {
            let area_y = top + excerpt_h + t.mm(3.0);
            (Some(Rect::new(t.margin, top, w, excerpt_h)), Rect::new(t.margin, area_y, w, bottom - area_y))
        } else {
            (None, Rect::new(t.margin, top, w, bottom - top))
        }
    }
    pub fn draw(&mut self, cx: &mut DrawCx, state: &State) {
        let t = cx.theme;
        cx.canvas.fill(PAPER);
        self.top.title = if self.editing_note() { "Note" } else { "Highlights & notes" }.into();
        self.top.draw(cx);
        match &self.mode {
            Mode::List => {
                self.import.enabled = state.ready && !state.pending && !state.resolving;
                if state.save_error.is_some() && state.ready {
                    self.retry.draw(cx);
                } else {
                    self.import.draw(cx);
                }
                self.export.label = if state.exporting { "Exporting…" } else { "Export" }.into();
                self.export.enabled = state.can_export();
                self.export.draw(cx);
                self.list.page = self.pager.page;
                self.list.draw(cx);
                self.pager.draw(cx);
                if self.ids.is_empty() {
                    cx.paragraph(
                        state.load_error.as_deref().unwrap_or(if state.ready {
                            "No highlights yet"
                        } else {
                            "Loading highlights…"
                        }),
                        t.body,
                        self.body,
                        HAlign::Left,
                    );
                }
                // The latest export stays visible (with its full path) below
                // the list, apart from saving and import messages.
                let status = match &state.export_result {
                    Some(Ok(path)) => Some(format!("Exported to {}", path.display())),
                    Some(Err(e)) => Some(format!("Couldn't export: {e}")),
                    None => None,
                };
                if let Some(status) = status {
                    let r = Rect::new(
                        t.margin,
                        self.body.bottom() + t.mm(1.0),
                        self.body.w,
                        self.pager.rect().y - self.body.bottom() - t.mm(1.0),
                    );
                    cx.paragraph(&status, t.small.gray(INK2), r, HAlign::Left);
                }
            }
            &Mode::Detail(index) => {
                if let Some(a) = state.collection.records.get(index) {
                    if self.detail_pages.is_empty() {
                        let text = format!(
                            "{}\n{}\n\n{}{}{}",
                            a.chapter,
                            a.created,
                            a.quotation,
                            a.note.as_ref().map(|_| "\n\nNote\n").unwrap_or(""),
                            a.note.as_deref().unwrap_or("")
                        );
                        self.detail_pages = cx.text.pages(&text, t.body, self.detail_body.w, self.detail_body.h);
                    }
                    self.pager.set(self.pager.page, self.detail_pages.len());
                    if let Some(text) = self.detail_pages.get(self.pager.page) {
                        cx.paragraph(text, t.body, self.detail_body, HAlign::Left);
                    }
                    self.primary.label = "Jump to passage".into();
                    self.primary.enabled = state.resolved.iter().any(|e| e.index == index && e.page.is_some());
                    self.primary.draw(cx);
                    self.remove.enabled = !state.pending;
                    self.remove.draw(cx);
                    self.edit_note.label = if a.note.is_some() { "Edit note" } else { "Add note" }.into();
                    self.edit_note.enabled = state.ready;
                    self.edit_note.draw(cx);
                    if a.note.is_some() {
                        self.delete_note.enabled = state.can_save();
                        self.delete_note.draw(cx);
                    }
                    self.pager.draw(cx);
                }
            }
            Mode::Import => {
                self.field.draw(cx);
                let stale = self.preview_path.as_ref().is_none_or(|p| p != std::path::Path::new(self.field.value()));
                let status = if state.importing {
                    "Reading and validating…".into()
                } else if let Some(e) = &state.import_error {
                    format!("Couldn't preview this import: {e}\n\nPreview again to continue.")
                } else if state.preview.is_some() && stale {
                    "The path changed. Preview the new file to see what it contains.".into()
                } else if let Some(p) = &state.preview {
                    format!(
                        "{} new · {} duplicate · {} unresolved · {} unsupported\n\nOne-way import. KOReader files stay untouched.",
                        p.records.len(),
                        p.duplicates,
                        state.preview_resolved.iter().filter(|e| e.page.is_none()).count(),
                        p.unsupported
                    )
                } else {
                    "Choose a metadata.epub.lua file and preview the import.".into()
                };
                cx.paragraph(
                    &status,
                    t.body,
                    Rect { y: self.body.y + t.target + t.mm(4.0), h: self.body.h - t.target - t.mm(4.0), ..self.body },
                    HAlign::Left,
                );
                self.primary.label = if state.preview.is_some()
                    && self.preview_path.as_ref().is_some_and(|p| p == std::path::Path::new(self.field.value()))
                {
                    "Confirm import"
                } else {
                    "Preview import"
                }
                .into();
                self.primary.enabled = !state.importing && !state.pending && !state.resolving;
                self.primary.draw(cx);
                self.keyboard.draw(cx);
            }
            Mode::Note(target) => {
                let quotation = match target {
                    NoteTarget::New(a) => a.quotation.as_str(),
                    NoteTarget::Existing(i) => state.collection.records.get(*i).map_or("", |a| a.quotation.as_str()),
                };
                let (excerpt, area) = self.note_layout(t);
                if let Some(r) = excerpt {
                    let text = if self.note.full() {
                        format!("A note can hold up to {NOTE_LIMIT} characters.")
                    } else {
                        format!("“{}”", quotation.split_whitespace().collect::<Vec<_>>().join(" "))
                    };
                    cx.paragraph(&text, t.small.gray(INK2), r, HAlign::Left);
                }
                self.note.place(area);
                self.note.draw(cx);
                if !self.keyboard.visible {
                    self.save_note.draw(cx);
                    self.cancel_note.draw(cx);
                }
                self.keyboard.draw(cx);
            }
        }
    }
    /// Text input for the import path or the note. Returns whether it was
    /// consumed and, for the note's Save key, the save request.
    pub fn input(&mut self, input: &TextInput) -> (bool, Option<Event>) {
        match &self.mode {
            Mode::Import if self.field.focused => {
                match input {
                    TextInput::Dismiss | TextInput::Submit => {
                        self.field.focused = false;
                        self.keyboard.visible = false;
                    }
                    _ => {
                        self.field.edit(input);
                    }
                }
                (true, None)
            }
            Mode::Note(target) if self.note.focused => {
                match input {
                    TextInput::Submit => {
                        return (true, Some(Event::SaveNote(target.clone(), self.note.value().into())));
                    }
                    TextInput::Dismiss => {
                        self.note.focused = false;
                        self.keyboard.visible = false;
                    }
                    _ => {
                        self.note.edit(input);
                    }
                }
                (true, None)
            }
            _ => (false, None),
        }
    }
    /// Leaves the note editor: back to the entry's details, or (for a new
    /// highlight) the caller closes the view.
    pub fn close_note(&mut self, t: &Theme) {
        if let Mode::Note(NoteTarget::Existing(i)) = self.mode {
            self.focus_note(false, t);
            self.detail(i);
        }
    }
    pub fn gesture(&mut self, g: &Gesture, state: &State, t: &Theme) -> Option<Event> {
        if self.keyboard.visible {
            if let Some(i) = self.keyboard.on_gesture(g, t) {
                return self.input(&i).1;
            }
            return None;
        }
        if self.top.on_gesture(g) {
            return match &self.mode {
                Mode::List => Some(Event::Close),
                Mode::Import => {
                    self.mode = Mode::List;
                    Some(Event::CancelImport)
                }
                Mode::Detail(_) => {
                    self.mode = Mode::List;
                    self.pager.set(0, self.list.paging().pages());
                    None
                }
                Mode::Note(NoteTarget::New(_)) => Some(Event::Close),
                Mode::Note(NoteTarget::Existing(_)) => {
                    self.close_note(t);
                    None
                }
            };
        }
        match &self.mode {
            Mode::List => {
                if state.save_error.is_some() && state.ready && self.retry.on_gesture(g) {
                    return Some(Event::Retry);
                }
                if self.import.on_gesture(g) {
                    self.mode = Mode::Import;
                    self.preview_path = Some(self.field.value().into());
                    return Some(Event::Preview(self.field.value().into()));
                }
                if self.export.on_gesture(g) {
                    return Some(Event::Export);
                }
                self.pager.on_gesture(g);
                if let Gesture::Swipe { dir, .. } = g {
                    self.pager.on_swipe(*dir);
                }
                self.list.page = self.pager.page;
                if let Some(ListEvent::Tapped(i)) = self.list.on_gesture(g) {
                    self.detail(self.ids[i]);
                }
            }
            &Mode::Detail(i) => {
                self.pager.on_gesture(g);
                if self.primary.on_gesture(g) {
                    return Some(Event::Jump(i));
                }
                if self.remove.on_gesture(g) {
                    return Some(Event::Remove(i));
                }
                if self.edit_note.on_gesture(g) {
                    self.edit_note(NoteTarget::Existing(i), state, t);
                }
                let noted = state.collection.records.get(i).is_some_and(|a| a.note.is_some());
                if noted && self.delete_note.on_gesture(g) {
                    return Some(Event::DeleteNote(i));
                }
            }
            Mode::Import => {
                if self.field.on_gesture(g) {
                    self.field.focused = true;
                    self.keyboard.visible = true;
                }
                if self.primary.on_gesture(g) {
                    return Some(
                        if state.preview.is_some()
                            && self.preview_path.as_ref().is_some_and(|p| p == std::path::Path::new(self.field.value()))
                        {
                            Event::Confirm
                        } else {
                            self.preview_path = Some(self.field.value().into());
                            Event::Preview(self.field.value().into())
                        },
                    );
                }
            }
            Mode::Note(target) => {
                let target = target.clone();
                if self.note.on_gesture(g) {
                    self.focus_note(true, t);
                }
                if self.save_note.on_gesture(g) {
                    return Some(Event::SaveNote(target, self.note.value().into()));
                }
                if self.cancel_note.on_gesture(g) {
                    return match target {
                        NoteTarget::New(_) => Some(Event::Close),
                        NoteTarget::Existing(_) => {
                            self.close_note(t);
                            None
                        }
                    };
                }
            }
        }
        None
    }
}
pub(super) struct ExtendBar {
    buttons: Vec<Button>,
    rect: Rect,
}
impl ExtendBar {
    pub fn new(t: &Theme, screen: Rect) -> Self {
        let rect = screen.split_bottom(t.mm(13.0)).0;
        let gap = t.mm(2.0);
        let w = (rect.w - 2 * t.margin - 3 * gap) / 4;
        let buttons = ["Previous", "Next", "Highlight", "Cancel"]
            .into_iter()
            .enumerate()
            .map(|(i, label)| {
                let mut b = Button::new(label, ButtonKind::Secondary);
                b.place(Rect::new(t.margin + i as i32 * (w + gap), rect.y, w, t.target), t);
                b
            })
            .collect();
        Self { buttons, rect }
    }
    pub fn draw(&mut self, cx: &mut DrawCx, page: usize, pages: usize, busy: bool) {
        cx.fill(self.rect, PAPER);
        self.buttons[0].enabled = page > 0 && !busy;
        self.buttons[1].enabled = page + 1 < pages && !busy;
        self.buttons[2].enabled = !busy;
        for b in &self.buttons {
            b.draw(cx);
        }
    }
    pub fn gesture(&mut self, g: &Gesture) -> (bool, Option<usize>) {
        if !self.rect.contains(g.pos()) {
            return (false, None);
        }
        let e = self.buttons.iter_mut().position(|b| b.on_gesture(g));
        (true, e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn obsolete_resolutions_and_cancelled_imports_never_change_saved_entries() {
        let mut state = State { revision: 7, resolving: true, import_id: 9, importing: true, ..Default::default() };
        let reply = |revision, preview| Reply::Resolved {
            generation: 1,
            revision,
            entries: vec![Resolved { index: 0, page: Some(3) }],
            preview,
        };
        assert!(!state.receive(reply(6, false)));
        assert!(state.resolved.is_empty());
        assert!(!state.receive(reply(8, true)));
        assert!(state.preview_resolved.is_empty());
        state.cancel_import();
        assert!(!state.receive(reply(9, true)));
        assert!(state.collection.records.is_empty());
        assert!(state.receive(reply(7, false)));
        assert!(!state.resolving);
        assert_eq!(state.resolved[0].page, Some(3));
    }
    #[test]
    fn committed_resolution_invalidates_queued_running_and_completed_import_previews() {
        for stage in 0..3 {
            let mut state = ready(&["saved"]);
            let mut sent = Vec::new();
            state.start_import("/metadata.epub.lua".into());
            let old = state.import_id;
            let preview = Preview { records: vec![entry("imported", None)], ..Default::default() };
            let resolved = |revision, preview| Reply::Resolved {
                generation: 1,
                revision,
                entries: vec![Resolved { index: 0, page: Some(3) }],
                preview,
            };
            if stage >= 1 {
                state.apply(worker::Reply::Preview(old, Ok(preview.clone())), 1, &mut |c| sent.push(c));
                assert!(matches!(sent.pop(), Some(Command::Resolve { preview: true, .. })));
            }
            if stage == 2 {
                assert!(state.receive(resolved(old, true)));
                assert!(!state.importing && state.preview.is_some());
            }

            state.resolve_with(1, &mut |c| sent.push(c));
            assert!(matches!(sent.pop(), Some(Command::Resolve { preview: false, .. })));
            assert!(!state.importing && state.preview.is_none());
            assert!(state.preview_resolved.is_empty());
            assert!(state.import_error.as_deref().unwrap().contains("Preview the import again"));
            assert_ne!(state.import_id, old);
            // Either worker may finish obsolete work after cancellation.
            state.apply(worker::Reply::Preview(old, Ok(preview.clone())), 1, &mut |c| sent.push(c));
            assert!(sent.is_empty(), "stale I/O cannot restart preview resolution");
            assert!(!state.receive(resolved(old, true)));
            assert!(state.preview.is_none(), "an invalidated preview cannot be confirmed");
            assert_eq!(state.collection.records.len(), 1, "saved highlights stay untouched");

            assert!(state.receive(resolved(state.revision, false)));
            state.start_import("/metadata.epub.lua".into());
            let retried = state.import_id;
            assert!(state.importing && state.import_error.is_none());
            state.apply(worker::Reply::Preview(retried, Ok(preview)), 1, &mut |c| sent.push(c));
            assert!(matches!(sent.pop(), Some(Command::Resolve { preview: true, .. })));
            assert!(state.receive(resolved(retried, true)));
            assert!(!state.importing && state.preview.is_some(), "retry reaches a confirmable preview");
        }
    }

    #[test]
    fn import_errors_stay_separate_and_a_flushed_retry_clears_only_the_save_error() {
        let mut state = State { ready: true, ..Default::default() };
        let mut sent = vec![];
        let mut send = |c: Command| sent.push(c);
        state.pending = true;
        assert!(state.apply(worker::Reply::Saved(Err("disk full".into())), 1, &mut send).is_some());
        assert_eq!(state.save_error.as_deref(), Some("disk full"));
        assert!(!state.pending);
        state.start_import("/missing/metadata.epub.lua".into());
        let id = state.import_id;
        assert!(state.apply(worker::Reply::Preview(id, Err("not found".into())), 1, &mut send).is_none());
        assert_eq!(state.import_error.as_deref(), Some("not found"));
        assert_eq!(state.save_error.as_deref(), Some("disk full"), "import errors never replace save errors");
        // A stale preview reply after a newer request changes nothing.
        state.start_import("/other.lua".into());
        assert!(state.import_error.is_none());
        state.apply(worker::Reply::Preview(id, Err("old".into())), 1, &mut send);
        assert!(state.import_error.is_none() && state.importing);
        // The ordered worker reports a retry that a flush completed.
        let message = state.apply(worker::Reply::Saved(Ok(())), 1, &mut send);
        assert_eq!(message.as_deref(), Some("Saved after retry"));
        assert!(state.save_error.is_none());
        assert!(state.importing, "queued import state survives the save acknowledgement");
        assert!(sent.is_empty());
    }
    fn entry(id: &str, note: Option<&str>) -> Annotation {
        Annotation {
            version: 1,
            id: id.into(),
            book: "book".into(),
            fingerprint: "content".into(),
            range: crate::document::TextRange {
                start: crate::document::Position("/a".into()),
                end: crate::document::Position("/b".into()),
            },
            quotation: format!("Quote {id}"),
            created: "2026-11-15".into(),
            chapter: "Chapter".into(),
            note: note.map(Into::into),
            provenance: Some(crate::annotations::Provenance { source: "s".into(), entry: id.into() }),
        }
    }
    fn ready(ids: &[&str]) -> State {
        let mut state = State { ready: true, ..Default::default() };
        for id in ids {
            state.collection.add(entry(id, None));
        }
        state
    }
    #[test]
    fn a_new_note_saves_with_its_highlight_and_waits_while_a_save_runs() {
        let mut state = ready(&[]);
        let mut a = entry("new", None);
        a.note = crate::annotations::clean_note("  a thought ");
        assert!(state.add(a.clone()).is_ok());
        assert!(state.pending, "one save carries the highlight and its note");
        assert_eq!(state.collection.records[0].note.as_deref(), Some("a thought"));
        assert_eq!(state.add(entry("other", None)), Err(NOT_READY), "nothing is added while saving");
        assert_eq!(state.set_note(0, Some("x".into())), Err(NOT_READY));
        assert_eq!(state.collection.records.len(), 1);
        let mut send = |_: Command| {};
        assert_eq!(state.apply(worker::Reply::Saved(Ok(())), 1, &mut send).as_deref(), Some("Saved"));
        let mut loading = State::default();
        assert_eq!(loading.add(entry("early", None)), Err(NOT_READY));
        assert!(loading.collection.records.is_empty());
    }
    #[test]
    fn editing_a_note_replaces_the_record_in_place_and_deleting_keeps_the_highlight() {
        let mut state = ready(&["first", "second", "third"]);
        assert_eq!(state.set_note(1, Some("Fresh".into())), Ok(true));
        assert!(state.pending);
        let edited = state.collection.records[1].clone();
        assert_ne!(edited.id, "second", "record files are immutable: an edit is a new record");
        assert_eq!(edited.note.as_deref(), Some("Fresh"));
        assert_eq!((edited.quotation.as_str(), edited.created.as_str()), ("Quote second", "2026-11-15"));
        assert_eq!(state.collection.manifest.records, ["first", edited.id.as_str(), "third"]);
        assert!(state.collection.manifest.sources.contains("second"), "imports stay suppressed");
        state.pending = false;
        assert_eq!(state.set_note(1, Some("Fresh".into())), Ok(false), "unchanged text saves nothing");
        assert!(!state.pending);
        assert_eq!(state.set_note(1, None), Ok(true));
        assert!(state.collection.records[1].note.is_none());
        assert_eq!(state.collection.records.len(), 3, "deleting a note keeps the highlight");
        state.pending = false;
        assert!(state.set_note(9, None).is_err());
    }
    #[test]
    fn a_failed_save_keeps_the_note_in_memory_and_exports_never_touch_save_errors() {
        let mut state = ready(&["first"]);
        let mut send = |_: Command| {};
        state.set_note(0, Some("Kept".into())).unwrap();
        let message = state.apply(worker::Reply::Saved(Err("disk full".into())), 1, &mut send);
        assert!(message.unwrap().starts_with("Couldn't save"));
        assert_eq!(state.collection.records[0].note.as_deref(), Some("Kept"), "visible until a retry");
        assert!(state.export("Title".into(), "Author".into()), "the in-memory collection exports");
        assert!(state.exporting && !state.can_export());
        assert!(!state.export("Title".into(), "Author".into()), "one export at a time");
        let message = state.apply(worker::Reply::Exported(Err("read-only".into())), 1, &mut send);
        assert!(message.is_none(), "the status line reports exports");
        assert!(matches!(&state.export_result, Some(Err(e)) if e == "read-only"));
        assert!(!state.exporting);
        assert_eq!(state.save_error.as_deref(), Some("disk full"), "an export never clears a save error");
        state.export("Title".into(), "Author".into());
        let path = PathBuf::from("/mnt/us/verso/data/exports/Title (book).md");
        let message = state.apply(worker::Reply::Exported(Ok(path.clone())), 1, &mut send);
        assert!(message.is_none());
        assert!(matches!(&state.export_result, Some(Ok(p)) if *p == path));
        assert_eq!(state.save_error.as_deref(), Some("disk full"));
        assert!(!ready(&[]).can_export(), "nothing to export");
        assert!(!State::default().can_export(), "not loaded yet");
    }
    #[test]
    fn the_note_editor_saves_on_submit_hides_its_keyboard_and_fits_short_screens() {
        use crate::gfx::{Canvas, IconRenderer, TextRenderer};
        let mut state = ready(&["first"]);
        state.collection.records[0].note = Some("Old\nnote".into());
        let mut text = TextRenderer::new();
        let mut icons = IconRenderer::default();
        for (w, h) in [(1072, 1448), (1448, 1072), (1272, 1696), (1696, 1272)] {
            let t = Theme::for_dpi(300);
            let screen = Rect::new(0, 0, w, h);
            let mut view = View::new(&t, screen, &state, None);
            view.detail(0);
            view.edit_note(NoteTarget::Existing(0), &state, &t);
            assert!(view.editing_note() && view.keyboard.visible);
            assert_eq!(view.note.value(), "Old\nnote", "existing text (with its break) is editable");
            let mut canvas = Canvas::new(w as u32, h as u32);
            view.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut Vec::new()), &state);
            let area = view.note.rect();
            assert!(area.h >= 3 * t.body.line_height() as i32, "{w}x{h}: room to type");
            assert!(area.bottom() <= view.keyboard.rect().y, "{w}x{h}: above the keyboard");
            assert!(matches!(view.input(&TextInput::Insert(" more".into())), (true, None)));
            assert!(matches!(view.input(&TextInput::Dismiss), (true, None)));
            assert!(!view.keyboard.visible && view.editing_note(), "Hide keeps the editor");
            view.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut Vec::new()), &state);
            assert!(view.note.rect().bottom() <= view.save_note.rect().y);
            assert!(view.save_note.rect().h >= t.target);
            assert!(matches!(view.input(&TextInput::Insert("x".into())), (false, None)), "unfocused");
            view.focus_note(true, &t);
            match view.input(&TextInput::Submit) {
                (true, Some(Event::SaveNote(NoteTarget::Existing(0), text))) => assert_eq!(text, "Old\nnote more"),
                _ => panic!("Submit saves"),
            }
            view.close_note(&t);
            assert!(!view.editing_note() && !view.keyboard.visible);
        }
    }
}
