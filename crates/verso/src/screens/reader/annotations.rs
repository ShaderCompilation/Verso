use crate::{
    annotations::{
        Collection,
        worker::{self, Preview},
    },
    document::thread::{Command, Reply, Resolved, Worker},
    geom::Rect,
    gfx::HAlign,
    platform::TextInput,
    screens::EventCx,
    ui::{
        gesture::Gesture,
        theme::{PAPER, Theme},
        widget::DrawCx,
        widgets::{Button, ButtonKind, Keyboard, List, ListEvent, Pager, Row, TextField, TopBar},
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
    /// Reading or parsing the chosen KOReader file failed. Shown only in the
    /// import view; it never turns Import into Retry saving.
    pub import_error: Option<String>,
    pub preview: Option<Preview>,
    pub preview_resolved: Vec<Resolved>,
    pub import_id: u64,
    pub importing: bool,
}
impl State {
    pub fn resolve(&mut self, generation: u64, worker: &Worker) {
        self.resolve_with(generation, &mut |c| worker.send(c));
    }
    fn resolve_with(&mut self, generation: u64, send: &mut dyn FnMut(Command)) {
        if !self.ready {
            return;
        }
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
                        Some(if retried { "Highlights saved after retry" } else { "Highlights saved" }.into())
                    }
                    Err(e) => {
                        self.save_error = Some(e.clone());
                        Some(format!("Couldn't save highlights: {e}"))
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
}
enum Mode {
    List,
    Detail(usize),
    Import,
}
pub(super) struct View {
    top: TopBar,
    list: List,
    pager: Pager,
    ids: Vec<usize>,
    mode: Mode,
    import: Button,
    primary: Button,
    remove: Button,
    retry: Button,
    field: TextField,
    keyboard: Keyboard,
    body: Rect,
    detail_pages: Vec<String>,
    preview_path: Option<PathBuf>,
}
impl View {
    pub fn new(t: &Theme, screen: Rect, state: &State, ids: Option<Vec<usize>>) -> Self {
        let mut top = TopBar::new("Highlights", true);
        top.place(screen, t);
        let mut pager = Pager::new(1);
        pager.place(Rect::new(t.margin, screen.bottom() - t.mm(11.0), screen.w - 2 * t.margin, t.mm(11.0)), t);
        let mut import = Button::new("Import from KOReader", ButtonKind::Secondary);
        import.place(Rect::new(t.margin, t.top_bar + t.mm(2.0), screen.w - 2 * t.margin, t.target), t);
        let mut primary = Button::new("Jump to passage", ButtonKind::Primary);
        let mut remove = Button::new("Remove", ButtonKind::Secondary);
        let width = (screen.w - 2 * t.margin - t.mm(3.0)) / 2;
        let y = pager.rect().y - t.target - t.mm(3.0);
        primary.place(Rect::new(t.margin, y, width, t.target), t);
        remove.place(Rect::new(screen.w - t.margin - width, y, width, t.target), t);
        let mut retry = Button::new("Retry saving", ButtonKind::Secondary);
        retry.place(import.rect(), t);
        let body = Rect::new(
            t.margin,
            import.rect().bottom() + t.mm(3.0),
            screen.w - 2 * t.margin,
            y - import.rect().bottom() - t.mm(5.0),
        );
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
            primary,
            remove,
            retry,
            field,
            keyboard,
            body,
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
    pub fn refresh(&mut self, state: &State, ids: Option<Vec<usize>>, t: &Theme) {
        self.ids = ids.unwrap_or_else(|| {
            if state.resolved.len() == state.collection.records.len() {
                state.resolved.iter().map(|e| e.index).collect()
            } else {
                (0..state.collection.records.len()).collect()
            }
        });
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
                    Row::new(a.quotation.split_whitespace().collect::<Vec<_>>().join(" "))
                        .sub(format!("{}{status}", a.chapter))
                })
            })
            .collect();
        self.list.place(self.body, t.row + t.mm(9.0), 0);
        self.pager.set(self.pager.page, self.list.paging().pages());
    }
    pub fn draw(&mut self, cx: &mut DrawCx, state: &State) {
        let t = cx.theme;
        cx.canvas.fill(PAPER);
        self.top.draw(cx);
        match self.mode {
            Mode::List => {
                self.import.enabled = state.ready && !state.pending && !state.resolving;
                if state.save_error.is_some() && state.ready {
                    self.retry.draw(cx);
                } else {
                    self.import.draw(cx);
                }
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
            }
            Mode::Detail(index) => {
                if let Some(a) = state.collection.records.get(index) {
                    if self.detail_pages.is_empty() {
                        let text = format!(
                            "{}\n{}\n\n{}{}{}",
                            a.chapter,
                            a.created,
                            a.quotation,
                            a.note.as_ref().map(|_| "\n\nImported note (read-only)\n").unwrap_or(""),
                            a.note.as_deref().unwrap_or("")
                        );
                        self.detail_pages = cx.text.pages(&text, t.body, self.body.w, self.body.h);
                    }
                    self.pager.set(self.pager.page, self.detail_pages.len());
                    if let Some(text) = self.detail_pages.get(self.pager.page) {
                        cx.paragraph(text, t.body, self.body, HAlign::Left);
                    }
                    self.primary.label = "Jump to passage".into();
                    self.primary.enabled = state.resolved.iter().any(|e| e.index == index && e.page.is_some());
                    self.primary.draw(cx);
                    self.remove.enabled = !state.pending;
                    self.remove.draw(cx);
                    self.pager.draw(cx);
                }
            }
            Mode::Import => {
                self.field.draw(cx);
                let stale = self.preview_path.as_ref().is_none_or(|p| p != std::path::Path::new(self.field.value()));
                let status = if state.importing {
                    "Reading and validating…".into()
                } else if let Some(e) = &state.import_error {
                    format!("Couldn't read this file: {e}\n\nCheck the path and preview again.")
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
        }
    }
    pub fn input(&mut self, input: &TextInput) -> bool {
        if !self.field.focused || !matches!(self.mode, Mode::Import) {
            return false;
        }
        match input {
            TextInput::Dismiss | TextInput::Submit => {
                self.field.focused = false;
                self.keyboard.visible = false;
            }
            _ => {
                self.field.edit(input);
            }
        }
        true
    }
    pub fn gesture(&mut self, g: &Gesture, state: &State, t: &Theme) -> Option<Event> {
        if self.keyboard.visible {
            if let Some(i) = self.keyboard.on_gesture(g, t) {
                self.input(&i);
            }
            return None;
        }
        if self.top.on_gesture(g) {
            return match self.mode {
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
            };
        }
        match self.mode {
            Mode::List => {
                if state.save_error.is_some() && state.ready && self.retry.on_gesture(g) {
                    return Some(Event::Retry);
                }
                if self.import.on_gesture(g) {
                    self.mode = Mode::Import;
                    self.preview_path = Some(self.field.value().into());
                    return Some(Event::Preview(self.field.value().into()));
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
            Mode::Detail(i) => {
                self.pager.on_gesture(g);
                if self.primary.on_gesture(g) {
                    return Some(Event::Jump(i));
                }
                if self.remove.on_gesture(g) {
                    return Some(Event::Remove(i));
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
        assert_eq!(message.as_deref(), Some("Highlights saved after retry"));
        assert!(state.save_error.is_none());
        assert!(state.importing, "queued import state survives the save acknowledgement");
        assert!(sent.is_empty());
    }
}
