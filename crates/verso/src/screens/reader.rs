//! A book screen. Only displayed pages are persisted; speculative renders never are.
mod annotations;
mod bars;
mod layout;
mod search;
mod selection;
use crate::document::{
    FontFace, Link, Metadata, Position, SelectionQuery, TocEntry,
    thread::{Command, LinkRequest, Page, Reply, SelectionRequest, Worker},
};
use crate::geom::Rect;
use crate::gfx::{HAlign, Icon};
use crate::library::language::{self, DetectedLanguage};
use crate::page_layout::PageLayout;
use crate::platform::{KeyAction, RefreshMode};
use crate::reader::speed::{ReadingSpeed, Sampler, Turn};
use crate::reader::{self, PageCache, Refresh, TextMap, Zone};
use crate::screens::books::ConfirmDelete;
use crate::screens::contents::{Contents, ContentsEvent};
use crate::screens::dictionary::{Lookup, LookupEvent, SelectionAction, SelectionActions};
use crate::screens::footnote::{Footnote, FootnoteEvent};
use crate::screens::reader_menu::{MenuEvent, ReaderMenu};
use crate::screens::text_sheet::{FontEvent, FontList, TextEvent, TextSheet};
use crate::screens::{Action, EventCx, Screen, TopDrawerBook, TopDrawerBookAction};
use crate::store::{
    Settings, Store,
    position::{Presented, Writer},
};
use crate::ui::gesture::{Direction, Gesture};
use crate::ui::theme::{INK, PAPER, Theme};
use crate::ui::widget::DrawCx;
use crate::ui::widgets::Toast;
use layout::{LayoutEvent, LayoutView};
use search::{BarEvent, Origin, SearchBar, SearchEvent, SearchView, Session};
use selection::SelectionState;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub struct Reader {
    screen: Rect,
    worker: Rc<Worker>,
    store: Store,
    id: String,
    settings: Settings,
    pub fonts: Vec<FontFace>,
    metadata: Metadata,
    toc: Vec<TocEntry>,
    pages: usize,
    target: usize,
    cache: PageCache,
    shown: Option<Arc<Page>>,
    refresh: Refresh,
    overlay: Overlay,
    layout_at: Option<Instant>,
    laying_out: bool,
    anchor: Position,
    turn_pending: bool,
    /// True while draining a batch of already-queued input events.
    batching: bool,
    /// A page turn queued during the current event batch, committed on `finish_batch`.
    pending_turn: bool,
    /// A page turn was drawn since the last `take_page_turn`.
    turned: bool,
    save_pending: bool,
    persistence: Writer,
    save_error: Option<String>,
    settings_save_pending: bool,
    /// `Store::settings_saves` when settings were last synced.
    settings_seen: u64,
    error: Option<String>,
    opened_at: Option<Instant>,
    /// Grace period before painting `"Opening book…"` so cached books land
    /// straight on their first page without flashing white.
    opening_notice_at: Option<Instant>,
    selection: SelectionState,
    dictionary: Option<Rc<crate::dictionary::worker::Worker>>,
    /// The EPUB language tag from the library metadata.
    language_tag: String,
    /// The book file, for language detection.
    book_path: Option<std::path::PathBuf>,
    /// The language detected from the book's text, saved or just detected.
    detected_language: Option<DetectedLanguage>,
    /// When to detect the language in the background: with the dictionary
    /// warm-up, once per open book that has no current detection.
    language_detect_at: Option<Instant>,
    /// The running detection's result.
    language_detection: Option<std::sync::mpsc::Receiver<anyhow::Result<DetectedLanguage>>>,
    /// When to open the dictionaries' indexes in the background: a moment
    /// after the first page is on screen, once per open book.
    dictionary_prepare_at: Option<Instant>,
    search: Session,
    search_bar: Option<SearchBar>,
    search_redraw: Option<Instant>,
    /// The opened search hit on the displayed page, drawn inverted.
    highlight: Option<(SelectionRequest, Vec<Rect>)>,
    highlight_pending: Option<SelectionRequest>,
    next_highlight: u64,
    /// A `Command::Position` whose reply decides the target page; 0 = none.
    jump: u64,
    next_jump: u64,
    jump_destination: Option<Position>,
    pending_position: Option<Position>,
    annotations: annotations::State,
    extension: Option<annotations::ExtendBar>,
    bars: BarState,
    /// Where the reader was before the last jump (Contents, highlights,
    /// chapter buttons, the progress slider), offered back until the next
    /// page turn. Jumps made while it is offered keep the first place.
    back: Option<Origin>,
    /// Where the back pill was drawn last; empty when it wasn't.
    back_rect: Rect,
    /// The current touch started on the back pill.
    back_touch: bool,
    /// A tapped link waiting for the book thread, and where it is on the page.
    link_pending: Option<(LinkRequest, Rect)>,
    next_link: u64,
    /// The bookmark mark changed over the bare page: the next draw updates
    /// just its corner, with the fast black-and-white waveform.
    mark_fast: bool,
}
/// What the bars show beyond the page itself: chapter offsets, reading
/// speed, and the clock and battery as of the last reader redraw.
#[derive(Default)]
struct BarState {
    /// The generation the book was opened with; its text map carries it.
    opened: u64,
    text: Option<Arc<TextMap>>,
    speed: ReadingSpeed,
    sampler: Sampler,
    /// The next page shown came from a single step (`go`), not a jump.
    stepping: bool,
    /// A sample was added since the global average was last written.
    unsaved_seed: bool,
    clock: Option<crate::clock::LocalTime>,
    battery: Option<crate::platform::Battery>,
    /// Clock and battery are read again at the next draw.
    live_due: bool,
    /// The last draw showed the bare page, with no overlay.
    drawn_bare: bool,
}
/// Highlight requests share the selection reply; this bit keeps their IDs apart.
const HIGHLIGHT_ID: u64 = 1 << 63;
/// How long after a book's first page the dictionaries' indexes open in the
/// background: past the first page's neighbours and the chapter offsets.
const DICTIONARY_PREPARE_DELAY: std::time::Duration = std::time::Duration::from_secs(2);
/// Delay before showing `"Opening book…"` so cached books land directly on
/// their first page without an intermediate white screen.
const OPENING_NOTICE_DELAY: std::time::Duration = std::time::Duration::from_millis(150);
enum Overlay {
    None,
    Menu(Box<ReaderMenu>),
    Text(Box<TextSheet>),
    Layout(Box<LayoutView>),
    Fonts(Box<FontList>),
    Contents(Box<Contents>),
    Lookup(Box<Lookup>),
    SelectionActions(Box<SelectionActions>),
    Search(Box<SearchView>),
    Highlights(Box<annotations::View>),
    Footnote(Box<Footnote>),
    /// "Remove bookmark and its note?", for a toggle on a page whose
    /// bookmark has a note; true when it came from the menu, which returns.
    RemoveBookmark(Box<ConfirmDelete<bool>>),
}
impl Reader {
    pub fn book_id(&self) -> &str {
        &self.id
    }
    #[allow(clippy::too_many_arguments)] // screen services and the restored book state
    pub fn new(
        screen: Rect,
        worker: Rc<Worker>,
        store: Store,
        id: String,
        generation: u64,
        settings: Settings,
        fonts: Vec<FontFace>,
        anchor: Position,
    ) -> Self {
        let mut cache = PageCache::default();
        cache.reset(generation);
        let refresh = Refresh::new(settings.page_flash);
        Self {
            settings_seen: store.settings_saves(),
            screen,
            persistence: Writer::new(store.clone(), id.clone(), worker.waker()),
            worker,
            store,
            id,
            settings,
            fonts,
            metadata: Metadata::default(),
            toc: Vec::new(),
            pages: 0,
            target: 0,
            cache,
            shown: None,
            refresh,
            overlay: Overlay::None,
            layout_at: None,
            laying_out: true,
            anchor,
            turn_pending: false,
            batching: false,
            pending_turn: false,
            turned: false,
            save_pending: false,
            save_error: None,
            settings_save_pending: false,
            error: None,
            opened_at: Some(Instant::now()),
            opening_notice_at: Some(Instant::now() + OPENING_NOTICE_DELAY),
            selection: SelectionState::default(),
            dictionary: None,
            language_tag: String::new(),
            book_path: None,
            detected_language: None,
            language_detect_at: None,
            language_detection: None,
            dictionary_prepare_at: None,
            search: Session::default(),
            search_bar: None,
            search_redraw: None,
            highlight: None,
            highlight_pending: None,
            next_highlight: 0,
            jump: 0,
            next_jump: 0,
            jump_destination: None,
            pending_position: None,
            annotations: annotations::State::default(),
            extension: None,
            bars: BarState { opened: generation, live_due: true, ..BarState::default() },
            back: None,
            back_rect: Rect::default(),
            back_touch: false,
            link_pending: None,
            next_link: 0,
            mark_fast: false,
        }
    }
    /// The book's saved reading speed and the average carried over from
    /// previous books, for the time-left items.
    pub fn reading_speed(&mut self, book: Option<ReadingSpeed>, global: Option<ReadingSpeed>) {
        self.bars.speed = ReadingSpeed::start(book, global);
    }
    pub fn annotations(&mut self, mut book: crate::library::Book) {
        self.language_tag = book.metadata.language.clone();
        self.book_path = Some(book.path.clone());
        if book.annotation_id.is_empty() {
            book.annotation_id = book.id.clone();
        }
        self.annotations.service =
            Some(crate::annotations::worker::Worker::new(self.store.clone(), book, self.worker.waker()));
    }
    /// The language saved in the book's state, if any.
    pub fn detected_language(&mut self, detected: Option<DetectedLanguage>) {
        self.detected_language = detected;
    }
    /// The book language for dictionary sets and word forms: a reliable
    /// detection, else the tag.
    fn book_language(&self) -> String {
        language::resolve(&self.language_tag, self.detected_language.as_ref())
    }
    /// Detects the book's language on a background thread; the result
    /// arrives in `on_background` and is saved with the reading state.
    fn start_language_detection(&mut self) {
        let Some(path) = self.book_path.clone() else { return };
        if self.language_detection.is_some() || !language::needs_detection(self.detected_language.as_ref()) {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let waker = self.worker.waker();
        let spawned = std::thread::Builder::new().name("verso-language".into()).spawn(move || {
            let started = Instant::now();
            let result = language::detect_book(&path);
            match &result {
                Ok(d) => log::info!(
                    "language detection: {:?} reliable={} elapsed_us={}",
                    d.code,
                    d.reliable,
                    started.elapsed().as_micros()
                ),
                Err(e) => log::warn!("language detection: {e:#}"),
            }
            let _ = tx.send(result);
            waker.wake();
        });
        match spawned {
            Ok(_) => self.language_detection = Some(rx),
            Err(e) => log::warn!("starting language detection: {e}"),
        }
    }
    fn receive_language(&mut self) {
        let Some(rx) = &self.language_detection else { return };
        match rx.try_recv() {
            Ok(result) => {
                self.language_detection = None;
                // An unreadable book stores nothing and keeps its tag.
                if let Ok(detected) = result {
                    self.persistence.save_language(detected.clone());
                    let before = self.book_language();
                    self.detected_language = Some(detected);
                    // The warm-up (or a lookup) already ran with the tag:
                    // warm the detected language's word forms instead.
                    if self.book_language() != before
                        && self.dictionary_prepare_at.is_none()
                        && self.dictionary.is_some()
                    {
                        let selection = self.dictionary_selection();
                        self.dictionary_service().prepare(selection);
                    }
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => self.language_detection = None,
        }
    }
    pub fn failed_open(&self) -> bool {
        self.error.is_some() && self.shown.is_none()
    }
    pub fn generation(&self) -> u64 {
        self.cache.generation
    }
    pub fn receive(&mut self, reply: Reply, cx: &mut EventCx) {
        match reply {
            reply @ Reply::Resolved { generation, preview, revision, .. } if generation == self.cache.generation => {
                let current = if preview {
                    revision == self.annotations.import_id
                } else {
                    revision == self.annotations.revision
                };
                if current && self.annotations.receive(reply) {
                    if !preview {
                        self.annotations.paint_pending = true;
                        self.cache.reset(self.cache.generation);
                        self.cache.request(self.target);
                        self.worker.send(Command::Page { generation: self.cache.generation, number: self.target });
                        self.prefetch();
                    }
                    if let Overlay::Highlights(view) = &mut self.overlay {
                        view.refresh_keep(&self.annotations, cx.theme);
                    }
                    cx.redraw();
                }
            }
            Reply::Selection { request, result } if self.highlight_pending == Some(request) => {
                self.highlight_pending = None;
                let context = self.selection_context();
                match result {
                    Ok(Some(found))
                        if context.is_some_and(|c| c.generation == request.generation && c.page == request.page) =>
                    {
                        self.highlight = Some((request, found.rects));
                        cx.redraw();
                    }
                    Ok(_) => {}
                    Err(message) => log::warn!("search highlight: {message}"),
                }
            }
            Reply::Selection { request, result } => {
                let context = self.selection_context();
                self.selection.receive(request, result, context, &self.worker, cx);
                self.selection_released(cx);
            }
            Reply::Jumped { jump, page } if page.generation == self.cache.generation => {
                if jump == self.jump {
                    self.jump = 0;
                    self.jump_destination = None;
                    self.target = page.number;
                }
                self.cache.insert(page.clone(), self.target);
                if page.number == self.target {
                    self.display(page, cx);
                }
            }
            Reply::Link { request, result } => self.link_arrived(request, result, cx),
            Reply::Opening { request, result } => {
                let chapter =
                    reader::chapter(&self.toc, request.page).map(|i| self.toc[i].title.clone()).unwrap_or_default();
                let before = self.annotations.bookmarked(request.page);
                if let Some(message) = self.annotations.receive_opening(request, result, self.cache.generation, chapter)
                {
                    cx.toast(message, None);
                }
                if self.annotations.bookmarked(request.page) != before {
                    self.mark_changed();
                }
                cx.redraw();
            }
            Reply::Search { request, result } => {
                if self.search.receive(request, result)
                    && (matches!(self.overlay, Overlay::Search(_)) || self.search_bar.is_some())
                    && (!self.search.running() || self.search_redraw.is_none_or(|t| cx.now >= t))
                {
                    // Bounded e-ink updates while hits stream in; the final reply always shows.
                    self.search_redraw = Some(cx.now + Duration::from_secs(1));
                    cx.redraw();
                }
            }
            Reply::Opened { generation, metadata, toc, pages, current } if generation == self.cache.generation => {
                self.metadata = metadata;
                self.toc = toc;
                self.pages = pages.max(1);
                self.annotations.resolve(generation, &self.worker);
                self.target = current.min(self.pages - 1);
                if matches!(self.overlay, Overlay::Contents(_)) {
                    self.overlay = Overlay::Contents(Box::new(Contents::new(
                        cx.theme,
                        self.screen,
                        &self.toc,
                        self.target,
                        false,
                    )));
                    cx.redraw();
                } else if matches!(self.overlay, Overlay::Menu(_)) {
                    cx.redraw();
                }
            }
            Reply::TextMap { generation, map } if generation == self.bars.opened => {
                self.bars.text = Some(map);
                // Time left and ticks can now show; the first page is often up already.
                let layout = &self.settings.page_layout;
                let uses = |bar: &crate::page_layout::Bar| {
                    bar.on
                        && bar.slots.iter().flatten().any(|i| {
                            matches!(
                                i,
                                crate::page_layout::BarItem::TimeLeftChapter
                                    | crate::page_layout::BarItem::TimeLeftBook
                            )
                        })
                };
                let time_left = self.bars.speed.value().is_some() && (uses(&layout.top) || uses(&layout.bottom));
                if self.shown.is_some() && (time_left || layout.progress != crate::page_layout::ProgressLine::Off) {
                    cx.redraw();
                }
            }
            Reply::Page(page)
                if page.generation == self.cache.generation
                    && (self.annotations.service.is_none()
                        || self.annotations.resolving
                        || page.annotation_revision == self.annotations.revision) =>
            {
                self.cache.insert(page.clone(), self.target);
                if page.number == self.target && !self.pending_turn {
                    self.annotations.paint_pending = false;
                    self.display(page, cx);
                }
            }
            Reply::PageFailed { generation, number, message } if generation == self.cache.generation => {
                self.cache.failed(number);
                log::error!("page {}: {message}", number + 1);
                // A speculative failure must not abandon another pending turn.
                if number == self.target {
                    self.target = self.shown.as_ref().map_or(0, |p| p.number);
                    self.turn_pending = false;
                    self.pending_turn = false;
                    self.error = Some(message.clone());
                    cx.toast(message, None);
                }
            }
            Reply::Error { generation, message } if generation == 0 || generation == self.cache.generation => {
                log::error!("{message}");
                self.error = Some(message.clone());
                self.opening_notice_at = None;
                self.laying_out = false;
                self.layout_at = None;
                self.pending_position = None;
                self.jump = 0;
                self.jump_destination = None;
                self.turn_pending = false;
                self.pending_turn = false;
                self.target = self.shown.as_ref().map_or(0, |p| p.number);
                if self.shown.is_none() {
                    cx.refresh(self.screen, RefreshMode::Ui);
                }
                cx.toast(message, None);
            }
            _ => {}
        }
    }
    fn dictionary_service(&mut self) -> &Rc<crate::dictionary::worker::Worker> {
        self.dictionary.get_or_insert_with(|| {
            Rc::new(crate::dictionary::worker::Worker::new(self.store.clone(), self.worker.waker()))
        })
    }
    /// The book's dictionary set choice and language, for lookups and the
    /// warm-up.
    fn dictionary_selection(&self) -> crate::dictionary::sets::Selection {
        crate::dictionary::sets::Selection {
            book_set: self.store.book(&self.id).dictionary_set,
            language: self.book_language(),
        }
    }
    /// Where the selection is on the page: the box around its rectangles.
    fn selected_rect(&self) -> Option<Rect> {
        self.selection.selected.as_ref().and_then(|s| s.rects.iter().copied().reduce(|a, b| a.union(&b)))
    }
    pub(super) fn lookup(&mut self, query: &str, cx: &mut EventCx) {
        self.dictionary_prepare_at = None;
        let service = self.dictionary_service().clone();
        let word = self.selected_rect();
        let selection = self.dictionary_selection();
        self.overlay = Overlay::Lookup(Box::new(Lookup::new(query, service, cx.theme, self.screen, word, selection)));
        cx.redraw();
    }
    fn selection_released(&mut self, cx: &mut EventCx) {
        if let Some(text) = self.selection.take_released() {
            if self.selection.extending {
                return;
            }
            if text.split_whitespace().count() == 1 {
                self.lookup(&text, cx);
            } else {
                self.open_passage_actions(cx);
            }
        }
    }
    /// The floating bar beside the selected passage; centred when its
    /// rectangles are unknown.
    fn open_passage_actions(&mut self, cx: &mut EventCx) {
        let passage = self.selected_rect().unwrap_or_else(|| Rect::centered(self.screen.center(), 0, 0));
        self.overlay = Overlay::SelectionActions(Box::new(SelectionActions::new(cx.theme, self.screen, passage)));
        cx.redraw();
    }
    fn display(&mut self, page: Arc<Page>, cx: &mut EventCx) {
        let layout_changed = self.laying_out;
        let different = self.shown.as_ref().is_none_or(|p| p.generation != page.generation || p.number != page.number);
        if !different {
            if self.shown.as_ref().is_some_and(|p| p.annotation_revision != page.annotation_revision) {
                self.shown = Some(page);
                cx.redraw();
            }
            return;
        }
        self.laying_out = false;
        self.opening_notice_at = None;
        let previous = self.shown.as_ref().map(|p| p.number);
        let turn = match previous {
            Some(n) if std::mem::take(&mut self.bars.stepping) && !layout_changed => {
                if page.number == n + 1 {
                    Turn::Forward
                } else if page.number + 1 == n {
                    Turn::Back
                } else {
                    Turn::Jump
                }
            }
            _ => Turn::Jump,
        };
        if let Some(sample) = self.bars.sampler.shown(cx.now, page.offset, turn) {
            self.bars.speed.add(sample);
            self.bars.unsaved_seed = true;
        }
        self.bars.stepping = false;
        self.bars.live_due = true;
        if !self.selection.extending {
            self.selection.clear();
        }
        self.highlight = None;
        self.anchor = page.position.clone();
        self.shown = Some(page.clone());
        if self.selection.extending
            && let Some(context) = self.selection_context()
        {
            self.selection.page(context, &self.worker);
        }
        self.save_pending = true;
        self.error = None;
        let chapter_start = self.toc.iter().any(|c| c.level <= 2 && c.page == page.number);
        let mode = if layout_changed {
            RefreshMode::Full
        } else if self.turn_pending {
            self.turned = true;
            self.refresh.turn(page.images, chapter_start, cx.now)
        } else {
            RefreshMode::Ui
        };
        self.turn_pending = false;
        cx.refresh(self.screen, mode);
        self.prefetch();
        if let Overlay::Menu(menu) = &mut self.overlay {
            menu.progress.value = page.number as f32;
            menu.progress.max = self.pages.saturating_sub(1) as f32;
        }
        if layout_changed && let Some(position) = self.pending_position.take() {
            self.jump_position(None, position, cx);
        }
    }
    fn prefetch(&mut self) {
        self.cache.retain_neighbours(self.target);
        // Forward first: it is the common direction.
        for n in [self.target.checked_add(1), self.target.checked_sub(1)].into_iter().flatten() {
            if n < self.pages && self.cache.request(n) {
                self.worker.send(Command::Page { generation: self.cache.generation, number: n });
            }
        }
    }
    fn go(&mut self, page: usize, cx: &mut EventCx) {
        if self.laying_out || self.layout_at.is_some() || self.pages == 0 {
            return;
        }
        let page = page.min(self.pages - 1);
        if self.target == page {
            return;
        }
        self.back = None;
        self.jump = 0;
        self.jump_destination = None;
        self.target = page;
        self.turn_pending = true;
        self.bars.stepping = true;
        if self.batching {
            // Wait until the queued input batch finishes so rapid taps skip
            // intermediate pages rather than drawing or requesting them.
            self.pending_turn = true;
            return;
        }
        self.commit_turn(cx);
    }
    fn commit_turn(&mut self, cx: &mut EventCx) {
        if !std::mem::take(&mut self.pending_turn) && self.batching {
            return;
        }
        if self.laying_out || self.layout_at.is_some() || self.pages == 0 {
            return;
        }
        let page = self.target;
        if self.shown.as_ref().is_some_and(|p| p.number == page) {
            self.turn_pending = false;
            self.bars.stepping = false;
            return;
        }
        if let Some(p) = self.cache.get(page) {
            self.display(p, cx);
        } else if self.cache.request(page) {
            self.worker.send(Command::Page { generation: self.cache.generation, number: page });
        }
    }
    /// `page` is the position's page in the current layout, when known.
    fn jump_position(&mut self, page: Option<usize>, position: Position, cx: &mut EventCx) -> bool {
        self.pending_turn = false;
        if self.laying_out || self.layout_at.is_some() {
            // Page numbers will change. Keep only the stable text destination;
            // the latest choice wins after the new layout's first page arrives.
            self.pending_position = Some(position);
            self.jump = 0;
            self.jump_destination = None;
            return true;
        }
        self.pending_position = None;
        self.turn_pending = true;
        self.bars.stepping = false;
        if let Some(p) = page.map(|p| p.min(self.pages.saturating_sub(1))).and_then(|p| self.cache.get(p)) {
            self.jump = 0;
            self.jump_destination = None;
            self.target = p.number;
            self.display(p, cx);
        } else {
            self.next_jump += 1;
            self.jump = self.next_jump;
            if let Some(page) = page {
                self.target = page.min(self.pages.saturating_sub(1));
            }
            self.jump_destination = Some(position.clone());
            self.worker.send(Command::Position { generation: self.cache.generation, position, jump: self.jump });
        }
        true
    }
    /// The displayed page, as a place to come back to.
    fn here(&self) -> Option<Origin> {
        self.shown.as_ref().map(|p| Origin { position: p.position.clone(), page: p.number, generation: p.generation })
    }
    /// A jump that offers the way back to where the reader was.
    fn jump_away(&mut self, page: Option<usize>, position: Position, cx: &mut EventCx) {
        let origin = self.back.take().or_else(|| self.here());
        self.jump_position(page, position, cx);
        self.back = origin;
    }
    /// Asks the book thread where the link under `p` leads: the nearest link
    /// text within `slop` pixels on the displayed page. False when there is
    /// none, so the tap turns the page as usual.
    fn follow_link(&mut self, p: crate::geom::Point, slop: i32) -> bool {
        if self.laying_out || self.layout_at.is_some() {
            return false;
        }
        let Some(page) = self.shown.as_ref().filter(|page| page.generation == self.cache.generation) else {
            return false;
        };
        let distance = |r: &Rect| {
            let dx = i64::from((r.x - p.x).max(p.x - (r.right() - 1)).max(0));
            let dy = i64::from((r.y - p.y).max(p.y - (r.bottom() - 1)).max(0));
            dx * dx + dy * dy
        };
        let slop = i64::from(slop);
        let Some(&(link, rect)) =
            page.links.iter().filter(|(_, r)| distance(r) <= slop * slop).min_by_key(|(_, r)| distance(r))
        else {
            return false;
        };
        self.next_link += 1;
        let request = LinkRequest { generation: page.generation, page: page.number, link, id: self.next_link };
        self.link_pending = Some((request, rect));
        self.worker.send(Command::Link(request));
        true
    }
    /// A footnote opens over the page it's marked on; any other link jumps,
    /// with the way back offered. Only for the tap still waiting, while its
    /// page is shown with nothing over it.
    fn link_arrived(&mut self, request: LinkRequest, result: Result<Option<Link>, String>, cx: &mut EventCx) {
        let Some((_, marker)) = self.link_pending.take_if(|(pending, _)| *pending == request) else { return };
        let shown = self.shown.as_ref().is_some_and(|p| p.generation == request.generation && p.number == request.page);
        if !shown || !matches!(self.overlay, Overlay::None) || self.selection.selected.is_some() {
            return;
        }
        match result {
            Ok(Some(Link { target, note: Some(note) })) => {
                self.overlay = Overlay::Footnote(Box::new(Footnote::new(&note, target, cx.theme, self.screen, marker)));
                cx.redraw();
            }
            Ok(Some(link)) => self.jump_away(None, link.target, cx),
            Ok(None) => {}
            Err(message) => log::warn!("link: {message}"),
        }
    }
    /// The back pill shows on the page a jump arrived at, over the bare page.
    fn back_visible(&self) -> bool {
        matches!(self.overlay, Overlay::None)
            && !self.selection.extending
            && self.selection.selected.is_none()
            && self.search_bar.is_none()
            && !self.laying_out
            && self.layout_at.is_none()
            && self.back.as_ref().is_some_and(|o| {
                self.shown.as_ref().is_some_and(|p| p.generation != o.generation || p.number != o.page)
            })
    }
    fn back_label(&self) -> String {
        let Some(origin) = &self.back else { return String::new() };
        if origin.generation != self.cache.generation {
            // Page numbers changed with the layout; the place itself didn't.
            return "Back to where you were".into();
        }
        let mut label = format!("Back to p. {}", origin.page + 1);
        if let Some(i) = reader::chapter(&self.toc, origin.page) {
            label.push_str(" · ");
            label.push_str(&self.toc[i].title);
        }
        label
    }
    fn go_back(&mut self, cx: &mut EventCx) {
        let Some(origin) = self.back.take() else { return };
        let page = (origin.generation == self.cache.generation).then_some(origin.page);
        self.jump_position(page, origin.position, cx);
        cx.redraw();
    }
    fn open_search(&mut self, prefill: Option<&str>, cx: &mut EventCx) {
        let generation = self.cache.generation;
        let query = prefill.map(|q| q.split_whitespace().collect::<Vec<_>>().join(" ")).unwrap_or_default();
        if !query.is_empty() && query != self.search.query {
            self.search_redraw = None;
            self.search.start(&query, generation, &self.worker);
        } else {
            self.search.refresh(generation, &self.worker);
        }
        self.selection.clear();
        let mut view = SearchView::new(cx.theme, self.screen, &self.search, self.search.query.is_empty());
        view.reveal(&mut self.search);
        self.overlay = Overlay::Search(Box::new(view));
        cx.redraw();
    }
    fn open_hit(&mut self, index: usize, cx: &mut EventCx) {
        let Some(hit) = self.search.hits.get(index).cloned() else { return };
        let Some(shown) = self.shown.clone() else { return };
        // During a relayout the page numbers are about to change: queue the
        // hit's stable text position, like any other jump.
        let relayout = self.laying_out || self.layout_at.is_some();
        if !relayout && self.search.stale(self.cache.generation) {
            return;
        }
        self.search.origin.get_or_insert_with(|| Origin {
            position: shown.position.clone(),
            page: shown.number,
            generation: shown.generation,
        });
        self.search.current = Some(index);
        // The search bar's Return is the way back from hits.
        self.back = None;
        self.overlay = Overlay::None;
        self.search_bar.get_or_insert_with(|| SearchBar::new(cx.theme, self.screen));
        self.highlight = None;
        if self.jump_position(Some(hit.page), hit.range.start.clone(), cx) && !relayout {
            self.next_highlight += 1;
            let request = SelectionRequest {
                generation: self.cache.generation,
                page: hit.page,
                id: HIGHLIGHT_ID | self.next_highlight,
            };
            self.highlight_pending = Some(request);
            self.worker.send(Command::Select { request, query: SelectionQuery::Range(hit.range.clone()) });
        }
        cx.redraw();
    }
    fn return_to_origin(&mut self, cx: &mut EventCx) {
        let Some(origin) = self.search.origin.clone() else { return };
        let page = (origin.generation == self.cache.generation).then_some(origin.page);
        if self.jump_position(page, origin.position, cx) {
            self.search.origin = None;
            self.search_bar = None;
            self.highlight = None;
            self.highlight_pending = None;
            self.overlay = Overlay::None;
        }
        cx.redraw();
    }
    fn on_bar(&mut self, event: BarEvent, cx: &mut EventCx) {
        // After a relayout the old hits are useless, so restart first. After a
        // sleep interruption they are still valid: step through them, then
        // restart the search so it streams the rest.
        let stale = self.search.stale(self.cache.generation);
        if stale {
            self.search.refresh(self.cache.generation, &self.worker);
        }
        let current = self.search.current.unwrap_or(0);
        match event {
            BarEvent::Previous if current > 0 => self.open_hit(current - 1, cx),
            BarEvent::Next => self.open_hit(current + 1, cx),
            BarEvent::Previous => {}
            BarEvent::Results => self.open_search(None, cx),
            BarEvent::Return => self.return_to_origin(cx),
            BarEvent::Close => {
                self.search.origin = None;
                self.search_bar = None;
                self.highlight = None;
                self.highlight_pending = None;
            }
        }
        if !stale {
            self.search.refresh(self.cache.generation, &self.worker);
        }
    }
    fn on_search(&mut self, event: SearchEvent, cx: &mut EventCx) {
        match event {
            SearchEvent::Close => self.overlay = Overlay::None,
            SearchEvent::Submit(query) => {
                self.search_redraw = None;
                self.search.start(&query, self.cache.generation, &self.worker);
            }
            SearchEvent::Open(index) => self.open_hit(index, cx),
            SearchEvent::Return => self.return_to_origin(cx),
        }
        cx.redraw();
    }
    fn open_highlights(&mut self, ids: Option<Vec<usize>>, cx: &mut EventCx) {
        self.selection.clear();
        self.extension = None;
        let here = self.shown.as_ref().filter(|p| p.generation == self.cache.generation).map(|p| p.number);
        self.overlay =
            Overlay::Highlights(Box::new(annotations::View::new(cx.theme, self.screen, &self.annotations, ids, here)));
        cx.redraw();
    }
    /// A new record for the current selection (not yet added or saved).
    fn draft(&self) -> Option<crate::annotations::Annotation> {
        let s = self.selection.selected.clone()?;
        let service = self.annotations.service.as_ref()?;
        let created = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos().to_string();
        let id = crate::annotations::digest(
            format!("{}:{}:{}", service.book.annotation_id, created, s.range.start.0).as_bytes(),
        );
        Some(crate::annotations::Annotation {
            version: 1,
            kind: crate::annotations::Kind::Highlight,
            id,
            book: service.book.annotation_id.clone(),
            fingerprint: service.book.fingerprint.clone(),
            range: s.range,
            quotation: s.text,
            created: crate::annotations::created_now(),
            chapter: reader::chapter(&self.toc, s.start_page).map(|i| self.toc[i].title.clone()).unwrap_or_default(),
            note: None,
            provenance: None,
        })
    }
    fn save_highlight(&mut self, cx: &mut EventCx) {
        if !self.annotations.can_save() || self.selection.busy() {
            cx.toast(annotations::NOT_READY, None);
            return;
        }
        let Some(a) = self.draft() else { return };
        if let Err(message) = self.annotations.add(a) {
            cx.toast(message, None);
            return;
        }
        self.annotations.resolve(self.cache.generation, &self.worker);
        self.selection.clear();
        self.extension = None;
        self.overlay = Overlay::None;
        cx.redraw();
    }
    /// Opens the note editor for the selection; saving the note saves the
    /// highlight with it. The selection is cleared: Cancel returns to reading.
    fn note_selection(&mut self, cx: &mut EventCx) {
        if !self.annotations.ready || self.selection.busy() {
            cx.toast(annotations::NOT_READY, None);
            return;
        }
        let Some(a) = self.draft() else { return };
        self.open_highlights(None, cx);
        if let Overlay::Highlights(view) = &mut self.overlay {
            view.edit_note(annotations::NoteTarget::New(Box::new(a)), &self.annotations, cx.theme);
        }
    }
    /// Book title and author for an export: library metadata, then the
    /// opened document's, then the file name.
    fn export_names(&self) -> (String, String) {
        let book = self.annotations.service.as_ref().map(|w| &w.book);
        let pick = |a: Option<&String>, b: &String| a.filter(|s| !s.trim().is_empty()).unwrap_or(b).clone();
        let mut title = pick(book.map(|b| &b.metadata.title), &self.metadata.title);
        if title.trim().is_empty() {
            title = book.and_then(|b| b.path.file_stem()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        }
        (title, pick(book.map(|b| &b.metadata.authors), &self.metadata.authors))
    }
    fn annotation_event(&mut self, event: annotations::Event, cx: &mut EventCx) {
        use annotations::Event;
        match event {
            Event::Close => {
                self.annotations.cancel_import();
                self.overlay = Overlay::None;
            }
            Event::Jump(i) => {
                if let Some(a) = self.annotations.collection.records.get(i) {
                    let p = a.range.start.clone();
                    self.overlay = Overlay::None;
                    self.jump_away(None, p, cx);
                }
            }
            Event::Remove(i) => {
                if self.annotations.remove(i, self.cache.generation, &self.worker) {
                    // A removed bookmark's mark goes when the list closes,
                    // which draws the page again.
                    if let Overlay::Highlights(view) = &mut self.overlay {
                        view.after_remove(&self.annotations, cx.theme);
                    }
                }
            }
            Event::Preview(path) => self.annotations.start_import(path),
            Event::Confirm => {
                self.annotations.confirm(self.cache.generation, &self.worker);
                self.open_highlights(None, cx);
            }
            Event::CancelImport => {
                self.annotations.cancel_import();
                self.annotations.resolve(self.cache.generation, &self.worker);
            }
            Event::Retry => self.annotations.save(),
            Event::SaveNote(target, text) => {
                let note = crate::annotations::clean_note(&text);
                // A note that can't be saved yet stays in the open editor.
                let result = match target {
                    annotations::NoteTarget::New(mut a) => {
                        a.note = note;
                        self.annotations.add(*a).map(|()| {
                            self.annotations.resolve(self.cache.generation, &self.worker);
                            self.overlay = Overlay::None;
                        })
                    }
                    annotations::NoteTarget::Existing(i) => self.annotations.set_note(i, note).map(|_| {
                        if let Overlay::Highlights(view) = &mut self.overlay {
                            view.refresh_keep(&self.annotations, cx.theme);
                            view.close_note(cx.theme);
                        }
                    }),
                };
                if let Err(message) = result {
                    cx.toast(message, None);
                }
            }
            Event::DeleteNote(i) => match self.annotations.set_note(i, None) {
                Ok(_) => {
                    if let Overlay::Highlights(view) = &mut self.overlay {
                        view.refresh_keep(&self.annotations, cx.theme);
                        view.detail(i);
                    }
                }
                Err(message) => cx.toast(message, None),
            },
            Event::Export => {
                let (title, author) = self.export_names();
                self.annotations.export(title, author);
            }
        }
        cx.redraw();
    }
    /// The displayed page, when its bookmark can be toggled: laid out in the
    /// current layout.
    fn bookmark_page(&self) -> Option<usize> {
        let laid_out = !self.laying_out && self.layout_at.is_none();
        self.shown.as_ref().filter(|p| laid_out && p.generation == self.cache.generation).map(|p| p.number)
    }
    /// The displayed page shows the bookmark mark.
    fn page_bookmarked(&self) -> bool {
        self.shown.as_ref().is_some_and(|p| self.annotations.bookmarked(p.number))
    }
    /// Toggles the bookmark on the displayed page (the corner tap, the menu's
    /// ribbon or a confirmed removal). Bookmarks with a note are removed only
    /// after asking; `from_menu` returns there after the question.
    fn toggle_bookmark(&mut self, confirmed: bool, from_menu: bool, cx: &mut EventCx) {
        let Some(page) = self.bookmark_page() else {
            cx.toast(annotations::NOT_READY, None);
            return;
        };
        match self.annotations.toggle_bookmark(self.cache.generation, page, confirmed, &self.worker) {
            Err(message) => cx.toast(message, None),
            // Added when the page's opening words arrive (`Reply::Opening`).
            Ok(annotations::Toggle::Requested) => {}
            Ok(annotations::Toggle::Removed) => self.mark_changed(),
            Ok(annotations::Toggle::Confirm) => {
                let several = self.annotations.bookmarks_on(page).len() > 1;
                let message = if several {
                    "This page has several bookmarks, and one has a note. Removing them deletes the note too."
                } else {
                    "This bookmark has a note. Removing the bookmark deletes the note too."
                };
                self.overlay = Overlay::RemoveBookmark(Box::new(ConfirmDelete::with_action(
                    cx.theme,
                    self.screen,
                    from_menu,
                    "Remove bookmark and its note?",
                    message.into(),
                    "Remove",
                    Icon::Trash,
                )));
            }
        }
        cx.redraw();
    }
    /// The mark appeared or went: over the bare page only its corner is
    /// updated, fast. Under an overlay the page is drawn again when it closes.
    fn mark_changed(&mut self) {
        self.mark_fast = matches!(self.overlay, Overlay::None);
    }
    /// Where the bookmark mark hangs, for the current margins.
    fn mark_rect(&self, t: &Theme) -> Rect {
        let font_px = self.settings.typography.font_px(t.dpi);
        let side = self.settings.page_layout.geometry(self.screen.w, self.screen.h, t.dpi, font_px).side;
        reader::bookmark_mark(self.screen, side, |mm| t.mm(mm))
    }
    /// The bookmark corner's side in pixels, when the corner tap is on.
    fn bookmark_corner(&self, t: &Theme) -> Option<i32> {
        self.settings.bookmark_corner.then(|| t.mm(reader::BOOKMARK_CORNER_MM))
    }
    fn menu(&mut self, cx: &EventCx) {
        self.overlay = Overlay::Menu(Box::new(ReaderMenu::new(
            cx.theme,
            self.screen,
            &self.metadata.title,
            self.target,
            self.pages,
        )));
    }
    fn text(&mut self, t: &Theme) {
        self.overlay = Overlay::Text(Box::new(TextSheet::new(t, self.screen, &self.settings.typography)));
    }
    /// Closes the Page layout screen, if open, and applies what it changed:
    /// one re-layout when margins or bars moved the text, otherwise only a
    /// save and a redraw.
    fn leave_layout(&mut self) -> bool {
        if !matches!(self.overlay, Overlay::Layout(_)) {
            return false;
        }
        let Overlay::Layout(view) = std::mem::replace(&mut self.overlay, Overlay::None) else { unreachable!() };
        self.apply_layout(view.layout);
        true
    }
    fn apply_layout(&mut self, layout: PageLayout) {
        if layout == self.settings.page_layout {
            return;
        }
        let moves_text = layout.moves_text(&self.settings.page_layout);
        self.settings.page_layout = layout;
        if moves_text {
            self.changed();
        } else {
            self.save_text_settings();
        }
    }
    /// Text settings and the page layout are saved together; a failure stays
    /// pending, shows a message and is retried on the next change, sleep and exit.
    fn save_text_settings(&mut self) {
        self.settings_save_pending = true;
        let _ = self.flush_text_settings();
    }
    fn changed(&mut self) {
        self.pending_turn = false;
        if self.jump != 0 {
            self.pending_position = self.jump_destination.take();
            self.jump = 0;
        }
        self.extension = None;
        self.selection.clear();
        self.highlight = None;
        self.highlight_pending = None;
        // A short debounce makes several quick taps one layout.
        self.annotations.invalidate_import("Text layout changed. Preview the import again.");
        self.cache.reset(self.cache.generation + 1);
        self.layout_at = Some(Instant::now() + Duration::from_millis(180));
        self.save_text_settings();
    }
    fn flush_text_settings(&mut self) -> anyhow::Result<()> {
        if !self.settings_save_pending {
            return Ok(());
        }
        // Other settings screens may have saved while this reader was retained.
        let mut settings = self.store.settings();
        settings.typography = self.settings.typography.clone();
        settings.page_layout = self.settings.page_layout.clone();
        self.store.save_settings(&settings).map_err(|e| {
            log::error!("saving typography: {e:#}");
            anyhow::anyhow!("Couldn't save text settings: {e:#}")
        })?;
        self.settings_save_pending = false;
        // Includes any other screen's saves read above.
        self.sync_settings(&settings);
        Ok(())
    }
    /// Takes the settings the reader doesn't own (Settings may change them
    /// over an open book); typography and page layout stay this reader's.
    fn sync_settings(&mut self, saved: &Settings) {
        self.settings_seen = self.store.settings_saves();
        self.settings.tap_zones = saved.tap_zones;
        self.settings.bookmark_corner = saved.bookmark_corner;
        self.settings.developer = saved.developer;
        if self.settings.page_flash != saved.page_flash {
            self.settings.page_flash = saved.page_flash;
            self.refresh.set_every(saved.page_flash);
        }
    }
    fn save(&mut self) {
        if !self.save_pending {
            return;
        }
        let Some(page) = &self.shown else {
            return;
        };
        let position = Presented {
            position: page.position.clone(),
            page: page.number,
            progress: page.progress,
            speed: (self.bars.speed.samples > 0).then_some(self.bars.speed),
            at: SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()),
        };
        match self.persistence.save(position) {
            Ok(()) => self.save_pending = false,
            Err(e) => {
                self.save_error = Some(format!("{e:#}"));
                self.worker.waker().wake();
            }
        }
    }
    /// Carries this book's reading speed over to the next new book. Written
    /// on close and sleep, not per page turn.
    fn save_seed(&mut self) {
        if self.bars.unsaved_seed && self.bars.speed.value().is_some() {
            self.persistence.seed(self.bars.speed);
            self.bars.unsaved_seed = false;
        }
    }
    fn flush_position(&mut self) -> anyhow::Result<()> {
        // Only after_present may submit a position. A page received immediately
        // before sleep/close may not have reached the display yet.
        let result = self.persistence.flush();
        self.persistence.drain().for_each(drop);
        self.save_error = result.as_ref().err().map(|e| format!("{e:#}"));
        if let Err(e) = &result {
            log::error!("completing position saves: {e:#}");
        }
        result.map_err(|e| anyhow::anyhow!("Couldn't save your place: {e:#}"))
    }
    pub fn close(&mut self) -> anyhow::Result<()> {
        self.selection.clear();
        self.flush_text_settings()?;
        self.annotations.flush()?;
        self.flush_position()?;
        self.save_seed();
        if let Some(worker) = &self.dictionary {
            worker.flush()?;
        }
        self.worker.send(Command::Close);
        Ok(())
    }

    fn selection_context(&self) -> Option<SelectionRequest> {
        if self.laying_out || self.layout_at.is_some() {
            return None;
        }
        self.shown.as_ref().filter(|p| p.generation == self.cache.generation).map(|p| SelectionRequest {
            generation: p.generation,
            page: p.number,
            id: 0,
        })
    }
}
/// What the bars show for `page` (or the target page before one is shown).
fn bar_data<'a>(
    toc: &'a [TocEntry],
    metadata: &'a Metadata,
    page: Option<&Page>,
    target: usize,
    pages: usize,
    bars: &'a BarState,
) -> reader::bars::BarData<'a> {
    reader::bars::BarData {
        toc,
        title: &metadata.title,
        author: &metadata.authors,
        page: page.map_or(target, |p| p.number),
        pages,
        progress: page.map_or(0, |p| p.progress),
        offset: page.and_then(|p| p.offset),
        text: bars.text.as_deref(),
        speed: bars.speed.value(),
        clock: bars.clock,
        battery: bars.battery,
    }
}

impl Reader {
    fn gesture(&mut self, g: Gesture, cx: &mut EventCx) -> Action {
        if self.selection.extending {
            if let Some(bar) = &mut self.extension {
                let on_handle = matches!(g, Gesture::Down(p) if self.selection.on_handle(p, cx.theme));
                let (owned, event) = bar.gesture(&g, on_handle);
                if owned {
                    if let Some(e) = event {
                        match e {
                            0 => self.go(self.target.saturating_sub(1), cx),
                            1 => self.go(self.target + 1, cx),
                            2 => self.save_highlight(cx),
                            _ => {
                                self.selection.clear();
                                self.extension = None;
                            }
                        }
                    }
                    cx.redraw();
                    return Action::None;
                }
            }
            if let Some(context) = self.selection_context() {
                self.selection.on_gesture(g, context, &self.worker, cx);
                self.selection_released(cx);
            }
            return Action::None;
        }
        if let Gesture::Down(p) = g {
            self.back_touch = self.back_visible() && self.back_rect.contains(p);
        }
        if self.back_touch {
            // Touches on the back pill never turn pages or select text.
            if g.ends_touch() {
                self.back_touch = false;
                if matches!(g, Gesture::Tap(_)) {
                    if self.pending_turn {
                        self.commit_turn(cx);
                    }
                    self.go_back(cx);
                }
            }
            return Action::None;
        }
        // A tap on a link follows it; one beside a link, outside any
        // highlight, still does, since markers are small.
        let page_tap = match g {
            Gesture::Tap(p)
                if matches!(self.overlay, Overlay::None) && !self.pending_turn && self.selection.selected.is_none() =>
            {
                Some(p)
            }
            _ => None,
        };
        // The bookmark corner comes before links and highlights under it.
        if let Some(p) = page_tap
            && !self.selection.extending
            && self.extension.is_none()
            && reader::tap_zone(self.screen, p, self.settings.tap_zones, self.bookmark_corner(cx.theme))
                == Zone::Bookmark
        {
            self.toggle_bookmark(false, false, cx);
            return Action::None;
        }
        if let Some(p) = page_tap
            && self.follow_link(p, 0)
        {
            return Action::None;
        }
        if let Some(p) = page_tap {
            let ids: Vec<_> = self
                .shown
                .as_ref()
                .filter(|page| page.annotation_revision == self.annotations.revision)
                .map(|page| {
                    page.annotations
                        .iter()
                        .filter(|(_, rects)| rects.iter().any(|r| r.contains(p)))
                        .filter_map(|(number, _)| self.annotations.highlight(*number))
                        .collect()
                })
                .unwrap_or_default();
            if !ids.is_empty() {
                let single = (ids.len() == 1).then_some(ids[0]);
                self.open_highlights(Some(ids), cx);
                if let (Some(i), Overlay::Highlights(view)) = (single, &mut self.overlay) {
                    view.detail(i);
                }
                cx.redraw();
                return Action::None;
            }
            if self.follow_link(p, cx.theme.mm(2.0)) {
                return Action::None;
            }
        }
        if matches!(self.overlay, Overlay::None)
            && let Some(bar) = &mut self.search_bar
        {
            let (owned, event) = bar.on_gesture(&g);
            if owned {
                cx.redraw(); // pressed feedback
                if let Some(event) = event {
                    if self.pending_turn {
                        self.commit_turn(cx);
                    }
                    self.on_bar(event, cx);
                }
                return Action::None;
            }
        }
        if self.pending_turn && matches!(g, Gesture::LongPress(_)) {
            self.commit_turn(cx);
        }
        // A selection handle beside the dictionary card or the passage bar:
        // the overlay closes and the drag adjusts the selection, which
        // reopens the card for a single word or the bar for a passage on
        // release.
        let beside = match &self.overlay {
            Overlay::Lookup(lookup) => {
                lookup.allows_handles() && matches!(g, Gesture::Down(p) if !lookup.rect(cx.theme).contains(p))
            }
            Overlay::SelectionActions(bar) => matches!(g, Gesture::Down(p) if !bar.bounds(cx.theme).contains(p)),
            _ => false,
        };
        if beside && self.selection.adjusting(g, cx.theme) {
            if matches!(self.overlay, Overlay::Lookup(_))
                && let Some(worker) = &self.dictionary
            {
                worker.cancel();
            }
            self.overlay = Overlay::None;
            cx.redraw();
        }
        if matches!(self.overlay, Overlay::None)
            && let Some(context) = self.selection_context()
            && self.selection.on_gesture(g, context, &self.worker, cx)
        {
            if self.highlight.take().is_some() {
                cx.redraw();
            }
            self.selection_released(cx);
            return Action::None;
        }
        match &mut self.overlay {
            Overlay::Lookup(lookup) => {
                if let Some(event) = lookup.on_gesture(&g, cx) {
                    cx.redraw();
                    if let LookupEvent::Set(set) = &event {
                        let result = self.flush_position().and_then(|()| {
                            let mut state = self.store.book(&self.id);
                            state.dictionary_set = Some(set.clone());
                            self.store.save_book(&self.id, &state)
                        });
                        if let Err(error) = result {
                            log::error!("saving dictionary set: {error:#}");
                            cx.toast("Couldn’t save the dictionary set for this book", None);
                        }
                        return Action::None;
                    }
                    if let LookupEvent::Manage(set) = &event {
                        return Action::Push(Box::new(crate::screens::dictionaries::Dictionaries::new(
                            cx.theme,
                            self.screen,
                            self.store.clone(),
                            self.worker.waker(),
                            set.as_deref(),
                        )));
                    }
                    match event {
                        // Each opens its own screen or closes the card on success.
                        LookupEvent::Highlight => self.save_highlight(cx),
                        LookupEvent::Note => self.note_selection(cx),
                        LookupEvent::Search(query) => self.open_search(Some(&query), cx),
                        LookupEvent::Extend => {
                            self.selection.extend();
                            self.extension = Some(annotations::ExtendBar::new(cx.theme, self.screen));
                            self.overlay = Overlay::None;
                        }
                        _ => {
                            self.overlay = Overlay::None;
                            self.selection.clear();
                        }
                    }
                    if !matches!(self.overlay, Overlay::Lookup(_))
                        && let Some(worker) = &self.dictionary
                    {
                        worker.cancel();
                    }
                }
            }
            Overlay::SelectionActions(actions) => {
                let before = actions.visual_state();
                let event = actions.on_gesture(&g);
                if event.is_some() || actions.visual_state() != before {
                    cx.redraw();
                }
                match event {
                    Some(SelectionAction::Highlight) => self.save_highlight(cx),
                    Some(SelectionAction::Note) => self.note_selection(cx),
                    Some(SelectionAction::Extend) => {
                        self.selection.extend();
                        self.extension = Some(annotations::ExtendBar::new(cx.theme, self.screen));
                        self.overlay = Overlay::None;
                    }
                    Some(SelectionAction::Dictionary) => {
                        if let Some(query) = self.selection.selected.as_ref().map(|s| s.text.clone()) {
                            self.lookup(&query, cx);
                        }
                    }
                    Some(SelectionAction::Search) => {
                        let query = self.selection.selected.as_ref().map(|s| s.text.clone());
                        self.open_search(query.as_deref(), cx);
                    }
                    Some(SelectionAction::Dismiss) => {
                        self.overlay = Overlay::None;
                        self.selection.clear();
                    }
                    None => {}
                }
            }
            Overlay::Highlights(view) => {
                cx.redraw();
                if let Some(event) = view.gesture(&g, &self.annotations, cx.theme) {
                    self.annotation_event(event, cx);
                }
            }
            Overlay::Search(view) => {
                let before = view.visual_state();
                let event = view.on_gesture(&g, &mut self.search, cx.theme);
                if event.is_some() || view.visual_state() != before {
                    cx.redraw();
                }
                if let Some(event) = event {
                    self.on_search(event, cx);
                }
            }
            Overlay::Menu(menu) => {
                let before = menu.visual_state();
                let event = menu.on_gesture(&g, !self.laying_out && self.layout_at.is_none());
                if event.is_some() || menu.visual_state() != before {
                    cx.redraw();
                }
                match event {
                    Some(MenuEvent::Books) => {
                        return match self.close() {
                            Ok(()) => Action::Pop,
                            Err(e) => {
                                cx.toast(e.to_string(), None);
                                Action::None
                            }
                        };
                    }
                    Some(MenuEvent::Close) => self.overlay = Overlay::None,
                    Some(MenuEvent::Contents) => {
                        self.overlay = Overlay::Contents(Box::new(Contents::new(
                            cx.theme,
                            self.screen,
                            &self.toc,
                            self.target,
                            self.pages == 0,
                        )))
                    }
                    Some(MenuEvent::Search) => self.open_search(None, cx),
                    Some(MenuEvent::Bookmark) => self.toggle_bookmark(false, true, cx),
                    Some(MenuEvent::Notes) => self.open_highlights(None, cx),
                    Some(MenuEvent::Text) => self.text(cx.theme),
                    Some(MenuEvent::Layout) => {
                        self.overlay = Overlay::Layout(Box::new(LayoutView::new(
                            cx.theme,
                            self.screen,
                            &self.settings.page_layout,
                            &self.settings.typography,
                        )))
                    }
                    Some(MenuEvent::QuickSettings) => {
                        self.overlay = Overlay::None;
                        return Action::OpenTopDrawer;
                    }
                    Some(MenuEvent::Page(page)) => {
                        let origin = self.back.take().or_else(|| self.here());
                        self.go(page, cx);
                        self.back = origin;
                    }
                    Some(MenuEvent::Chapter(delta)) => {
                        let chapter = if delta > 0 {
                            self.toc.iter().find(|c| c.level <= 2 && c.page > self.target)
                        } else {
                            self.toc.iter().rev().find(|c| c.level <= 2 && c.page < self.target)
                        };
                        if let Some(c) = chapter {
                            let (page, position) = (c.page, c.position.clone());
                            self.jump_away(Some(page), position, cx);
                        }
                    }
                    None => {}
                }
            }
            Overlay::Text(sheet) => {
                let before = sheet.visual_state();
                let event = sheet.on_gesture(&g, &mut self.settings.typography);
                if event.is_some() || sheet.visual_state() != before {
                    cx.redraw();
                }
                match event {
                    Some(TextEvent::Close) => self.overlay = Overlay::None,
                    Some(TextEvent::Fonts) => {
                        self.overlay = Overlay::Fonts(Box::new(FontList::new(
                            cx.theme,
                            self.screen,
                            &self.fonts,
                            self.metadata.publisher_fonts,
                            &self.settings.typography.font,
                        )))
                    }
                    Some(TextEvent::Changed) => self.changed(),
                    None => {}
                }
            }
            Overlay::Layout(view) => {
                let data =
                    bar_data(&self.toc, &self.metadata, self.shown.as_deref(), self.target, self.pages, &self.bars);
                let before = view.visual_state();
                let event = view.on_gesture(&g, cx.theme, &data);
                if event.is_some() || view.visual_state() != before {
                    cx.redraw();
                }
                if let Some(LayoutEvent::Close) = event {
                    // Back to reading, with the new layout.
                    self.leave_layout();
                }
            }
            Overlay::Fonts(fonts) => {
                let before = fonts.visual_state();
                let event = fonts.on_gesture(&g);
                if event.is_some() || fonts.visual_state() != before {
                    cx.redraw();
                }
                match event {
                    Some(FontEvent::Close) => self.text(cx.theme),
                    Some(FontEvent::Chosen(name)) => {
                        self.settings.typography.font = name;
                        self.changed();
                        self.text(cx.theme);
                    }
                    None => {}
                }
            }
            Overlay::Footnote(note) => {
                let before = note.visual_state();
                let event = note.on_gesture(&g);
                if event.is_some() || note.visual_state() != before {
                    cx.redraw();
                }
                match event {
                    Some(FootnoteEvent::Close) => self.overlay = Overlay::None,
                    Some(FootnoteEvent::GoTo(position)) => {
                        self.overlay = Overlay::None;
                        self.jump_away(None, position, cx);
                    }
                    None => {}
                }
            }
            Overlay::RemoveBookmark(confirm) => {
                cx.redraw();
                if let Some(remove) = confirm.on_gesture(&g) {
                    let from_menu = confirm.target;
                    if from_menu {
                        self.menu(cx);
                    } else {
                        self.overlay = Overlay::None;
                    }
                    if remove {
                        self.toggle_bookmark(true, from_menu, cx);
                    }
                }
            }
            Overlay::Contents(contents) => {
                let before = contents.visual_state();
                let event = contents.on_gesture(&g);
                if event.is_some() || contents.visual_state() != before {
                    cx.redraw();
                }
                match event {
                    Some(ContentsEvent::Close) => self.menu(cx),
                    Some(ContentsEvent::Position(position)) => {
                        self.overlay = Overlay::None;
                        self.jump_away(None, position, cx);
                    }
                    None => {}
                }
            }
            Overlay::None => {
                let step = match g {
                    Gesture::Tap(p) => {
                        match reader::tap_zone(self.screen, p, self.settings.tap_zones, self.bookmark_corner(cx.theme))
                        {
                            // A tap right after a turn bookmarks the page it turned to.
                            Zone::Bookmark => {
                                if self.pending_turn {
                                    self.commit_turn(cx);
                                }
                                self.toggle_bookmark(false, false, cx);
                                return Action::None;
                            }
                            Zone::Menu => {
                                if self.pending_turn {
                                    self.commit_turn(cx);
                                }
                                self.menu(cx);
                                cx.redraw();
                                return Action::None;
                            }
                            Zone::Back => -1,
                            Zone::Forward => 1,
                        }
                    }
                    Gesture::Swipe { dir: Direction::Left, .. } => 1,
                    Gesture::Swipe { dir: Direction::Right, .. } => -1,
                    // The top edge belongs to the Top Drawer (`App`).
                    Gesture::Swipe { dir: Direction::Up, from, .. }
                        if self.screen.contains(from) && from.y >= self.screen.bottom() - self.screen.h / 10 =>
                    {
                        if self.pending_turn {
                            self.commit_turn(cx);
                        }
                        self.menu(cx);
                        cx.redraw();
                        return Action::None;
                    }
                    _ => return Action::None,
                };
                self.go(self.target.saturating_add_signed(step), cx);
            }
        }
        Action::None
    }
    /// True while a book is opening within the brief grace period before
    /// showing `"Opening book…"`, leaving the previous screen intact so cached
    /// books land straight on their first page.
    fn waiting_for_open(&self) -> bool {
        self.shown.is_none()
            && self.opening_notice_at.is_some()
            && self.error.is_none()
            && matches!(self.overlay, Overlay::None)
    }
}

impl Screen for Reader {
    fn fills_canvas(&self) -> bool {
        self.shown.is_some() || self.waiting_for_open()
    }

