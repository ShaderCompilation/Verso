use crate::{
    annotations::{Annotation, Collection, Kind, NOTE_LIMIT, export::Export, import::Preview, worker},
    document::{
        Opening,
        thread::{Command, OpeningRequest, Reply, Resolved, Worker},
    },
    geom::Rect,
    gfx::{HAlign, Icon},
    platform::TextInput,
    screens::EventCx,
    ui::{
        gesture::Gesture,
        theme::{INK2, PAPER, Theme},
        widget::DrawCx,
        widgets::{Button, ButtonKind, Keyboard, List, ListEvent, Pager, Row, Segmented, TextArea, TextField, TopBar},
    },
};
use std::path::PathBuf;
#[derive(Default)]
pub(super) struct State {
    pub service: Option<worker::Worker>,
    pub collection: Collection,
    pub revision: u64,
    /// The `Page::annotation_revision` of pages painted with the current
    /// records, from the last resolution (`Reply::Resolved::painted`).
    pub painted: u64,
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
    /// A page bookmark waiting for its page's opening words, with the
    /// resolution revision it was asked under.
    pub bookmark_request: Option<(OpeningRequest, u64)>,
    /// The last opening-words request ID; IDs are never reused.
    bookmark_id: u64,
    /// The running save toggled a bookmark: its success shows no "Saved"
    /// (the mark or the ribbon is the confirmation). Failures still show.
    quiet: bool,
    /// The KOReader check's reply, for the reader to take
    /// (`Reader::koreader_checked`).
    pub koreader: Option<Result<Option<(crate::koreader::OnOpen, crate::koreader::Progress)>, String>>,
}
/// What a KOReader import preview found (`State::preview_counts`).
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct ImportCounts {
    /// New highlights, including unresolved ones (imported all the same).
    pub highlights: usize,
    pub unresolved_highlights: usize,
    /// New page bookmarks that resolved; only these are imported.
    pub bookmarks: usize,
    pub unresolved_bookmarks: usize,
    pub duplicates: usize,
    pub unsupported: usize,
}
/// What toggling the bookmark on a page did or needs.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Toggle {
    /// Every bookmark on the page was removed.
    Removed,
    /// A bookmark on the page has a note: ask before removing it.
    Confirm,
    /// The page's opening words were asked for; the bookmark is added when
    /// they arrive (`State::receive_opening`).
    Requested,
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
        // The book thread drops a request from an older layout unanswered,
        // and a reply under an older resolution would be dropped here anyway.
        self.bookmark_request = None;
        self.resolving = true;
        self.revision += 1;
        self.resolved.clear();
        send(Command::Resolve {
            generation,
            revision: self.revision,
            records: self.collection.records.clone(),
            fingerprint: self.fingerprint(),
            preview: false,
        });
    }
    /// The open book's content fingerprint, once loaded.
    fn fingerprint(&self) -> String {
        self.service.as_ref().map(|w| w.book.fingerprint.clone()).unwrap_or_default()
    }
    /// The saved index of highlight `number`, counting highlights only in
    /// saved order (what `Page::annotations` holds).
    pub fn highlight(&self, number: usize) -> Option<usize> {
        self.collection
            .records
            .iter()
            .enumerate()
            .filter(|(_, a)| a.kind == Kind::Highlight)
            .nth(number)
            .map(|(i, _)| i)
    }
    /// The saved indices of the page bookmarks on `page` in the current
    /// layout. Empty while a resolution runs: pages are known only after it.
    pub fn bookmarks_on(&self, page: usize) -> Vec<usize> {
        if self.resolving || self.resolved.len() != self.collection.records.len() {
            return vec![];
        }
        self.resolved
            .iter()
            .filter(|e| e.page == Some(page))
            .map(|e| e.index)
            .filter(|&i| self.collection.records.get(i).is_some_and(|a| a.kind == Kind::Bookmark))
            .collect()
    }
    /// Whether a page rendered under `revision` paints the current records'
    /// highlights. Never while a resolution runs: their numbers may change.
    pub fn paints_current(&self, revision: u64) -> bool {
        !self.resolving && revision == self.painted
    }
    /// Whether a resolved highlight covers `page` in the current layout.
    pub fn highlighted(&self, page: usize) -> bool {
        self.resolved.iter().any(|e| {
            e.page.is_some_and(|first| (first..=e.last).contains(&page))
                && self.collection.records.get(e.index).is_some_and(|a| a.kind == Kind::Highlight)
        })
    }
    /// Whether `page` shows the bookmark mark.
    pub fn bookmarked(&self, page: usize) -> bool {
        !self.bookmarks_on(page).is_empty()
    }
    /// The saved entries are still loading or being checked for this layout
    /// (the bookmark button is dimmed meanwhile).
    pub fn checking(&self) -> bool {
        !self.ready || self.resolving
    }
    /// True when a page's bookmark can be toggled now: the saved entries are
    /// loaded and resolved for this layout, and nothing is being saved.
    pub fn can_toggle(&self) -> bool {
        self.can_save() && !self.resolving && self.bookmark_request.is_none()
    }
    /// Toggles the bookmark on `page`. A page with bookmarks loses all of
    /// them, so one toggle clears what the page shows; when any has a note,
    /// that waits for `confirmed`. A page without one asks for its opening
    /// words. Toggling never re-renders a page: the mark is drawn over it.
    pub fn toggle_bookmark(
        &mut self,
        generation: u64,
        page: usize,
        confirmed: bool,
        worker: &Worker,
    ) -> Result<Toggle, &'static str> {
        self.toggle_with(generation, page, confirmed, &mut |c| worker.send(c))
    }
    fn toggle_with(
        &mut self,
        generation: u64,
        page: usize,
        confirmed: bool,
        send: &mut dyn FnMut(Command),
    ) -> Result<Toggle, &'static str> {
        if !self.can_toggle() {
            return Err(NOT_READY);
        }
        let marks = self.bookmarks_on(page);
        if marks.is_empty() {
            self.bookmark_id += 1;
            let request = OpeningRequest { generation, page, id: self.bookmark_id };
            self.bookmark_request = Some((request, self.revision));
            send(Command::Opening(request));
            return Ok(Toggle::Requested);
        }
        if !confirmed && marks.iter().any(|&i| self.collection.records[i].note.is_some()) {
            return Ok(Toggle::Confirm);
        }
        self.remove_resolved(&marks);
        self.save();
        self.quiet = true;
        Ok(Toggle::Removed)
    }
    /// Removes saved entries without a new resolution, keeping the resolved
    /// table in step: later indices shift down. Only for entries that are
    /// never painted (page bookmarks), so cached pages stay valid.
    fn remove_resolved(&mut self, indices: &[usize]) {
        let mut indices = indices.to_vec();
        indices.sort_unstable_by(|a, b| b.cmp(a));
        indices.dedup();
        for i in indices {
            let id = self.collection.records[i].id.clone();
            self.collection.remove(&id);
            self.resolved.retain(|e| e.index != i);
            for e in &mut self.resolved {
                if e.index > i {
                    e.index -= 1;
                }
            }
        }
    }
    /// Adds the bookmark a toggle asked for, once its page's opening words
    /// arrive. Replies for another request, an older layout or an older
    /// resolution are dropped, and so is a page that gained a bookmark
    /// meanwhile. Returns a message to show briefly.
    pub fn receive_opening(
        &mut self,
        request: OpeningRequest,
        result: Result<Opening, String>,
        generation: u64,
        chapter: String,
    ) -> Option<String> {
        let (pending, revision) = self.bookmark_request?;
        if pending != request {
            return None;
        }
        self.bookmark_request = None;
        if request.generation != generation || revision != self.revision || self.resolving {
            return None;
        }
        let opening = match result {
            Ok(o) => o,
            Err(e) => return Some(format!("Couldn't bookmark the page: {e}")),
        };
        if !self.bookmarks_on(request.page).is_empty() {
            return None;
        }
        let book = self.service.as_ref().map(|w| w.book.annotation_id.clone()).unwrap_or_default();
        let created = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        let id = crate::annotations::digest(format!("{book}:{created}:bookmark:{}", opening.range.start.0).as_bytes());
        let bookmark = Annotation {
            version: 1,
            kind: Kind::Bookmark,
            id,
            book,
            fingerprint: self.fingerprint(),
            range: opening.range,
            quotation: opening.text,
            created: crate::annotations::created_now(),
            chapter,
            note: None,
            provenance: None,
        };
        if let Err(message) = self.add(bookmark) {
            return Some(message.into());
        }
        self.quiet = true;
        // In reading order: the page start comes before anything starting on
        // the page, and unresolved entries stay last.
        let index = self.collection.records.len() - 1;
        let at =
            self.resolved.iter().position(|e| e.page.is_none_or(|p| p >= request.page)).unwrap_or(self.resolved.len());
        self.resolved.insert(at, Resolved { index, page: Some(request.page), last: request.page });
        None
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
    /// Removes saved entry `index` and starts saving. A page bookmark leaves
    /// the resolution as it is (nothing painted changes, so no page is
    /// rendered again); a highlight, or any entry while a resolution runs,
    /// starts a fresh one.
    pub fn remove(&mut self, index: usize, generation: u64, worker: &Worker) -> bool {
        self.remove_with(index, generation, &mut |c| worker.send(c))
    }
    fn remove_with(&mut self, index: usize, generation: u64, send: &mut dyn FnMut(Command)) -> bool {
        let Some(a) = self.collection.records.get(index) else { return false };
        if a.kind == Kind::Bookmark && !self.resolving && self.resolved.len() == self.collection.records.len() {
            self.remove_resolved(&[index]);
            self.save();
        } else {
            let id = a.id.clone();
            self.collection.remove(&id);
            self.save();
            self.resolve_with(generation, send);
        }
        true
    }
    /// The page of each saved entry in the current layout, by saved index;
    /// None while locating and for unresolved entries.
    pub fn pages(&self) -> Vec<Option<usize>> {
        let mut pages = vec![None; self.collection.records.len()];
        if !self.resolving {
            for e in &self.resolved {
                if let Some(p) = pages.get_mut(e.index) {
                    *p = e.page;
                }
            }
        }
        pages
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
        let pages = self.pages();
        let (records, pages) = self
            .ordered()
            .into_iter()
            .filter_map(|i| Some((self.collection.records.get(i)?.clone(), pages.get(i).copied().flatten())))
            .unzip();
        self.exporting = true;
        self.export_result = None;
        if let Some(w) = &self.service {
            w.send(worker::Command::Export(Export { title, author, records, pages }));
        }
        true
    }
    pub fn save(&mut self) {
        self.pending = true;
        self.quiet = false;
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
                let quiet = std::mem::take(&mut self.quiet);
                match result {
                    Ok(()) => {
                        let retried = self.save_error.take().is_some();
                        (retried || !quiet).then(|| if retried { "Saved after retry" } else { "Saved" }.into())
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
                        let fingerprint = self.fingerprint();
                        send(Command::Resolve {
                            generation,
                            revision: id,
                            records: p.records.clone(),
                            fingerprint,
                            preview: true,
                        });
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
            worker::Reply::Koreader(result) => {
                self.koreader = Some(result);
                None
            }
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
        if let Reply::Resolved { revision, entries, openings, preview, painted, .. } = reply {
            if preview {
                if revision == self.import_id && self.importing {
                    // Imported page bookmarks store the opening words at their
                    // position, so later checks work as for Verso's own.
                    if let Some(p) = &mut self.preview {
                        for (i, opening) in openings {
                            if let Some(a) = p.records.get_mut(i).filter(|a| a.kind == Kind::Bookmark) {
                                a.range = opening.range;
                                a.quotation = opening.text;
                            }
                        }
                    }
                    self.preview_resolved = entries;
                    self.importing = false;
                    return true;
                }
            } else if revision == self.revision {
                self.resolved = entries;
                self.painted = painted;
                self.resolving = false;
                self.place_imported(openings);
                return true;
            }
        }
        false
    }
    /// Saves the bulk-imported page bookmarks the resolution just placed at
    /// their opening words, and removes the ones whose position isn't in the
    /// book (their import identity stays, so they aren't imported again).
    /// The resolution stays valid: bookmarks aren't painted.
    fn place_imported(&mut self, openings: Vec<(usize, Opening)>) {
        let fingerprint = self.fingerprint();
        let mut changed = false;
        for (i, opening) in openings {
            if let Some(old) = self.collection.records.get(i).filter(|a| a.awaits_opening()) {
                let (id, placed) = (old.id.clone(), old.placed(opening, &fingerprint));
                changed |= self.collection.replace(&id, placed);
            }
        }
        let lost: Vec<_> = self
            .resolved
            .iter()
            .filter(|e| {
                e.page.is_none() && self.collection.records.get(e.index).is_some_and(Annotation::awaits_opening)
            })
            .map(|e| e.index)
            .collect();
        changed |= !lost.is_empty();
        self.remove_resolved(&lost);
        if changed {
            self.save();
            self.quiet = true;
        }
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
    /// The finished import preview's counts by kind. An unresolved highlight
    /// is still imported (listed as unresolved); an unresolved bookmark has
    /// no opening words to store, so it is left out and not counted as new.
    pub fn preview_counts(&self) -> ImportCounts {
        let Some(p) = &self.preview else { return ImportCounts::default() };
        let unresolved = self.preview_unresolved();
        let mut counts = ImportCounts { duplicates: p.duplicates, unsupported: p.unsupported, ..Default::default() };
        for (a, lost) in p.records.iter().zip(unresolved) {
            match (a.kind, lost) {
                (Kind::Highlight, false) => counts.highlights += 1,
                (Kind::Highlight, true) => {
                    counts.highlights += 1;
                    counts.unresolved_highlights += 1;
                }
                (Kind::Bookmark, false) => counts.bookmarks += 1,
                (Kind::Bookmark, true) => counts.unresolved_bookmarks += 1,
            }
        }
        counts
    }
    /// Which preview records didn't resolve, by index.
    fn preview_unresolved(&self) -> Vec<bool> {
        let n = self.preview.as_ref().map_or(0, |p| p.records.len());
        let mut unresolved = vec![false; n];
        for e in self.preview_resolved.iter().filter(|e| e.page.is_none()) {
            if let Some(u) = unresolved.get_mut(e.index) {
                *u = true;
            }
        }
        unresolved
    }
    pub fn confirm(&mut self, generation: u64, worker: &Worker) {
        self.confirm_with(generation, &mut |c| worker.send(c));
    }
    fn confirm_with(&mut self, generation: u64, send: &mut dyn FnMut(Command)) {
        if self.importing {
            return;
        }
        let unresolved = self.preview_unresolved();
        if let Some(p) = self.preview.take() {
            for (i, a) in p.records.into_iter().enumerate() {
                if a.kind == Kind::Bookmark && unresolved[i] {
                    continue;
                }
                self.collection.add(a);
            }
            self.save();
            self.resolve_with(generation, send);
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
/// A bookmark's opening words for its row and details, or what stands in
/// for them on a page without text.
fn bookmark_label(opening: &str) -> String {
    let words = opening.split_whitespace().collect::<Vec<_>>().join(" ");
    if words.is_empty() { "A page without text".into() } else { format!("{words}…") }
}
/// The import preview's status: what is new by kind, what was found
/// before, and what won't show on a page or isn't imported.
fn import_summary(c: &ImportCounts) -> String {
    let count = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let mut text = format!(
        "New: {}, {} · {} already imported · {} unresolved · {} unsupported",
        count(c.highlights, "highlight", "highlights"),
        count(c.bookmarks, "bookmark", "bookmarks"),
        c.duplicates,
        c.unresolved_highlights + c.unresolved_bookmarks,
        c.unsupported,
    );
    // One paragraph after the counts, so the text stays clear of the
    // Confirm button on 6" screens in landscape.
    text.push_str("\n\n");
    if c.unresolved_highlights > 0 {
        text.push_str("Unresolved highlights are imported without a page. ");
    }
    if c.unresolved_bookmarks > 0 {
        text.push_str("Unresolved bookmarks are left out. ");
    }
    text.push_str("One-way import. KOReader files stay untouched.");
    text
}
/// The Notes list's filter segments, in order.
const FILTERS: [&str; 3] = ["All", "Highlights", "Bookmarks"];
pub(super) struct View {
    top: TopBar,
    list: List,
    pager: Pager,
    /// All · Highlights · Bookmarks; hidden for a tapped overlap subset.
    filter: Segmented,
    ids: Vec<usize>,
    /// True when opened for a tapped overlap subset rather than the full list.
    subset: bool,
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
    /// The list's rows: below the filter, down to the export status line.
    list_rect: Rect,
    /// The page shown when the list opened: the list starts near it.
    here: Option<usize>,
    detail_body: Rect,
    detail_pages: Vec<String>,
    preview_path: Option<PathBuf>,
}
impl View {
    /// The Notes list, or (`ids`) just the highlights under a tap. It opens
    /// on the pager page holding the entry nearest page `here`.
    pub fn new(t: &Theme, screen: Rect, state: &State, ids: Option<Vec<usize>>, here: Option<usize>) -> Self {
        let mut top = TopBar::new("Notes", true);
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
        let mut filter = Segmented::texts(&FILTERS, 0);
        filter.place(Rect::new(t.margin, body.y, body.w, t.target));
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
            subset: ids.is_some(),
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
            list_rect: Rect::default(),
            here,
            detail_body,
            detail_pages: vec![],
            preview_path: None,
            filter,
        };
        v.place_list(t);
        v.refresh(state, ids, t);
        v.show_here(state);
        v
    }
    /// The rows' area: under the filter (or right under the buttons for a
    /// subset, which has none), leaving room for the export status line,
    /// whose path can wrap once.
    fn place_list(&mut self, t: &Theme) {
        let top = if self.subset { self.body.y } else { self.filter.rect().bottom() + t.mm(3.0) };
        let bottom = self.pager.rect().y - 2 * t.small.line_height() as i32 - t.mm(2.0);
        self.list_rect = Rect::new(self.body.x, top, self.body.w, bottom - top);
    }
    /// Turns the list to the pager page holding the entry nearest the page
    /// the list was opened from: the last one starting on or before it, or
    /// else the first after it.
    fn show_here(&mut self, state: &State) {
        let pages = state.pages();
        let located: Vec<(usize, usize)> =
            self.ids.iter().enumerate().filter_map(|(k, &i)| pages.get(i).copied().flatten().map(|p| (k, p))).collect();
        let row = self.here.and_then(|here| {
            located.iter().rev().find(|(_, p)| *p <= here).or_else(|| located.first()).map(|(k, _)| *k)
        });
        let page = self.list.paging().page_of(row.unwrap_or(0));
        self.pager.set(page, self.list.paging().pages());
        self.list.page = self.pager.page;
    }
    /// Back to the full list after an entry was removed (indices shifted, so
    /// a tapped overlap subset no longer applies), keeping the filter.
    pub fn after_remove(&mut self, state: &State, t: &Theme) {
        self.subset = false;
        self.place_list(t);
        self.mode = Mode::List;
        self.refresh(state, None, t);
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
    /// Refreshes after a worker reply. A view opened for a tapped overlap
    /// keeps that subset; the full list refreshes from the whole collection so
    /// opening it while highlights are still loading populates once ready.
    pub fn refresh_keep(&mut self, state: &State, t: &Theme) {
        let ids =
            self.subset.then(|| self.ids.iter().copied().filter(|i| *i < state.collection.records.len()).collect());
        self.refresh(state, ids, t);
    }
    pub fn refresh(&mut self, state: &State, ids: Option<Vec<usize>>, t: &Theme) {
        let records = &state.collection.records;
        let kind = match self.filter.selected {
            1 => Some(Kind::Highlight),
            2 => Some(Kind::Bookmark),
            _ => None,
        };
        self.ids = ids.unwrap_or_else(|| {
            state
                .ordered()
                .into_iter()
                .filter(|&i| kind.is_none_or(|k| records.get(i).is_some_and(|a| a.kind == k)))
                .collect()
        });
        let pages = state.pages();
        self.list.rows = self
            .ids
            .iter()
            .filter_map(|i| {
                state.collection.records.get(*i).map(|a| {
                    // A fresh resolution batch clears earlier results; until it
                    // finishes, no row is known to be unresolved.
                    let status = if state.resolving {
                        "Locating…".to_string()
                    } else if let Some(page) = pages.get(*i).copied().flatten() {
                        format!("p. {}", page + 1)
                    } else {
                        "Unresolved".to_string()
                    };
                    let one_line = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
                    let mut chapter = a.chapter.trim().to_string();
                    if a.note.is_some() && chapter.chars().count() > 24 {
                        // Keep a long chapter title from hiding the note.
                        chapter = format!("{}…", chapter.chars().take(24).collect::<String>().trim_end());
                    }
                    let mut sub: Vec<String> = [chapter, status].into_iter().filter(|s| !s.is_empty()).collect();
                    if let Some(note) = a.note.as_deref() {
                        sub.push(format!("Note: {}", one_line(note)));
                    }
                    let (icon, label) = match a.kind {
                        Kind::Highlight => (Icon::Highlighter, one_line(&a.quotation)),
                        Kind::Bookmark => (Icon::BookmarkFilled, bookmark_label(&a.quotation)),
                    };
                    Row::new(label).sub(sub.join(" · ")).icon(icon)
                })
            })
            .collect();
        self.list.place(self.list_rect, t.row2, 0);
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
        self.top.title = if self.editing_note() { "Note" } else { "Notes" }.into();
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
                if !self.subset {
                    self.filter.draw(cx);
                }
                self.list.page = self.pager.page;
                self.list.draw(cx);
                self.pager.draw(cx);
                if self.ids.is_empty() {
                    let empty = match self.filter.selected {
                        1 => "No highlights yet",
                        2 => "No bookmarks yet",
                        _ => "No highlights or bookmarks yet",
                    };
                    cx.paragraph(
                        state.load_error.as_deref().unwrap_or(if state.ready { empty } else { "Loading notes…" }),
                        t.body,
                        self.list_rect,
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
                        self.list_rect.bottom() + t.mm(1.0),
                        self.list_rect.w,
                        self.pager.rect().y - self.list_rect.bottom() - t.mm(1.0),
                    );
                    cx.paragraph(&status, t.small.gray(INK2), r, HAlign::Left);
                }
            }
            &Mode::Detail(index) => {
                if let Some(a) = state.collection.records.get(index) {
                    if self.detail_pages.is_empty() {
                        let page = state.pages().get(index).copied().flatten().map(|p| format!("p. {}", p + 1));
                        let place: Vec<&str> = [Some(a.chapter.trim()), page.as_deref()]
                            .into_iter()
                            .flatten()
                            .filter(|s| !s.is_empty())
                            .collect();
                        let text = format!(
                            "{}\n{}\n\n{}{}{}",
                            place.join(" · "),
                            a.created,
                            match a.kind {
                                Kind::Highlight => a.quotation.clone(),
                                Kind::Bookmark => format!("Bookmark · {}", bookmark_label(&a.quotation)),
                            },
                            a.note.as_ref().map(|_| "\n\nNote\n").unwrap_or(""),
                            a.note.as_deref().unwrap_or("")
                        );
                        self.detail_pages = cx.text.pages(&text, t.body, self.detail_body.w, self.detail_body.h);
                    }
                    self.pager.set(self.pager.page, self.detail_pages.len());
                    if let Some(text) = self.detail_pages.get(self.pager.page) {
                        cx.paragraph(text, t.body, self.detail_body, HAlign::Left);
                    }
                    self.primary.label =
                        if a.kind == Kind::Bookmark { "Jump to page" } else { "Jump to passage" }.into();
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
                } else if state.preview.is_some() {
                    import_summary(&state.preview_counts())
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
                    // Back to the list page the entry was opened from.
                    self.mode = Mode::List;
                    self.pager.set(self.list.page, self.list.paging().pages());
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
                if !self.subset && self.filter.on_gesture(g).is_some() {
                    self.refresh(state, None, t);
                    self.show_here(state);
                    return None;
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
    /// The current touch started on the bar (and not on a selection handle).
    owns: bool,
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
        Self { buttons, rect, owns: false }
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
    /// Returns whether the bar owns this touch. Ownership is decided by where
    /// the touch starts, so a drag that began on a selection handle (which
    /// takes priority over the buttons under it) is never swallowed on its way
    /// through the strip, and a touch that began on the bar never reaches the
    /// selection.
    pub fn gesture(&mut self, g: &Gesture, on_handle: bool) -> (bool, Option<usize>) {
        if let Gesture::Down(p) = *g {
            self.owns = self.rect.contains(p) && !on_handle;
        }
        if !self.owns {
            return (false, None);
        }
        if g.ends_touch() {
            self.owns = false;
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
            entries: vec![Resolved { index: 0, page: Some(3), last: 3 }],
            openings: vec![],
            preview,
            painted: 0,
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
                entries: vec![Resolved { index: 0, page: Some(3), last: 3 }],
                openings: vec![],
                preview,
                painted: 0,
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
    /// A loaded, resolved state: highlights "h0", "h1" on pages 2 and 6, and
    /// bookmarks "b0" (p. 4) and "b1", "b2" (both p. 6 after a size change).
    fn with_bookmarks() -> State {
        let mut state = ready(&["h0", "b0", "h1", "b1", "b2"]);
        for i in [1, 3, 4] {
            state.collection.records[i].kind = Kind::Bookmark;
        }
        state.resolved = [(0, 2), (1, 4), (3, 6), (4, 6), (2, 6)]
            .into_iter()
            .map(|(index, page)| Resolved { index, page: Some(page), last: page })
            .collect();
        state
    }
    fn opening(start: &str) -> Opening {
        Opening {
            range: crate::document::TextRange {
                start: crate::document::Position(start.into()),
                end: crate::document::Position(format!("{start}/text().20")),
            },
            text: "It was a bright cold".into(),
        }
    }
    #[test]
    fn toggling_adds_one_bookmark_and_removes_every_bookmark_on_the_page() {
        let mut state = with_bookmarks();
        let mut sent = vec![];
        assert_eq!(state.bookmarks_on(6), vec![3, 4]);
        assert!(state.bookmarked(4) && !state.bookmarked(2), "a highlight is not a bookmark");
        assert_eq!((state.highlight(0), state.highlight(1), state.highlight(2)), (Some(0), Some(2), None));

        // One bookmark: removed at once; the highlight numbers cached pages
        // hold still name the same highlights.
        assert_eq!(state.toggle_with(1, 4, false, &mut |c| sent.push(c)), Ok(Toggle::Removed));
        assert!(sent.is_empty() && state.pending, "removing needs no engine call, just a save");
        assert_eq!(state.collection.manifest.records, ["h0", "h1", "b1", "b2"]);
        assert!(!state.bookmarked(4));
        assert_eq!(state.ordered(), vec![0, 2, 3, 1], "reading order kept, indices shifted");
        assert_eq!((state.highlight(0), state.highlight(1)), (Some(0), Some(1)));
        assert_eq!(state.toggle_with(1, 6, false, &mut |c| sent.push(c)), Err(NOT_READY), "while saving");
        state.pending = false;

        // Several bookmarks on one page: all go, after asking when one has a note.
        state.collection.records[3].note = Some("typed".into());
        assert_eq!(state.toggle_with(1, 6, false, &mut |c| sent.push(c)), Ok(Toggle::Confirm));
        assert_eq!(state.collection.records.len(), 4, "nothing removed before confirming");
        assert_eq!(state.toggle_with(1, 6, true, &mut |c| sent.push(c)), Ok(Toggle::Removed));
        assert_eq!(state.collection.manifest.records, ["h0", "h1"]);
        assert_eq!(state.ordered(), vec![0, 1]);
        state.pending = false;

        // No bookmark: the page's opening words are asked for, then added.
        assert_eq!(state.toggle_with(1, 6, false, &mut |c| sent.push(c)), Ok(Toggle::Requested));
        let Some(Command::Opening(request)) = sent.pop() else { panic!("asks for the opening words") };
        assert_eq!((request.generation, request.page), (1, 6));
        assert!(!state.can_toggle(), "one request at a time");
        assert!(state.receive_opening(request, Ok(opening("/p[9]")), 1, "Chapter 2".into()).is_none());
        let added = state.collection.records.last().unwrap();
        assert_eq!(added.kind, Kind::Bookmark);
        assert_eq!((added.quotation.as_str(), added.chapter.as_str()), ("It was a bright cold", "Chapter 2"));
        assert_eq!(added.range.start.0, "/p[9]");
        assert!(state.pending && state.bookmarked(6));
        assert_eq!(state.ordered(), vec![0, 2, 1], "the page start comes before a highlight on that page");
        // Bookmark saves are quiet: the mark is the confirmation. A failure
        // still says so, and a highlight's save still says "Saved".
        assert_eq!(state.apply(worker::Reply::Saved(Ok(())), 1, &mut |c| sent.push(c)), None);
        assert_eq!(state.toggle_with(1, 6, false, &mut |c| sent.push(c)), Ok(Toggle::Removed));
        let failed = state.apply(worker::Reply::Saved(Err("disk full".into())), 1, &mut |c| sent.push(c));
        assert!(failed.unwrap().contains("disk full"));
        state.save();
        assert_eq!(
            state.apply(worker::Reply::Saved(Ok(())), 1, &mut |c| sent.push(c)).as_deref(),
            Some("Saved after retry")
        );
        state.save();
        assert_eq!(state.apply(worker::Reply::Saved(Ok(())), 1, &mut |c| sent.push(c)).as_deref(), Some("Saved"));
        assert_eq!(state.highlight(1), Some(1));
    }
    #[test]
    fn stale_or_failed_opening_replies_add_nothing() {
        let mut state = with_bookmarks();
        let mut sent = vec![];
        let ask = |state: &mut State, page| {
            let mut sent = vec![];
            assert_eq!(state.toggle_with(1, page, false, &mut |c| sent.push(c)), Ok(Toggle::Requested));
            match sent.pop() {
                Some(Command::Opening(r)) => r,
                _ => panic!("asks for the opening words"),
            }
        };
        let count = state.collection.records.len();
        // A newer layout.
        let request = ask(&mut state, 3);
        assert!(state.receive_opening(request, Ok(opening("/p")), 2, String::new()).is_none());
        // A different request.
        let request = ask(&mut state, 3);
        let other = OpeningRequest { id: request.id + 1, ..request };
        assert!(state.receive_opening(other, Ok(opening("/p")), 1, String::new()).is_none());
        assert!(state.bookmark_request.is_some(), "still waiting for its own reply");
        // A resolution started meanwhile (a relayout or another change).
        state.resolve_with(1, &mut |c| sent.push(c));
        assert!(matches!(sent.pop(), Some(Command::Resolve { fingerprint, .. }) if fingerprint.is_empty()));
        assert!(state.bookmark_request.is_none() && state.bookmarks_on(6).is_empty());
        assert!(state.receive_opening(request, Ok(opening("/p")), 1, String::new()).is_none());
        state.receive(Reply::Resolved {
            generation: 1,
            revision: state.revision,
            entries: (0..count).map(|index| Resolved { index, page: Some(1), last: 1 }).collect(),
            openings: vec![],
            preview: false,
            painted: 0,
        });
        // The engine couldn't read the page.
        let request = ask(&mut state, 3);
        let message = state.receive_opening(request, Err("no page".into()), 1, String::new());
        assert!(message.unwrap().contains("no page"));
        assert_eq!(state.collection.records.len(), count);
        assert!(!state.pending && state.can_toggle());
        // Not ready: before loading, and while resolving.
        assert_eq!(State::default().toggle_with(1, 3, false, &mut |c| sent.push(c)), Err(NOT_READY));
        state.resolving = true;
        assert_eq!(state.toggle_with(1, 3, false, &mut |c| sent.push(c)), Err(NOT_READY));
        assert!(sent.is_empty());
    }
    fn entry(id: &str, note: Option<&str>) -> Annotation {
        Annotation {
            version: 1,
            kind: crate::annotations::Kind::Highlight,
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
    fn imported_bookmarks_take_their_opening_words_and_unresolved_ones_are_left_out() {
        let mut state = ready(&["saved"]);
        let mut sent = vec![];
        state.start_import("/metadata.epub.lua".into());
        let id = state.import_id;
        let mut records = vec![entry("highlight", None), entry("bookmark", None), entry("lost", None)];
        for a in &mut records[1..] {
            a.kind = Kind::Bookmark;
            a.range.end = crate::document::Position(String::new());
            a.quotation.clear();
        }
        let preview = Preview { records, duplicates: 4, unsupported: 1 };
        state.apply(worker::Reply::Preview(id, Ok(preview)), 1, &mut |c| sent.push(c));
        assert!(matches!(sent.pop(), Some(Command::Resolve { preview: true, .. })));
        let found = [(0, None), (1, Some(7)), (2, None)];
        assert!(state.receive(Reply::Resolved {
            generation: 1,
            revision: id,
            entries: found.into_iter().map(|(index, page)| Resolved { index, page, last: page.unwrap_or(0) }).collect(),
            openings: vec![(1, opening("/body/p[3]"))],
            preview: true,
            painted: 0,
        }));
        let counts = state.preview_counts();
        let expected = ImportCounts {
            highlights: 1,
            unresolved_highlights: 1,
            bookmarks: 1,
            unresolved_bookmarks: 1,
            duplicates: 4,
            unsupported: 1,
        };
        assert_eq!(counts, expected);
        let summary = import_summary(&counts);
        assert!(
            summary.starts_with("New: 1 highlight, 1 bookmark · 4 already imported · 2 unresolved · 1 unsupported")
        );
        assert!(summary.contains("imported without a page") && summary.contains("are left out"));
        assert!(!import_summary(&ImportCounts { bookmarks: 1, ..Default::default() }).contains("left out"));
        state.confirm_with(1, &mut |c| sent.push(c));
        let ids: Vec<_> = state.collection.records.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["saved", "highlight", "bookmark"], "the unresolved highlight stays; the bookmark doesn't");
        let b = &state.collection.records[2];
        assert_eq!(b.quotation, "It was a bright cold");
        assert_eq!(b.range, opening("/body/p[3]").range);
        assert!(!state.collection.manifest.sources.contains("lost"), "a later import can try it again");
        assert!(state.pending);
        assert!(matches!(sent.pop(), Some(Command::Resolve { preview: false, .. })));
    }
    #[test]
    fn bulk_imported_bookmarks_are_placed_on_open_and_lost_ones_removed() {
        let mut state = ready(&["h", "placed", "lost", "own"]);
        for (i, a) in state.collection.records.iter_mut().enumerate().skip(1) {
            a.kind = Kind::Bookmark;
            a.range.end = crate::document::Position(String::new());
            a.quotation.clear();
            if i < 3 {
                a.fingerprint.clear();
            }
        }
        let mut sent = vec![];
        state.resolve_with(1, &mut |c| sent.push(c));
        let found = [(0, Some(2)), (1, Some(5)), (2, None), (3, None)];
        assert!(state.receive(Reply::Resolved {
            generation: 1,
            revision: state.revision,
            entries: found.into_iter().map(|(index, page)| Resolved { index, page, last: page.unwrap_or(0) }).collect(),
            openings: vec![(1, opening("/body/p[3]"))],
            preview: false,
            painted: 0,
        }));
        let records = &state.collection.records;
        assert_eq!(records.len(), 3, "the lost import is removed; Verso's own unresolved bookmark stays");
        let placed = &records[1];
        assert_ne!(placed.id, "placed", "a new record");
        assert_eq!(
            (placed.range.clone(), placed.quotation.as_str()),
            (opening("/body/p[3]").range, "It was a bright cold")
        );
        assert_eq!(records[2].id, "own");
        assert!(state.collection.manifest.sources.contains("lost"), "not imported again");
        assert_eq!(state.pages(), [Some(2), Some(5), None]);
        assert!(state.pending && state.quiet, "saved without a message");
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
        let t = Theme::for_dpi(300);
        for (w, h) in [(1072, 1448), (1448, 1072), (1272, 1696), (1696, 1272)] {
            let screen = Rect::new(0, 0, w, h);
            let mut view = View::new(&t, screen, &state, None, None);
            view.detail(0);
            view.edit_note(NoteTarget::Existing(0), &state, &t);
            let mut canvas = Canvas::new(w as u32, h as u32);
            view.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut Vec::new()), &state);
            let area = view.note.rect();
            assert!(area.h >= 3 * t.body.line_height() as i32, "{w}x{h}: room to type");
            assert!(area.bottom() <= view.keyboard.rect().y, "{w}x{h}: above the keyboard");
            view.input(&TextInput::Dismiss);
            view.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut Vec::new()), &state);
            assert!(view.note.rect().bottom() <= view.save_note.rect().y, "{w}x{h}");
            assert!(view.save_note.rect().h >= t.target, "{w}x{h}");
        }
        // The flow, once.
        let mut view = View::new(&t, Rect::new(0, 0, 1072, 1448), &state, None, None);
        view.detail(0);
        view.edit_note(NoteTarget::Existing(0), &state, &t);
        assert!(view.editing_note() && view.keyboard.visible);
        assert_eq!(view.note.value(), "Old\nnote", "existing text (with its break) is editable");
        assert!(matches!(view.input(&TextInput::Insert(" more".into())), (true, None)));
        assert!(matches!(view.input(&TextInput::Dismiss), (true, None)));
        assert!(!view.keyboard.visible && view.editing_note(), "Hide keeps the editor");
        assert!(matches!(view.input(&TextInput::Insert("x".into())), (false, None)), "unfocused");
        view.focus_note(true, &t);
        match view.input(&TextInput::Submit) {
            (true, Some(Event::SaveNote(NoteTarget::Existing(0), text))) => assert_eq!(text, "Old\nnote more"),
            _ => panic!("Submit saves"),
        }
        view.close_note(&t);
        assert!(!view.editing_note() && !view.keyboard.visible);
    }
    #[test]
    fn the_notes_list_mixes_kinds_in_reading_order_filters_them_and_opens_near_the_page() {
        use crate::gfx::{Canvas, IconRenderer, TextRenderer};
        let state = with_bookmarks();
        let (mut text, mut icons) = (TextRenderer::new(), IconRenderer::default());
        // 6" screens in both orientations, and the 300 dpi presets.
        for (w, h, dpi) in [(600, 800, 167), (800, 600, 167), (1072, 1448, 300), (1696, 1272, 300)] {
            let t = Theme::for_dpi(dpi);
            let screen = Rect::new(0, 0, w, h);
            let mut view = View::new(&t, screen, &state, None, Some(6));
            assert_eq!(view.ids, vec![0, 1, 3, 4, 2], "{w}x{h}: reading order, both kinds");
            assert!(view.import.rect().bottom() < view.filter.rect().y, "{w}x{h}");
            assert!(view.filter.rect().bottom() < view.list_rect.y, "{w}x{h}");
            assert!(view.list_rect.bottom() < view.pager.rect().y, "{w}x{h}");
            let per_page = view.list.paging().range(0).len();
            assert!(per_page >= 2, "{w}x{h}: {per_page} rows a page");
            // Opens on the pager page with the last entry on or before p. 7
            // (page 6), the highlight that follows the page's bookmarks.
            assert_eq!(view.pager.page, view.list.paging().page_of(4), "{w}x{h}");
            let mut canvas = Canvas::new(w as u32, h as u32);
            view.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut Vec::new()), &state);
            assert_eq!(View::new(&t, screen, &state, None, Some(0)).pager.page, 0, "before every entry");
        }
        let t = Theme::for_dpi(300);
        let mut view = View::new(&t, Rect::new(0, 0, 1072, 1448), &state, None, None);
        let rows = &view.list.rows;
        assert_eq!((rows[0].icon, rows[1].icon), (Some(Icon::Highlighter), Some(Icon::BookmarkFilled)));
        assert_eq!(rows[0].label, "Quote h0");
        assert_eq!(rows[1].label, "Quote b0…", "a bookmark shows its opening words");
        assert_eq!(rows[1].sub.as_deref(), Some("Chapter · p. 5"));
        // The filter, from its segments' taps.
        let tap = |view: &mut View, segment: usize| {
            let r = view.filter.rect();
            let p = crate::geom::Point::new(r.x + r.w * (2 * segment as i32 + 1) / 6, r.center().y);
            view.gesture(&Gesture::Down(p), &state, &t);
            view.gesture(&Gesture::Tap(p), &state, &t)
        };
        assert!(tap(&mut view, 2).is_none());
        assert_eq!(view.ids, vec![1, 3, 4]);
        tap(&mut view, 1);
        assert_eq!(view.ids, vec![0, 2]);
        tap(&mut view, 0);
        assert_eq!(view.ids.len(), 5);
        // A tapped overlap subset has no filter; removing returns to the full list.
        let mut subset = View::new(&t, Rect::new(0, 0, 1072, 1448), &state, Some(vec![2]), None);
        assert_eq!(subset.list_rect.y, subset.body.y, "no room kept for a filter");
        subset.after_remove(&state, &t);
        assert_eq!((subset.ids.len(), subset.list_rect.y > subset.filter.rect().bottom()), (5, true));
        // Unresolved and textless bookmarks.
        let mut state = with_bookmarks();
        state.collection.records[1].quotation.clear();
        state.resolved.retain(|e| e.index != 3);
        state.resolved.push(Resolved { index: 3, page: None, last: 0 });
        let view = View::new(&t, Rect::new(0, 0, 1072, 1448), &state, None, None);
        assert_eq!(view.list.rows[1].label, "A page without text");
        assert_eq!(view.list.rows[4].sub.as_deref(), Some("Chapter · Unresolved"));
    }
    #[test]
    fn removing_a_bookmark_from_the_list_keeps_the_resolution_and_a_highlight_starts_one() {
        let mut state = with_bookmarks();
        let mut sent = vec![];
        let revision = state.revision;
        assert!(state.remove_with(1, 1, &mut |c| sent.push(c)));
        assert!(sent.is_empty() && state.pending && state.revision == revision, "no new resolution");
        assert_eq!(state.collection.manifest.records, ["h0", "h1", "b1", "b2"]);
        assert_eq!(state.ordered(), vec![0, 2, 3, 1]);
        assert_eq!(state.pages(), vec![Some(2), Some(6), Some(6), Some(6)]);
        state.pending = false;
        assert!(state.remove_with(0, 1, &mut |c| sent.push(c)));
        assert!(matches!(sent.pop(), Some(Command::Resolve { .. })), "a highlight is painted: resolve again");
        assert!(state.pages().iter().all(Option::is_none), "pages are unknown while locating");
        // While locating, a bookmark's removal resolves again too: the
        // running batch's indices no longer match.
        state.pending = false;
        assert!(state.remove_with(1, 1, &mut |c| sent.push(c)));
        assert!(matches!(sent.pop(), Some(Command::Resolve { .. })));
        assert!(!state.remove_with(9, 1, &mut |c| sent.push(c)));
    }
    #[test]
    fn full_list_opened_while_loading_populates_when_loaded_while_subset_stays_narrow() {
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1072, 1448);
        let mut state = State::default();
        let mut full = View::new(&t, screen, &state, None, None);
        assert!(full.ids.is_empty(), "empty while loading");

        let mut loaded = Collection::default();
        loaded.add(entry("first", None));
        loaded.add(entry("second", None));
        let mut send = |_: Command| {};
        state.apply(worker::Reply::Loaded(Ok((loaded, "fp".into()))), 1, &mut send);
        full.refresh_keep(&state, &t);
        assert_eq!(full.ids, vec![0, 1], "full list populates once the collection loads");
        assert_eq!(full.list.rows.len(), 2);

        let mut subset = View::new(&t, screen, &state, Some(vec![1]), None);
        subset.refresh_keep(&state, &t);
        assert_eq!(subset.ids, vec![1], "overlap subset stays narrowed across refreshes");
    }
}