    /// Plain reading (no selection, highlight extension or search bar) and
    /// the Reader Menu, which the drawer replaces.
    fn allows_top_drawer(&self) -> bool {
        match self.overlay {
            Overlay::Menu(_) => true,
            Overlay::None => {
                self.selection.selected.is_none()
                    && !self.selection.extending
                    && self.extension.is_none()
                    && self.search_bar.is_none()
            }
            _ => false,
        }
    }

    fn top_drawer_book(&self) -> Option<TopDrawerBook> {
        Some(TopDrawerBook { title: self.metadata.title.clone() })
    }

    fn on_top_drawer_book(&mut self, action: TopDrawerBookAction, cx: &mut EventCx) -> Action {
        match action {
            TopDrawerBookAction::CloseBook => match self.close() {
                Ok(()) => Action::Pop,
                Err(e) => {
                    cx.toast(e.to_string(), None);
                    Action::None
                }
            },
            TopDrawerBookAction::Search => {
                self.open_search(None, cx);
                Action::None
            }
        }
    }

    fn top_drawer_opened(&mut self) {
        if matches!(self.overlay, Overlay::Menu(_)) {
            self.overlay = Overlay::None;
        }
    }

    fn draw(&mut self, cx: &mut DrawCx) {
        if self.waiting_for_open() {
            return;
        }
        let t = cx.theme;
        // The clock and battery change only with the reader's own redraws
        // (a new page, closing an overlay, wake), never with a battery poll
        // alone. The layout screen shows them live.
        let bare = matches!(self.overlay, Overlay::None);
        if self.bars.live_due || bare && !self.bars.drawn_bare || matches!(self.overlay, Overlay::Layout(_)) {
            self.bars.clock = crate::clock::LocalTime::now();
            self.bars.battery = cx.battery;
            self.bars.live_due = false;
        }
        self.bars.drawn_bare = bare;
        if let Overlay::Layout(view) = &self.overlay {
            // Full screen: the page underneath isn't drawn.
            let data = bar_data(&self.toc, &self.metadata, self.shown.as_deref(), self.target, self.pages, &self.bars);
            view.draw(cx, &data);
            return;
        }
        if let Some(page) = &self.shown {
            match page.color {
                crate::gfx::ColorMode::Gray8 => cx.canvas.set_gray(&page.pixels),
                crate::gfx::ColorMode::Color32 => cx.canvas.set_rgba_opaque(&page.pixels),
            }
            let layout = &self.settings.page_layout;
            if layout.top.on || layout.bottom.on {
                let started = Instant::now();
                let font_px = self.settings.typography.font_px(t.dpi);
                let geometry = layout.geometry(self.screen.w, self.screen.h, t.dpi, font_px);
                let data = bar_data(&self.toc, &self.metadata, Some(page), self.target, self.pages, &self.bars);
                bars::draw(cx, layout, &geometry, &data);
                log::debug!("bars drawn in {} us", started.elapsed().as_micros());
            }
        } else {
            cx.line("Opening book…", t.heading, self.screen.inset(t.margin), HAlign::Center);
        }
        if let Some(page) = &self.shown {
            // Prepared patches are the gray highlight; a color canvas tints
            // the page itself.
            let color = cx.canvas.mode() == crate::gfx::ColorMode::Color32;
            for patch in &page.painted {
                if patch.pixels.is_empty() || color {
                    cx.canvas.highlight_rect(patch.rect);
                } else {
                    cx.canvas.blit_gray(patch.rect, &patch.pixels, patch.rect.w as usize);
                }
            }
        }
        if let Some((_, rects)) = &self.highlight {
            for rect in rects {
                cx.canvas.invert_rect(*rect);
            }
        }
        // Looked up at draw time, so toggling never renders the page again.
        let mark = self.mark_rect(t);
        if self.page_bookmarked() {
            cx.icons.draw(cx.canvas, Icon::BookmarkRibbon, mark, INK);
        }
        if std::mem::take(&mut self.mark_fast) && bare {
            cx.hint(mark, RefreshMode::Fast);
        }
        if let Some(bar) = &mut self.extension {
            bar.draw(cx, self.target, self.pages, self.selection.busy() || self.laying_out);
        }
        // After the bar, so a handle over its buttons stays visible.
        self.selection.draw(cx);
        if !self.selection.extending
            && let Some(bar) = &mut self.search_bar
        {
            bar.draw(cx, &self.search);
        }
        self.back_rect = Rect::default();
        if self.back_visible() {
            // Over the bottom margin, where the search bar sits.
            let pill = Toast { text: self.back_label(), icon: Some(Icon::ArrowLeft), until: Instant::now() };
            self.back_rect = pill.draw_above(cx, self.screen.bottom() - t.mm(1.5));
        }
        match &mut self.overlay {
            Overlay::None => {}
            Overlay::Menu(menu) => {
                let page = menu.progress.value as usize;
                let title = reader::chapter(&self.toc, page).map(|i| self.toc[i].title.as_str()).unwrap_or("Book");
                // The same percentage as the bars and Home, for the page on screen.
                let percent = (self.shown.as_ref())
                    .filter(|p| p.number == page)
                    .map(|p| reader::bars::book_percent(p.number, self.pages, p.progress));
                let laid_out = !self.laying_out && self.layout_at.is_none();
                let on = self.shown.as_ref().is_some_and(|p| self.annotations.bookmarked(p.number));
                menu.set_bookmark(on, laid_out && !self.annotations.checking());
                menu.draw(cx, title, self.pages, percent, laid_out);
            }
            Overlay::Text(sheet) => sheet.draw(cx, &self.settings.typography),
            Overlay::Layout(_) => {}
            Overlay::Fonts(fonts) => fonts.draw(cx),
            Overlay::Contents(contents) => contents.draw(cx),
            Overlay::Lookup(lookup) => lookup.draw(cx),
            Overlay::SelectionActions(actions) => actions.draw(cx),
            Overlay::Search(view) => view.draw(cx, &self.search, &self.toc, self.cache.generation),
            Overlay::Highlights(view) => view.draw(cx, &self.annotations),
            Overlay::Footnote(note) => note.draw(cx),
            Overlay::RemoveBookmark(confirm) => confirm.draw(cx),
        }
        if self.laying_out
            || self.layout_at.is_some()
            || self.error.is_some()
            || self.save_error.is_some()
            || self.settings_save_pending
        {
            let message = self
                .error
                .as_deref()
                .or(self.save_error.as_ref().map(|_| "Couldn't save your place"))
                .or(self.settings_save_pending.then_some("Couldn't save text settings"))
                .unwrap_or(if self.shown.is_some() { "Laying out…" } else { "Opening book…" });
            let width = self.screen.w - 2 * t.margin;
            let r = Rect::new(t.margin, t.mm(16.0), width, t.mm(10.0));
            cx.fill(r, PAPER);
            cx.line(message, t.small, r, HAlign::Center);
        }
    }
    /// Page buttons turn pages in plain reading, like a tap on a page-turn
    /// zone; over a menu, sheet, card or selection they do nothing.
    fn on_key(&mut self, key: KeyAction, cx: &mut EventCx) -> Action {
        if matches!(self.overlay, Overlay::None)
            && self.selection.selected.is_none()
            && !self.selection.extending
            && self.extension.is_none()
        {
            self.go(self.target.saturating_add_signed(key.step()), cx);
        }
        Action::None
    }
    fn on_gesture(&mut self, g: Gesture, cx: &mut EventCx) -> Action {
        let action = self.gesture(g, cx);
        // Menus, selection and lookups interrupt reading: the page shown
        // now isn't timed for reading speed.
        if !matches!(self.overlay, Overlay::None) || self.selection.selected.is_some() || self.selection.extending {
            self.bars.sampler.reset();
        }
        action
    }
    fn begin_batch(&mut self) {
        self.batching = true;
    }
    fn finish_batch(&mut self, cx: &mut EventCx) {
        self.batching = false;
        if self.pending_turn {
            self.commit_turn(cx);
        }
    }
    fn busy(&self) -> bool {
        self.annotations.resolving
            || self.language_detect_at.is_some()
            || self.language_detection.is_some()
            || self.annotations.paint_pending
            || self.annotations.importing
            || self.annotations.exporting
            || self.annotations.pending
            || self.annotations.service.is_some() && !self.annotations.ready && self.annotations.load_error.is_none()
            || matches!(&self.overlay, Overlay::Lookup(lookup) if lookup.busy())
            || matches!(&self.overlay, Overlay::Search(_)) && self.search.running()
            || self.highlight_pending.is_some()
            || self.link_pending.is_some()
            || self.jump != 0
            || self.selection.busy()
            || self.laying_out
            || self.layout_at.is_some()
            || self.shown.as_ref().is_some_and(|p| p.number != self.target)
    }
    fn deadline(&self) -> Option<Instant> {
        let step = match &self.overlay {
            Overlay::Text(s) => s.deadline(),
            Overlay::Layout(view) => view.deadline(),
            _ => None,
        };
        [self.layout_at, step, self.opening_notice_at, self.dictionary_prepare_at, self.language_detect_at]
            .into_iter()
            .flatten()
            .min()
    }
    fn has_idle_work(&self) -> bool {
        matches!(&self.overlay, Overlay::Lookup(lookup) if lookup.paginating())
    }
    fn on_idle(&mut self, text: &mut crate::gfx::TextRenderer) -> bool {
        if let Overlay::Lookup(lookup) = &mut self.overlay { lookup.on_idle(text) } else { false }
    }
    fn on_tick(&mut self, cx: &mut EventCx) -> Action {
        if let Overlay::Text(s) = &mut self.overlay
            && s.on_tick(cx.now, &mut self.settings.typography)
        {
            self.changed();
            cx.redraw();
        }
        if let Overlay::Layout(view) = &mut self.overlay
            && view.on_tick(cx.now)
        {
            cx.redraw();
        }
        if self.opening_notice_at.is_some_and(|t| cx.now >= t) {
            self.opening_notice_at = None;
            if self.shown.is_none() && self.error.is_none() {
                cx.refresh(self.screen, RefreshMode::Ui);
            }
        }
        if self.dictionary_prepare_at.is_some_and(|t| cx.now >= t) {
            self.dictionary_prepare_at = None;
            let selection = self.dictionary_selection();
            self.dictionary_service().prepare(selection);
        }
        if self.language_detect_at.is_some_and(|t| cx.now >= t) {
            self.language_detect_at = None;
            self.start_language_detection();
        }
        if self.layout_at.is_some_and(|t| cx.now >= t) {
            self.layout_at = None;
            self.laying_out = true;
            self.worker.send(Command::Layout {
                generation: self.cache.generation,
                typography: self.settings.typography.clone(),
                page: self.settings.page_layout.clone(),
                position: self.anchor.clone(),
            });
            // Hit pages belong to a layout: re-run the query for the new one.
            self.search.refresh_after_layout(self.cache.generation, &self.worker);
        }
        Action::None
    }
    fn on_background(&mut self, cx: &mut EventCx) {
        if self.store.settings_saves() != self.settings_seen {
            // Settings (perhaps opened over this book) saved: apply what the
            // reader reads from them on its own.
            let saved = self.store.settings();
            self.sync_settings(&saved);
        }
        if let Overlay::Lookup(lookup) = &mut self.overlay {
            lookup.receive(cx);
        }
        if self.annotations.drain(self.cache.generation, &self.worker, cx) {
            if let Overlay::Highlights(view) = &mut self.overlay {
                view.refresh_keep(&self.annotations, cx.theme);
            }
            cx.redraw();
        }
        self.receive_language();
        let had_error = self.save_error.is_some();
        for result in self.persistence.drain() {
            self.save_error = result.err().map(|e| format!("{e:#}"));
            if self.save_error.is_some() {
                // Report even a failure followed by a successful later save
                // in this same batch of worker replies.
                cx.toast("Couldn't save your place", None);
            }
        }
        if self.save_error.is_some() {
            cx.toast("Couldn't save your place", None);
        } else if had_error {
            cx.redraw();
        }
    }
    fn on_text_input(&mut self, input: &crate::platform::TextInput, cx: &mut EventCx) -> bool {
        match &mut self.overlay {
            Overlay::Lookup(lookup) => lookup.on_text_input(input, cx),
            Overlay::Highlights(view) => {
                let (consumed, event) = view.input(input);
                if consumed {
                    cx.redraw();
                }
                if let Some(event) = event {
                    self.annotation_event(event, cx);
                }
                consumed
            }
            Overlay::Search(view) => {
                let (consumed, event) = view.on_text_input(input);
                if consumed {
                    cx.redraw();
                }
                if let Some(event) = event {
                    self.on_search(event, cx);
                }
                consumed
            }
            _ => false,
        }
    }
    fn take_page_turn(&mut self) -> bool {
        std::mem::take(&mut self.turned)
    }
    fn after_present(&mut self) {
        if self.shown.is_some()
            && let Some(started) = self.opened_at.take()
        {
            log::info!("startup stage: book first page requested elapsed_us={}", started.elapsed().as_micros());
            self.dictionary_prepare_at = Some(Instant::now() + DICTIONARY_PREPARE_DELAY);
            if language::needs_detection(self.detected_language.as_ref()) {
                self.language_detect_at = self.dictionary_prepare_at;
            }
        }
        self.save();
    }
    fn on_sleep(&mut self) {
        self.batching = false;
        self.pending_turn = false;
        self.extension = None;
        self.annotations.cancel_import();
        self.selection.sleep();
        self.search.cancel(&self.worker);
        self.highlight = None;
        self.highlight_pending = None;
        self.link_pending = None;
        if let Some(worker) = &self.dictionary {
            worker.cancel();
        }
        // Sleep closes the layout screen; keep what was set there.
        self.leave_layout();
        let _ = self.flush_text_settings();
        let _ = self.annotations.flush();
        let _ = self.flush_position();
        self.save_seed();
        // Sleep interrupts reading; after wake the bars show the new time.
        self.bars.sampler.reset();
        self.bars.live_due = true;
        // Dropping the controls cancels held steppers and slider drags. An open
        // note editor stays, so typed text survives sleep.
        if !matches!(&self.overlay, Overlay::Highlights(view) if view.editing_note()) {
            self.overlay = Overlay::None;
        }
    }
    fn on_quit(&mut self) -> anyhow::Result<()> {
        self.leave_layout();
        self.flush_text_settings()?;
        self.annotations.flush()?;
        self.flush_position()?;
        self.save_seed();
        if let Some(worker) = &self.dictionary {
            worker.flush()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::BookState;

    fn with_bars(top: bool, bottom: bool) -> Settings {
        let mut settings = Settings::default();
        settings.page_layout.top.on = top;
        settings.page_layout.bottom.on = bottom;
        settings
    }

    fn test_reader(name: &str) -> (Reader, std::sync::mpsc::Receiver<Command>) {
        let (worker, commands) = Worker::recording_stub();
        let reader = Reader::new(
            Rect::new(0, 0, 600, 800),
            Rc::new(worker),
            Store::at(std::env::temp_dir().join(format!("verso-{name}-{}", std::process::id()))),
            "book".into(),
            1,
            with_bars(false, false),
            vec![],
            Position::default(),
        );
        (reader, commands)
    }
    #[test]
    fn the_detected_language_is_saved_and_used_for_lookups() {
        use crate::library::language::{DETECTOR_VERSION, DetectedLanguage};
        let theme = Theme::for_dpi(150);
        let (mut r, _commands) = test_reader("language-detection");
        let dir = tempfile::tempdir().unwrap();
        let wait = |r: &mut Reader| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while r.language_detection.is_some() {
                assert!(Instant::now() < deadline, "detection didn't finish");
                std::thread::sleep(Duration::from_millis(2));
                test_cx(&theme, |cx| r.on_background(cx));
            }
        };
        // A German book tagged English.
        let path = dir.path().join("german.epub");
        crate::library::epub::tests::fixture(
            &path,
            r#"<package><metadata/><manifest><item id="a" href="a.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="a"/></spine></package>"#,
            &[("OPS/a.xhtml", format!("<html><body><p>{}</p></body></html>", crate::library::language::tests::GERMAN.repeat(3)).as_bytes())],
        )
        .unwrap();
        r.language_tag = "en".into();
        r.book_path = Some(path.clone());
        assert_eq!(r.book_language(), "en");
        r.start_language_detection();
        wait(&mut r);
        assert_eq!(r.book_language(), "de");
        r.persistence.flush().unwrap();
        let saved = r.store.book("book").language.unwrap();
        assert_eq!(saved, DetectedLanguage { code: "de".into(), reliable: true, version: DETECTOR_VERSION });
        // A current result isn't detected again; an older detector's is.
        r.start_language_detection();
        assert!(r.language_detection.is_none());
        r.detected_language(Some(DetectedLanguage { version: DETECTOR_VERSION - 1, ..saved }));
        assert_eq!(r.book_language(), "en");
        r.start_language_detection();
        assert!(r.language_detection.is_some());
        wait(&mut r);
        assert_eq!(r.book_language(), "de");
        // An unreadable book keeps its tag and stores nothing.
        let (mut r, _commands) = test_reader("language-unreadable");
        let broken = dir.path().join("broken.epub");
        std::fs::write(&broken, b"not an EPUB").unwrap();
        r.language_tag = "fr".into();
        r.book_path = Some(broken);
        r.start_language_detection();
        wait(&mut r);
        assert_eq!(r.book_language(), "fr");
        r.persistence.flush().unwrap();
        assert!(r.store.book("book").language.is_none());
        for name in ["language-detection", "language-unreadable"] {
            let _ = std::fs::remove_dir_all(std::env::temp_dir().join(format!("verso-{name}-{}", std::process::id())));
        }
    }
    fn test_page(generation: u64, number: usize) -> Arc<Page> {
        Arc::new(Page {
            generation,
            number,
            color: crate::gfx::ColorMode::Gray8,
            pixels: vec![255; 600 * 800].into(),
            position: Position(format!("page-{number}")),
            progress: 0,
            offset: None,
            images: false,
            annotation_revision: 0,
            annotations: vec![],
            painted: vec![],
            links: vec![],
        })
    }
    fn test_cx(theme: &Theme, f: impl FnOnce(&mut EventCx)) {
        f(&mut EventCx {
            theme,
            screen: Rect::new(0, 0, 600, 800),
            now: Instant::now(),
            toast: &mut None,
            forced: &mut vec![],
            needs_redraw: &mut false,
            battery: None,
        });
    }

    /// Page `number` of a book with 600 characters per page.
    fn text_page(number: usize) -> Arc<Page> {
        let mut page = (*test_page(1, number)).clone();
        page.offset = Some(number as u64 * 600);
        Arc::new(page)
    }
    fn at(now: Instant, theme: &Theme, f: impl FnOnce(&mut EventCx)) {
        f(&mut EventCx {
            theme,
            screen: Rect::new(0, 0, 600, 800),
            now,
            toast: &mut None,
            forced: &mut vec![],
            needs_redraw: &mut false,
            battery: None,
        });
    }

    #[test]
    fn forward_turns_measure_reading_speed_and_it_is_saved_and_carried_over() -> anyhow::Result<()> {
        use crate::geom::Point;
        let theme = Theme::for_dpi(150);
        let (mut r, _commands) = test_reader("reading-speed");
        let store = r.store.clone();
        r.reading_speed(None, None);
        r.pages = 40;
        r.laying_out = false;
        let start = Instant::now();
        let mut clock = start;
        // Taps 30 s apart unless said otherwise; each turn reads 600 characters.
        let mut tap = |r: &mut Reader, x: i32, wait: u64| {
            clock += Duration::from_secs(wait);
            let next = if x > 400 { r.target + 1 } else { r.target.saturating_sub(1) };
            r.cache.insert(text_page(next), r.target);
            at(clock, &theme, |cx| {
                r.on_gesture(Gesture::Tap(Point::new(x, 400)), cx);
            });
            r.after_present();
        };
        at(start, &theme, |cx| r.receive(Reply::Page(text_page(0)), cx));
        for _ in 0..4 {
            tap(&mut r, 550, 30);
        }
        assert_eq!(r.bars.speed.value(), None, "four samples aren't enough");
        tap(&mut r, 550, 30);
        assert_eq!(r.bars.speed.value(), Some(20.0));
        assert_eq!(r.bars.speed.samples, 5);
        // Skimming and stepping away aren't samples.
        tap(&mut r, 550, 1);
        tap(&mut r, 550, 600);
        assert_eq!(r.bars.speed.samples, 5);
        // The menu interrupts the page shown under it.
        tap(&mut r, 550, 30);
        assert_eq!(r.bars.speed.samples, 6);
        test_cx(&theme, |cx| {
            r.on_gesture(Gesture::Tap(Point::new(300, 400)), cx);
        });
        assert!(matches!(r.overlay, Overlay::Menu(_)));
        r.overlay = Overlay::None;
        tap(&mut r, 550, 30);
        assert_eq!(r.bars.speed.samples, 6, "the page under the menu isn't timed");
        // Back and forward again re-reads: no sample until a new page is read.
        tap(&mut r, 50, 30);
        tap(&mut r, 550, 30);
        assert_eq!(r.bars.speed.samples, 6);
        tap(&mut r, 550, 30);
        assert_eq!(r.bars.speed.samples, 7);
        // Sleep interrupts too.
        r.on_sleep();
        tap(&mut r, 550, 30);
        assert_eq!(r.bars.speed.samples, 7);

        // Saved with the position, without an extra write per turn.
        r.flush_position()?;
        let saved = store.book("book").reading_speed.expect("speed saved with the position");
        assert_eq!(saved, r.bars.speed);
        // Sleep wrote the global average; closing writes it again only after new samples.
        tap(&mut r, 550, 30);
        r.close()?;
        drop(r);
        let global = store.reading_speed().expect("carried over to other books");
        assert_eq!(global, store.book("book").reading_speed.unwrap());

        // Another book starts from it; this book from its own.
        let (mut other, _) = test_reader("reading-speed");
        other.reading_speed(store.book("other").reading_speed, store.reading_speed());
        assert_eq!(other.bars.speed.value(), Some(20.0));
        assert_eq!(other.bars.speed.samples, crate::reader::speed::ESTABLISHED);
        drop(other);
        std::fs::remove_dir_all(std::env::temp_dir().join(format!("verso-reading-speed-{}", std::process::id())))?;
        Ok(())
    }

    #[test]
    fn jumps_offer_one_tap_back_until_the_next_page_turn() {
        use crate::document::TocEntry;
        use crate::geom::Point;
        use crate::gfx::{Canvas, IconRenderer, TextRenderer};
        let theme = Theme::for_dpi(150);
        let (mut r, commands) = test_reader("jump-back");
        r.pages = 20;
        r.laying_out = false;
        let entry = |title: &str, page: usize| TocEntry {
            title: title.into(),
            level: 1,
            page,
            position: Position(format!("page-{page}")),
        };
        r.toc = vec![entry("One", 0), entry("Nine", 9)];
        r.shown = Some(test_page(1, 3));
        r.target = 3;
        let (mut canvas, mut text, mut icons) = (Canvas::new(600, 800), TextRenderer::new(), IconRenderer::default());
        let mut draw = |r: &mut Reader| {
            let mut hints = vec![];
            r.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &theme, &mut hints));
            r.back_rect
        };
        // The book thread locates the latest jump and sends its page.
        let arrive = |r: &mut Reader, page: usize| {
            let jump = commands
                .try_iter()
                .filter_map(|c| match c {
                    Command::Position { jump, .. } => Some(jump),
                    _ => None,
                })
                .last()
                .expect("a jump was sent");
            test_cx(&theme, |cx| r.receive(Reply::Jumped { jump, page: test_page(1, page) }, cx));
        };
        let tap = |r: &mut Reader, p: Point| {
            test_cx(&theme, |cx| {
                r.on_gesture(Gesture::Down(p), cx);
                r.on_gesture(Gesture::Tap(p), cx);
            })
        };
        assert!(draw(&mut r).is_empty(), "nothing extra while reading");

        // A Contents jump: the pill names the page and chapter left behind.
        test_cx(&theme, |cx| r.jump_away(None, Position("page-9".into()), cx));
        assert!(!r.back_visible(), "not before the jump arrives");
        arrive(&mut r, 9);
        assert_eq!(r.back_label(), "Back to p. 4 · One");
        let pill = draw(&mut r);
        assert!(!pill.is_empty() && pill.bottom() <= 800);

        // A second jump before reading on keeps the first place.
        test_cx(&theme, |cx| r.jump_away(None, Position("page-12".into()), cx));
        arrive(&mut r, 12);
        assert_eq!(r.back.as_ref().map(|o| o.page), Some(3));
        // Hidden under overlays.
        test_cx(&theme, |cx| r.menu(cx));
        assert!(draw(&mut r).is_empty());
        r.overlay = Overlay::None;
        let pill = draw(&mut r);

        // Tapping it goes back, and the pill is gone.
        tap(&mut r, pill.center());
        assert!(r.back.is_none());
        arrive(&mut r, 3);
        assert_eq!(r.shown.as_ref().unwrap().number, 3);
        assert!(draw(&mut r).is_empty());

        // The next page turn after a jump drops the offer.
        test_cx(&theme, |cx| r.jump_away(None, Position("page-9".into()), cx));
        arrive(&mut r, 9);
        assert!(!draw(&mut r).is_empty());
        r.cache.insert(test_page(1, 10), 9);
        tap(&mut r, Point::new(590, 400));
        assert_eq!(r.shown.as_ref().unwrap().number, 10);
        assert!(r.back.is_none());
        assert!(draw(&mut r).is_empty());

        // Search hits keep their own Return on the search bar.
        test_cx(&theme, |cx| r.jump_away(None, Position("page-9".into()), cx));
        arrive(&mut r, 9);
        r.search_bar = Some(SearchBar::new(&theme, r.screen));
        assert!(draw(&mut r).is_empty());
    }

    #[test]
    fn a_footnote_marker_opens_its_note_and_other_links_jump() {
        use crate::document::{Link, Note};
        use crate::geom::Point;
        use crate::gfx::{Canvas, IconRenderer, TextRenderer};
        let theme = Theme::for_dpi(150);
        let (mut r, commands) = test_reader("footnotes");
        r.pages = 20;
        r.laying_out = false;
        let mut page = (*test_page(1, 3)).clone();
        page.links = vec![(4, Rect::new(200, 300, 10, 12)), (7, Rect::new(100, 500, 200, 20))];
        let linked = Arc::new(page);
        r.shown = Some(linked.clone());
        r.target = 3;
        let tap = |r: &mut Reader, p: Point| {
            test_cx(&theme, |cx| {
                r.on_gesture(Gesture::Down(p), cx);
                r.on_gesture(Gesture::Tap(p), cx);
            })
        };
        let link_sent = || {
            commands.try_iter().find_map(|c| match c {
                Command::Link(request) => Some(request),
                _ => None,
            })
        };

        // Away from links a tap turns the page as before.
        r.cache.insert(test_page(1, 4), 3);
        tap(&mut r, Point::new(590, 700));
        assert!(link_sent().is_none());
        assert_eq!(r.shown.as_ref().unwrap().number, 4);
        r.shown = Some(linked.clone());
        r.target = 3;

        // Just beside the small marker still follows it.
        tap(&mut r, Point::new(213, 306));
        let request = link_sent().expect("the marker is followed");
        assert_eq!((request.page, request.link), (3, 4));
        assert!(r.busy());
        let stale = LinkRequest { id: request.id + 1, ..request };
        test_cx(&theme, |cx| r.receive(Reply::Link { request: stale, result: Ok(None) }, cx));
        assert!(r.link_pending.is_some(), "another request's reply is ignored");
        let note = Note { label: "12".into(), html: "<div>The note.</div>".into() };
        let link = Link { target: Position("note-12".into()), note: Some(note) };
        test_cx(&theme, |cx| r.receive(Reply::Link { request, result: Ok(Some(link)) }, cx));
        assert!(matches!(r.overlay, Overlay::Footnote(_)));
        assert_eq!(r.target, 3, "the reading position stays");
        let (mut canvas, mut text, mut icons) = (Canvas::new(600, 800), TextRenderer::new(), IconRenderer::default());
        let mut hints = vec![];
        r.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &theme, &mut hints));

        // Go to note jumps there and offers the way back.
        let Overlay::Footnote(sheet) = &r.overlay else { unreachable!() };
        let go = sheet.go_rect().center();
        tap(&mut r, go);
        assert!(matches!(r.overlay, Overlay::None));
        let (position, jump) = commands
            .try_iter()
            .find_map(|c| match c {
                Command::Position { position, jump, .. } => Some((position, jump)),
                _ => None,
            })
            .expect("go to note jumps");
        assert_eq!(position, Position("note-12".into()));
        test_cx(&theme, |cx| r.receive(Reply::Jumped { jump, page: test_page(1, 15) }, cx));
        assert_eq!(r.back.as_ref().map(|o| o.page), Some(3));

        // A cross-reference jumps at once.
        r.back = None;
        r.shown = Some(linked.clone());
        r.target = 3;
        tap(&mut r, Point::new(150, 510));
        let request = link_sent().expect("the link is followed");
        assert_eq!(request.link, 7);
        let link = Link { target: Position("chapter-2".into()), note: None };
        test_cx(&theme, |cx| r.receive(Reply::Link { request, result: Ok(Some(link)) }, cx));
        assert!(matches!(r.overlay, Overlay::None));
        assert!(
            commands
                .try_iter()
                .any(|c| matches!(c, Command::Position { position, .. } if position == Position("chapter-2".into())))
        );
        assert_eq!(r.back.as_ref().map(|o| o.page), Some(3));
    }

    #[test]
    fn bars_take_the_clock_and_battery_only_on_reader_redraws() {
        use crate::gfx::{Canvas, IconRenderer, TextRenderer};
        use crate::platform::Battery;
        let theme = Theme::for_dpi(150);
        let (mut r, _commands) = test_reader("live-bars");
        r.pages = 20;
        r.laying_out = false;
        r.shown = Some(test_page(1, 3));
        let (mut canvas, mut text, mut icons) = (Canvas::new(600, 800), TextRenderer::new(), IconRenderer::default());
        let mut draw = |r: &mut Reader, percent| {
            let mut hints = vec![];
            let mut cx = DrawCx::new(&mut canvas, &mut text, &mut icons, &theme, &mut hints);
            cx.battery = Some(Battery { percent, charging: false });
            r.draw(&mut cx);
        };
        draw(&mut r, 80);
        assert_eq!(r.bars.battery.map(|b| b.percent), Some(80));
        assert!(r.bars.clock.is_some());
        // A battery poll redraws the screen but the bars keep their value.
        draw(&mut r, 79);
        assert_eq!(r.bars.battery.map(|b| b.percent), Some(80));
        // Closing an overlay, a new page and wake take fresh values.
        r.overlay = Overlay::Text(Box::new(TextSheet::new(&theme, r.screen, &r.settings.typography)));
        draw(&mut r, 78);
        r.overlay = Overlay::None;
        draw(&mut r, 77);
        assert_eq!(r.bars.battery.map(|b| b.percent), Some(77));
        r.cache.insert(test_page(1, 4), 3);
        test_cx(&theme, |cx| r.go(4, cx));
        draw(&mut r, 76);
        assert_eq!(r.bars.battery.map(|b| b.percent), Some(76));
        r.on_sleep();
        draw(&mut r, 75);
        assert_eq!(r.bars.battery.map(|b| b.percent), Some(75));
    }

    #[test]
    fn failed_page_requests_retry_without_releasing_other_inflight_pages() {
        let (mut r, commands) = test_reader("page-retry");
        r.pages = 20;
        r.laying_out = false;
        r.target = 5;
        r.shown = Some(test_page(1, 5));
        r.cache.insert(test_page(1, 5), 5);
        assert!(r.cache.request(4));
        assert!(r.cache.request(6));
        test_cx(&Theme::for_dpi(150), |cx| {
            r.go(6, cx);
            r.receive(Reply::PageFailed { generation: 0, number: 6, message: "stale".into() }, cx);
            assert!(!r.cache.request(6), "a stale failure must not release this generation's request");
            r.receive(Reply::PageFailed { generation: 1, number: 6, message: "transient".into() }, cx);
            assert_eq!(r.target, 5);
            assert!(!r.cache.request(4), "another in-flight page stays requested");
            assert!(commands.try_iter().next().is_none(), "errors do not automatically retry");
            r.go(6, cx);
            assert!(matches!(commands.try_recv(), Ok(Command::Page { generation: 1, number: 6 })));
            // A failed speculative page must not discard this retry's target.
            r.receive(Reply::PageFailed { generation: 1, number: 4, message: "prefetch".into() }, cx);
            assert_eq!(r.target, 6);
            assert!(r.cache.request(4));
            r.receive(Reply::Page(test_page(1, 6)), cx);
            assert_eq!(r.shown.as_ref().unwrap().number, 6);
            assert!(r.error.is_none());
        });
    }

    #[test]
    fn repagination_keeps_the_latest_stable_destination_and_disables_old_page_numbers() {
        let (mut r, commands) = test_reader("pending-navigation");
        let theme = Theme::for_dpi(150);
        r.pages = 20;
        r.laying_out = false;
        r.shown = Some(test_page(1, 0));
        r.layout_at = Some(Instant::now() + Duration::from_secs(1));
        r.cache.reset(2);
        test_cx(&theme, |cx| {
            assert!(r.jump_position(Some(9), Position("chapter".into()), cx));
            assert_eq!(r.pending_position, Some(Position("chapter".into())));
            r.menu(cx);
            let point = match &r.overlay {
                Overlay::Menu(menu) => menu.progress.rect().center(),
                _ => unreachable!(),
            };
            r.on_gesture(Gesture::Down(point), cx);
            r.on_gesture(Gesture::Tap(point), cx);
            if let Overlay::Menu(menu) = &r.overlay {
                assert!(!menu.progress.is_dragging());
                assert_eq!(menu.progress.value, 0.0, "old page control cannot move during layout");
            }
            cx.now += Duration::from_secs(2);
            r.on_tick(cx);
            assert!(r.laying_out);
            assert!(matches!(commands.try_recv(), Ok(Command::Layout { generation: 2, .. })));
            // A passage chosen during layout supersedes the earlier chapter.
            assert!(r.jump_position(None, Position("passage".into()), cx));
            r.receive(
                Reply::Opened { generation: 2, metadata: Metadata::default(), toc: vec![], pages: 40, current: 1 },
                cx,
            );
            assert!(commands.try_iter().all(|c| !matches!(c, Command::Position { .. })));
            r.receive(Reply::Page(test_page(2, 1)), cx);
            let jump = commands
                .try_iter()
                .find_map(|c| match c {
                    Command::Position { generation, position, jump } => {
                        assert_eq!(generation, 2);
                        assert_eq!(position, Position("passage".into()));
                        Some(jump)
                    }
                    _ => None,
                })
                .expect("stable destination is located in the new layout");
            assert!(r.pending_position.is_none());
            r.receive(Reply::Jumped { jump, page: test_page(2, 25) }, cx);
            assert_eq!(r.shown.as_ref().unwrap().number, 25);
            assert_eq!(r.target, 25);
        });
    }

    #[test]
    fn search_started_during_layout_debounce_is_reissued_after_layout() {
        let (mut r, commands) = test_reader("debounced-search");
        r.laying_out = false;
        r.search.start("chapter", 1, &r.worker);
        assert!(matches!(commands.try_recv(), Ok(Command::Search { request, .. }) if request.generation == 1));
        r.changed();
        test_cx(&Theme::for_dpi(150), |cx| {
            r.open_search(None, cx);
            let early = match commands.try_recv() {
                Ok(Command::Search { request, .. }) => request,
                _ => panic!("search should be requested during debounce"),
            };
            assert_eq!(early.generation, 2);
            cx.now += Duration::from_secs(1);
            r.on_tick(cx);
            assert!(matches!(commands.try_recv(), Ok(Command::Layout { generation: 2, .. })));
            assert!(matches!(commands.try_recv(), Ok(Command::Search { request, query })
                if request.generation == 2 && request.id > early.id && query == "chapter"));
        });
    }

    #[test]
    fn in_flight_jump_survives_repagination() {
        let (mut r, commands) = test_reader("jump-layout-race");
        r.laying_out = false;
        r.pages = 20;
        r.shown = Some(test_page(1, 0));
        test_cx(&Theme::for_dpi(150), |cx| {
            r.jump_position(None, Position("chapter".into()), cx);
            let old_jump = match commands.try_recv() {
                Ok(Command::Position { generation: 1, jump, .. }) => jump,
                _ => panic!("initial jump missing"),
            };
            r.changed();
            assert_eq!(r.pending_position, Some(Position("chapter".into())));
            assert_eq!(r.jump, 0);
            r.receive(Reply::Jumped { jump: old_jump, page: test_page(1, 12) }, cx);
            assert_eq!(r.shown.as_ref().unwrap().number, 0);
            cx.now += Duration::from_secs(1);
            r.on_tick(cx);
            assert!(matches!(commands.try_recv(), Ok(Command::Layout { generation: 2, .. })));
            r.receive(
                Reply::Opened { generation: 2, metadata: Metadata::default(), toc: vec![], pages: 20, current: 0 },
                cx,
            );
            r.receive(Reply::Page(test_page(2, 0)), cx);
            let new_jump = commands.try_iter().find_map(|command| match command {
                Command::Position { generation: 2, position, jump } if position == Position("chapter".into()) => {
                    Some(jump)
                }
                _ => None,
            });
            assert!(new_jump.is_some_and(|jump| jump > old_jump));
        });
    }

    #[test]
    fn contents_opened_before_repagination_uses_positions_after_it_finishes() {
        let (mut r, commands) = test_reader("stale-contents");
        let theme = Theme::for_dpi(150);
        r.pages = 40;
        r.cache.reset(2);
        r.target = 0;
        r.shown = Some(test_page(2, 0));
        r.cache.insert(test_page(2, 1), 0);
        let old_toc =
            vec![TocEntry { title: "Chapter".into(), level: 1, page: 1, position: Position("chapter".into()) }];
        r.overlay = Overlay::Contents(Box::new(Contents::new(&theme, r.screen, &old_toc, 0, false)));
        r.laying_out = false; // The new layout has finished since opening Contents.
        let p = crate::geom::Point::new(300, theme.top_bar + theme.mm(2.0) + theme.row / 2);
        test_cx(&theme, |cx| {
            r.on_gesture(Gesture::Down(p), cx);
            r.on_gesture(Gesture::Tap(p), cx);
        });
        assert!(matches!(r.overlay, Overlay::None));
        assert_eq!(r.shown.as_ref().unwrap().number, 0, "old page 1 must not use the new cached page 1");
        assert!(
            matches!(commands.try_recv(), Ok(Command::Position { generation: 2, position: Position(p), .. }) if p == "chapter")
        );
    }

    #[test]
    fn contents_opened_while_book_is_opening_populates_when_opened_arrives() {
        let (mut r, commands) = test_reader("early-contents");
        let theme = Theme::for_dpi(150);
        // Initial open hasn't finished yet: pages is 0 and toc is empty.
        assert_eq!(r.pages, 0);
        r.overlay = Overlay::Contents(Box::new(Contents::new(&theme, r.screen, &r.toc, r.target, r.pages == 0)));
        let toc = vec![TocEntry { title: "Chapter 1".into(), level: 1, page: 2, position: Position("ch1".into()) }];
        let p = crate::geom::Point::new(300, theme.top_bar + theme.mm(2.0) + theme.row / 2);
        test_cx(&theme, |cx| {
            r.receive(
                Reply::Opened {
                    generation: r.cache.generation,
                    metadata: Metadata::default(),
                    toc,
                    pages: 10,
                    current: 0,
                },
                cx,
            );
            r.laying_out = false;
            r.on_gesture(Gesture::Down(p), cx);
            r.on_gesture(Gesture::Tap(p), cx);
        });
        assert!(matches!(r.overlay, Overlay::None));
        assert!(
            matches!(commands.try_recv(), Ok(Command::Position { position: Position(pos), .. }) if pos == "ch1"),
            "tapping the populated row jumps to the chapter"
        );
    }

    #[test]
    fn text_settings_errors_survive_rendering_and_retry_without_overwriting_other_settings() -> anyhow::Result<()> {
        let (mut r, _commands) = test_reader("text-settings-retry");
        let root = r.store.dictionary_preferences().parent().unwrap().to_owned();
        let path = root.join("settings.json");
        std::fs::create_dir_all(&path)?; // Atomic file replacement must fail.
        r.laying_out = false;
        r.settings.typography.size_pt += 2.0;
        r.changed();
        assert!(r.settings_save_pending);
        test_cx(&Theme::for_dpi(150), |cx| {
            cx.now += Duration::from_secs(1);
            r.on_tick(cx);
            r.receive(
                Reply::Opened { generation: 2, metadata: Metadata::default(), toc: vec![], pages: 10, current: 0 },
                cx,
            );
            r.receive(Reply::Page(test_page(2, 0)), cx);
            assert!(r.settings_save_pending, "page display cannot acknowledge a failed settings save");
        });
        assert!(r.on_quit().is_err(), "lifecycle save retries and reports a continuing failure");
        std::fs::remove_dir(&path)?;
        let settings =
            Settings { orientation: crate::platform::Orientation::Landscape, developer: true, ..Default::default() };
        r.store.save_settings(&settings)?;
        r.on_quit()?;
        assert!(!r.settings_save_pending);
        let saved = r.store.settings();
        assert_eq!(saved.typography.size_pt, r.settings.typography.size_pt);
        assert!(!saved.page_layout.bottom.on);
        assert_eq!(saved.orientation, settings.orientation);
        assert!(saved.developer);
        drop(r);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    /// Opens the Page layout screen from the Reader Menu's Layout button.
    fn open_layout(r: &mut Reader, theme: &Theme) {
        test_cx(theme, |cx| {
            r.menu(cx);
            let Overlay::Menu(menu) = &r.overlay else { unreachable!() };
            let layout = menu.button_rects()[3].center();
            r.on_gesture(Gesture::Down(layout), cx);
            r.on_gesture(Gesture::Tap(layout), cx);
        });
        assert!(matches!(r.overlay, Overlay::Layout(_)));
    }
    fn settings_path(r: &Reader) -> std::path::PathBuf {
        r.store.dictionary_preferences().parent().unwrap().join("settings.json")
    }
    fn edit_layout(r: &mut Reader, f: impl FnOnce(&mut PageLayout)) {
        let Overlay::Layout(view) = &mut r.overlay else { panic!("layout screen is not open") };
        f(&mut view.layout);
    }
    /// Taps the back arrow, then lets any debounced layout start.
    fn leave_layout_screen(r: &mut Reader, theme: &Theme) {
        let back = crate::geom::Point::new(theme.margin + theme.target / 2 - theme.mm(2.0), theme.top_bar / 2);
        test_cx(theme, |cx| {
            r.on_gesture(Gesture::Down(back), cx);
            r.on_gesture(Gesture::Tap(back), cx);
            assert!(matches!(r.overlay, Overlay::None), "back returns to reading");
            cx.now += Duration::from_secs(1);
            r.on_tick(cx);
        });
    }
    fn layouts(commands: &std::sync::mpsc::Receiver<Command>) -> Vec<PageLayout> {
        commands
            .try_iter()
            .filter_map(|c| match c {
                Command::Layout { page, .. } => Some(page),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn leaving_page_layout_relays_out_once_only_when_the_text_moves() {
        let (mut r, commands) = test_reader("page-layout-leave");
        let _ = std::fs::remove_file(settings_path(&r));
        let theme = Theme::for_dpi(150);
        r.laying_out = false;
        r.pages = 20;
        r.shown = Some(test_page(1, 3));
        // No changes: nothing to lay out or save.
        open_layout(&mut r, &theme);
        leave_layout_screen(&mut r, &theme);
        assert!(layouts(&commands).is_empty());
        assert_eq!(r.cache.generation, 1);
        assert!(!settings_path(&r).exists(), "an unchanged layout is not saved");
        // Slots and the progress line only redraw, but are saved.
        open_layout(&mut r, &theme);
        edit_layout(&mut r, |l| {
            l.bottom.slots[1] = Some(crate::page_layout::BarItem::PageTotal);
            l.progress = crate::page_layout::ProgressLine::Bottom;
        });
        leave_layout_screen(&mut r, &theme);
        assert!(layouts(&commands).is_empty(), "slot picks keep the layout");
        assert_eq!(r.cache.generation, 1);
        assert!(r.layout_at.is_none() && !r.settings_save_pending);
        let saved = r.store.settings().page_layout;
        assert_eq!(saved.bottom.slots[1], Some(crate::page_layout::BarItem::PageTotal));
        assert_eq!(saved, r.settings.page_layout);
        // Several margin and bar changes in one visit: one layout, on leaving.
        open_layout(&mut r, &theme);
        edit_layout(&mut r, |l| {
            l.margins.sides += 2;
            l.margins.top = 9;
            l.top.on = true;
        });
        test_cx(&theme, |cx| {
            cx.now += Duration::from_secs(1);
            r.on_tick(cx);
        });
        assert!(layouts(&commands).is_empty(), "nothing is laid out while the screen is open");
        leave_layout_screen(&mut r, &theme);
        let sent = layouts(&commands);
        assert_eq!(sent.len(), 1);
        assert_eq!((sent[0].margins.sides, sent[0].margins.top, sent[0].top.on), (9, 9, true));
        assert_eq!(r.cache.generation, 2);
        assert_eq!(r.store.settings().page_layout, sent[0]);
        let _ = std::fs::remove_dir_all(settings_path(&r).parent().unwrap());
    }

    #[test]
    fn page_layout_save_failures_stay_pending_and_retry() -> anyhow::Result<()> {
        let (mut r, commands) = test_reader("page-layout-retry");
        let theme = Theme::for_dpi(150);
        let path = settings_path(&r);
        std::fs::create_dir_all(&path)?; // Atomic file replacement must fail.
        r.laying_out = false;
        r.shown = Some(test_page(1, 0));
        open_layout(&mut r, &theme);
        edit_layout(&mut r, |l| l.top.slots = [None, Some(crate::page_layout::BarItem::Author), None]);
        leave_layout_screen(&mut r, &theme);
        assert!(r.settings_save_pending, "a failed save stays pending");
        assert!(layouts(&commands).is_empty());
        assert!(r.on_quit().is_err(), "exit retries and reports a continuing failure");
        std::fs::remove_dir(&path)?;
        // Sleep with the screen open keeps its edits and retries the save.
        open_layout(&mut r, &theme);
        edit_layout(&mut r, |l| l.margins.bottom = 12);
        r.on_sleep();
        assert!(matches!(r.overlay, Overlay::None));
        assert!(!r.settings_save_pending);
        let saved = r.store.settings().page_layout;
        assert_eq!(saved.top.slots[1], Some(crate::page_layout::BarItem::Author));
        assert_eq!(saved.margins.bottom, 12);
        assert!(r.layout_at.is_some(), "the margin change lays out once, after wake");
        std::fs::remove_dir_all(path.parent().unwrap())?;
        Ok(())
    }

    #[test]
    fn extension_bar_never_takes_a_selection_handle_or_a_drag_through_its_strip() {
        use crate::{
            document::{Selection, TextRange},
            geom::{Point, Rect},
        };
        let (mut r, _commands) = test_reader("extension-handle");
        let theme = Theme::for_dpi(150);
        r.pages = 10;
        r.target = 2;
        r.laying_out = false;
        r.shown = Some(test_page(1, 2));
        // The last line ends 12 mm above the bottom, so its end handle sits inside the 13 mm bar.
        let line = Rect::new(100, r.screen.h - theme.mm(12.0) - 30, 200, 30);
        r.selection.selected = Some(Selection {
            range: TextRange { start: Position("a".into()), end: Position("b".into()) },
            text: "two words".into(),
            rects: vec![line],
            start_page: 2,
            start_visible: true,
            end_visible: true,
        });
        r.selection.extend();
        r.extension = Some(annotations::ExtendBar::new(&theme, r.screen));
        let handle = Point::new(line.right(), line.bottom() + theme.mm(1.2));
        test_cx(&theme, |cx| {
            r.on_gesture(Gesture::Down(handle), cx);
        });
        assert!(r.selection.adjusting(Gesture::Move(handle), &theme), "the handle was grabbed");
        // Dragging on through the strip and lifting there still reaches the selection.
        let low = Point::new(handle.x, r.screen.h - theme.mm(2.0));
        test_cx(&theme, |cx| {
            r.on_gesture(Gesture::Move(low), cx);
            r.on_gesture(Gesture::End(low), cx);
        });
        assert!(!r.selection.adjusting(Gesture::Move(low), &theme), "the drag ended");
        // A touch that starts on the bar itself still belongs to the bar.
        let blank = Point::new(10, r.screen.h - theme.mm(2.0));
        test_cx(&theme, |cx| {
            r.on_gesture(Gesture::Down(blank), cx);
        });
        assert!(!r.selection.adjusting(Gesture::Move(blank), &theme));
    }

    #[test]
    fn the_dictionary_card_acts_on_the_book_selection_and_yields_handles_beside_it() {
        use crate::{
            document::{Selection, TextRange},
            geom::{Point, Rect},
        };
        let theme = Theme::for_dpi(150);
        let (mut r, commands) = test_reader("lookup-actions");
        r.pages = 10;
        r.target = 2;
        r.laying_out = false;
        r.shown = Some(test_page(1, 2));
        let word = Rect::new(100, 100, 120, 30);
        let open = |r: &mut Reader| {
            r.selection.clear();
            r.extension = None;
            r.selection.selected = Some(Selection {
                range: TextRange { start: Position("a".into()), end: Position("b".into()) },
                text: "word".into(),
                rects: vec![word],
                start_page: 2,
                start_visible: true,
                end_visible: true,
            });
            test_cx(&theme, |cx| r.lookup("word", cx));
            let Overlay::Lookup(lookup) = &r.overlay else { panic!("no lookup") };
            lookup.rect(&theme)
        };
        let tap = |r: &mut Reader, p: Point| {
            test_cx(&theme, |cx| {
                r.on_gesture(Gesture::Down(p), cx);
                r.on_gesture(Gesture::Tap(p), cx);
            })
        };
        let action = |r: &Reader, i| match &r.overlay {
            Overlay::Lookup(lookup) => lookup.action_point(i),
            _ => panic!("no lookup"),
        };
        // The compact card opens at the bottom, away from the word near the top.
        let card = open(&mut r);
        assert_eq!(card.bottom(), r.screen.h);
        assert!(card.intersect(&word).is_none());
        // Highlight can't save before annotations are ready: the card stays.
        let p = action(&r, 0);
        tap(&mut r, p);
        assert!(matches!(r.overlay, Overlay::Lookup(_)));
        assert!(r.selection.selected.is_some());
        // Extend goes straight to extending the book selection.
        let p = action(&r, 3);
        tap(&mut r, p);
        assert!(matches!(r.overlay, Overlay::None));
        assert!(r.selection.extending && r.extension.is_some());
        // Search opens book search for the query.
        open(&mut r);
        let p = action(&r, 2);
        tap(&mut r, p);
        assert!(matches!(r.overlay, Overlay::Search(_)));
        assert_eq!(r.search.query, "word");
        assert!(commands.try_iter().any(|c| matches!(c, Command::Search { query, .. } if query == "word")));
        // A handle beside the card closes it and drags the selection.
        open(&mut r);
        let handle = Point::new(word.right(), word.bottom() + theme.mm(1.2));
        assert!(!card.contains(handle));
        test_cx(&theme, |cx| {
            r.on_gesture(Gesture::Down(handle), cx);
        });
        assert!(matches!(r.overlay, Overlay::None));
        assert!(r.selection.adjusting(Gesture::Move(handle), &theme), "the handle was grabbed");
        let to = Point::new(400, 300);
        test_cx(&theme, |cx| {
            r.on_gesture(Gesture::Move(to), cx);
            r.on_gesture(Gesture::End(to), cx);
        });
        assert!(
            commands.try_iter().any(|c| matches!(c, Command::Select { query: SelectionQuery::Extend { .. }, .. })),
            "the drag extends the selection"
        );
        // Elsewhere outside the card, a tap closes it and drops the selection.
        open(&mut r);
        tap(&mut r, Point::new(500, 300));
        assert!(matches!(r.overlay, Overlay::None));
        assert!(r.selection.selected.is_none());
    }

    #[test]
    fn the_passage_bar_floats_beside_the_passage_and_yields_its_handles() {
        use crate::{
            document::{Selection, TextRange},
            geom::{Point, Rect},
        };
        let theme = Theme::for_dpi(150);
        let (mut r, commands) = test_reader("passage-bar");
        r.pages = 10;
        r.target = 2;
        r.laying_out = false;
        r.shown = Some(test_page(1, 2));
        let lines = [Rect::new(60, 200, 480, 30), Rect::new(60, 230, 200, 30)];
        let open = |r: &mut Reader| {
            r.selection.clear();
            r.selection.selected = Some(Selection {
                range: TextRange { start: Position("a".into()), end: Position("b".into()) },
                text: "two words".into(),
                rects: lines.to_vec(),
                start_page: 2,
                start_visible: true,
                end_visible: true,
            });
            test_cx(&theme, |cx| r.open_passage_actions(cx));
            let Overlay::SelectionActions(bar) = &r.overlay else { panic!("no passage bar") };
            bar.rect()
        };
        let tap = |r: &mut Reader, p: Point| {
            test_cx(&theme, |cx| {
                r.on_gesture(Gesture::Down(p), cx);
                r.on_gesture(Gesture::Tap(p), cx);
            })
        };
        let bar = open(&mut r);
        assert_eq!(bar.y, lines[1].bottom() + theme.mm(4.5), "below the passage, clear of its handles");
        // The end handle beside the bar closes it and drags the selection.
        let handle = Point::new(lines[1].right(), lines[1].bottom() + theme.mm(1.2));
        assert!(!bar.contains(handle));
        test_cx(&theme, |cx| {
            r.on_gesture(Gesture::Down(handle), cx);
        });
        assert!(matches!(r.overlay, Overlay::None));
        assert!(r.selection.adjusting(Gesture::Move(handle), &theme), "the handle was grabbed");
        let to = Point::new(400, 300);
        test_cx(&theme, |cx| {
            r.on_gesture(Gesture::Move(to), cx);
            r.on_gesture(Gesture::End(to), cx);
        });
        assert!(
            commands.try_iter().any(|c| matches!(c, Command::Select { query: SelectionQuery::Extend { .. }, .. })),
            "the drag extends the selection"
        );
        // Dict looks the passage up.
        open(&mut r);
        let dict = match &r.overlay {
            Overlay::SelectionActions(bar) => bar.cell(SelectionAction::Dictionary).center(),
            _ => unreachable!(),
        };
        tap(&mut r, dict);
        assert!(matches!(r.overlay, Overlay::Lookup(_)));
        // A tap elsewhere dismisses the bar and the selection.
        open(&mut r);
        tap(&mut r, Point::new(300, 700));
        assert!(matches!(r.overlay, Overlay::None));
        assert!(r.selection.selected.is_none());
    }

    #[test]
    fn extending_hides_search_controls_and_restores_them_on_cancel() {
        use crate::{
            document::{Selection, TextRange},
            gfx::{Canvas, IconRenderer, TextRenderer},
        };
        let (mut r, _commands) = test_reader("extension-bars");
        let theme = Theme::for_dpi(150);
        r.pages = 10;
        r.target = 2;
        r.laying_out = false;
        r.shown = Some(test_page(1, 2));
        r.search_bar = Some(SearchBar::new(&theme, r.screen));
        r.search.query = "search".into();
        r.selection.selected = Some(Selection {
            range: TextRange { start: Position("a".into()), end: Position("b".into()) },
            text: "two words".into(),
            rects: vec![],
            start_page: 2,
            start_visible: true,
            end_visible: true,
        });
        r.selection.extend();
        r.extension = Some(annotations::ExtendBar::new(&theme, r.screen));
        let mut canvas = Canvas::new(600, 800);
        let mut text = TextRenderer::new();
        let mut icons = IconRenderer::default();
        let mut hints = vec![];
        let mut draw = |r: &mut Reader| {
            r.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &theme, &mut hints));
            canvas.gray_data().to_vec()
        };
        let extending = draw(&mut r);
        let search_bar = r.search_bar.take();
        assert_eq!(extending, draw(&mut r), "search controls cannot cover the active extension controls");
        r.search_bar = search_bar;
        // Cancel is the fourth extension button, at the right end of the strip.
        let p = crate::geom::Point::new(530, r.screen.h - theme.mm(10.0));
        test_cx(&theme, |cx| {
            r.on_gesture(Gesture::Down(p), cx);
            r.on_gesture(Gesture::Tap(p), cx);
        });
        assert!(!r.selection.extending && r.extension.is_none());
        assert!(r.search_bar.is_some());
        assert_eq!(r.search.query, "search");
        let restored = draw(&mut r);
        r.search_bar = None;
        assert_ne!(restored, draw(&mut r), "the retained search bar is visible again");
    }

    #[test]
    fn a_retained_reader_rediscovers_after_dictionary_folders_change() -> anyhow::Result<()> {
        let root = std::env::temp_dir().join(format!("verso-reader-dictionary-folders-{}", std::process::id()));
        let install = |dir: &std::path::Path, name: &str| -> std::io::Result<()> {
            std::fs::create_dir_all(dir)?;
            let path = dir.join(name);
            std::fs::write(
                path.with_extension("ifo"),
                format!(
                    "StarDict's dict ifo file\nversion=2.4.2\nbookname={name}\nwordcount=1\nidxfilesize=14\nsametypesequence=m\n"
                ),
            )?;
            std::fs::write(path.with_extension("idx"), b"Hello\0\0\0\0\0\0\0\0\x07")?;
            std::fs::write(path.with_extension("dict"), b"meaning")
        };
        let store = Store::at(root.join("data"));
        store.save_dictionary_folders(&[])?;
        install(&store.dictionary_dir(), "native")?;
        install(&root.join("koreader"), "external")?;
        let screen = Rect::new(0, 0, 600, 800);
        let theme = Theme::for_dpi(150);
        let mut r = Reader::new(
            screen,
            Rc::new(Worker::stub()),
            store.clone(),
            "book".into(),
            1,
            Settings::default(),
            Vec::new(),
            Position::default(),
        );
        let (mut toast, mut forced, mut redraw) = (None, Vec::new(), false);
        let mut cx = EventCx {
            theme: &theme,
            screen,
            now: Instant::now(),
            toast: &mut toast,
            forced: &mut forced,
            needs_redraw: &mut redraw,
            battery: None,
        };
        let catalog = |r: &Reader| {
            let worker = r.dictionary.as_ref().unwrap();
            let id = worker.catalog(false);
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(reply) = worker.receive().filter(|reply| reply.id == id) {
                    let mut names: Vec<_> = reply.dictionaries.into_iter().map(|d| d.name).collect();
                    names.sort();
                    return names;
                }
                assert!(Instant::now() < deadline, "no catalog");
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        r.lookup("Hello", &mut cx);
        assert_eq!(catalog(&r), ["native"]);
        // The management button pushes a screen without dismissing the lookup
        // or changing the book/selected passage. Returning refreshes the query.
        r.lookup("Hello", &mut cx);
        let before = r.anchor.clone();
        let before_target = r.target;
        let button = crate::geom::Point::new(screen.w / 2, screen.h - theme.mm(20.0));
        r.on_gesture(Gesture::Down(button), &mut cx);
        let action = r.on_gesture(Gesture::Tap(button), &mut cx);
        assert!(matches!(action, Action::Push(_)));
        assert!(matches!(r.overlay, Overlay::Lookup(_)));
        assert_eq!(r.anchor, before);
        assert_eq!(r.target, before_target);
        drop(action);
        r.on_background(&mut cx);
        // Settings (stacked above the retained reader) adds a folder.
        store.save_dictionary_folders(&[root.join("koreader")])?;
        assert_eq!(catalog(&r), ["external", "native"], "the shared service notices folder saves itself");
        r.lookup("Hello", &mut cx);
        assert_eq!(catalog(&r), ["external", "native"], "the next lookup uses the new folders");
        // Settings edits the same service while this reader is retained.
        use crate::dictionary::{
            sets::{Edit, Selection},
            worker::Worker as Dictionaries,
        };
        let settings = Dictionaries::new(store.clone(), crate::platform::Waker::new(|| {}));
        let wait = |worker: &Dictionaries, id| {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(reply) = worker.receive() {
                    assert_eq!(reply.id, id);
                    return reply;
                }
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        let edit = |change| {
            let reply = wait(&settings, settings.edit(change));
            assert!(reply.change_error.is_none() && reply.save_error.is_none());
        };
        edit(Edit::Create { id: "english".into(), name: "English".into() });
        edit(Edit::Languages { set: "english".into(), languages: vec!["eng".into()] });
        edit(Edit::Disabled { set: "english".into(), dictionary: "native.ifo".into(), disabled: false });
        // The engine tag disagrees: use library metadata, as supplied when the
        // book is handed to Reader::annotations, and then the persisted override.
        r.metadata.language = "ru".into();
        r.language_tag = "en-US".into();
        let lookup = |r: &mut Reader, cx: &mut EventCx| {
            r.lookup("Hello", cx);
            let worker = r.dictionary.as_ref().unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(reply) = worker.receive() {
                    return reply;
                }
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        let reply = lookup(&mut r, &mut cx);
        assert_eq!(reply.active_set, "english");
        assert_eq!(reply.selected.as_deref(), Some("native.ifo"));
        edit(Edit::Disabled { set: "english".into(), dictionary: "native.ifo".into(), disabled: true });
        assert_eq!(lookup(&mut r, &mut cx).state, Some(crate::dictionary::worker::State::Disabled));
        let mut state = store.book("book");
        state.dictionary_set = Some(crate::dictionary::sets::DEFAULT.into());
        store.save_book("book", &state)?;
        assert_eq!(lookup(&mut r, &mut cx).active_set, crate::dictionary::sets::DEFAULT);
        // A recreated client still resolves the persisted book choice.
        let selection = Selection { book_set: store.book("book").dictionary_set, language: "en".into() };
        let reply = wait(&settings, settings.lookup_in("Hello".into(), false, selection));
        assert_eq!(reply.active_set, crate::dictionary::sets::DEFAULT);
        drop(r);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn selection_composes_on_canvas_and_dismissal_or_sleep_restores_cached_pixels() {
        use crate::{
            app::App,
            document::{Selection, TextRange},
            geom::Point,
            platform::DeviceInfo,
        };
        for sleep in [false, true] {
            let screen = Rect::new(0, 0, 600, 800);
            let pixels: Vec<u8> = (0..600 * 800).map(|i| (i % 256) as u8).collect();
            let page = Arc::new(Page {
                generation: 1,
                number: 0,
                color: crate::gfx::ColorMode::Gray8,
                pixels: pixels.clone().into(),
                position: Position("page0".into()),
                progress: 0,
                offset: None,
                images: false,
                annotation_revision: 0,
                annotations: vec![],
                painted: vec![],
                links: vec![],
            });
            let mut r = Reader::new(
                screen,
                Rc::new(Worker::stub()),
                Store::at(std::env::temp_dir().join("verso-selection-draw-unused")),
                "book".into(),
                1,
                with_bars(false, false),
                Vec::new(),
                Position::default(),
            );
            r.shown = Some(page.clone());
            r.cache.insert(page.clone(), 0);
            r.pages = 3;
            r.laying_out = false;
            r.selection.selected = Some(Selection {
                range: TextRange { start: Position("a".into()), end: Position("b".into()) },
                text: "selected".into(),
                rects: vec![Rect::new(100, 100, 200, 30)],
                start_page: 0,
                start_visible: true,
                end_visible: true,
            });
            let info = DeviceInfo {
                name: "test".into(),
                width: 600,
                height: 800,
                dpi: 150,
                color: crate::gfx::ColorMode::Gray8,
            };
            let mut app = App::new(&info, Theme::for_dpi(150), Box::new(r));
            app.refresh_all(RefreshMode::Page);
            assert!(!app.frame().is_empty());
            assert_ne!(app.canvas().gray_data(), pixels);
            assert_eq!(page.pixels, pixels, "selection must not modify the shared page cache");
            if sleep {
                app.on_sleep();
                app.on_wake();
            } else {
                let p = Point::new(590, 400); // Forward page-turn zone.
                app.on_gesture(Gesture::Down(p));
                app.on_gesture(Gesture::Tap(p));
            }
            assert!(!app.frame().is_empty());
            assert_eq!(app.canvas().gray_data(), pixels, "same displayed page returns without selection");
        }
    }

    #[test]
    fn search_hits_highlight_navigate_and_return_to_where_search_began() {
        use crate::document::{SearchBatch, SearchHit, SearchStatus, Selection, TextRange, thread::SearchRequest};
        use crate::geom::Point;
        let (worker, commands) = Worker::recording_stub();
        let screen = Rect::new(0, 0, 600, 800);
        let theme = Theme::for_dpi(150);
        let mut r = Reader::new(
            screen,
            Rc::new(worker),
            Store::at(std::env::temp_dir().join("verso-search-reader-unused")),
            "book".into(),
            1,
            Settings::default(),
            Vec::new(),
            Position::default(),
        );
        let page = |number: usize| {
            Arc::new(Page {
                generation: 1,
                number,
                color: crate::gfx::ColorMode::Gray8,
                pixels: vec![255; 600 * 800].into(),
                position: Position(format!("page-{number}")),
                progress: 0,
                offset: None,
                images: false,
                annotation_revision: 0,
                annotations: vec![],
                painted: vec![],
                links: vec![],
            })
        };
        r.pages = 10;
        r.laying_out = false;
        r.shown = Some(page(2));
        r.target = 2;
        r.cache.insert(page(2), 2);
        r.cache.insert(page(3), 2);
        let mut cx = EventCx {
            theme: &theme,
            screen,
            now: Instant::now(),
            toast: &mut None,
            forced: &mut Vec::new(),
            needs_redraw: &mut false,
            battery: None,
        };
        // Neighbour prefetches are covered elsewhere.
        let drain = || commands.try_iter().filter(|c| !matches!(c, Command::Page { .. })).collect::<Vec<_>>();
        r.open_search(Some("  Ada "), &mut cx);
        let request = match drain().as_slice() {
            [Command::Search { request, query }] if query == "Ada" => *request,
            _ => panic!("expected one search"),
        };
        let hit = |page: usize| SearchHit {
            range: TextRange { start: Position(format!("hit-{page}")), end: Position(format!("end-{page}")) },
            page,
            before: "Paragraph. ".into(),
            matched: "Ada".into(),
            after: " watched.".into(),
        };
        let batch =
            SearchBatch { hits: vec![hit(3), hit(7)], searched_pages: 10, pages: 10, status: SearchStatus::Finished };
        let stale = SearchRequest { id: request.id + 1, ..request };
        r.receive(Reply::Search { request: stale, result: Ok(batch.clone()) }, &mut cx);
        assert!(r.search.hits.is_empty(), "replies for other requests are ignored");
        r.receive(Reply::Search { request, result: Ok(batch) }, &mut cx);
        assert_eq!(r.search.hits.len(), 2);

        // A cached hit page shows at once, then its range is highlighted.
        r.on_search(SearchEvent::Open(0), &mut cx);
        assert!(matches!(r.overlay, Overlay::None));
        assert_eq!(r.shown.as_ref().unwrap().number, 3);
        assert_eq!(r.search.origin.as_ref().unwrap().position, Position("page-2".into()));
        let highlight = match drain().as_slice() {
            [Command::Select { request, query: SelectionQuery::Range(range) }] => {
                assert_eq!(range.start, Position("hit-3".into()));
                *request
            }
            _ => panic!("expected a highlight request"),
        };
        assert_eq!((highlight.page, highlight.id & HIGHLIGHT_ID), (3, HIGHLIGHT_ID));
        let rects = vec![Rect::new(10, 10, 50, 20)];
        let found = Selection {
            start_page: 0,
            start_visible: true,
            end_visible: true,
            range: TextRange { start: Position("hit-3".into()), end: Position("end-3".into()) },
            text: "Ada".into(),
            rects: rects.clone(),
        };
        r.receive(Reply::Selection { request: highlight, result: Ok(Some(found.clone())) }, &mut cx);
        assert_eq!(r.highlight.as_ref().map(|(_, r)| r.clone()), Some(rects));

        // Touches on the bar never turn pages; touches above it still do.
        let bar = r.search_bar.as_ref().unwrap().rect();
        let on_bar = Point::new(bar.center().x - 60, bar.center().y);
        r.on_gesture(Gesture::Down(on_bar), &mut cx);
        r.on_gesture(Gesture::Tap(on_bar), &mut cx);
        assert_eq!(r.shown.as_ref().unwrap().number, 3);

        // An uncached hit asks the book thread to locate it; only that jump's
        // reply decides the page, not an earlier prefetch.
        r.on_bar(BarEvent::Next, &mut cx);
        let jump = match drain().as_slice() {
            [Command::Position { position, jump, .. }, Command::Select { request, .. }] => {
                assert_eq!(*position, Position("hit-7".into()));
                assert_eq!(request.page, 7);
                *jump
            }
            _ => panic!("expected a jump and a highlight"),
        };
        r.receive(Reply::Page(page(4)), &mut cx);
        assert_eq!(r.shown.as_ref().unwrap().number, 3);
        r.receive(Reply::Jumped { jump, page: page(7) }, &mut cx);
        assert_eq!(r.shown.as_ref().unwrap().number, 7);
        assert!(r.highlight.is_none(), "a different page drops the old highlight");
        assert_eq!(r.search.current, Some(1));

        r.on_bar(BarEvent::Return, &mut cx);
        let jump = match drain().as_slice() {
            [Command::Position { position, jump, .. }] if *position == Position("page-2".into()) => *jump,
            _ => panic!("expected a jump back to the origin"),
        };
        r.receive(Reply::Jumped { jump, page: page(2) }, &mut cx);
        assert_eq!(r.shown.as_ref().unwrap().number, 2);
        r.cache.insert(page(3), 2);
        assert!(r.search.origin.is_none() && r.search_bar.is_none());
        let forward = Point::new(590, 700);
        r.on_gesture(Gesture::Down(forward), &mut cx);
        r.on_gesture(Gesture::Tap(forward), &mut cx);
        assert_eq!(r.shown.as_ref().unwrap().number, 3, "normal reading resumes");

        // The session survives; a relayout re-runs the query for its pages.
        r.open_search(None, &mut cx);
        assert_eq!(r.search.hits.len(), 2);
        assert!(drain().is_empty());
        r.overlay = Overlay::None;
        r.changed();
        r.on_tick(&mut EventCx { now: Instant::now() + Duration::from_secs(1), ..cx });
        match drain().as_slice() {
            [Command::Layout { generation: 2, .. }, Command::Search { request, query }] => {
                assert_eq!((request.generation, query.as_str()), (2, "Ada"));
            }
            _ => panic!("expected a relayout and a fresh search"),
        }
    }

    #[test]
    fn grayscale_pages_restore_after_menus_usb_and_wake_with_or_without_bars() {
        use crate::{
            app::App,
            geom::Point,
            platform::{DeviceInfo, Update},
        };
        let info =
            DeviceInfo { name: "test".into(), width: 600, height: 800, dpi: 150, color: crate::gfx::ColorMode::Gray8 };
        let screen = Rect::new(0, 0, 600, 800);
        let root = std::env::temp_dir().join(format!("verso-gray-reader-{}", std::process::id()));
        for (top, bottom) in [(false, false), (false, true), (true, true)] {
            let page = |number| {
                Arc::new(Page {
                    generation: 1,
                    number,
                    color: crate::gfx::ColorMode::Gray8,
                    pixels: (0..600 * 800).map(|i| ((i + number * 31) % 256) as u8).collect::<Vec<_>>().into(),
                    position: Position(format!("page-{number}")),
                    progress: 2500,
                    offset: None,
                    images: false,
                    annotation_revision: 0,
                    annotations: vec![],
                    painted: vec![],
                    links: vec![],
                })
            };
            let mut reader = Reader::new(
                screen,
                Rc::new(Worker::stub()),
                Store::at(root.clone()),
                "book".into(),
                1,
                Settings {
                    page_layout: crate::page_layout::PageLayout {
                        progress: crate::page_layout::ProgressLine::Top,
                        ..with_bars(top, bottom).page_layout
                    },
                    ..Settings::default()
                },
                Vec::new(),
                Position::default(),
            );
            reader.shown = Some(page(0));
            reader.cache.insert(page(1), 0);
            reader.pages = 3;
            reader.laying_out = false;
            let mut app = App::new(&info, Theme::for_dpi(150), Box::new(reader));
            app.refresh_all(RefreshMode::Page);
            assert_eq!(app.frame(), vec![Update { rect: screen, mode: RefreshMode::Page }]);
            let first = app.canvas().gray_data().to_vec();
            for _ in 0..2 {
                app.on_gesture(Gesture::Down(Point::new(300, 400)));
                app.on_gesture(Gesture::Tap(Point::new(300, 400)));
                assert!(!app.frame().is_empty());
            }
            assert_eq!(app.canvas().gray_data(), first);
            // The next cached turn must bypass comparison even after an overlay.
            app.on_gesture(Gesture::Tap(Point::new(550, 400)));
            assert_eq!(app.frame(), vec![Update { rect: screen, mode: RefreshMode::Page }]);
            app.after_present();
            let second = app.canvas().gray_data().to_vec();
            assert_ne!(first, second);
            app.on_usb(true);
            assert!(!app.frame().is_empty());
            app.on_usb(false);
            assert!(!app.frame().is_empty());
            assert_eq!(app.canvas().gray_data(), second);
            app.on_sleep();
            assert!(app.frame().is_empty());
            app.on_wake();
            assert_eq!(app.frame(), vec![Update { rect: screen, mode: RefreshMode::Full }]);
            assert_eq!(app.canvas().gray_data(), second);
            app.on_tick(Instant::now() + Duration::from_secs(2));
            assert_eq!(app.frame(), vec![Update { rect: Rect::new(0, 719, 600, 81), mode: RefreshMode::Ui }]);
            assert_eq!(app.canvas().gray_data(), second);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cache_replies_and_bare_touches_do_not_invalidate_but_pages_and_controls_do() {
        use crate::geom::Point;
        let theme = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut r = Reader::new(
            screen,
            Rc::new(Worker::stub()),
            Store::at(std::env::temp_dir().join("verso-redraw-test-unused")),
            "book".into(),
            1,
            Settings::default(),
            Vec::new(),
            Position::default(),
        );
        let mut cx = EventCx {
            theme: &theme,
            screen,
            now: Instant::now(),
            toast: &mut None,
            forced: &mut Vec::new(),
            needs_redraw: &mut false,
            battery: None,
        };
        let page = |generation, number| {
            Reply::Page(Arc::new(Page {
                generation,
                number,
                color: crate::gfx::ColorMode::Gray8,
                pixels: Vec::new().into(),
                position: Position(format!("page-{number}")),
                progress: 2500,
                offset: None,
                images: false,
                annotation_revision: 0,
                annotations: vec![],
                painted: vec![],
                links: vec![],
            }))
        };
        r.receive(
            Reply::Opened { generation: 1, metadata: Metadata::default(), toc: Vec::new(), pages: 10, current: 0 },
            &mut cx,
        );
        assert!(!*cx.needs_redraw);
        r.receive(page(1, 0), &mut cx);
        assert!(*cx.needs_redraw);
        *cx.needs_redraw = false;
        cx.forced.clear();
        for reply in [page(1, 0), page(1, 1), page(0, 2)] {
            r.receive(reply, &mut cx);
            assert!(!*cx.needs_redraw);
            assert!(cx.forced.is_empty());
        }
        assert!(r.cache.get(1).is_some());
        // Only inward swipes up from the bottom edge open the controls (the
        // top edge belongs to the Top Drawer, in `App`). Opening them
        // preserves the displayed page and requests a redraw.
        for (dir, from_y, to_y, opens) in [
            (Direction::Down, 0, 400, false),
            (Direction::Down, 168, 500, false),
            (Direction::Down, 169, 500, false),
            (Direction::Up, 1695, 1300, true),
            (Direction::Up, 1527, 1200, true),
            (Direction::Up, 1526, 1200, false),
            (Direction::Up, 100, 0, false),
            (Direction::Down, 1600, 1695, false),
            (Direction::Down, 700, 1100, false),
            (Direction::Up, 1100, 700, false),
        ] {
            let from = Point::new(636, from_y);
            let to = Point::new(636, to_y);
            for g in [Gesture::Down(from), Gesture::Move(to), Gesture::Swipe { dir, from, to }] {
                r.on_gesture(g, &mut cx);
            }
            assert_eq!(matches!(r.overlay, Overlay::Menu(_)), opens, "{dir:?} from {from_y}");
            assert_eq!(*cx.needs_redraw, opens);
            assert_eq!(r.target, 0);
            assert_eq!(r.shown.as_ref().unwrap().number, 0);
            assert!(cx.forced.is_empty());
            r.overlay = Overlay::None;
            *cx.needs_redraw = false;
        }
        // Horizontal swipes still turn pages, including from the same edges.
        for (dir, from_y, expected) in [(Direction::Left, 100, 1), (Direction::Right, 1600, 0)] {
            let (from_x, to_x) = if dir == Direction::Left { (1000, 300) } else { (300, 1000) };
            r.on_gesture(
                Gesture::Swipe { dir, from: Point::new(from_x, from_y), to: Point::new(to_x, from_y) },
                &mut cx,
            );
            assert!(matches!(r.overlay, Overlay::None));
            assert_eq!(r.shown.as_ref().unwrap().number, expected);
            assert!(*cx.needs_redraw);
            *cx.needs_redraw = false;
            cx.forced.clear();
        }
        let p = Point::new(1100, 1000);
        for g in [Gesture::Down(p), Gesture::Move(p), Gesture::Cancel(p)] {
            r.on_gesture(g, &mut cx);
            assert!(!*cx.needs_redraw);
        }
        // A cached turn updates both page and footer and keeps the refresh policy.
        r.on_gesture(Gesture::Tap(p), &mut cx);
        assert!(*cx.needs_redraw);
        assert_eq!(r.shown.as_ref().unwrap().number, 1);
        assert_eq!(cx.forced.len(), 1);
        *cx.needs_redraw = false;
        r.on_gesture(Gesture::Tap(Point::new(636, 848)), &mut cx);
        assert!(*cx.needs_redraw);
        let slider = match &r.overlay {
            Overlay::Menu(menu) => menu.progress.rect(),
            _ => panic!("menu did not open"),
        };
        for g in [Gesture::Down(slider.center()), Gesture::Move(Point::new(slider.right(), slider.center().y))] {
            *cx.needs_redraw = false;
            r.on_gesture(g, &mut cx);
            assert!(*cx.needs_redraw, "slider must update before release");
        }
    }
    #[test]
    fn failed_opens_keep_state_successful_presentation_clears_flags_and_saves_preserve_later_flags()
    -> anyhow::Result<()> {
        let root = std::env::temp_dir().join(format!("verso-reader-library-{}", std::process::id()));
        let store = Store::at(root.clone());
        let saved = BookState {
            position: Position("old".into()),
            finished: true,
            hidden_from_continue: true,
            last_opened: 10,
            dictionary_set: Some("english".into()),
            ..Default::default()
        };
        store.save_book("book", &saved)?;
        let screen = Rect::new(0, 0, 20, 20);
        let mut r = Reader::new(
            screen,
            Rc::new(Worker::stub()),
            store.clone(),
            "book".into(),
            1,
            store.settings(),
            Vec::new(),
            saved.position,
        );
        let theme = Theme::for_dpi(300);
        let mut toast = None;
        let mut forced = Vec::new();
        let mut cx = EventCx {
            theme: &theme,
            screen,
            now: Instant::now(),
            toast: &mut toast,
            forced: &mut forced,
            needs_redraw: &mut false,
            battery: None,
        };
        r.receive(Reply::Error { generation: 1, message: "failed open".into() }, &mut cx);
        r.after_present();
        assert!(r.failed_open());
        assert!(store.book("book").finished && store.book("book").hidden_from_continue);
        assert_eq!(store.book("book").last_opened, 10);
        let page = |number| {
            Arc::new(Page {
                generation: 1,
                number,
                color: crate::gfx::ColorMode::Gray8,
                pixels: vec![255; 400].into(),
                position: Position(format!("page-{number}")),
                progress: 2500,
                offset: None,
                images: false,
                annotation_revision: 0,
                annotations: vec![],
                painted: vec![],
                links: vec![],
            })
        };
        r.receive(
            Reply::Opened { generation: 1, metadata: Metadata::default(), toc: Vec::new(), pages: 10, current: 0 },
            &mut cx,
        );
        r.receive(Reply::Page(page(0)), &mut cx);
        // A render reply alone must not overwrite the old place before presentation.
        assert_eq!(store.book("book").position.0, "old");
        r.after_present();
        r.flush_position()?;
        let mut state = store.book("book");
        assert!(!state.finished && !state.hidden_from_continue);
        assert!(state.last_opened > 10);
        assert_eq!(state.dictionary_set.as_deref(), Some("english"));
        state.dictionary_set = Some("russian".into());
        state.hidden_from_continue = true;
        store.save_book("book", &state)?;
        r.target = 1;
        r.receive(Reply::Page(page(1)), &mut cx);
        r.after_present();
        r.flush_position()?;
        assert!(store.book("book").hidden_from_continue);
        assert_eq!(store.book("book").position.0, "page-1");
        assert_eq!(store.book("book").dictionary_set.as_deref(), Some("russian"));
        // Cached neighbours, stale generations and an undisplayed target must
        // not replace the last submitted position at any lifecycle barrier.
        r.receive(Reply::Page(page(2)), &mut cx);
        let mut stale = (*page(0)).clone();
        stale.generation = 0;
        r.receive(Reply::Page(Arc::new(stale)), &mut cx);
        r.target = 3;
        r.receive(Reply::Page(page(3)), &mut cx);
        r.on_sleep();
        assert_eq!(store.book("book").page, 1);
        r.on_quit()?;
        assert_eq!(store.book("book").page, 1);
        r.close()?;
        assert_eq!(store.book("book").page, 1);
        r.after_present();
        r.on_sleep();
        assert_eq!(store.book("book").page, 3);
        r.target = 4;
        r.receive(Reply::Page(page(4)), &mut cx);
        r.after_present();
        r.on_quit()?;
        assert_eq!(store.book("book").page, 4);
        r.target = 5;
        r.receive(Reply::Page(page(5)), &mut cx);
        r.after_present();
        r.close()?;
        assert_eq!(store.book("book").page, 5);

        // An actual filesystem failure is visible, blocks close/exit, and
        // survives sleep until recovery. Retrying saves the latest display.
        *cx.toast = None;
        let backup = root.with_extension("backup");
        std::fs::rename(&root, &backup)?;
        std::fs::write(&root, b"blocked directory")?;
        r.target = 6;
        r.receive(Reply::Page(page(6)), &mut cx);
        r.after_present();
        assert!(r.persistence.flush().is_err());
        r.on_background(&mut cx);
        assert!(cx.toast.is_some());
        assert!(r.save_error.is_some());
        assert!(r.close().is_err());
        assert!(r.on_quit().is_err());
        r.on_sleep();
        assert!(r.save_error.is_some());
        std::fs::remove_file(&root)?;
        std::fs::rename(&backup, &root)?;
        r.close()?;
        assert!(r.save_error.is_none());
        assert_eq!(store.book("book").page, 6);
        drop(r);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn queued_turns_in_one_batch_skip_intermediate_pages() {
        let (mut r, commands) = test_reader("batch-skip");
        let theme = Theme::for_dpi(150);
        r.pages = 20;
        r.laying_out = false;
        r.target = 5;
        r.shown = Some(test_page(1, 5));
        r.cache.insert(test_page(1, 5), 5);
        r.cache.insert(test_page(1, 6), 5); // already prefetched
        let forward = crate::geom::Point::new(500, 400);
        let back = crate::geom::Point::new(50, 400);

        // Three rapid forward taps queued together skip cached page 6 and uncached page 7.
        test_cx(&theme, |cx| {
            r.begin_batch();
            for _ in 0..3 {
                r.on_gesture(Gesture::Tap(forward), cx);
            }
            assert_eq!(r.target, 8);
            assert_eq!(r.shown.as_ref().unwrap().number, 5);
            assert!(!*cx.needs_redraw);
            assert!(cx.forced.is_empty());
            assert!(commands.try_iter().next().is_none());
            r.finish_batch(cx);
            assert_eq!(r.shown.as_ref().unwrap().number, 5, "stale cached page 6 must not be shown");
            assert!(cx.forced.is_empty());
            let requested: Vec<_> = commands
                .try_iter()
                .filter_map(|c| match c {
                    Command::Page { number, .. } => Some(number),
                    _ => None,
                })
                .collect();
            assert_eq!(requested, vec![8], "only the final target page should be requested");
            r.receive(Reply::Page(test_page(1, 8)), cx);
            assert_eq!(r.shown.as_ref().unwrap().number, 8);
            assert_eq!(cx.forced.len(), 1);
            // Drain neighbour prefetches queued by display(8).
            let _ = commands.try_iter().count();
        });

        // Forward then back in the same batch cancels out with no redraw or request.
        r.cache.insert(test_page(1, 9), 8);
        test_cx(&theme, |cx| {
            r.begin_batch();
            r.on_gesture(Gesture::Tap(forward), cx);
            r.on_gesture(Gesture::Tap(back), cx);
            r.finish_batch(cx);
            assert_eq!(r.target, 8);
            assert_eq!(r.shown.as_ref().unwrap().number, 8);
            assert!(!*cx.needs_redraw);
            assert!(cx.forced.is_empty());
            assert!(commands.try_iter().next().is_none());
        });

        // A single cached tap in a batch still displays immediately on finish_batch.
        test_cx(&theme, |cx| {
            r.begin_batch();
            r.on_gesture(Gesture::Tap(forward), cx);
            r.finish_batch(cx);
            assert_eq!(r.shown.as_ref().unwrap().number, 9);
            assert_eq!(cx.forced.len(), 1);
        });

        // A background page reply arriving mid-batch doesn't display before later taps run.
        test_cx(&theme, |cx| {
            r.begin_batch();
            r.on_gesture(Gesture::Tap(forward), cx); // target -> 10
            r.receive(Reply::Page(test_page(1, 10)), cx);
            assert_eq!(r.shown.as_ref().unwrap().number, 9);
            r.on_gesture(Gesture::Tap(forward), cx); // target -> 11
            r.finish_batch(cx);
            assert_eq!(r.shown.as_ref().unwrap().number, 9);
            assert_eq!(r.target, 11);
        });

        // A turn followed by opening the menu in the same batch commits the turn first.
        r.shown = Some(test_page(1, 9));
        r.target = 9;
        r.cache.insert(test_page(1, 10), 9);
        test_cx(&theme, |cx| {
            r.begin_batch();
            r.on_gesture(Gesture::Tap(forward), cx);
            r.on_gesture(Gesture::Tap(crate::geom::Point::new(300, 400)), cx);
            r.finish_batch(cx);
            assert_eq!(r.shown.as_ref().unwrap().number, 10);
            assert!(matches!(r.overlay, Overlay::Menu(_)));
        });

        let _ = std::fs::remove_dir_all(settings_path(&r).parent().unwrap());
    }

    #[test]
    fn page_buttons_turn_pages_in_plain_reading_only() {
        let (mut r, commands) = test_reader("page-buttons");
        let theme = Theme::for_dpi(150);
        r.pages = 20;
        r.laying_out = false;
        r.target = 5;
        r.shown = Some(test_page(1, 5));
        r.cache.insert(test_page(1, 5), 5);
        r.cache.insert(test_page(1, 6), 5);
        r.cache.insert(test_page(1, 4), 5);
        test_cx(&theme, |cx| {
            r.on_key(KeyAction::PageNext, cx);
            assert_eq!(r.target, 6);
            assert_eq!(r.shown.as_ref().unwrap().number, 6, "a cached page shows at once");
            r.cache.insert(test_page(1, 5), 6);
            r.on_key(KeyAction::PagePrev, cx);
            r.on_key(KeyAction::PagePrev, cx);
            assert_eq!(r.target, 4);
        });
        // The first page stays put going back.
        r.target = 0;
        r.shown = Some(test_page(1, 0));
        test_cx(&theme, |cx| {
            r.on_key(KeyAction::PagePrev, cx);
            assert_eq!(r.target, 0);
        });
        // Over the Reader Menu a button does nothing.
        test_cx(&theme, |cx| {
            r.menu(cx);
            assert!(matches!(r.overlay, Overlay::Menu(_)));
            r.on_key(KeyAction::PageNext, cx);
            assert_eq!(r.target, 0);
            assert!(matches!(r.overlay, Overlay::Menu(_)));
        });
        let _ = commands.try_iter().count();
        let _ = std::fs::remove_dir_all(settings_path(&r).parent().unwrap());
    }

    #[test]
    fn the_corner_and_the_menu_ribbon_toggle_the_bookmark_without_rendering_the_page() {
        use crate::document::{Opening, TextRange};
        use crate::geom::Point;
        let theme = Theme::for_dpi(150);
        let (mut r, commands) = test_reader("bookmarks");
        r.pages = 10;
        r.laying_out = false;
        r.target = 3;
        r.shown = Some(test_page(1, 3));
        r.cache.insert(test_page(1, 3), 3);
        r.cache.insert(test_page(1, 4), 3);
        r.annotations.ready = true;
        let corner = Point::new(598, 2);
        let tap = |r: &mut Reader, p: Point, cx: &mut EventCx| {
            r.on_gesture(Gesture::Down(p), cx);
            r.on_gesture(Gesture::Tap(p), cx);
        };
        let opening = |commands: &std::sync::mpsc::Receiver<Command>| {
            commands.try_iter().find_map(|c| match c {
                Command::Opening(request) => Some(request),
                _ => None,
            })
        };
        let reply = |request| Reply::Opening {
            request,
            result: Ok(Opening {
                range: TextRange { start: Position("/p[3]".into()), end: Position("/p[3]/text().20".into()) },
                text: "It was a bright cold".into(),
            }),
        };
        let draw = |r: &mut Reader| {
            let (mut canvas, mut text, mut icons) = (
                crate::gfx::Canvas::new(600, 800),
                crate::gfx::TextRenderer::new(),
                crate::gfx::IconRenderer::default(),
            );
            let mut hints = vec![];
            r.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &theme, &mut hints));
            let mark = r.mark_rect(&theme);
            let inked = canvas.gray_data()[(mark.center().y * 600 + mark.center().x) as usize] == INK;
            (inked, hints.iter().any(|h| h.rect == mark && h.mode == RefreshMode::Fast))
        };
        assert_eq!(draw(&mut r), (false, false));
        // The corner tap asks for the page's opening words, then shows the mark,
        // updating just the corner, fast. The page stays the cached one.
        test_cx(&theme, |cx| tap(&mut r, corner, cx));
        let request = opening(&commands).expect("asks for the opening words");
        assert_eq!((request.page, r.target), (3, 3), "a corner tap never turns the page");
        test_cx(&theme, |cx| r.receive(reply(request), cx));
        assert!(r.annotations.bookmarked(3));
        assert_eq!(draw(&mut r), (true, true));
        assert_eq!(draw(&mut r), (true, false), "the fast update happens once");
        assert!(!commands.try_iter().any(|c| matches!(c, Command::Page { .. } | Command::Resolve { .. })));
        r.annotations.pending = false;
        // The menu's ribbon shows the state and removes it; the menu stays.
        test_cx(&theme, |cx| {
            r.menu(cx);
            let Overlay::Menu(menu) = &r.overlay else { unreachable!() };
            let ribbon = menu.bookmark_rect().center();
            tap(&mut r, ribbon, cx);
        });
        assert!(matches!(r.overlay, Overlay::Menu(_)) && !r.annotations.bookmarked(3));
        assert!(!r.mark_fast, "under the menu the page is drawn again when it closes");
        // Tapping again while the removal saves says it isn't ready yet.
        let mut toast = None;
        let Overlay::Menu(menu) = &r.overlay else { unreachable!() };
        let ribbon = menu.bookmark_rect().center();
        r.on_gesture(
            Gesture::Down(ribbon),
            &mut EventCx {
                theme: &theme,
                screen: r.screen,
                now: Instant::now(),
                toast: &mut toast,
                forced: &mut vec![],
                needs_redraw: &mut false,
                battery: None,
            },
        );
        r.on_gesture(
            Gesture::Tap(ribbon),
            &mut EventCx {
                theme: &theme,
                screen: r.screen,
                now: Instant::now(),
                toast: &mut toast,
                forced: &mut vec![],
                needs_redraw: &mut false,
                battery: None,
            },
        );
        assert_eq!(toast.map(|t| t.text).as_deref(), Some(annotations::NOT_READY));
        r.annotations.pending = false;
        r.overlay = Overlay::None;
        // A bookmark with a note is removed only after asking.
        test_cx(&theme, |cx| tap(&mut r, corner, cx));
        let request = opening(&commands).unwrap();
        test_cx(&theme, |cx| r.receive(reply(request), cx));
        r.annotations.pending = false;
        r.annotations.collection.records[0].note = Some("Opening image".into());
        for remove in [false, true] {
            test_cx(&theme, |cx| tap(&mut r, corner, cx));
            let Overlay::RemoveBookmark(confirm) = &r.overlay else { panic!("asks first") };
            let button = if remove { confirm.delete.rect() } else { confirm.cancel.rect() };
            test_cx(&theme, |cx| tap(&mut r, button.center(), cx));
            assert!(matches!(r.overlay, Overlay::None));
            assert_eq!(r.annotations.bookmarked(3), !remove);
        }
        // With the corner tap off, the corner turns the page like the rest of
        // the forward zone.
        r.annotations.pending = false;
        r.settings.bookmark_corner = false;
        test_cx(&theme, |cx| tap(&mut r, corner, cx));
        assert_eq!(r.target, 4);
        assert!(opening(&commands).is_none());
        let _ = std::fs::remove_dir_all(settings_path(&r).parent().unwrap());
    }

    #[test]
    fn menu_opens_quick_settings_and_the_drawer_only_in_plain_reading() {
        let theme = Theme::for_dpi(150);
        let (mut r, _) = test_reader("menu-quick-settings");
        r.pages = 10;
        r.laying_out = false;
        r.metadata.title = "Ender's Game".into();
        assert!(r.allows_top_drawer(), "plain reading");
        assert_eq!(r.top_drawer_book(), Some(TopDrawerBook { title: "Ender's Game".into() }));
        test_cx(&theme, |cx| {
            // Both the Sun button and the Battery indicator close the menu and ask for the drawer.
            for target in [true, false] {
                r.menu(cx);
                assert!(r.allows_top_drawer(), "the drawer replaces the menu");
                let Overlay::Menu(menu) = &r.overlay else { unreachable!() };
                let p = if target { menu.light_rect().center() } else { menu.battery_rect().center() };
                r.on_gesture(Gesture::Down(p), cx);
                assert!(matches!(r.on_gesture(Gesture::Tap(p), cx), Action::OpenTopDrawer));
                assert!(matches!(r.overlay, Overlay::None));
            }
            // In-book drawer actions open search and close the book.
            assert!(matches!(r.on_top_drawer_book(TopDrawerBookAction::Search, cx), Action::None));
            assert!(matches!(r.overlay, Overlay::Search(_)));
            r.overlay = Overlay::None;
            assert!(matches!(r.on_top_drawer_book(TopDrawerBookAction::CloseBook, cx), Action::Pop));
            // A swipe opening the drawer over the menu closes it too.
            r.menu(cx);
        });
        r.top_drawer_opened();
        assert!(matches!(r.overlay, Overlay::None));
        r.top_drawer_opened();
        assert!(matches!(r.overlay, Overlay::None), "nothing else is closed");
        r.text(&theme);
        assert!(!r.allows_top_drawer(), "a sheet is open");
        r.overlay = Overlay::None;
        r.extension = Some(annotations::ExtendBar::new(&theme, r.screen));
        assert!(!r.allows_top_drawer(), "extending a highlight");
        r.extension = None;
        r.selection.extending = true;
        assert!(!r.allows_top_drawer(), "selecting");
        r.selection.clear();
        assert!(r.allows_top_drawer());
        let _ = std::fs::remove_dir_all(settings_path(&r).parent().unwrap());
    }

    #[test]
    fn settings_saved_over_the_book_apply_without_reopening_it() -> anyhow::Result<()> {
        let theme = Theme::for_dpi(150);
        let (mut r, _) = test_reader("settings-sync");
        let mut settings = r.store.settings();
        settings.typography.size_pt = 20.0;
        settings.tap_zones = crate::reader::TapZones::Halves;
        settings.page_flash = 3;
        settings.developer = true;
        r.store.save_settings(&settings)?;
        let size = r.settings.typography.size_pt;
        test_cx(&theme, |cx| r.on_background(cx));
        assert_eq!(r.settings.tap_zones, crate::reader::TapZones::Halves);
        assert_eq!(r.settings.page_flash, 3);
        assert!(r.settings.developer);
        assert_eq!(r.settings.typography.size_pt, size, "the reader keeps its own typography");
        // The new refresh interval applies to the next turns.
        let start = Instant::now();
        let modes: Vec<_> =
            (0..3u32).map(|i| r.refresh.turn(false, false, start + i * Duration::from_secs(1))).collect();
        assert_eq!(modes, [RefreshMode::Page, RefreshMode::Page, RefreshMode::Full]);
        let _ = std::fs::remove_dir_all(settings_path(&r).parent().unwrap());
        Ok(())
    }

    #[test]
    fn menu_ignores_noop_outside_down_but_redraws_on_press_drag_and_close() {
        let theme = Theme::for_dpi(150);
        let (mut r, _) = test_reader("menu-noop");
        r.pages = 20;
        r.laying_out = false;
        r.shown = Some(test_page(1, 5));
        r.target = 5;
        let run = |r: &mut Reader, g: Gesture| {
            let mut toast = None;
            let mut forced = Vec::new();
            let mut needs_redraw = false;
            let mut cx = EventCx {
                theme: &theme,
                screen: r.screen,
                now: Instant::now(),
                battery: None,
                toast: &mut toast,
                forced: &mut forced,
                needs_redraw: &mut needs_redraw,
            };
            r.on_gesture(g, &mut cx);
            needs_redraw
        };
        test_cx(&theme, |cx| r.menu(cx));
        let Overlay::Menu(menu) = &r.overlay else { unreachable!() };
        let outside = crate::geom::Point::new(300, 400);
        let text_btn = menu.button_rects()[2].center();
        let slider_right = crate::geom::Point::new(menu.progress.rect().right() - 4, menu.progress.rect().center().y);
        // Down outside the menu changes nothing visually.
        assert!(!run(&mut r, Gesture::Down(outside)));
        assert!(!run(&mut r, Gesture::Move(outside)));
        // Pressing and cancelling a button redraws immediately.
        assert!(run(&mut r, Gesture::Down(text_btn)));
        assert!(run(&mut r, Gesture::Cancel(text_btn)));
        assert!(!run(&mut r, Gesture::Cancel(text_btn)));
        // Scrubbing the progress slider redraws as value changes.
        assert!(run(&mut r, Gesture::Down(slider_right)));
        assert!(run(&mut r, Gesture::Tap(slider_right)));
        // Tap outside closes the menu and redraws.
        assert!(!run(&mut r, Gesture::Down(outside)));
        assert!(run(&mut r, Gesture::Tap(outside)));
        assert!(matches!(r.overlay, Overlay::None));
        let _ = std::fs::remove_dir_all(settings_path(&r).parent().unwrap());
    }

    #[test]
    fn opening_notice_is_deferred_and_uses_one_full_ui_refresh_when_slow() {
        let theme = Theme::for_dpi(150);
        let (mut r, _) = test_reader("opening-defer");
        // Immediately after open, Reader leaves the canvas intact until OPENING_NOTICE_DELAY.
        assert!(r.waiting_for_open());
        assert!(r.fills_canvas());
        assert!(r.deadline().is_some());
        let mut canvas = crate::gfx::Canvas::new(r.screen.w as u32, r.screen.h as u32);
        canvas.fill(119);
        let (mut text, mut icons, mut hints) =
            (crate::gfx::TextRenderer::new(), crate::gfx::IconRenderer::default(), Vec::new());
        r.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &theme, &mut hints));
        assert!(canvas.gray_data().iter().all(|&p| p == 119), "canvas left untouched during grace period");
        // When the 150 ms timer fires before page 0 arrives, it requests one full Ui refresh.
        let due = r.opening_notice_at.unwrap();
        let mut toast = None;
        let mut forced = Vec::new();
        let mut needs_redraw = false;
        let mut cx = EventCx {
            theme: &theme,
            screen: r.screen,
            now: due,
            battery: None,
            toast: &mut toast,
            forced: &mut forced,
            needs_redraw: &mut needs_redraw,
        };
        r.on_tick(&mut cx);
        assert!(!r.waiting_for_open());
        assert!(!r.fills_canvas());
        assert!(needs_redraw);
        assert_eq!(forced, vec![crate::platform::Update { rect: r.screen, mode: RefreshMode::Ui }]);
        // If page 0 arrives before the deadline, the timer is cleared cleanly.
        let (mut r2, _) = test_reader("opening-fast");
        assert!(r2.opening_notice_at.is_some());
        test_cx(&theme, |cx| r2.display(test_page(1, 0), cx));
        assert!(r2.opening_notice_at.is_none());
        assert!(r2.fills_canvas());
        let _ = std::fs::remove_dir_all(settings_path(&r).parent().unwrap());
        let _ = std::fs::remove_dir_all(settings_path(&r2).parent().unwrap());
    }

    #[test]
    fn non_visual_reader_ticks_do_not_redraw_but_stepper_repeat_does() {
        let theme = Theme::for_dpi(150);
        let (mut r, _) = test_reader("reader-ticks");
        r.opening_notice_at = None;
        r.shown = Some(test_page(1, 0));
        r.laying_out = false;
        let now = Instant::now();
        r.layout_at = Some(now);
        let mut toast = None;
        let mut forced = Vec::new();
        let mut needs_redraw = false;
        let mut cx = EventCx {
            theme: &theme,
            screen: r.screen,
            now,
            battery: None,
            toast: &mut toast,
            forced: &mut forced,
            needs_redraw: &mut needs_redraw,
        };
        r.on_tick(&mut cx);
        assert!(r.laying_out && r.layout_at.is_none());
        assert!(!needs_redraw, "debounced layout_at sends command without a redundant redraw");
        // Held TextSheet stepper repeats and redraws.
        r.text(&theme);
        let Overlay::Text(sheet) = &mut r.overlay else { unreachable!() };
        let more = crate::geom::Point::new(
            r.screen.w - theme.margin - 4,
            r.screen.bottom() - theme.mm(64.0) + theme.mm(13.0) + theme.mm(11.5) + theme.mm(5.5),
        );
        let _ = sheet.on_gesture(&Gesture::Down(more), &mut r.settings.typography);
        let _ = sheet.on_gesture(&Gesture::LongPress(more), &mut r.settings.typography);
        let repeat_due = r.deadline().expect("stepper repeat deadline");
        let mut needs_redraw = false;
        let mut cx = EventCx {
            theme: &theme,
            screen: r.screen,
            now: repeat_due,
            battery: None,
            toast: &mut toast,
            forced: &mut forced,
            needs_redraw: &mut needs_redraw,
        };
        r.on_tick(&mut cx);
        assert!(needs_redraw, "held stepper tick updates visible font size");
        let _ = std::fs::remove_dir_all(settings_path(&r).parent().unwrap());
    }
}
