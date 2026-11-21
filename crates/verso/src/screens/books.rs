//! Paginated library, engine-independent discovery, and the existing reader flow.
use crate::document::{
    FontFace, Layout, Typography,
    thread::{Command, Reply, Worker},
};
use crate::geom::{Point, Rect};
use crate::gfx::{Family, HAlign, Icon, TextStyle, Weight};
use crate::library::{
    self, Book, Cover, LibraryIndex, PathChange,
    collections::{self, Collections, Navigation},
    covers, files,
    home::Summary,
    previews::Previews,
    search::{self, Scope, SearchCache},
    worker::{LISTED_STATES, Snapshot, WarmRequest, WarmResult, WarmTile, Worker as LibraryWorker},
};
use crate::page_layout::PageLayout;
use crate::platform::{RefreshMode, TextInput};
use crate::screens::reader::Reader;
use crate::screens::update::{OfferEvent, OfferSheet, UpdateScreen};
use crate::screens::{Action, EventCx, Screen};
use crate::store::{BookState, FolderPreview, Store, navigation::Writer as NavigationWriter};
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK, INK2, LINE, PAPER, Theme};
use crate::ui::widget::{DrawCx, Press, PressEvent};
use crate::ui::widgets::{
    Button, ButtonKind, Grid, GridEvent, IconButton, Keyboard, MenuEvent, Pager, PagerEvent, PopupMenu, Sheet, Slider,
    SliderEvent, TextField,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use verso_update::check::Offer;

mod folder_tile;
mod moving;
mod selection;
mod thumbnails;
use moving::{Carry, ClashSheet, PasteEnd};
use selection::Selection;
use thumbnails::{CoverSpec, Fit, Thumbnails};

const SEARCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(180);
/// Untouched this long with the library settled, books likely to be opened
/// next are laid out in the background (docs/prelayout.md).
const PRELAYOUT_IDLE: std::time::Duration = std::time::Duration::from_secs(5);
/// Idle time on Home before the library's first page is prepared.
const WARM_IDLE: std::time::Duration = std::time::Duration::from_secs(1);
/// Below this charge, unless charging, nothing is laid out in the background.
const PRELAYOUT_MIN_BATTERY: u8 = 20;

struct SearchSession {
    field: TextField,
    folded: String,
    debounce_at: Option<std::time::Instant>,
    scope: Scope,
    collection: Option<PathBuf>,
    page: usize,
    cache: SearchCache,
    all: Button,
    current: Button,
    keyboard: Keyboard,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Target {
    Book(String),
    Folder(PathBuf),
}

#[derive(Clone)]
enum NameOperation {
    Create(PathBuf),
    Rename(PathBuf),
}
enum NameEvent {
    Submit,
    Cancel,
}
struct NameSheet {
    sheet: Sheet,
    operation: NameOperation,
    field: TextField,
    keyboard: Keyboard,
    submit: Button,
    cancel: Button,
    error: String,
    error_rect: Rect,
}
impl NameSheet {
    fn new(t: &Theme, screen: Rect, operation: NameOperation) -> Self {
        let (title, label, value) = match &operation {
            NameOperation::Create(_) => ("New folder", "Create", String::new()),
            NameOperation::Rename(path) => ("Rename folder", "Rename", collections::name(path)),
        };
        let mut keyboard = Keyboard::default();
        keyboard.set_submit(label);
        keyboard.visible = true;
        keyboard.place(screen, t);
        // Reserve the tallest language keyboard, keeping the editor stationary
        // while languages change and on the smaller simulator panels.
        let height = (screen.bottom() - keyboard.rect().y + t.target + t.mm(50.0)).min(screen.h - t.top_bar);
        let sheet = Sheet::open(title, height, screen, t);
        let area = sheet.content(t);
        let mut field = TextField::new("Folder name");
        field.edit(&TextInput::Insert(value));
        field.focused = true;
        let landscape = screen.w > screen.h;
        let field_w = if landscape { area.w * 3 / 5 - t.mm(3.0) } else { area.w };
        field.place(Rect::new(area.x, area.y, field_w, t.target));
        let error_rect =
            Rect::new(area.x, area.y + t.target + t.mm(1.0), area.w, t.mm(if landscape { 6.0 } else { 7.0 }));
        let y = error_rect.bottom() + t.mm(1.0);
        let width = (area.w - t.mm(3.0)) / 2;
        let mut submit = Button::new(label, ButtonKind::Primary);
        submit.place(Rect::new(area.right() - width, y, width, t.button_h), t);
        let mut cancel = Button::new("Cancel", ButtonKind::Secondary);
        cancel.place(Rect::new(area.x, y, width, t.button_h), t);
        if landscape {
            let width = (area.w - field_w - 2 * t.mm(3.0)) / 2;
            cancel.place(Rect::new(area.x + field_w + t.mm(3.0), area.y, width, t.target), t);
            submit.place(Rect::new(area.right() - width, area.y, width, t.target), t);
        }
        Self { sheet, operation, field, keyboard, submit, cancel, error: String::new(), error_rect }
    }
    fn draw(&mut self, cx: &mut DrawCx) {
        self.sheet.draw(cx);
        self.field.draw(cx);
        cx.paragraph(&self.error, cx.theme.caption, self.error_rect, HAlign::Left);
        self.submit.draw(cx);
        self.cancel.draw(cx);
        self.keyboard.draw(cx);
    }
    fn edit(&mut self, input: &TextInput) -> Option<NameEvent> {
        match input {
            TextInput::Submit => return Some(NameEvent::Submit),
            TextInput::Dismiss => {
                self.keyboard.visible = false;
                self.field.focused = false;
            }
            _ if self.field.focused && self.field.edit(input) => self.error.clear(),
            _ => {}
        }
        None
    }
    fn on_gesture(&mut self, g: &Gesture, t: &Theme) -> Option<NameEvent> {
        if self.keyboard.visible
            && let Some(input) = self.keyboard.on_gesture_inside(g, t)
        {
            return input.and_then(|i| self.edit(&i));
        }
        if self.sheet.on_gesture(g) || self.cancel.on_gesture(g) {
            return Some(NameEvent::Cancel);
        }
        if self.submit.on_gesture(g) {
            return Some(NameEvent::Submit);
        }
        if self.field.on_gesture(g) {
            self.field.focused = true;
            self.keyboard.visible = true;
        }
        None
    }
}

pub struct Books {
    screen: Rect,
    worker: Rc<Worker>,
    library: LibraryWorker,
    store: Store,
    roots: Vec<PathBuf>,
    /// `Store::settings_saves` when `roots` was last checked.
    settings_seen: u64,
    index: LibraryIndex,
    browsing_ready: bool,
    home_active: bool,
    home: Summary,
    pending_home: Option<Summary>,
    pending_home_revision: u64,
    books: Vec<Book>,
    /// Shared with the library worker, which builds it (see `use_collections`).
    collections: Arc<Collections>,
    /// Preview picks for folder tiles shown so far.
    previews: Previews,
    /// What folder tiles show; changes when Settings closes.
    folder_preview: FolderPreview,
    folders: Vec<PathBuf>,
    navigation: Navigation,
    /// Navigation changed since it was last sent to the writer; see `persist_navigation`.
    navigation_unsaved: bool,
    navigation_writer: NavigationWriter,
    navigation_error: Option<String>,
    home_action: IconButton,
    back: IconButton,
    new_folder: IconButton,
    path_rect: Rect,
    path_press: Press,
    states: HashMap<String, BookState>,
    /// True once every saved record in `books/` has been loaded, so ids absent
    /// from `states` are known to have default state without storing them.
    states_loaded: bool,
    fonts: Vec<FontFace>,
    title_rect: Rect,
    battery_rect: Rect,
    exit: IconButton,
    search_action: IconButton,
    rescan_action: IconButton,
    settings_action: IconButton,
    /// Opens Wi-Fi transfer ("Add books over Wi-Fi").
    transfer_action: IconButton,
    search: Option<SearchSession>,
    grid: Grid,
    pager: Pager,
    continue_id: Option<String>,
    continue_rect: Rect,
    continue_press: Press,
    home_divider_y: Option<i32>,
    library_card: Rect,
    library_card_press: Press,
    header: Rect,
    menu: Option<(Target, PopupMenu)>,
    name_sheet: Option<NameSheet>,
    details: Option<Details>,
    confirm: Option<ConfirmDelete<DeleteRequest>>,
    selection: Option<Selection>,
    carry: Option<Carry>,
    clash: Option<ClashSheet>,
    /// Edits after a publication's revision are replayed in order. Acknowledged
    /// publications are never patched twice, including rename chains and reuse.
    changes: Vec<(u64, PathChange)>,
    /// Worker revisions behind the applied index and the newest worker Home.
    /// Edits at or below both are acknowledged and dropped from `changes`.
    index_revision: u64,
    worker_home_revision: u64,
    jump: Option<PageJump>,
    touching: bool,
    pending: Option<Snapshot>,
    scanning: bool,
    working: bool,
    /// User tapped Refresh; toast once the scan completes.
    manual_rescan: bool,
    errors: Vec<String>,
    thumbnails: Thumbnails,
    reader: Option<Reader>,
    generation: u64,
    engine_error: Option<String>,
    // The update notice on Home and the dot on the Settings icon
    // (docs/updates.md, Phase 5).
    updates: Arc<crate::update::Updates>,
    /// `Updates::revision` when the notice and dot were last taken.
    update_revision: u64,
    /// The update Home's notice offers.
    notice: Option<Offer>,
    /// An update is available, even if skipped or postponed.
    update_dot: bool,
    /// The notice's spot, kept free on Home whether or not there is one.
    notice_rect: Rect,
    notice_press: Press,
    offer: Option<OfferSheet>,
    /// Background pre-layout should run at the next idle moment: set on start,
    /// on returning from a book, on settings and library changes and on sleep.
    prelayout_due: bool,
    /// The last touch, for the idle delay.
    last_input: std::time::Instant,
    /// Home should prepare the library's first page in the background at the
    /// next idle moment: set on start and on coming back from a book.
    warm_due: bool,
    /// Bumped for each warm-up request and on actions that invalidate it.
    warm_seq: u64,
    /// Book ids whose reading state changed or was deleted since the last warm
    /// request, so off-thread reads never overwrite newer writes.
    warm_touched: HashSet<String>,
    /// True when the opened order was invalidated since the last warm request.
    opened_touched: bool,
}
impl Books {
    pub fn new(t: &Theme, screen: Rect, worker: Rc<Worker>) -> Self {
        Self::with_store(t, screen, worker, Store::new())
    }
    /// Screens sharing one Store also share its dictionary service.
    pub fn with_store(t: &Theme, screen: Rect, worker: Rc<Worker>, store: Store) -> Self {
        Self::with_library(t, screen, worker, store, None)
    }
    /// [`Books::with_store`]'s setup with the library worker supplied by the
    /// caller, so tests can pass [`LibraryWorker::stub`] and run without the
    /// background thread; `None` starts the real worker where `with_store`
    /// always has.
    fn with_library(t: &Theme, screen: Rect, worker: Rc<Worker>, store: Store, library: Option<LibraryWorker>) -> Self {
        let roots = store.library_folders();
        let settings_seen = store.settings_saves();
        let settings = store.settings();
        let started = std::time::Instant::now();
        let home = store.home_summary();
        log::info!(
            "startup stage: Home summary books={} valid={} elapsed_us={}",
            home.as_ref().map_or(0, |summary| summary.books.len()),
            home.is_some(),
            started.elapsed().as_micros()
        );
        let home = home.unwrap_or_default();
        let navigation = store.navigation();
        let library = library.unwrap_or_else(|| store.library_worker(worker.waker()));
        let navigation_writer = NavigationWriter::new(store.clone(), worker.waker());
        let cy = t.top_bar / 2;
        let narrow = screen.w < t.mm(100.0);
        let (group_step, exit_step) = if narrow { (t.mm(10.0), t.mm(10.0)) } else { (t.mm(12.2), t.mm(10.5)) };
        let mut exit = IconButton::new(Icon::Exit);
        let exit_cx = screen.right() - t.margin - t.icon / 2;
        exit.place_at(Point::new(exit_cx, cy), t);
        let mut settings_action = IconButton::new(Icon::Settings);
        let settings_cx = exit_cx - exit_step;
        settings_action.place_at(Point::new(settings_cx, cy), t);
        let mut transfer_action = IconButton::new(Icon::Wifi);
        let wifi_cx = settings_cx - t.target;
        transfer_action.place_at(Point::new(wifi_cx, cy), t);
        let mut rescan_action = IconButton::new(Icon::Refresh);
        let refresh_cx = wifi_cx - group_step;
        rescan_action.place_at(Point::new(refresh_cx, cy), t);
        let mut search_action = IconButton::new(Icon::Search);
        let search_cx = refresh_cx - t.target;
        search_action.place_at(Point::new(search_cx, cy), t);
        let mut home_action = IconButton::new(Icon::Home);
        home_action.place_at(Point::new(t.margin + t.icon / 2, cy), t);
        let search_left = search_action.rect().x;
        let battery_rect = if narrow {
            let title_end = t.margin + t.mm(16.5);
            let avail = (search_left - title_end - t.mm(1.5)).max(0);
            let slot_w = t.mm(14.5).min(avail);
            let x = title_end + (avail - slot_w) / 2;
            Rect::new(x, 0, slot_w, t.top_bar)
        } else {
            let x = search_left - t.mm(16.2);
            let w = (search_left - x - t.mm(1.5)).max(0);
            Rect::new(x, 0, w, t.top_bar)
        };
        let title_rect = Rect::new(t.margin, 0, (battery_rect.x - t.margin - t.mm(2.0)).max(0), t.top_bar);
        let mut pager = Pager::new(1);
        pager.jump = true;
        pager.compact = true;
        let fonts = worker.fonts();
        // Read with the first frame: a notice saved by an earlier check is
        // drawn at once (two small files).
        let updates = store.updates();
        let update_status = updates.status();
        let thumbnails = Thumbnails::new(store.clone());
        let mut s = Self {
            screen,
            worker,
            library,
            store,
            roots,
            settings_seen,
            index: LibraryIndex::default(),
            browsing_ready: false,
            home_active: true,
            home,
            pending_home: None,
            pending_home_revision: 0,
            books: Vec::new(),
            collections: Arc::default(),
            previews: Previews::default(),
            folder_preview: settings.folder_preview,
            folders: Vec::new(),
            navigation,
            navigation_unsaved: false,
            navigation_writer,
            navigation_error: None,
            home_action,
            back: IconButton::new(Icon::ArrowLeft),
            new_folder: IconButton::new(Icon::FolderPlus),
            path_rect: Rect::default(),
            path_press: Press::default(),
            states: HashMap::new(),
            states_loaded: false,
            fonts,
            title_rect,
            battery_rect,
            exit,
            search_action,
            rescan_action,
            settings_action,
            transfer_action,
            search: None,
            grid: Grid::new(0, 4, 3),
            pager,
            continue_id: None,
            continue_rect: Rect::default(),
            continue_press: Press::default(),
            home_divider_y: None,
            library_card: Rect::default(),
            library_card_press: Press::default(),
            header: Rect::default(),
            menu: None,
            name_sheet: None,
            details: None,
            confirm: None,
            selection: None,
            carry: None,
            clash: None,
            changes: Vec::new(),
            index_revision: 0,
            worker_home_revision: 0,
            jump: None,
            touching: false,
            pending: None,
            scanning: true,
            working: true,
            manual_rescan: false,
            errors: Vec::new(),
            thumbnails,
            reader: None,
            generation: 0,
            engine_error: None,
            notice: update_status.notice().cloned(),
            update_dot: update_status.update().is_some(),
            update_revision: update_status.revision,
            updates,
            notice_rect: Rect::default(),
            notice_press: Press::default(),
            offer: None,
            prelayout_due: true,
            last_input: std::time::Instant::now(),
            warm_due: true,
            warm_seq: 0,
            warm_touched: HashSet::new(),
            opened_touched: false,
        };
        s.pager.page = s.navigation.page();
        s.refresh(t);
        if s.is_home() {
            let covers = s.home_covers(t);
            s.thumbnails.prefetch(covers);
        }
        s
    }
    fn is_home(&self) -> bool {
        self.home_active && self.search.is_none()
    }
    /// True when Library is at its top level (either the single active root or
    /// the multi-root chooser `None`).
    fn is_top_level(&self) -> bool {
        self.navigation.folder.as_deref() == self.collections.single_root(&self.roots)
    }
    /// Subtitle for Home's Library card from cached counts, without touching the index.
    fn library_card_subtitle(&self) -> String {
        if self.home.complete || self.home.library_folders > 0 || self.home.library_books > 0 {
            smart_counts(self.home.library_folders, self.home.library_books)
        } else if self.scanning {
            "Scanning…".into()
        } else {
            "Empty".into()
        }
    }
    /// Loading the browsing model is explicitly deferred until the user asks for it.
    fn prepare_browsing(&mut self) {
        if self.browsing_ready {
            return;
        }
        // Usually the worker's first pass has published the index with its
        // hierarchy by now; reading and building them here is the fallback.
        let snapshot = self.pending.take();
        let complete = snapshot.as_ref().is_none_or(|s| !s.partial);
        let (mut index, revision, built) =
            snapshot.map_or_else(|| (self.store.library(), 0, None), |s| (s.index, s.revision, s.collections));
        // Before browsing, only Home deletions can be queued. Their removals
        // are idempotent, so replaying them over the saved index is safe even
        // when the worker already saved them.
        let mut replayed = false;
        for (r, change) in &self.changes {
            if *r > revision {
                change.apply(&mut index);
                replayed = true;
            }
        }
        self.index = index;
        self.index_revision = revision;
        let started = std::time::Instant::now();
        let prebuilt = self.use_collections(built, replayed);
        log::info!(
            "startup stage: collections books={} elapsed_us={} prebuilt={prebuilt}",
            self.index.books.len(),
            started.elapsed().as_micros()
        );
        if complete {
            self.navigation.folder = self.collections.restore_for(self.navigation.folder.as_deref(), &self.roots);
        }
        self.browsing_ready = true;
    }
    fn refresh(&mut self, t: &Theme) {
        if self.is_home() {
            self.refresh_home(t);
            return;
        }
        self.prepare_browsing();
        let started = std::time::Instant::now();
        let in_library = self.search.is_none();
        let top_level = self.is_top_level();
        self.back.enabled = !top_level;
        self.new_folder.enabled = self.navigation.folder.as_deref().is_some_and(|p| self.folder_creatable(p));
        self.folders =
            if in_library { self.collections.children(self.navigation.folder.as_deref()) } else { Vec::new() };
        let folder = self.navigation.folder.as_ref().and_then(|p| self.collections.folders.get(p));
        self.books = self
            .index
            .books
            .iter()
            .filter(|b| {
                b.available
                    && if let Some(search) = &self.search {
                        search.cache.matches(b, &search.folded, &search.scope)
                    } else {
                        folder.is_some_and(|f| f.books.contains(collections::normalize_cow(&b.path).as_ref()))
                    }
            })
            .cloned()
            .collect();
        // Only shown books need a state, for their reading status.
        // Folder-only views read none.
        let states_started = std::time::Instant::now();
        let (reads, listed) = load_states(&self.store, &mut self.states, &mut self.states_loaded, &self.books);
        log::debug!(
            "startup stage: cached states reads={reads} listed={listed} elapsed_us={}",
            states_started.elapsed().as_micros()
        );
        library::sort_books(&mut self.books);
        // The resume card lives on Home only; Library and Search are plain grids.
        self.continue_id = None;
        self.continue_rect = Rect::default();
        self.home_divider_y = None;
        self.library_card = Rect::default();
        let landscape = self.screen.w > self.screen.h;
        let width = self.screen.w - 2 * t.margin;
        let pager_w = Pager::compact_width(t);
        let pager_h = t.mm(4.1);
        let (cols, rows) = library_grid_dims(self.screen, t);
        let (gap_x, gap_y) = library_grid_gaps(cols, t);
        let grid_bottom = self.screen.bottom() - t.margin;
        if let Some(search) = &mut self.search {
            search.all.kind = if search.scope == Scope::All { ButtonKind::Primary } else { ButtonKind::Secondary };
            search.current.kind = if search.scope == Scope::All { ButtonKind::Secondary } else { ButtonKind::Primary };
            let y = t.top_bar + t.mm(2.0);
            let gap = t.mm(2.0);
            let header_y = if landscape {
                // One row for the field and scope so the top row of results
                // stays visible above the open keyboard.
                let w = (width * 2 / 5 - gap) / 2;
                let field_w = width - 2 * (w + gap);
                search.field.place(Rect::new(t.margin, y, field_w, t.target));
                search.all.place(Rect::new(t.margin + field_w + gap, y, w, t.target), t);
                search.current.place(Rect::new(self.screen.right() - t.margin - w, y, w, t.target), t);
                y + t.target + t.mm(1.8)
            } else {
                search.field.place(Rect::new(t.margin, y, width, t.target));
                let scope_y = y + t.target + gap;
                let w = (width - gap) / 2;
                search.all.place(Rect::new(t.margin, scope_y, w, t.target), t);
                search.current.place(Rect::new(t.margin + w + gap, scope_y, w, t.target), t);
                scope_y + t.target + t.mm(1.7)
            };
            let header_h = t.mm(7.0);
            let grid_top = header_y + header_h + t.mm(1.8);
            self.grid = Grid::new(self.books.len(), cols, rows);
            self.grid.place(Rect::new(t.margin, grid_top, width, (grid_bottom - grid_top).max(t.target)), gap_x, gap_y);
            self.pager.set(self.pager.page, self.grid.paging().pages());
            self.grid.page = self.pager.page;
            search.page = self.pager.page;
            let header_cy = header_y + header_h / 2;
            let pager_rect =
                Rect::new(self.screen.right() - t.margin - pager_w, header_cy - pager_h / 2, pager_w, pager_h);
            self.pager.place(pager_rect, t);
            let header_right =
                if self.pager.visible() { pager_rect.x - t.mm(2.0) } else { self.screen.right() - t.margin };
            self.header = Rect::new(t.margin, header_y, (header_right - t.margin).max(0), header_h);
            self.path_rect = Rect::default();
        } else {
            let header_y = t.top_bar + t.mm(LIBRARY_HEADER_TOP_MM);
            let header_h = t.mm(LIBRARY_HEADER_H_MM);
            let back_cy = header_y + t.mm(3.6);
            let controls_cy = header_y + t.mm(5.7);
            self.grid = self.browsing_grid(self.folders.len() + self.books.len(), t);
            self.pager.set(self.pager.page, self.grid.paging().pages());
            self.grid.page = self.pager.page;
            let pager_rect =
                Rect::new(self.screen.right() - t.margin - pager_w, controls_cy - pager_h / 2, pager_w, pager_h);
            self.pager.place(pager_rect, t);
            self.back.place_at(Point::new(t.margin + t.icon / 2, back_cy), t);
            let show_new_folder = self.navigation.folder.is_some();
            let new_folder_cx = if self.pager.visible() {
                pager_rect.x - t.mm(6.0) - t.icon / 2
            } else {
                self.screen.right() - t.margin - t.icon / 2
            };
            self.new_folder.place_at(Point::new(new_folder_cx, controls_cy), t);
            let left = if top_level {
                t.margin
            } else {
                (t.margin + t.icon + t.mm(2.0)).max(self.back.rect().right() + t.mm(0.5))
            };
            let right = if show_new_folder {
                self.new_folder.rect().x - t.mm(1.5)
            } else if self.pager.visible() {
                pager_rect.x - t.mm(3.0)
            } else {
                self.screen.right() - t.margin
            };
            self.path_rect = Rect::new(left, header_y, (right - left).max(0), header_h);
            self.header = Rect::default();
        }
        self.retain_selection();
        self.prioritize();
        log::debug!("startup stage: view books={} elapsed_us={}", self.books.len(), started.elapsed().as_micros());
    }
    fn refresh_home(&mut self, t: &Theme) {
        let width = self.screen.w - 2 * t.margin;
        let landscape = self.screen.w > self.screen.h;
        let narrow = self.screen.w < t.mm(100.0);
        let short = self.screen.h < t.mm(100.0);
        let card_h = t.mm(13.2).min(self.screen.h / 8).max(t.target);
        self.library_card = Rect::new(t.margin, self.screen.bottom() - t.margin - card_h, width, card_h);
        self.folders.clear();
        self.continue_id = self.home.resume().map(|e| e.book.id.clone());
        self.home_divider_y = None;

        // The update notice's spot stays reserved inside the gap above the
        // Library card (or under Recent books in landscape), so a check
        // finishing never moves anything.
        let notice_h = t.mm(8.2);
        let notice_y = self.library_card.y - t.mm(1.5) - notice_h;
        let content_top = t.top_bar + t.mm(if narrow || short { 2.5 } else { 3.5 });
        let content_bottom = notice_y - t.mm(1.4);
        let header_h = t.mm(4.5);
        let header_gap = t.mm(if narrow || short { 2.0 } else { 2.7 });
        let recent_labels_h = t.mm(HOME_RECENT_LABELS_MM);

        if landscape && self.continue_id.is_some() {
            // Continue reading on the left, Recent books + reserved notice on
            // the right, Library card full-width along the bottom.
            let col_gap = t.mm(if short { 4.5 } else { 5.0 });
            let left_w = (width - col_gap) / 2;
            let right_x = t.margin + left_w + col_gap;
            let right_w = width - left_w - col_gap;
            let max_cover_w = left_w * 42 / 100;
            let max_h_from_w = max_cover_w * 3 / 2;
            let max_h_from_band = (self.library_card.y - t.mm(3.0) - content_top).max(t.target);
            let continue_h = t.mm(39.6).min(max_h_from_w).min(max_h_from_band);
            self.continue_rect = Rect::new(t.margin, content_top, left_w, continue_h);

            self.header = Rect::new(right_x, content_top, right_w, header_h);
            let cols = (right_w / t.mm(17.0)).clamp(1, 3) as usize;
            let gap_x = t.mm(if right_w < t.mm(60.0) { 2.4 } else { 3.0 });
            let cell_w = (right_w - gap_x * (cols as i32 - 1)).max(1) / cols as i32;
            let ideal_grid_h = cell_w * 3 / 2 + recent_labels_h;
            self.books = self.home.recents(cols).iter().map(|e| e.book.clone()).collect();
            self.grid = Grid::new(self.books.len(), cols, 1);
            let grid_y = self.header.bottom() + header_gap;
            let grid_h = (content_bottom - grid_y).min(ideal_grid_h).max(t.target);
            self.grid.place(Rect::new(right_x, grid_y, right_w, grid_h), gap_x, 0);
            self.notice_rect = Rect::new(right_x, notice_y, right_w, notice_h);
        } else {
            let cols = if width >= t.mm(75.0) { 4 } else { (width / t.mm(19.0)).clamp(1, 4) as usize };
            let gap_x = t.mm(if narrow { 2.6 } else { 3.4 });
            let cell_w = (width - gap_x * (cols as i32 - 1)).max(1) / cols as i32;
            let ideal_grid_h = cell_w * 3 / 2 + recent_labels_h;
            self.books = self.home.recents(cols).iter().map(|e| e.book.clone()).collect();
            self.grid = Grid::new(self.books.len(), cols, 1);

            if self.continue_id.is_some() {
                let div_gap_top = t.mm(if narrow { 3.2 } else { 5.2 });
                let div_gap_bot = t.mm(if narrow { 2.6 } else { 3.8 });
                let overhead = div_gap_top + div_gap_bot + header_h + header_gap;
                let max_continue_h = t.mm(39.6);
                let spare = content_bottom - content_top - overhead - ideal_grid_h;
                let continue_h =
                    spare.clamp(t.mm(28.0), max_continue_h).min((content_bottom - content_top).max(t.target));
                self.continue_rect = Rect::new(t.margin, content_top, width, continue_h);
                let div_y = self.continue_rect.bottom() + div_gap_top;
                if !self.books.is_empty() {
                    self.home_divider_y = Some(div_y);
                }
                self.header = Rect::new(t.margin, div_y + div_gap_bot, width, header_h);
            } else {
                self.continue_rect = Rect::default();
                self.header = Rect::new(t.margin, content_top, width, header_h);
            }
            let grid_y = self.header.bottom() + header_gap;
            let grid_h = (content_bottom - grid_y).min(ideal_grid_h).max(t.target);
            self.grid.place(Rect::new(t.margin, grid_y, width, grid_h), gap_x, 0);
            self.notice_rect = Rect::new(t.margin, notice_y, width, notice_h);
        }
        self.prioritize();
    }
    /// Covers of visible books come first, then those of visible previews.
    fn prioritize(&mut self) {
        self.pick_previews();
        let mut ids: Vec<_> = self.continue_id.iter().cloned().collect();
        ids.extend(self.grid.cells().iter().filter_map(|(i, _)| self.grid_book(*i)).map(|b| b.id.clone()));
        for folder in self.visible_folders() {
            ids.extend(self.preview_books(folder).map(|b| b.id.clone()));
        }
        self.library.prioritize(ids);
    }
    /// The books `folder`'s tile shows: the prefix of its pick the mode uses.
    fn preview_books(&self, folder: &std::path::Path) -> impl Iterator<Item = &Book> {
        let picks = self.previews.get(folder).unwrap_or_default();
        picks.iter().take(self.folder_preview.slots()).filter_map(|&i| self.index.books.get(i))
    }
    fn visible_folders(&self) -> impl Iterator<Item = &PathBuf> {
        self.grid.cells().into_iter().filter_map(|(i, _)| self.folders.get(i))
    }
    /// Folders on the current page and the pages either side of it.
    fn nearby_folders(&self) -> impl Iterator<Item = &PathBuf> {
        let page = self.grid.page;
        (page.saturating_sub(1)..=page + 1)
            .flat_map(|p| self.grid.cells_for_page(p))
            .filter_map(|(i, _)| self.folders.get(i))
    }
    /// Picks previews for the folder tiles on the current page and the pages
    /// either side, which the prefetcher prepares. The opened order is read
    /// on first need, so views without folders read nothing.
    fn pick_previews(&mut self) {
        if self.folder_preview == FolderPreview::Icon || self.nearby_folders().next().is_none() {
            return;
        }
        if !self.previews.has_order() {
            let started = std::time::Instant::now();
            let opened = self.store.opened().unwrap_or_default();
            self.previews.set_order(&opened);
            log::debug!("opened order: {} books in {:?}", opened.books.len(), started.elapsed());
        }
        let started = std::time::Instant::now();
        let folders: Vec<_> = self.nearby_folders().cloned().collect();
        self.previews.update(&self.index, &self.collections, &folders);
        log::debug!("folder previews: {} folders in {:?}", folders.len(), started.elapsed());
    }
    /// Every new index or folder list goes through here, so preview picks,
    /// which hold index positions, never outlive their index.
    fn rebuild_collections(&mut self) {
        self.collections = Arc::new(Collections::from_index(&self.index, &self.roots));
        self.previews.index_changed();
    }
    /// Takes a hierarchy the worker built for the index just applied, unless
    /// the folder list changed since or edits were replayed over that index.
    /// Returns whether it could.
    fn use_collections(&mut self, built: Option<(Vec<PathBuf>, Arc<Collections>)>, replayed: bool) -> bool {
        match built {
            Some((roots, collections)) if roots == self.roots && !replayed => {
                self.collections = collections;
                self.previews.index_changed();
                true
            }
            _ => {
                self.rebuild_collections();
                false
            }
        }
    }
    /// Folders inside a folder share its parent, already shown in the path
    /// row, so their tiles give that line to the preview. Only configured
    /// roots at the top level can share a name; those keep their parent.
    fn shared_names(&self) -> HashSet<String> {
        if self.navigation.folder.is_some() {
            return HashSet::new();
        }
        let mut seen = HashSet::new();
        self.folders.iter().map(|p| collections::name(p)).filter(|name| !seen.insert(name.clone())).collect()
    }
    /// Browsing grid layout (outside search) for `count` items.
    /// Shared by `refresh` and Home's off-thread warm-up so prefetched cover
    /// sizes match the browsing grid pixel-for-pixel.
    fn browsing_grid(&self, count: usize, t: &Theme) -> Grid {
        let top = t.top_bar + t.mm(LIBRARY_HEADER_TOP_MM) + t.mm(LIBRARY_HEADER_H_MM) + t.mm(LIBRARY_HEADER_GAP_MM);
        let bottom = self.screen.bottom() - t.margin;
        let (cols, rows) = library_grid_dims(self.screen, t);
        let (gap_x, gap_y) = library_grid_gaps(cols, t);
        let mut grid = Grid::new(count, cols, rows);
        grid.place(Rect::new(t.margin, top, self.screen.w - 2 * t.margin, (bottom - top).max(t.target)), gap_x, gap_y);
        grid
    }
    /// A book's cover in grid cell `cell`. Every cell's cover has the size
    /// of the narrowest cell's, so a page draws one tile thumbnail size.
    fn cover_rect(&self, cell: Rect, t: &Theme) -> Rect {
        if self.is_home() { home_recent_cover_rect(&self.grid, cell, t) } else { grid_cover_rect(&self.grid, cell, t) }
    }
    /// A folder tile's preview or icon area in `cell`, above its labels; the
    /// same size in every cell, like covers.
    fn folder_rect(&self, cell: Rect, parent_line: bool, t: &Theme) -> Rect {
        grid_folder_rect(&self.grid, cell, parent_line, t)
    }
    /// The covers Home's first draw will load: the resume card first, then
    /// the Recent books row. Shared geometry with `draw` guarantees cache hits.
    fn home_covers(&self, t: &Theme) -> Vec<CoverSpec> {
        if !self.is_home() {
            return Vec::new();
        }
        let mut specs = Vec::new();
        let mut add = |book: &Book, rect: Rect| {
            if let Cover::Ready(key) = &book.cover {
                specs.push(CoverSpec {
                    book_id: book.id.clone(),
                    key: key.clone(),
                    w: rect.w,
                    h: rect.h,
                    fit: Fit::Cover,
                });
            }
        };
        if let Some(book) = self.continue_id.as_ref().and_then(|id| self.book(id)) {
            add(book, continue_cover_rect(self.continue_rect));
        }
        for (i, cell) in self.grid.cells() {
            if let Some(book) = self.grid_book(i) {
                add(book, self.cover_rect(cell, t));
            }
        }
        specs
    }
    /// The covers on the pages either side of the current one, next page
    /// first, for the prefetcher to decode while this one is on screen.
    fn neighbour_covers(&self, t: &Theme) -> Vec<CoverSpec> {
        if self.is_home() {
            return Vec::new();
        }
        let page = self.grid.page;
        self.page_covers(t, [Some(page + 1), page.checked_sub(1)].into_iter().flatten())
    }
    /// The covers drawn on `pages` of the browsing grid, in that order.
    fn page_covers(&self, t: &Theme, pages: impl IntoIterator<Item = usize>) -> Vec<CoverSpec> {
        let shared_names = self.shared_names();
        let mut specs = Vec::new();
        let mut add = |book: &Book, rect: Rect, fit: Fit| {
            if let Cover::Ready(key) = &book.cover {
                specs.push(CoverSpec { book_id: book.id.clone(), key: key.clone(), w: rect.w, h: rect.h, fit });
            }
        };
        for p in pages {
            for (i, cell) in self.grid.cells_for_page(p) {
                if let Some(path) = self.folders.get(i) {
                    let parent_line = shared_names.contains(&collections::name(path));
                    let picks: Vec<&Book> = self.preview_books(path).collect();
                    let r = self.folder_rect(cell, parent_line, t);
                    for (k, rect, fit) in folder_tile::layout(self.folder_preview, picks.len(), r, t)
                        .map(|l| l.covers)
                        .unwrap_or_default()
                    {
                        add(picks[k], rect, fit);
                    }
                } else if let Some(book) = self.grid_book(i) {
                    add(book, self.cover_rect(cell, t), Fit::Cover);
                }
            }
        }
        specs
    }
    fn grid_book(&self, i: usize) -> Option<&Book> {
        i.checked_sub(self.folders.len()).and_then(|i| self.books.get(i))
    }
    fn save_navigation(&mut self) {
        if let Some(search) = &mut self.search {
            search.page = self.pager.page;
            return;
        }
        if self.is_home() {
            return;
        }
        self.navigation.set_page(self.pager.page);
        self.navigation_unsaved = true;
    }
    /// Hands navigation to its writer thread once its frame is on screen. The
    /// synced write took 5–951 ms on the Kindle (measured); on the UI thread
    /// it held back the frame, and later the next touch.
    fn persist_navigation(&mut self) {
        if std::mem::take(&mut self.navigation_unsaved)
            && let Err(e) = self.navigation_writer.save(self.navigation.clone())
        {
            self.navigation_failed(e);
        }
    }
    /// Sleep and quit wait until the newest navigation is durably written.
    fn flush_navigation(&mut self) {
        self.persist_navigation();
        if let Err(e) = self.navigation_writer.flush() {
            // The writer reported this failure too; show it once.
            self.navigation_writer.errors().for_each(drop);
            self.navigation_failed(e);
        }
    }
    /// Shown as a toast at the next tick.
    fn navigation_failed(&mut self, e: anyhow::Error) {
        self.navigation_error = Some(format!("Couldn't save library navigation: {e:#}"));
    }
    /// Flashes only the browsing grid (covering its previous and current bounds)
    /// so the top bar, tabs and other chrome stay steady.
    fn refresh_grid(&self, cx: &mut EventCx, previous: Rect) {
        let current = self.grid.rect();
        let rect = match (previous.w > 0 && previous.h > 0, current.w > 0 && current.h > 0) {
            (true, true) => previous.union(&current),
            (true, false) => previous,
            (false, true) => current,
            (false, false) => {
                cx.redraw();
                return;
            }
        };
        cx.refresh(rect, RefreshMode::Full);
    }
    /// Shows another page of the browsing grid: arrows, swipes and the page jump.
    fn go_to_page(&mut self, page: usize, cx: &mut EventCx) {
        self.pager.set(page, self.pager.pages);
        self.grid.page = self.pager.page;
        self.prioritize();
        self.save_navigation();
        self.refresh_grid(cx, self.grid.rect());
    }
    /// What the jump sheet shows for `page`: the first folder or book on it.
    fn page_start(&self, page: usize) -> String {
        let i = page * self.grid.paging().per_page;
        if let Some(path) = self.folders.get(i) {
            return collections::name(path);
        }
        self.grid_book(i).map_or_else(String::new, |b| b.metadata.title.clone())
    }
    fn open_library(&mut self, cx: &mut EventCx) {
        self.end_selection(cx);
        self.home_active = false;
        self.prepare_browsing();
        self.pager.page = self.navigation.page();
        self.refresh(cx.theme);
        self.save_navigation();
        cx.refresh(self.screen, RefreshMode::Full);
    }
    fn open_home(&mut self, cx: &mut EventCx) {
        if self.is_home() || self.carrying() {
            return;
        }
        self.end_selection(cx);
        if self.search.is_none() {
            self.save_navigation();
        }
        self.home_active = true;
        self.refresh(cx.theme);
        cx.refresh(self.screen, RefreshMode::Full);
    }
    fn navigate(&mut self, folder: Option<PathBuf>, cx: &mut EventCx) {
        if !self.carrying() {
            self.end_selection(cx);
        }
        let started = std::time::Instant::now();
        let old_grid = self.grid.rect();
        self.home_active = false;
        self.prepare_browsing();
        self.navigation.set_page(self.pager.page);
        self.navigation.folder =
            folder.or_else(|| self.collections.single_root(&self.roots).map(std::path::Path::to_owned));
        self.pager.page = self.navigation.page();
        self.refresh(cx.theme);
        self.save_navigation();
        self.refresh_grid(cx, old_grid);
        log::info!("collection switch {:?}: prepared in {:?}", self.navigation.folder, started.elapsed());
    }
    /// Library folders edited in Settings: scan the new list. The index that
    /// scan publishes rebuilds collections and restores navigation as usual.
    fn apply_folder_changes(&mut self, cx: &mut EventCx) {
        let saves = self.store.settings_saves();
        if saves == self.settings_seen {
            return;
        }
        self.settings_seen = saves;
        self.prelayout_due = true;
        let settings = self.store.settings();
        let mode = settings.folder_preview;
        if mode != self.folder_preview {
            log::info!("folder previews: {mode:?}");
            self.folder_preview = mode;
            self.warm_seq = self.warm_seq.wrapping_add(1);
            if self.is_home() {
                self.warm_due = true;
            }
            // Picks don't depend on the mode; each mode shows a prefix.
            self.thumbnails.clear();
            if !self.is_home() {
                self.refresh(cx.theme);
                cx.refresh(self.screen, RefreshMode::Full);
            }
        }
        let roots = self.store.library_folders();
        if roots != self.roots {
            log::info!("library folders changed: {roots:?}");
            self.end_selection(cx);
            self.roots = roots.clone();
            self.warm_seq = self.warm_seq.wrapping_add(1);
            if self.is_home() {
                self.warm_due = true;
            }
            self.library.set_roots(roots);
            self.scanning = true;
            self.working = true;
            if self.browsing_ready {
                // Folders outside the new list leave Collections at once.
                self.rebuild_collections();
                self.navigation.folder = self.collections.restore_for(self.navigation.folder.as_deref(), &self.roots);
                self.home.set_library_counts(self.collections.top_level_counts(&self.roots));
            }
            self.refresh(cx.theme);
            cx.redraw();
        }
    }
    /// A published index waits for touches and overlays in `pending`. The
    /// saved index, unchanged, is only of use before browsing has read one.
    fn receive(&mut self, snapshot: Snapshot, cx: &mut EventCx) {
        self.prelayout_due = true;
        if snapshot.cached && self.browsing_ready {
            return;
        }
        self.pending = Some(snapshot);
        self.apply_pending(cx);
    }
    /// A menu, sheet or dialog is open over the screen.
    fn overlay_open(&self) -> bool {
        self.menu.is_some()
            || self.details.is_some()
            || self.confirm.is_some()
            || self.clash.is_some()
            || self.name_sheet.is_some()
            || self.jump.is_some()
            || self.offer.is_some()
    }
    fn apply_pending(&mut self, cx: &mut EventCx) {
        if self.touching || self.overlay_open() || self.reader.is_some() {
            return;
        }
        self.apply_update_status(cx);
        if let Some(mut home) = self.pending_home.take()
            && {
                let mut replayed = false;
                for (revision, change) in &self.changes {
                    if *revision > self.pending_home_revision {
                        change.apply_home(&mut home, &self.roots);
                        replayed = true;
                    }
                }
                if replayed && self.browsing_ready {
                    home.set_library_counts(self.collections.top_level_counts(&self.roots));
                }
                self.home != home
            }
        {
            self.home = home;
            if self.is_home() {
                self.refresh(cx.theme);
                cx.redraw();
            }
        }
        // Retain only the newest index while Home is visible. Do not prepare it.
        if self.is_home() {
            return;
        }
        if let Some(mut snapshot) = self.pending.take() {
            let mut replayed = false;
            for (revision, change) in &self.changes {
                if *revision > snapshot.revision {
                    change.apply(&mut snapshot.index);
                    replayed = true;
                }
            }
            let started = std::time::Instant::now();
            cx.redraw();
            self.index = snapshot.index;
            self.index_revision = snapshot.revision;
            self.prune_changes();
            self.use_collections(snapshot.collections.take(), replayed);
            if let Some(search) = &mut self.search {
                search.cache.sync(&self.index);
            }
            let old_navigation = self.navigation.clone();
            let old_folder = self.navigation.folder.clone();
            // Initial metadata checkpoints may not yet contain the saved folder.
            if !snapshot.partial {
                self.navigation.folder = self.collections.restore_for(self.navigation.folder.as_deref(), &self.roots);
                self.navigation.folder_pages.retain(|p, _| self.collections.folders.contains_key(p));
                self.home.set_library_counts(self.collections.top_level_counts(&self.roots));
            }
            if old_folder != self.navigation.folder && !self.carrying() {
                self.end_selection(cx);
            }
            let old_grid = self.grid.rect();
            let page = self.pager.page;
            self.pager.page = self.search.as_ref().map_or_else(|| self.navigation.page(), |s| s.page);
            self.refresh(cx.theme);
            if !snapshot.partial {
                if self.search.is_none() {
                    self.navigation.set_page(self.pager.page);
                }
                self.navigation_unsaved |= old_navigation != self.navigation;
            }
            if self.search.is_none() && (self.pager.page != page || old_folder != self.navigation.folder) {
                self.refresh_grid(cx, old_grid);
            }
            log::info!(
                "startup stage: snapshot application books={} elapsed_us={}",
                self.index.books.len(),
                started.elapsed().as_micros()
            );
        }
    }
    fn toggle_search(&mut self, cx: &mut EventCx) {
        self.end_selection(cx);
        if self.search.take().is_some() {
            self.search_action.icon = Icon::Search;
            self.pager.page = self.navigation.page();
            self.refresh(cx.theme);
            self.save_navigation();
        } else {
            self.save_navigation();
            self.prepare_browsing();
            let mut cache = SearchCache::default();
            let started = std::time::Instant::now();
            cache.sync(&self.index);
            log::info!("search cache: {} books in {:?}", self.index.books.len(), started.elapsed());
            let collection = (!self.home_active).then(|| self.navigation.folder.clone()).flatten();
            let mut current = Button::new("This folder", ButtonKind::Secondary);
            current.enabled = collection.is_some();
            let mut keyboard = Keyboard::default();
            keyboard.visible = true;
            keyboard.place(self.screen, cx.theme);
            let mut field = TextField::new("Title, author or filename");
            field.focused = true;
            self.search = Some(SearchSession {
                field,
                folded: String::new(),
                debounce_at: None,
                scope: Scope::All,
                collection,
                page: 0,
                cache,
                all: Button::new("All books", ButtonKind::Primary),
                current,
                keyboard,
            });
            self.search_action.icon = Icon::Close;
            self.pager.page = 0;
            self.refresh(cx.theme);
        }
    }

    fn apply_search(&mut self, cx: &mut EventCx) {
        let Some(search) = &mut self.search else { return };
        search.debounce_at = None;
        let folded = search::fold(search.field.value().trim());
        if folded == search.folded {
            return;
        }
        self.selection = None;
        let started = std::time::Instant::now();
        search.folded = folded;
        self.pager.page = 0;
        self.refresh(cx.theme);
        cx.redraw();
        log::debug!("search query: {} matches prepared in {:?}", self.books.len(), started.elapsed());
    }

    fn edit_search(&mut self, input: &TextInput, cx: &mut EventCx) {
        let Some(search) = &mut self.search else { return };
        if matches!(input, TextInput::Submit | TextInput::Dismiss) {
            search.keyboard.visible = false;
            search.field.focused = false;
            self.apply_search(cx);
        } else if search.field.edit(input) {
            self.selection = None;
            if matches!(input, TextInput::Clear) {
                self.apply_search(cx);
            } else if search::fold(search.field.value().trim()) == search.folded {
                search.debounce_at = None;
            } else {
                // Coalesce rapid keystrokes so sorting and grid redraws wait for a brief pause.
                search.debounce_at = Some(cx.now + SEARCH_DEBOUNCE);
            }
        }
    }

    fn book(&self, id: &str) -> Option<&Book> {
        self.books
            .iter()
            .find(|b| b.id == id)
            .or_else(|| self.home.books.iter().find(|e| e.book.id == id).map(|e| &e.book))
    }
    /// Home entries are display summaries: use the full index when opening a
    /// reader so language and other source metadata reach its lookup session.
    fn reading_book(&self, id: &str) -> Option<Book> {
        let displayed = self.book(id)?.clone();
        if !self.is_home() {
            return Some(displayed);
        }
        let full = |index: &LibraryIndex| {
            index
                .books
                .iter()
                .find(|book| book.id == id && book.path == displayed.path && !book.metadata_pending)
                .cloned()
        };
        (self.browsing_ready.then(|| full(&self.index)).flatten())
            .or_else(|| self.pending.as_ref().and_then(|snapshot| full(&snapshot.index)))
            .or_else(|| full(&self.store.library()))
            .or(Some(displayed))
    }
    fn open(&mut self, id: String, cx: &mut EventCx) {
        if let Some(error) = &self.engine_error {
            cx.toast(error.clone(), None);
            return;
        }
        let Some(book) = self.reading_book(&id) else { return };
        let path = book.path.clone();
        let annotation_book = book.clone();
        self.store.select_home_book(&book);
        self.save_navigation();
        self.warm_seq = self.warm_seq.wrapping_add(1);
        self.warm_touched.insert(id.clone());
        self.library.pause(true);
        self.thumbnails.clear();
        let settings = self.store.settings();
        let (layout, typography, page) = self.open_settings(&settings, cx.theme);
        let saved = self.store.book(&id);
        self.generation += 1;
        self.reader = Some(Reader::new(
            self.screen,
            self.worker.clone(),
            self.store.clone(),
            id,
            self.generation,
            settings.clone(),
            self.fonts.clone(),
            saved.position.clone(),
        ));
        self.reader.as_mut().unwrap().annotations(annotation_book);
        self.reader.as_mut().unwrap().reading_speed(saved.reading_speed, self.store.reading_speed());
        self.worker.send(Command::Open {
            generation: self.generation,
            path,
            layout,
            typography,
            page,
            position: saved.position,
            progress: saved.progress,
        });
    }
    /// What a book opens with. Background pre-layout uses the same, so the
    /// layout it caches is the one the reader asks for.
    fn open_settings(&self, settings: &crate::store::Settings, t: &Theme) -> (Layout, Typography, PageLayout) {
        let margin = (7.0 * t.dpi / 25.4).round() as u32;
        let layout = Layout {
            width: self.screen.w as u32,
            height: self.screen.h as u32,
            margins: [margin; 4],
            font_size: t.pt(settings.typography.size_pt) as u32,
            shaping: settings.text_shaping,
        };
        (layout, settings.typography.clone(), settings.page_layout.clone())
    }
    /// When background pre-layout should start, if it's due: once the engine
    /// is up, the library has settled and nothing was touched for a while.
    fn prelayout_at(&self) -> Option<std::time::Instant> {
        (self.prelayout_due
            && self.reader.is_none()
            && self.engine_error.is_none()
            && !self.fonts.is_empty()
            && !self.scanning
            && !self.working)
            .then(|| self.last_input + PRELAYOUT_IDLE)
    }
    fn set_book_state(&mut self, id: String, state: BookState) {
        self.warm_touched.insert(id.clone());
        self.states.insert(id, state);
    }
    pub(super) fn remove_book_state(&mut self, id: &str) {
        self.warm_touched.insert(id.to_owned());
        self.states.remove(id);
    }
    /// When Home should prepare the library's first page: once the worker's
    /// index is in (preparing never reads it here), the scan is done and
    /// nothing was touched for a moment.
    fn warm_at(&self) -> Option<std::time::Instant> {
        (self.warm_due
            && self.is_home()
            && self.reader.is_none()
            && self.selection.is_none()
            && !self.overlay_open()
            && !self.touching
            && !self.scanning
            && (self.browsing_ready || self.pending.is_some()))
        .then(|| self.last_input + WARM_IDLE)
    }
    /// Starts preparing the remembered Library folder and page off the UI
    /// thread. The library worker reads states and `opened.json` and picks
    /// the page's tiles; `apply_warm` absorbs them and hands covers to
    /// `verso-thumb`.
    fn warm_library(&mut self, t: &Theme) {
        self.warm_due = false;
        self.warm_seq = self.warm_seq.wrapping_add(1);
        self.warm_touched.clear();
        self.opened_touched = false;
        let per_page = self.browsing_grid(0, t).paging().per_page;
        self.library.warm(WarmRequest {
            seq: self.warm_seq,
            navigation: self.navigation.clone(),
            per_page,
            folder_preview: self.folder_preview,
            need_states: !self.states_loaded,
            known_ranks: self.previews.ranks(),
        });
    }
    /// Absorbs the worker's off-thread warm-up result on the UI thread without
    /// file I/O or full-library cloning/sorting, and queues that page's covers
    /// on `verso-thumb` while still on Home.
    fn apply_warm(&mut self, mut warm: WarmResult, t: &Theme) -> usize {
        if warm.seq != self.warm_seq {
            return 0;
        }
        let started = std::time::Instant::now();
        if let Some(loaded) = warm.states.take() {
            for (id, state) in loaded {
                if !self.warm_touched.contains(&id) && !self.states.contains_key(&id) {
                    self.states.insert(id, state);
                }
            }
            if warm.states_complete {
                self.states_loaded = true;
            }
        }
        if let Some(ranks) = warm.opened_ranks.take()
            && !self.opened_touched
            && !self.previews.has_order()
        {
            self.previews.set_ranks(ranks);
        }
        if !self.is_home() || self.reader.is_some() {
            return 0;
        }
        let covers = self.warm_covers(&warm, t);
        let count = covers.len();
        log::info!(
            "library warm: page {} with {} covers prepared in {:?} (worker {:?}, reads={}, listed={})",
            warm.page,
            count,
            started.elapsed(),
            warm.elapsed,
            warm.reads,
            warm.listed,
        );
        self.thumbnails.prefetch(covers);
        count
    }
    /// Converts the worker's single-page `WarmTile`s into `CoverSpec`s using
    /// the exact same grid and tile geometry as `page_covers`.
    fn warm_covers(&self, warm: &WarmResult, t: &Theme) -> Vec<CoverSpec> {
        let grid = self.browsing_grid(warm.total_items, t);
        let mut specs = Vec::new();
        let mut add = |id: &str, cover: &Cover, rect: Rect, fit: Fit| {
            if let Cover::Ready(key) = cover {
                specs.push(CoverSpec { book_id: id.to_owned(), key: key.clone(), w: rect.w, h: rect.h, fit });
            }
        };
        for ((_, cell), tile) in grid.cells_for_page(warm.page).into_iter().zip(&warm.tiles) {
            match tile {
                WarmTile::Folder { parent_line, picks } => {
                    let r = grid_folder_rect(&grid, cell, *parent_line, t);
                    for (k, rect, fit) in folder_tile::layout(self.folder_preview, picks.len(), r, t)
                        .map(|l| l.covers)
                        .unwrap_or_default()
                    {
                        let (id, cover) = &picks[k];
                        add(id, cover, rect, fit);
                    }
                }
                WarmTile::Book { id, cover } => add(id, cover, grid_cover_rect(&grid, cell, t), Fit::Cover),
            }
        }
        specs
    }
    fn start_prelayout(&mut self, cx: &EventCx) {
        self.prelayout_due = false;
        if cx.battery.is_some_and(|b| b.percent < PRELAYOUT_MIN_BATTERY && !b.charging) {
            log::info!("prelayout: skipped on low battery");
            return;
        }
        let (layout, typography, page) = self.open_settings(&self.store.settings(), cx.theme);
        self.worker.send(Command::Prelayout { layout, typography, page });
    }
    fn returned(&mut self, cx: &mut EventCx) {
        self.library.pause(false);
        self.prelayout_due = true;
        self.warm_due = true;
        self.warm_seq = self.warm_seq.wrapping_add(1);
        self.last_input = cx.now;
        self.reload_home();
        if self.pending_home.as_ref().is_none_or(|summary| !summary.complete) {
            self.library.refresh_home();
        }
        // Only the reader writes reading state while it is open, so its book
        // is the only cached state that can be stale.
        if let Some(r) = self.reader.take() {
            self.generation = r.generation();
            let id = r.book_id();
            self.set_book_state(id.to_owned(), self.store.book(id));
            // The book now leads the opened order, unless it already did.
            if !self.previews.leads(id) {
                self.previews.forget_order();
                self.opened_touched = true;
            }
        }
        self.apply_pending(cx);
        self.refresh(cx.theme);
        self.save_navigation();
        cx.refresh(self.screen, RefreshMode::Full);
    }
    fn open_menu(&mut self, id: String, anchor: Point, t: &Theme) {
        let state = self.store.book(&id);
        let mut items = vec![
            (Some(Icon::BookOpen), "Open".into()),
            (Some(Icon::Info), "Details".into()),
            (Some(Icon::Check), "Mark finished".into()),
        ];
        if self.is_home() && !state.position.0.is_empty() && !state.hidden_from_continue && !state.finished {
            items.push((Some(Icon::Close), "Remove from continue reading".into()));
        }
        if !self.is_home() {
            items.push((Some(Icon::SquareCheck), "Select".into()));
            items.push((Some(Icon::FolderInput), "Move".into()));
        }
        items.push((Some(Icon::Trash), "Delete…".into()));
        // Keep the top bar readable; on a short screen the menu opens beside the book.
        let area = Rect { y: self.screen.y + t.top_bar, h: self.screen.h - t.top_bar, ..self.screen };
        let mut menu = PopupMenu::open(items, anchor, area, t);
        menu.set_enabled(2, !state.finished);
        self.menu = Some((Target::Book(id), menu));
        self.continue_press = Press::default();
        self.grid.on_gesture(&Gesture::Cancel(anchor));
    }
    fn mark(&mut self, id: &str, finished: bool, cx: &mut EventCx) {
        if let Some(book) = self.book(id) {
            self.store.select_home_book(book);
        }
        let mut state = self.store.book(id);
        if finished {
            state.finished = true;
        } else {
            state.hidden_from_continue = true;
        }
        match self.store.save_book(id, &state) {
            Ok(()) => {
                self.set_book_state(id.to_owned(), state);
                self.reload_home();
                self.library.refresh_home();
                self.apply_pending(cx);
                self.refresh(cx.theme);
                cx.toast(if finished { "Marked finished" } else { "Removed from continue reading" }, None);
            }
            Err(e) => cx.toast(format!("Couldn't save book status: {e}"), None),
        }
    }
    fn folder_protected(&self, path: &std::path::Path) -> bool {
        self.roots.iter().any(|r| r.starts_with(path))
    }
    /// A new folder must land inside a library folder, or no scan would ever
    /// show it. A folder that only contains a root lies outside every one.
    fn folder_creatable(&self, path: &std::path::Path) -> bool {
        self.roots.iter().any(|r| path.starts_with(r))
    }
    fn open_folder_menu(&mut self, path: PathBuf, anchor: Point, t: &Theme) {
        let items = vec![
            (Some(Icon::Folder), "Open".into()),
            (Some(Icon::Info), "Details".into()),
            (Some(Icon::Search), "Search in this folder".into()),
            (Some(Icon::SquareCheck), "Select".into()),
            (Some(Icon::FolderInput), "Move".into()),
            (Some(Icon::FolderPlus), "New folder…".into()),
            (Some(Icon::Pencil), "Rename…".into()),
            (Some(Icon::Trash), "Delete…".into()),
        ];
        let area = Rect { y: self.screen.y + t.top_bar, h: self.screen.h - t.top_bar, ..self.screen };
        let mut menu = PopupMenu::open(items, anchor, area, t);
        let protected = self.folder_protected(&path);
        menu.set_enabled(3, !protected);
        menu.set_enabled(4, !protected);
        menu.set_enabled(5, self.folder_creatable(&path));
        menu.set_enabled(6, !protected);
        menu.set_enabled(7, !protected);
        self.menu = Some((Target::Folder(path), menu));
        self.grid.on_gesture(&Gesture::Cancel(anchor));
    }
    fn folder_details(&mut self, path: &std::path::Path, t: &Theme) {
        let count = self.collections.folders.get(path).map_or(0, |f| f.count);
        self.details = Some(Details::from_text(
            t,
            self.screen,
            "Folder details",
            format!(
                "{}\n\n{}, including subfolders\n\n{}",
                collections::name(path),
                counted(count, "book"),
                path.display()
            ),
        ));
    }
    fn start_name(&mut self, operation: NameOperation, t: &Theme) {
        self.name_sheet = Some(NameSheet::new(t, self.screen, operation));
    }
    fn submit_name(&mut self, cx: &mut EventCx) {
        let Some(sheet) = &self.name_sheet else { return };
        let operation = sheet.operation.clone();
        let name = sheet.field.value().to_owned();
        let result = match &operation {
            NameOperation::Create(parent) => {
                if !self.folder_creatable(parent) {
                    return;
                }
                files::create_folder(parent, &name)
            }
            NameOperation::Rename(path) => {
                if self.folder_protected(path) {
                    return;
                }
                files::rename_folder(path, &name)
            }
        };
        match result {
            // An unchanged name: nothing moved, so there is nothing to rescan.
            Ok(path) if matches!(&operation, NameOperation::Rename(from) if *from == path) => {
                self.name_sheet = None;
                cx.redraw();
            }
            Ok(path) => {
                let (changes, message) = match operation {
                    NameOperation::Create(_) => (self.library.add_empty_folder(path), "Folder created"),
                    NameOperation::Rename(from) => (self.library.move_paths(vec![(from, path)]), "Folder renamed"),
                };
                self.name_sheet = None;
                self.apply_changes(changes, cx);
                cx.toast(message, None);
            }
            Err(e) => {
                let sheet = self.name_sheet.as_mut().unwrap();
                sheet.error = format!("{e:#}");
                sheet.field.focused = true;
                sheet.keyboard.visible = true;
                cx.redraw();
            }
        }
    }
    /// Applies queued edits to this view. A Home delete before browsing leaves
    /// the index unloaded; `prepare_browsing` replays the edits when it loads.
    fn apply_changes(&mut self, changes: Vec<(u64, PathChange)>, cx: &mut EventCx) {
        self.warm_seq = self.warm_seq.wrapping_add(1);
        for (_, change) in &changes {
            change.apply_home(&mut self.home, &self.roots);
            if self.browsing_ready {
                change.apply(&mut self.index);
            }
            if let PathChange::Move(from, to) = change {
                self.navigation.move_path(from, to);
            }
        }
        self.changes.extend(changes);
        if self.browsing_ready {
            self.rebuild_collections();
            self.navigation.folder = self.collections.restore_for(self.navigation.folder.as_deref(), &self.roots);
            self.navigation.folder_pages.retain(|p, _| self.collections.folders.contains_key(p));
            self.home.set_library_counts(self.collections.top_level_counts(&self.roots));
            if let Some(search) = &mut self.search {
                search.cache.sync(&self.index);
            }
        }
        let old_grid = self.grid.rect();
        self.refresh(cx.theme);
        self.save_navigation();
        if self.is_home() {
            cx.refresh(self.screen, RefreshMode::Full);
        } else {
            self.refresh_grid(cx, old_grid);
        }
    }
    fn ask_delete_folder(&mut self, path: PathBuf, cx: &mut EventCx) {
        if self.folder_protected(&path) {
            return;
        }
        match files::count_contents(&path) {
            Ok(contents) => {
                let message = folder_delete_message(&collections::name(&path), &contents);
                self.confirm = Some(ConfirmDelete::new(
                    cx.theme,
                    self.screen,
                    DeleteRequest::Item(Target::Folder(path)),
                    "Delete folder?",
                    message,
                ));
            }
            Err(e) => cx.toast(format!("{e:#}"), None),
        }
    }
    fn delete_target(&mut self, request: DeleteRequest, cx: &mut EventCx) {
        let target = match request {
            DeleteRequest::Item(target) => target,
            DeleteRequest::Selection(targets) => {
                self.delete_selection(targets, cx);
                return;
            }
        };
        match target {
            Target::Book(id) => self.delete(&id, cx),
            Target::Folder(path) => {
                if self.folder_protected(&path) {
                    return;
                }
                let deletion = files::delete_folder(&path, &self.index, &self.store);
                for (id, _) in &deletion.books {
                    self.remove_book_state(id);
                }
                match deletion.error {
                    None => {
                        let mut changes = self.library.remove_paths(vec![path.clone()]);
                        if let Some(parent) = path.parent().filter(|p| files::is_empty(p)) {
                            changes.extend(self.library.add_empty_folder(parent.to_owned()));
                        }
                        self.apply_changes(changes, cx);
                        cx.toast(format!("Deleted “{}”", collections::name(&path)), None);
                    }
                    Some(e) => {
                        // remove_dir_all can stop partway. Drop the books already
                        // gone now; the rescan settles subfolders and the rest.
                        let removed = deletion.books.len();
                        if removed > 0 {
                            let changes =
                                self.library.remove_paths(deletion.books.into_iter().map(|(_, p)| p).collect());
                            self.apply_changes(changes, cx);
                        }
                        self.library.rescan();
                        cx.toast(
                            match removed {
                                0 => format!("{e:#}"),
                                n => format!("{e:#}. {} removed before it stopped.", counted(n, "book")),
                            },
                            None,
                        );
                    }
                }
            }
        }
    }
    /// Reload status changes from Store without accepting older cached paths.
    /// A check finished, or Later or Skip was chosen: the notice and the
    /// dot follow, in their reserved places, once no finger or overlay is
    /// on Home (see `apply_pending`). Nothing else moves.
    fn apply_update_status(&mut self, cx: &mut EventCx) {
        if self.updates.revision() == self.update_revision {
            return;
        }
        let status = self.updates.status();
        self.update_revision = status.revision;
        let notice = status.notice().cloned();
        let dot = status.update().is_some();
        if notice != self.notice || dot != self.update_dot {
            self.notice = notice;
            self.update_dot = dot;
            cx.redraw();
        }
    }
    fn reload_home(&mut self) {
        self.pending_home = self.store.home_summary().map(|mut summary| {
            if self.browsing_ready {
                summary.books.retain_mut(|e| {
                    if let Some(book) = self.index.books.iter().find(|b| b.id == e.book.id && b.available) {
                        e.book.path = book.path.clone();
                        true
                    } else {
                        false
                    }
                });
                summary.set_library_counts(self.collections.top_level_counts(&self.roots));
            }
            summary
        });
        // Browsing patched paths from the index, which has every edit. Before
        // browsing, the saved summary may predate a Home deletion: replay all,
        // which only holds idempotent removals then.
        self.pending_home_revision = if self.browsing_ready { self.changes.last().map_or(0, |(r, _)| *r) } else { 0 };
    }
    fn prune_changes(&mut self) {
        let acknowledged = self.index_revision.min(self.worker_home_revision);
        self.changes.retain(|(r, _)| *r > acknowledged);
    }
    /// Deletes the book file and its reading state. The library worker drops it
    /// from the index and removes its cached cover; this view updates at once.
    fn delete(&mut self, id: &str, cx: &mut EventCx) {
        let Some(book) = self.book(id).cloned() else { return };
        let deletion = match files::delete_book(&book.id, &book.path, &self.store) {
            Ok(deletion) => deletion,
            Err(e) => {
                log::warn!("deleting {}: {e:#}", book.path.display());
                cx.toast(format!("Couldn't delete the book: {e:#}"), None);
                return;
            }
        };
        let changes = self.library.change_paths(deletion.changes);
        self.remove_book_state(&book.id);
        self.home.books.retain(|e| e.book.id != book.id);
        self.apply_changes(changes, cx);
        cx.toast(
            match deletion.sidecar_error {
                None => format!("Deleted “{}”", book.metadata.title),
                Some(_) => format!("Deleted “{}”, but couldn't remove its KOReader data", book.metadata.title),
            },
            None,
        );
    }
}
const HOME_RECENT_LABELS_MM: f32 = 8.0;
const LIBRARY_BOOK_LABELS_MM: f32 = 8.0;
const LIBRARY_HEADER_TOP_MM: f32 = 2.2;
const LIBRARY_HEADER_H_MM: f32 = 11.0;
const LIBRARY_HEADER_GAP_MM: f32 = 2.9;

/// Grid dimensions for Library and Search: 4 × 3 on Paperwhite portrait,
/// 6 × 2 in landscape, and 3 × 3 on narrow 6" portrait screens (< 100 mm).
fn library_grid_dims(screen: Rect, t: &Theme) -> (usize, usize) {
    if screen.w > screen.h {
        (6, 2)
    } else if screen.w < t.mm(100.0) {
        (3, 3)
    } else {
        (4, 3)
    }
}

fn library_grid_gaps(cols: usize, t: &Theme) -> (i32, i32) {
    let gap_x = match cols {
        6 => t.mm(2.6),
        3 => t.mm(2.8),
        _ => t.mm(3.4),
    };
    (gap_x, t.mm(2.0))
}

fn continue_cover_rect(r: Rect) -> Rect {
    Rect::new(r.x, r.y, r.h * 2 / 3, r.h)
}

/// Crisp 2 px cover outline at 300 dpi (1 px at 167 dpi).
fn cover_outline(t: &Theme) -> i32 {
    t.mm(0.17).max(1)
}

/// Home's Recent books row fills each column flush with the margins (2:3 cover
/// with 1-line title + 1-line author below).
fn home_recent_cover_rect(grid: &Grid, cell: Rect, t: &Theme) -> Rect {
    let (w, h) = grid.base_cell_size();
    let cover_h = (h - t.mm(HOME_RECENT_LABELS_MM)).min(w * 3 / 2).max(1);
    let cover_w = (cover_h * 2 / 3).max(1);
    Rect::new(cell.x, cell.y, cover_w, cover_h)
}

/// Left-aligned 2:3 book cover in a Library or Search grid cell (`214 × 321`
/// on Paperwhite portrait 4 × 3), aligning with labels and folder front covers.
fn grid_cover_rect(grid: &Grid, cell: Rect, t: &Theme) -> Rect {
    let (w, h) = grid.base_cell_size();
    let cover_h = (h - t.mm(LIBRARY_BOOK_LABELS_MM)).min(w * 3 / 2).max(1);
    let cover_w = (cover_h * 2 / 3).max(1);
    Rect::new(cell.x, cell.y, cover_w, cover_h)
}

fn grid_folder_rect(grid: &Grid, cell: Rect, parent_line: bool, t: &Theme) -> Rect {
    let (w, h) = grid.base_cell_size();
    let labels = t.mm(if parent_line { 8.0 } else { 5.1 });
    Rect::new(cell.x, cell.y, w, (h - labels).min(w * 3 / 2).max(1))
}

/// Top-bar title style (`"Home"` / `"Search"`): Literata SemiBold, scaled on narrow panels.
fn top_title_style(t: &Theme, screen: Rect) -> TextStyle {
    if screen.w < t.mm(100.0) { t.title } else { TextStyle::new(t.pt(15.4), Weight::SemiBold, Family::Serif) }
}

/// Library / Subfolder header title style: Literata SemiBold (`56 px` at 300 dpi).
fn folder_title_style(t: &Theme, screen: Rect) -> TextStyle {
    if screen.w < t.mm(100.0) { t.title } else { TextStyle::new(t.pt(13.4), Weight::SemiBold, Family::Serif) }
}

/// Bottom-anchored Library card on Home: folder icon, `"Library"`, cached counts, chevron.
fn draw_library_card(cx: &mut DrawCx, r: Rect, subtitle: &str, pressed: bool) {
    let t = cx.theme;
    let (fg, sub_fg) = if pressed {
        cx.canvas.fill_rounded_rect(r, t.radius, INK);
        cx.hint(r, RefreshMode::Fast);
        (PAPER, PAPER)
    } else {
        cx.canvas.stroke_rounded_rect(r, t.radius, t.border as f32, INK);
        (INK, INK2)
    };
    let icon_size = t.mm(6.1).min(r.h - t.mm(3.0));
    let pad = t.mm(3.9);
    let icon_rect = Rect::new(r.x + pad, r.y + (r.h - icon_size) / 2, icon_size, icon_size);
    cx.icon(Icon::Folder, icon_rect, icon_size, fg);
    let chevron_size = t.mm(4.7).min(icon_size);
    let chevron_pad = t.mm(2.8);
    let chevron_rect =
        Rect::new(r.right() - chevron_pad - chevron_size, r.y + (r.h - chevron_size) / 2, chevron_size, chevron_size);
    cx.icon(Icon::ChevronRight, chevron_rect, chevron_size, fg);
    let text_x = icon_rect.right() + t.mm(2.9);
    let text_w = (chevron_rect.x - text_x - t.mm(2.0)).max(0);
    let title_style = TextStyle::new(t.pt(10.5), Weight::SemiBold, Family::Sans).gray(fg);
    let title_h = t.mm(4.8);
    let sub_h = t.mm(3.8);
    let gap = t.mm(0.4);
    let block_y = r.y + (r.h - title_h - gap - sub_h) / 2;
    cx.line("Library", title_style, Rect::new(text_x, block_y, text_w, title_h), HAlign::Left);
    cx.line(subtitle, t.small.gray(sub_fg), Rect::new(text_x, block_y + title_h + gap, text_w, sub_h), HAlign::Left);
}

/// Smart folder/book count omitting zero parts:
/// `"23 folders · 7 books"`, `"14 books"`, `"5 folders"`, or `"Empty"`.
fn smart_counts(folders: usize, books: usize) -> String {
    match (folders, books) {
        (0, 0) => "Empty".into(),
        (f, 0) => counted(f, "folder"),
        (0, b) => counted(b, "book"),
        (f, b) => format!("{} · {}", counted(f, "folder"), counted(b, "book")),
    }
}

/// "Verso 0.3.0 is available · Update ›": one line in its reserved spot.
fn draw_notice(cx: &mut DrawCx, offer: &Offer, r: Rect, pressed: bool) {
    let t = cx.theme;
    let (text, accent) = if pressed {
        cx.canvas.fill_rounded_rect(r, t.radius, INK);
        cx.hint(r, RefreshMode::Fast);
        (crate::ui::theme::PAPER, crate::ui::theme::PAPER)
    } else {
        cx.canvas.stroke_rounded_rect(r, t.radius, t.hairline as f32, LINE);
        cx.hint(r, RefreshMode::Ui);
        (INK2, INK)
    };
    let tight = r.w < t.mm(60.0);
    let inner = r.inset_xy(t.mm(if tight { 2.0 } else { 3.0 }), 0);
    let (msg_style, act_style) = if tight { (t.small, t.small.weight(Weight::SemiBold)) } else { (t.body, t.button) };
    let action = "Update";
    let action_w = cx.text.width(action, act_style).ceil() as i32;
    let chevron = Rect::new(inner.right() - t.icon_small, inner.y, t.icon_small, inner.h);
    let label = Rect::new(chevron.x - t.mm(1.0) - action_w, inner.y, action_w + 1, inner.h);
    cx.line(action, act_style.gray(accent), label, HAlign::Left);
    cx.icon(Icon::ChevronRight, chevron, t.icon_small, accent);
    let gap = t.mm(if tight { 1.5 } else { 3.0 });
    let message = Rect { w: (label.x - gap - inner.x).max(0), ..inner };
    cx.line(&format!("Verso {} is available", offer.version), msg_style.gray(text), message, HAlign::Left);
}

/// The dot on the Settings icon while an update is available.
fn draw_dot(cx: &mut DrawCx, icon: Rect) {
    let t = cx.theme;
    let r = t.mm(0.9) as f32;
    let c = icon.center();
    let (x, y) = ((c.x + t.icon / 2) as f32 - r * 0.4, (c.y - t.icon / 2) as f32 + r * 0.4);
    // A paper ring keeps it apart from the gear's outline.
    cx.canvas.fill_circle(x, y, r + t.hairline as f32 * 2.0, crate::ui::theme::PAPER);
    cx.canvas.fill_circle(x, y, r, INK);
}

/// Caches the reading states of `books`. Returns the records read and whether
/// the state directory was listed. Once a listing succeeds (`states_loaded`),
/// only saved records stay in `states` and absent ids are known to be default.
fn load_states(
    store: &Store,
    states: &mut HashMap<String, BookState>,
    states_loaded: &mut bool,
    books: &[Book],
) -> (usize, bool) {
    if *states_loaded {
        return (0, false);
    }
    let mut missing: Vec<_> = books.iter().map(|b| b.id.as_str()).filter(|id| !states.contains_key(*id)).collect();
    missing.sort_unstable();
    missing.dedup();
    if missing.len() > LISTED_STATES
        && let Some(saved) = store.book_state_ids()
    {
        let mut reads = 0;
        for id in saved {
            states.entry(id).or_insert_with_key(|id| {
                reads += 1;
                store.book(id)
            });
        }
        *states_loaded = true;
        return (reads, true);
    }
    let mut reads = 0;
    for id in missing {
        reads += 1;
        states.insert(id.to_owned(), store.book(id));
    }
    (reads, false)
}
fn status(state: &BookState) -> String {
    if state.finished {
        "Finished".into()
    } else if state.position.0.is_empty() {
        "New".into()
    } else {
        format!("{}%", state.progress.min(10000) / 100)
    }
}
/// A folder holding only empty folders is not itself "Empty".
fn folder_subtitle(folder: &collections::Folder) -> String {
    match (folder.count, folder.children.len()) {
        (0, 0) => "Empty".into(),
        (0, children) => counted(children, "folder"),
        (count, _) => counted(count, "book"),
    }
}
/// Names what a folder delete would lose: books with their progress, and other
/// files such as PDFs and Kindle books. An empty tree says so instead of "0 books".
fn folder_delete_message(name: &str, contents: &files::Contents) -> String {
    if contents.books == 0 && contents.other_files == 0 {
        return match contents.folders {
            0 => format!("The empty folder “{name}” will be removed from this device."),
            1 => format!("“{name}” and the empty folder in it will be removed from this device."),
            n => format!("“{name}” and the {n} empty folders in it will be removed from this device."),
        };
    }
    let mut parts = Vec::new();
    if contents.books > 0 {
        let progress = if contents.books == 1 { "its" } else { "their" };
        parts.push(format!("{} and {progress} reading progress", counted(contents.books, "book")));
    }
    if contents.other_files > 0 {
        parts.push(counted(contents.other_files, "other file"));
    }
    format!("“{name}” and everything in it will be removed from this device: {}.", parts.join(", "))
}
fn counted(count: usize, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}
/// Keep the nearest parents visible; tapping a path shows its complete value.
fn path_tail(text: &mut crate::gfx::TextRenderer, path: &str, style: crate::gfx::TextStyle, width: i32) -> String {
    if text.width(path, style) <= width as f32 {
        return path.into();
    }
    let chars: Vec<_> = path.char_indices().map(|(i, _)| i).chain(std::iter::once(path.len())).collect();
    let mut low = 0;
    let mut high = chars.len() - 1;
    while low < high {
        let mid = (low + high) / 2;
        if text.width(&format!("…{}", &path[chars[mid]..]), style) <= width as f32 {
            high = mid;
        } else {
            low = mid + 1;
        }
    }
    format!("…{}", &path[chars[low]..])
}
fn draw_cover(cx: &mut DrawCx, book: &Book, rect: Rect, store: &Store, cache: &mut Thumbnails) {
    if let Some(cover) = cache.load(store, book, rect.w, rect.h, Fit::Cover) {
        cover.blit(cx.canvas, rect);
        cx.canvas.stroke_rounded_rect(rect, 0.0, cover_outline(cx.theme) as f32, INK);
        return;
    }
    let t = cx.theme;
    cx.canvas.stroke_rounded_rect(rect, t.radius, t.border as f32, INK);
    let inset = t.mm(2.0).min(rect.w / 8);
    let title = Rect::new(rect.x + inset, rect.y + rect.h / 8, rect.w - 2 * inset, rect.h * 3 / 5);
    let style = crate::gfx::TextStyle { family: Family::Serif, ..t.heading };
    cx.paragraph(&book.metadata.title, style, title, HAlign::Center);
    cx.hline(rect.y + rect.h * 3 / 4, rect.x + rect.w * 2 / 5, rect.x + rect.w * 3 / 5, t.hairline, INK);
    cx.line(
        &book.metadata.authors,
        t.caption.gray(INK2),
        Rect::new(rect.x + inset, rect.bottom() - rect.h / 6, rect.w - 2 * inset, rect.h / 8),
        HAlign::Center,
    );
}
impl Screen for Books {
    fn draw(&mut self, cx: &mut DrawCx) {
        if let Some(reader) = &mut self.reader {
            reader.draw(cx);
            return;
        }
        let t = cx.theme;
        self.thumbnails.home = self.is_home();
        let paste_enabled = self.paste_reason().is_none();
        if let Some(selection) = &mut self.selection {
            selection.paste.enabled = paste_enabled;
            selection.draw(cx);
        } else {
            if self.is_home() {
                cx.line("Home", top_title_style(t, self.screen), self.title_rect, HAlign::Left);
            } else if self.search.is_some() {
                cx.line("Search", top_title_style(t, self.screen), self.title_rect, HAlign::Left);
            } else {
                self.home_action.draw(cx);
            }
            cx.battery_indicator(self.battery_rect);
            self.search_action.draw(cx);
            self.rescan_action.draw(cx);
            self.transfer_action.draw(cx);
            self.settings_action.draw(cx);
            if self.update_dot {
                draw_dot(cx, self.settings_action.rect());
            }
            self.exit.draw(cx);
        }
        if let Some(search) = &mut self.search {
            search.field.draw(cx);
            search.all.draw(cx);
            search.current.draw(cx);
            let label = match &search.scope {
                Scope::All => counted(self.books.len(), "result"),
                Scope::Collection(folder) => {
                    format!("{} · {}", counted(self.books.len(), "result"), collections::name(folder))
                }
            };
            cx.line(&label, t.heading, self.header, HAlign::Left);
            self.pager.draw(cx);
        }
        if !self.is_home() && self.search.is_none() {
            if !self.is_top_level() {
                self.back.draw(cx);
            }
            let title = self.navigation.folder.as_deref().map(collections::name).unwrap_or_else(|| "Library".into());
            cx.line(
                &title,
                folder_title_style(t, self.screen),
                Rect::new(self.path_rect.x, self.path_rect.y, self.path_rect.w, t.mm(6.4)),
                HAlign::Left,
            );
            let subtitle = if let Some(reason) = self.paste_reason() {
                reason.to_owned()
            } else if self.scanning {
                "Scanning…".into()
            } else {
                smart_counts(self.folders.len(), self.books.len())
            };
            cx.line(
                &subtitle,
                t.small.gray(INK2),
                Rect::new(self.path_rect.x, self.path_rect.y + t.mm(7.1), self.path_rect.w, t.mm(3.8)),
                HAlign::Left,
            );
            if self.navigation.folder.is_some() {
                self.new_folder.draw(cx);
            }
            self.pager.draw(cx);
        }
        if let Some(book) = self.continue_id.as_ref().filter(|_| self.is_home()).and_then(|id| self.book(id)).cloned() {
            let r = self.continue_rect;
            let cover = continue_cover_rect(r);
            draw_cover(cx, &book, cover, &self.store, &mut self.thumbnails);
            cx.hint(cover, RefreshMode::Ui);
            if self.continue_press.is_down() {
                cx.canvas.stroke_rounded_rect(r, t.radius, t.border as f32, INK);
                cx.hint(r, RefreshMode::Fast);
            }
            let landscape = self.screen.w > self.screen.h;
            let narrow = self.screen.w < t.mm(100.0);
            let text_gap = t.mm(if narrow || landscape { 3.2 } else { 3.7 });
            let x = cover.right() + text_gap;
            let w = (r.right() - x).max(1);
            let bar_h = t.mm(0.5).max(2);
            let pct_style = t.small.gray(INK2);
            let progress = self.home.resume().map_or(0, |e| e.status.progress).min(10000);
            let pct_text = format!("{}%", progress / 100);
            let pct_w = cx.text.width(&pct_text, pct_style).ceil() as i32 + 2;
            let pct_gap = t.mm(2.0);
            let bar_cy = r.bottom() - t.mm(2.4);
            let bar = Rect::new(x, bar_cy - bar_h / 2, (w - pct_w - pct_gap).max(1), bar_h);
            cx.fill(bar, LINE);
            cx.fill(Rect { w: (bar.w as i64 * progress as i64 / 10000) as i32, ..bar }, INK);
            let pct_h = t.mm(3.6);
            let progress_top = bar_cy - pct_h / 2;
            cx.line(&pct_text, pct_style, Rect::new(r.right() - pct_w, progress_top, pct_w, pct_h), HAlign::Right);

            let section_y = r.y + t.mm(0.9);
            let section_h = t.mm(2.8);
            cx.section_label("Continue reading", Rect::new(x, section_y, w, section_h));
            let title_style =
                if narrow || landscape { t.title } else { TextStyle::new(t.pt(14.4), Weight::SemiBold, Family::Serif) };
            let title_y = section_y + section_h + t.mm(1.0);
            let lh = title_style.line_height().round() as i32;
            let author_style = if narrow || landscape { t.small.gray(INK2) } else { t.body.gray(INK2) };
            let author_gap = t.mm(1.2);
            let author_h = t.mm(3.8);
            let (_, measured_h) = cx.text.measure(&book.metadata.title, title_style, Some(w as f32));
            let needed_lines = ((measured_h / title_style.line_height()).round() as i32).clamp(1, 2);
            let max_lines =
                if title_y + 2 * lh + author_gap + author_h <= progress_top - t.mm(1.0) { needed_lines } else { 1 };
            let title_rect = Rect::new(x, title_y, w, max_lines * lh);
            cx.text.draw_lines(
                cx.canvas,
                &book.metadata.title,
                title_style,
                title_rect,
                HAlign::Left,
                max_lines as usize,
            );
            let author_y = title_rect.bottom() + author_gap;
            cx.line(&book.metadata.authors, author_style, Rect::new(x, author_y, w, author_h), HAlign::Left);
            if let Some(div_y) = self.home_divider_y {
                cx.hline(div_y, r.x, r.right(), t.hairline, INK);
            }
        }
        if self.is_home() && !self.books.is_empty() {
            cx.line("Recent books", t.heading, self.header, HAlign::Left);
        }
        let shared_names = self.shared_names();
        for (i, cell) in self.grid.cells() {
            if let Some(path) = self.folders.get(i) {
                let folder = &self.collections.folders[path];
                let name = collections::name(path);
                let parent_line = shared_names.contains(&name);
                let r = self.folder_rect(cell, parent_line, t);
                // Fields, not preview_books, so the cache can be borrowed too.
                let picks: Vec<&Book> = self
                    .previews
                    .get(path)
                    .unwrap_or_default()
                    .iter()
                    .take(self.folder_preview.slots())
                    .filter_map(|&i| self.index.books.get(i))
                    .collect();
                let preview = folder_tile::draw(
                    cx,
                    self.folder_preview,
                    &picks,
                    folder.count,
                    r,
                    &self.store,
                    &mut self.thumbnails,
                );
                match preview {
                    // Like book covers: no Ui hint while selecting, so the
                    // fast tick box keeps its own mode.
                    Some(preview) if self.grid.pressed() != Some(i) && self.selection.is_none() => {
                        cx.hint(preview, RefreshMode::Ui)
                    }
                    Some(_) => {}
                    None => {
                        let outline = Rect::new(r.x + t.mm(1.0), r.y + r.h / 5, r.w - t.mm(2.0), r.h / 2);
                        cx.icons.draw(cx.canvas, Icon::Folder, outline, INK);
                        let sub_y = outline.bottom() + t.mm(1.0);
                        let sub_h = (r.bottom() - sub_y).min(t.mm(4.5)).max(0);
                        cx.line(
                            &folder_subtitle(folder),
                            t.small.gray(INK2),
                            Rect::new(r.x, sub_y, r.w, sub_h),
                            HAlign::Center,
                        );
                    }
                }
                let name_y = r.bottom() + t.mm(1.1);
                let name_h = t.mm(3.5);
                cx.line(
                    &name,
                    t.small.weight(Weight::SemiBold),
                    Rect::new(cell.x, name_y, cell.w, name_h),
                    HAlign::Left,
                );
                if parent_line {
                    let context = path.parent().map(|p| p.display().to_string()).unwrap_or_default();
                    let context = path_tail(cx.text, &context, t.caption, cell.w);
                    cx.line(
                        &context,
                        t.caption.gray(INK2),
                        Rect::new(cell.x, name_y + name_h, cell.w, t.mm(3.2)),
                        HAlign::Left,
                    );
                }
                if self.grid.pressed() == Some(i) {
                    cx.canvas.stroke_rounded_rect(r, t.radius, (2 * t.border) as f32, INK);
                    cx.hint(r, RefreshMode::Fast);
                }
                self.draw_tick(cx, Target::Folder(path.clone()), cell);
                continue;
            }
            let book = &self.books[i - self.folders.len()];
            let cover = self.cover_rect(cell, t);
            draw_cover(cx, book, cover, &self.store, &mut self.thumbnails);
            if self.grid.pressed() == Some(i) {
                cx.canvas.stroke_rounded_rect(cover, 0.0, (2 * t.border) as f32, INK);
                cx.hint(cover, RefreshMode::Fast);
            } else if self.selection.is_none() {
                cx.hint(cover, RefreshMode::Ui);
            }
            // While selecting, the cover takes the default grayscale mode; a
            // Ui hint would override the fast tick box drawn over its corner.
            if self.is_home() {
                let y = cover.bottom() + t.mm(1.4);
                let title_h = t.mm(3.4);
                let author_h = t.mm(3.2);
                cx.line(
                    &book.metadata.title,
                    t.small.weight(Weight::SemiBold),
                    Rect::new(cell.x, y, cell.w, title_h),
                    HAlign::Left,
                );
                cx.line(
                    &book.metadata.authors,
                    t.caption.gray(INK2),
                    Rect::new(cell.x, y + title_h, cell.w, author_h),
                    HAlign::Left,
                );
            } else {
                let state = self.states.get(&book.id);
                let bar_h = t.mm(0.5).max(2);
                if let Some(state) = state {
                    if state.finished {
                        // 100% solid bar with a 2 px white gap below the cover outline
                        // so it stays distinct from dark covers.
                        let bar = Rect::new(cover.x, cover.bottom() + cover_outline(t), cover.w, bar_h);
                        cx.fill(bar, INK);
                    } else if !state.position.0.is_empty() {
                        let bar = Rect::new(cover.x, cover.bottom(), cover.w, bar_h);
                        let progress = state.progress.min(10000);
                        cx.fill(bar, LINE);
                        cx.fill(Rect { w: (bar.w as i64 * progress as i64 / 10000) as i32, ..bar }, INK);
                    }
                }
                let y = cover.bottom() + t.mm(1.1);
                let title_h = t.mm(3.4);
                let author_h = t.mm(3.2);
                cx.line(
                    &book.metadata.title,
                    t.small.weight(Weight::SemiBold),
                    Rect::new(cell.x, y, cell.w, title_h),
                    HAlign::Left,
                );
                cx.line(
                    &book.metadata.authors,
                    t.caption.gray(INK2),
                    Rect::new(cell.x, y + title_h, cell.w, author_h),
                    HAlign::Left,
                );
                self.draw_tick(cx, Target::Book(book.id.clone()), cell);
            }
        }
        if !self.is_home() && self.books.is_empty() && self.folders.is_empty() {
            let message = if let Some(search) = &self.search {
                if search.folded.is_empty() {
                    "No books in this scope.".into()
                } else {
                    "No matching books. Try another title, author or filename.".into()
                }
            } else if !self.is_top_level() {
                "This folder is empty".into()
            } else if self.scanning {
                format!(
                    "Finding books in {}…",
                    self.roots.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(" • ")
                )
            } else {
                format!(
                    "No EPUB books found in {}.\nAdd books, then tap Refresh.",
                    self.roots.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(" • ")
                )
            };
            cx.paragraph(
                &message,
                t.body.gray(INK2),
                Rect::new(t.margin, self.grid.rect().y + t.mm(4.0), self.screen.w - 2 * t.margin, t.mm(24.0)),
                HAlign::Left,
            );
        }
        if self.is_home() {
            if self.continue_id.is_none() && self.books.is_empty() {
                let top = t.top_bar + t.mm(4.0);
                let area =
                    Rect::new(t.margin, top, self.screen.w - 2 * t.margin, (self.notice_rect.y - top).max(t.target));
                cx.paragraph("Open a book from Library to start reading", t.body.gray(INK2), area, HAlign::Center);
            }
            if self.selection.is_none()
                && let Some(offer) = &self.notice
            {
                draw_notice(cx, offer, self.notice_rect, self.notice_press.is_down());
            }
            let subtitle = self.library_card_subtitle();
            draw_library_card(cx, self.library_card, &subtitle, self.library_card_press.is_down());
        }
        if let Some(search) = &self.search {
            search.keyboard.draw(cx);
        }
        if let Some((_, menu)) = &self.menu {
            menu.draw(cx);
        }
        if let Some(jump) = &self.jump {
            let start = self.page_start(jump.page());
            jump.draw(cx, self.pager.pages, &start);
        }
        if let Some(details) = &mut self.details {
            details.draw(cx);
        }
        if let Some(confirm) = &self.confirm {
            confirm.draw(cx);
        }
        if let Some(clash) = &self.clash {
            clash.draw(cx);
        }
        if let Some(sheet) = &mut self.name_sheet {
            sheet.draw(cx);
        }
        if let Some(offer) = &self.offer {
            offer.draw(cx);
        }
        if !self.thumbnails.requests.is_empty() {
            self.library.repair(std::mem::take(&mut self.thumbnails.requests));
        }
        let sizes = self.thumbnails.take_new_sizes();
        if !sizes.is_empty() {
            self.library.drew_cover_sizes(sizes);
        }
        let neighbours = self.neighbour_covers(t);
        self.thumbnails.prefetch(neighbours);
    }
    fn on_gesture(&mut self, g: Gesture, cx: &mut EventCx) -> Action {
        self.last_input = cx.now;
        if matches!(g, Gesture::Down(_)) {
            self.touching = true;
        }
        if g.ends_touch() {
            self.touching = false;
        }
        if let Some(reader) = &mut self.reader {
            match reader.on_gesture(g, cx) {
                Action::Pop => self.returned(cx),
                action => return action,
            }
            return Action::None;
        }
        // Library controls own their press/capture state, including gestures
        // that return no action. Reader gestures invalidate independently.
        cx.redraw();
        if let Some(offer) = &mut self.offer {
            if let Some(event) = offer.on_gesture(&g) {
                let offer = self.offer.take().unwrap();
                offer.apply(&event, &self.updates);
                if event == OfferEvent::Update {
                    let (updates, waker) = (self.updates.clone(), self.worker.waker());
                    let version = offer.offer.version.clone();
                    return Action::Push(Box::new(UpdateScreen::new(
                        cx.theme,
                        self.screen,
                        updates,
                        waker,
                        version,
                        cx,
                    )));
                }
            }
        } else if let Some(sheet) = &mut self.name_sheet {
            match sheet.on_gesture(&g, cx.theme) {
                Some(NameEvent::Cancel) => self.name_sheet = None,
                Some(NameEvent::Submit) => self.submit_name(cx),
                None => {}
            }
        } else if let Some(clash) = &mut self.clash {
            if let Some(answer) = clash.on_gesture(&g) {
                self.clash = None;
                match answer {
                    Some(choice) => self.continue_paste(Some(choice), cx),
                    None => self.finish_paste(PasteEnd::Cancelled, cx),
                }
            }
        } else if let Some(confirm) = &mut self.confirm {
            match confirm.on_gesture(&g) {
                // A selection can be large: show "Deleting…" first, then delete
                // in `run_pending_work` once that frame is on screen.
                Some(true) if matches!(confirm.target, DeleteRequest::Selection(_)) => confirm.start_working(),
                Some(true) => {
                    let target = self.confirm.take().unwrap().target;
                    self.delete_target(target, cx);
                }
                Some(false) => self.confirm = None,
                None => {}
            }
        } else if let Some(details) = &mut self.details {
            if details.on_gesture(&g, cx) {
                self.details = None;
            }
        } else if let Some((target, menu)) = &mut self.menu {
            let event = menu.on_gesture(&g, cx.theme);
            let delete_index = menu.items.len() - 1;
            let select_index = menu.items.iter().position(|(_, label)| label == "Select");
            let move_index = menu.items.iter().position(|(_, label)| label == "Move");
            let target = target.clone();
            if let Some(event) = event {
                self.menu = None;
                if matches!(event, MenuEvent::Chosen(i) if Some(i) == select_index) {
                    self.start_selection(target, cx);
                    return Action::None;
                }
                if matches!(event, MenuEvent::Chosen(i) if Some(i) == move_index) {
                    self.start_move(vec![target], cx);
                    return Action::None;
                }
                match (target, event) {
                    (Target::Book(id), event) => match event {
                        MenuEvent::Chosen(0) => self.open(id, cx),
                        MenuEvent::Chosen(1) => {
                            // Home entries are stripped summaries; show the full record.
                            if let Some(book) = self.reading_book(&id) {
                                self.details = Some(Details::new(cx.theme, self.screen, &book, &self.store.book(&id)));
                            }
                        }
                        MenuEvent::Chosen(2) => self.mark(&id, true, cx),
                        MenuEvent::Chosen(index) if index == delete_index => {
                            if let Some(book) = self.book(&id) {
                                let title: String = book.metadata.title.chars().take(160).collect();
                                let message =
                                    format!("“{title}” and its reading progress will be removed from this device.");
                                self.confirm = Some(ConfirmDelete::new(
                                    cx.theme,
                                    self.screen,
                                    DeleteRequest::Item(Target::Book(id)),
                                    "Delete book?",
                                    message,
                                ));
                            }
                        }
                        MenuEvent::Chosen(3) => self.mark(&id, false, cx),
                        _ => {}
                    },
                    (Target::Folder(path), event) => match event {
                        MenuEvent::Chosen(0) => self.navigate(Some(path), cx),
                        MenuEvent::Chosen(1) => self.folder_details(&path, cx.theme),
                        MenuEvent::Chosen(2) => {
                            self.navigate(Some(path.clone()), cx);
                            self.toggle_search(cx);
                            self.search.as_mut().unwrap().scope = Scope::Collection(path);
                            self.refresh(cx.theme);
                        }
                        MenuEvent::Chosen(5) => self.start_name(NameOperation::Create(path), cx.theme),
                        MenuEvent::Chosen(6) => self.start_name(NameOperation::Rename(path), cx.theme),
                        MenuEvent::Chosen(7) => self.ask_delete_folder(path, cx),
                        _ => {}
                    },
                }
            }
        } else if let Some(jump) = &mut self.jump {
            match jump.on_gesture(&g) {
                Some(JumpEvent::Close) => self.jump = None,
                Some(JumpEvent::Go(page)) => {
                    self.jump = None;
                    self.go_to_page(page, cx);
                }
                None => {}
            }
        } else if self.search.as_ref().is_some_and(|s| s.keyboard.visible) {
            let before = self.search.as_ref().map(|s| s.keyboard.rect());
            let input = self.search.as_mut().and_then(|s| s.keyboard.on_gesture(&g, cx.theme));
            if self.search.as_ref().map(|s| s.keyboard.rect()) != before {
                // Another language can add a row of keys.
                self.refresh(cx.theme);
            }
            if let Some(input) = input {
                self.edit_search(&input, cx);
            }
        } else {
            if self.selection_controls(&g, cx) {
                return Action::None;
            }
            if self.selection.is_none() && self.settings_action.on_gesture(&g) {
                return Action::Push(Box::new(crate::screens::system::SystemSettings::new(
                    cx.theme,
                    self.screen,
                    self.store.clone(),
                    self.worker.waker(),
                )));
            }
            if self.selection.is_none() && self.transfer_action.on_gesture(&g) {
                return Action::Push(Box::new(crate::screens::transfer::TransferScreen::new(
                    cx.theme,
                    self.screen,
                    self.store.clone(),
                    self.worker.waker(),
                )));
            }
            if self.selection.is_none() && self.search_action.on_gesture(&g) {
                self.toggle_search(cx);
                self.apply_pending(cx);
                return Action::None;
            }
            if self.is_home()
                && self.selection.is_none()
                && let Some(offer) = &self.notice
                && let Some(PressEvent::Tap) = self.notice_press.on_gesture(&g, self.notice_rect)
            {
                self.offer = Some(OfferSheet::new(cx.theme, self.screen, offer.clone()));
                return Action::None;
            }
            if let Some(search) = &mut self.search {
                if search.field.on_gesture(&g) {
                    search.keyboard.visible = true;
                    search.field.focused = true;
                    self.apply_pending(cx);
                    return Action::None;
                }
                let scope = if search.all.on_gesture(&g) {
                    Some(Scope::All)
                } else if search.current.on_gesture(&g) {
                    search.collection.clone().map(Scope::Collection)
                } else {
                    None
                };
                if let Some(scope) = scope {
                    self.selection = None;
                    search.scope = scope;
                    search.debounce_at = None;
                    search.folded = search::fold(search.field.value().trim());
                    self.pager.page = 0;
                    self.refresh(cx.theme);
                    self.apply_pending(cx);
                    return Action::None;
                }
            }
            if self.selection.is_none() && self.exit.on_gesture(&g) {
                return Action::Pop;
            }
            if self.selection.is_none() && self.rescan_action.on_gesture(&g) {
                // Drop any unconsumed completion from an earlier pass so the
                // completion toast waits for this manual rescan.
                let _ = self.library.take_status();
                self.errors.clear();
                self.library.rescan();
                self.scanning = true;
                self.working = true;
                self.manual_rescan = true;
                return Action::None;
            }
            if !self.is_home() && self.search.is_none() && self.selection.is_none() && self.home_action.on_gesture(&g) {
                self.open_home(cx);
                self.apply_pending(cx);
                return Action::None;
            }
            if self.is_home()
                && self.selection.is_none()
                && let Some(PressEvent::Tap) = self.library_card_press.on_gesture(&g, self.library_card)
            {
                self.open_library(cx);
                self.apply_pending(cx);
                return Action::None;
            }
            if !self.is_home() && self.search.is_none() {
                if let Some(path) = self.navigation.folder.clone()
                    && self.new_folder.on_gesture(&g)
                {
                    self.start_name(NameOperation::Create(path), cx.theme);
                    return Action::None;
                }
                if !self.is_top_level() && self.back.on_gesture(&g) {
                    if self.selection.is_some() && !self.carrying() {
                        self.end_selection(cx);
                        return Action::None;
                    }
                    let parent = self
                        .navigation
                        .folder
                        .as_ref()
                        .and_then(|p| self.collections.folders.get(p))
                        .and_then(|f| f.parent.clone());
                    self.navigate(parent, cx);
                    self.apply_pending(cx);
                    return Action::None;
                }
                if matches!(
                    self.path_press.on_gesture(&g, self.path_rect),
                    Some(PressEvent::Tap | PressEvent::LongPress)
                ) && let Some(path) = &self.navigation.folder
                {
                    self.details =
                        Some(Details::from_text(cx.theme, self.screen, "Folder path", path.display().to_string()));
                    return Action::None;
                }
            }
            if !self.is_home() {
                let old_page = self.pager.page;
                if self.pager.on_gesture(&g) == Some(PagerEvent::Jump) {
                    self.jump = Some(PageJump::new(cx.theme, self.screen, self.pager.page, self.pager.pages));
                    return Action::None;
                }
                if let Gesture::Swipe { dir, .. } = g {
                    self.pager.on_swipe(dir);
                }
                if old_page != self.pager.page {
                    self.go_to_page(self.pager.page, cx);
                }
            }
            let mut selected = None;
            if let Some(event) = self.grid.on_gesture(&g) {
                let (i, long) = match event {
                    GridEvent::Tapped(i) => (i, false),
                    GridEvent::LongPressed(i) => (i, true),
                };
                if self.carrying() {
                    if !long {
                        if let Some(path) = self.folders.get(i).cloned() {
                            self.navigate(Some(path), cx);
                        } else {
                            cx.toast("Paste or cancel first", None);
                        }
                    }
                    self.apply_pending(cx);
                    return Action::None;
                }
                if self.selection.is_some() {
                    if !long {
                        self.toggle_selection(i);
                    }
                    self.apply_pending(cx);
                    return Action::None;
                }
                if let Some(path) = self.folders.get(i).cloned() {
                    if long {
                        self.open_folder_menu(path, g.pos(), cx.theme);
                    } else {
                        self.navigate(Some(path), cx);
                    }
                    self.apply_pending(cx);
                    return Action::None;
                }
                selected = self.grid_book(i).map(|b| (b.id.clone(), long));
            }
            if let Some(event) = self.continue_press.on_gesture(&g, self.continue_rect)
                && let Some(id) = &self.continue_id
            {
                match event {
                    PressEvent::Tap => selected = Some((id.clone(), false)),
                    PressEvent::LongPress => selected = Some((id.clone(), true)),
                    _ => {}
                }
            }
            if let Some((id, long)) = selected {
                if long {
                    self.open_menu(id, g.pos(), cx.theme);
                } else {
                    self.open(id, cx);
                }
            }
        }
        self.apply_pending(cx);
        Action::None
    }
    fn begin_batch(&mut self) {
        if let Some(reader) = &mut self.reader {
            reader.begin_batch();
        }
    }
    fn finish_batch(&mut self, cx: &mut EventCx) {
        if let Some(reader) = &mut self.reader {
            reader.finish_batch(cx);
        }
    }
    fn on_text_input(&mut self, input: &TextInput, cx: &mut EventCx) -> bool {
        if let Some(reader) = &mut self.reader {
            return reader.on_text_input(input, cx);
        }
        if let Some(sheet) = &mut self.name_sheet {
            let event = sheet.edit(input);
            if matches!(event, Some(NameEvent::Submit)) {
                self.submit_name(cx);
            }
            cx.redraw();
            return true;
        }
        if self.confirm.is_some() || self.carrying() {
            return true;
        }
        if self.search.is_none() {
            return false;
        }
        // Entry overlays retain ownership until their touch is released.
        if self.menu.is_some() || self.details.is_some() || self.jump.is_some() || self.touching {
            return true;
        }
        if !self.search.as_ref().is_some_and(|s| s.field.focused) {
            return false;
        }
        self.edit_search(input, cx);
        self.apply_pending(cx);
        true
    }
    /// Scripted screenshots also wait for a running update check or search debounce.
    fn busy(&self) -> bool {
        self.reader.as_ref().map_or(
            self.working || self.updates.checking() || self.search.as_ref().is_some_and(|s| s.debounce_at.is_some()),
            Screen::busy,
        )
    }
    fn has_pending_work(&self) -> bool {
        self.reader.is_none() && self.confirm.as_ref().is_some_and(|c| c.working)
    }
    fn run_pending_work(&mut self, cx: &mut EventCx) -> Action {
        if let Some(confirm) = self.confirm.take_if(|c| c.working) {
            self.delete_target(confirm.target, cx);
        }
        Action::None
    }
    fn deadline(&self) -> Option<std::time::Instant> {
        // A failed navigation write is reported at the next tick.
        if self.navigation_error.is_some() {
            return Some(std::time::Instant::now());
        }
        if let Some(reader) = &self.reader {
            return reader.deadline();
        }
        [self.search.as_ref().and_then(|s| s.debounce_at), self.prelayout_at(), self.warm_at()]
            .into_iter()
            .flatten()
            .min()
    }
    fn has_idle_work(&self) -> bool {
        self.reader.as_ref().is_some_and(Screen::has_idle_work)
    }
    fn on_idle(&mut self, text: &mut crate::gfx::TextRenderer) -> bool {
        self.reader.as_mut().is_some_and(|r| r.on_idle(text))
    }
    fn on_tick(&mut self, cx: &mut EventCx) -> Action {
        if let Some(error) = self.navigation_error.take() {
            cx.toast(error, None);
        }
        if let Some(reader) = &mut self.reader {
            return reader.on_tick(cx);
        }
        if self.search.as_ref().and_then(|s| s.debounce_at).is_some_and(|at| cx.now >= at) {
            self.apply_search(cx);
        }
        if self.warm_at().is_some_and(|at| cx.now >= at) {
            self.warm_library(cx.theme);
        }
        if self.prelayout_at().is_some_and(|at| cx.now >= at) {
            self.start_prelayout(cx);
        }
        Action::None
    }
    fn on_background(&mut self, cx: &mut EventCx) {
        if let Some(e) = self.navigation_writer.errors().last() {
            self.navigation_failed(e);
        }
        self.apply_folder_changes(cx);
        if let Some((mut home, revision)) = self.library.take_home() {
            // Reading records can change after the worker assembled this summary.
            for entry in &mut home.books {
                entry.status = (&self.store.book(&entry.book.id)).into();
            }
            self.pending_home = Some(home);
            self.pending_home_revision = revision;
            self.worker_home_revision = revision;
            self.prune_changes();
        }
        if self.library.take_opened_changed() {
            self.previews.forget_order();
            self.opened_touched = true;
            if self.browsing_ready && !self.is_home() && self.reader.is_none() {
                self.prioritize();
            }
        }
        if let Some(warm) = self.library.take_warm() {
            self.apply_warm(warm, cx.theme);
        }
        let repaired = self.library.take_repaired();
        if !repaired.is_empty() {
            // Pixels change in place; no item moves beneath a touch or overlay.
            self.thumbnails.repaired(&repaired);
            if self.reader.is_none() {
                cx.redraw();
            }
        }
        self.apply_pending(cx);
        if let Some(r) = &mut self.reader {
            r.on_background(cx);
        }
        // Status only redraws the status line; it never touches the index.
        if let Some(status) = self.library.take_status() {
            if self.reader.is_none() {
                cx.redraw();
            }
            let was_scanning = self.scanning;
            self.scanning = status.scanning;
            self.working = status.working;
            for error in &status.errors {
                if !self.errors.contains(error) {
                    log::warn!("{error}");
                    cx.toast(error.clone(), None);
                }
            }
            if was_scanning && !status.scanning && self.manual_rescan {
                self.manual_rescan = false;
                if status.errors.is_empty() {
                    cx.toast("Library updated", None);
                }
            }
            self.errors = status.errors;
        }
        if let Some(snapshot) = self.library.take() {
            self.receive(snapshot, cx);
        }
        for reply in self.worker.drain() {
            match reply {
                Reply::Fonts(fonts) => {
                    self.fonts = fonts.clone();
                    if let Some(r) = &mut self.reader {
                        r.fonts = fonts;
                    }
                }
                Reply::Error { generation: 0, message } => {
                    self.engine_error = Some(message.clone());
                    cx.toast(message, None);
                    if self.reader.is_some() {
                        self.worker.send(Command::Close);
                        self.returned(cx);
                    }
                }
                reply => {
                    if let Some(r) = &mut self.reader {
                        r.receive(reply, cx);
                        if r.failed_open() {
                            self.worker.send(Command::Close);
                            self.returned(cx);
                        }
                    }
                }
            }
        }
    }
    fn after_present(&mut self) {
        self.persist_navigation();
        if let Some(r) = &mut self.reader {
            r.after_present();
        }
    }
    fn on_sleep(&mut self) {
        self.flush_navigation();
        self.touching = false;
        // Nothing runs while asleep; it starts over after waking.
        self.worker.send(Command::StopPrelayout);
        self.prelayout_due = true;
        if let Some(r) = &mut self.reader {
            r.on_sleep();
        }
    }
    fn on_wake(&mut self, now: std::time::Instant) {
        // Waking is usually for reading: wait for the idle delay again.
        self.last_input = now;
    }
    fn sleep_cover(&self, width: u32, height: u32) -> Option<image::GrayImage> {
        let book = if let Some(reader) = &self.reader {
            if self.browsing_ready {
                self.index.books.iter().find(|b| b.id == reader.book_id() && b.available)?
            } else {
                self.book(reader.book_id())?
            }
        } else if self.is_home() {
            &self
                .home
                .books
                .iter()
                .filter(|e| e.book.available && e.status.last_opened > 0)
                .max_by_key(|e| e.status.last_opened)?
                .book
        } else {
            // Only shown books have cached states. The Home summary holds the
            // most recently opened books, so together they find the newest.
            let summary: HashMap<_, _> =
                self.home.books.iter().map(|e| (e.book.id.as_str(), e.status.last_opened)).collect();
            let opened = |b: &Book| {
                let cached = self.states.get(&b.id).map_or(0, |s| s.last_opened);
                cached.max(summary.get(b.id.as_str()).copied().unwrap_or(0))
            };
            self.index.books.iter().filter(|b| b.available && opened(b) > 0).max_by_key(|b| opened(b))?
        };
        let mut book = book.clone();
        if book.metadata.cover.is_none() {
            book.metadata = library::epub::read(&book.path).ok()?;
        }
        match covers::sleep_cover(&book, width, height, 0) {
            Ok(image) => Some(image),
            Err(e) => {
                log::debug!("sleep cover unavailable: {e:#}");
                None
            }
        }
    }
    fn on_quit(&mut self) -> anyhow::Result<()> {
        self.persist_paste();
        // A failed navigation save is shown but doesn't keep the app open.
        self.flush_navigation();
        if let Some(r) = &mut self.reader {
            r.on_quit()?;
        }
        Ok(())
    }
}

/// A sheet of paged plain text, e.g. book details or a full folder path.
pub(crate) struct Details {
    sheet: Sheet,
    pager: Pager,
    pages: Vec<String>,
    content: String,
    text: Rect,
}
impl Details {
    fn new(t: &Theme, screen: Rect, book: &Book, state: &BookState) -> Self {
        let content = format!(
            "{}\n\nAuthor: {}\n\nLanguage: {}\n\nStatus: {}{}\n\nFile: {}{}",
            book.metadata.title,
            if book.metadata.authors.is_empty() { "Unknown" } else { &book.metadata.authors },
            if book.metadata.language.is_empty() { "Unknown" } else { &book.metadata.language },
            status(state),
            if state.hidden_from_continue { " (removed from continue reading)" } else { "" },
            book.path.display(),
            book.error.as_ref().map(|e| format!("\n\nMetadata error: {e}")).unwrap_or_default()
        );
        Self::from_text(t, screen, "Book details", content)
    }
    pub(crate) fn from_text(t: &Theme, screen: Rect, title: &str, content: String) -> Self {
        let sheet = Sheet::open(title, screen.h * 3 / 4, screen, t);
        let area = sheet.content(t);
        let mut pager = Pager::new(1);
        pager.place(Rect::new(area.x, area.bottom() - t.mm(11.0), area.w, t.mm(11.0)), t);
        let text = Rect { h: area.h - t.mm(13.0), ..area };
        Self { sheet, pager, pages: Vec::new(), content, text }
    }
    pub(crate) fn draw(&mut self, cx: &mut DrawCx) {
        if self.pages.is_empty() {
            self.pages = detail_pages(cx.text, &self.content, cx.theme.body, self.text);
            self.pager.set(0, self.pages.len());
        }
        self.sheet.draw(cx);
        if let Some(page) = self.pages.get(self.pager.page) {
            let height = cx.text.measure(page, cx.theme.body, Some(self.text.w as f32)).1.ceil() as i32;
            cx.paragraph(page, cx.theme.body, Rect { h: height.min(self.text.h), ..self.text }, HAlign::Left);
        }
        self.pager.draw(cx);
    }
    /// True when the sheet should close.
    pub(crate) fn on_gesture(&mut self, g: &Gesture, cx: &mut EventCx) -> bool {
        if self.sheet.on_gesture(g) {
            return true;
        }
        let page = self.pager.page;
        self.pager.on_gesture(g);
        if let Gesture::Swipe { dir, .. } = g {
            self.pager.on_swipe(*dir);
        }
        if page != self.pager.page {
            cx.refresh(self.sheet.rect(), RefreshMode::Full);
        }
        false
    }
}

/// What a library delete confirmation removes.
enum DeleteRequest {
    Item(Target),
    Selection(Vec<Target>),
}

/// "Delete book?" with Cancel and Delete. Nothing is removed until Delete.
/// Also asks before deleting a dictionary set.
pub(crate) struct ConfirmDelete<T> {
    pub(crate) target: T,
    sheet: Sheet,
    message: String,
    text: Rect,
    pub(crate) cancel: Button,
    pub(crate) delete: Button,
    /// Delete was chosen; the deletion runs after this frame is shown.
    working: bool,
}
impl<T> ConfirmDelete<T> {
    pub(crate) fn new(t: &Theme, screen: Rect, target: T, title: &str, message: String) -> Self {
        let sheet = Sheet::open(title, t.mm(62.0).min(screen.h - t.top_bar), screen, t);
        let c = sheet.content(t);
        let buttons_y = c.bottom() - t.margin - t.button_h;
        let text = Rect { h: buttons_y - c.y - t.mm(3.0), ..c };
        let w = (c.w - t.mm(3.0)) / 2;
        let mut cancel = Button::new("Cancel", ButtonKind::Secondary);
        cancel.place(Rect::new(c.x, buttons_y, w, t.button_h), t);
        let mut delete = Button::new("Delete", ButtonKind::Primary).with_icon(Icon::Trash);
        delete.place(Rect::new(c.right() - w, buttons_y, w, t.button_h), t);
        Self { target, sheet, message, text, cancel, delete, working: false }
    }
    pub(crate) fn draw(&self, cx: &mut DrawCx) {
        self.sheet.draw(cx);
        let height = cx.text.measure(&self.message, cx.theme.body, Some(self.text.w as f32)).1.ceil() as i32;
        cx.paragraph(&self.message, cx.theme.body, Rect { h: height.min(self.text.h), ..self.text }, HAlign::Left);
        self.cancel.draw(cx);
        self.delete.draw(cx);
    }
    /// Some(true) to delete, Some(false) to close without deleting.
    pub(crate) fn on_gesture(&mut self, g: &Gesture) -> Option<bool> {
        if self.working {
            return None;
        }
        if self.sheet.on_gesture(g) || self.cancel.on_gesture(g) {
            return Some(false);
        }
        self.delete.on_gesture(g).then_some(true)
    }
}
impl ConfirmDelete<DeleteRequest> {
    fn start_working(&mut self) {
        let count = match &self.target {
            DeleteRequest::Selection(targets) => targets.len(),
            DeleteRequest::Item(_) => 1,
        };
        self.message = format!("Deleting {}…", counted(count, "item"));
        self.cancel.enabled = false;
        self.delete.enabled = false;
        self.working = true;
    }
}

/// "Go to page": a slider over the browsing pages. Releasing it jumps there.
struct PageJump {
    sheet: Sheet,
    slider: Slider,
    label: Rect,
    start: Rect,
}
enum JumpEvent {
    Close,
    Go(usize),
}
impl PageJump {
    fn new(t: &Theme, screen: Rect, page: usize, pages: usize) -> Self {
        let sheet = Sheet::open("Go to page", t.mm(42.0), screen, t);
        let c = sheet.content(t);
        let label = Rect::new(c.x, c.y + t.mm(1.0), c.w, t.mm(8.0));
        let start = Rect::new(c.x, label.bottom(), c.w, t.mm(6.0));
        let mut slider = Slider::new(0.0, pages.saturating_sub(1) as f32, 1.0, page as f32);
        slider.place(Rect::new(c.x, start.bottom() + t.mm(5.0), c.w, t.mm(6.0)), t);
        Self { sheet, slider, label, start }
    }
    fn page(&self) -> usize {
        self.slider.value as usize
    }
    fn draw(&self, cx: &mut DrawCx, pages: usize, start: &str) {
        let t = cx.theme;
        self.sheet.draw(cx);
        cx.line(&format!("Page {} of {pages}", self.page() + 1), t.title, self.label, HAlign::Left);
        if !start.is_empty() {
            cx.line(&format!("Starts with {start}"), t.body.gray(INK2), self.start, HAlign::Left);
        }
        self.slider.draw(cx);
    }
    fn on_gesture(&mut self, g: &Gesture) -> Option<JumpEvent> {
        if self.sheet.on_gesture(g) {
            return Some(JumpEvent::Close);
        }
        match self.slider.on_gesture(g) {
            Some(SliderEvent::Released(v)) => Some(JumpEvent::Go(v as usize)),
            _ => None,
        }
    }
}

fn detail_pages(
    renderer: &mut crate::gfx::TextRenderer,
    content: &str,
    style: crate::gfx::TextStyle,
    area: Rect,
) -> Vec<String> {
    let mut pages = Vec::new();
    let mut rest = content;
    while !rest.is_empty() {
        let boundaries: Vec<_> =
            rest.char_indices().map(|(i, _)| i).take(4096).chain(std::iter::once(rest.len())).collect();
        let mut low = 1;
        let mut high = boundaries.len() - 1;
        while low < high {
            let mid = (low + high).div_ceil(2);
            if renderer.measure(&rest[..boundaries[mid]], style, Some(area.w as f32)).1 <= area.h as f32 {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        let end = boundaries[low];
        pages.push(rest[..end].to_owned());
        rest = &rest[end..];
    }
    pages
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::selection_tests::book;
    fn browsing_books(t: &Theme, screen: Rect, worker: Rc<Worker>, store: Store) -> Books {
        // The stubbed library worker never scans or saves, so synthetic
        // fixtures cannot be rewritten mid-test by its startup pass.
        let mut books = Books::with_library(t, screen, worker, store, Some(LibraryWorker::stub()));
        books.home_active = false;
        books.refresh(t);
        books
    }
    /// Like [`browsing_books`], but with the real background worker, for tests
    /// that wait for its saves, snapshots or status replies.
    fn browsing_books_live(t: &Theme, screen: Rect, worker: Rc<Worker>, store: Store) -> Books {
        let mut books = Books::with_store(t, screen, worker, store);
        books.home_active = false;
        books.refresh(t);
        books
    }
    pub(super) fn with_cx(t: &Theme, screen: Rect, f: impl FnOnce(&mut EventCx)) {
        f(&mut EventCx {
            theme: t,
            screen,
            now: std::time::Instant::now(),
            frontlight: &mut None,
            light_request: &mut None,
            toast: &mut None,
            forced: &mut Vec::new(),
            needs_redraw: &mut false,
            battery: None,
        });
    }
    pub(super) fn tap(books: &mut Books, p: Point, cx: &mut EventCx) {
        books.on_gesture(Gesture::Down(p), cx);
        books.on_gesture(Gesture::Tap(p), cx);
    }
    fn collection_fixture(label: &str) -> anyhow::Result<(Store, PathBuf)> {
        let root = std::env::temp_dir().join(format!(
            "verso-collections-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let store = Store::at(root.clone());
        let mut settings = store.settings();
        settings.library_folders = Some(vec!["/books".into(), "/books/Fiction".into(), "/other/books".into()]);
        store.save_settings(&settings)?;
        let mut books: Vec<_> = (0..20)
            .map(|i| {
                let mut b = book(i);
                b.path = format!("/books/Fiction/Nested/{i}.epub").into();
                b
            })
            .collect();
        let mut loose = book(20);
        loose.path = "/books/loose.epub".into();
        books.push(loose);
        let mut other = book(21);
        other.path = "/other/books/Nested/other.epub".into();
        books.push(other);
        store.save_library(&LibraryIndex { books, ..Default::default() })?;
        Ok((store, root))
    }
    #[test]
    fn selection_across_pages_and_ending_rules() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("selection")?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = browsing_books(&t, screen, Rc::new(Worker::stub()), store);
        with_cx(&t, screen, |cx| {
            books.navigate(Some("/books/Fiction/Nested".into()), cx);
            let first = Target::Book(books.books[0].id.clone());
            books.open_menu(books.books[0].id.clone(), Point::new(190, 650), &t);
            let menu = &books.menu.as_ref().unwrap().1;
            let row = menu.items.iter().position(|(_, label)| label == "Select").unwrap();
            let p = Point::new(menu.rect().center().x, menu.rect().y + t.mm(1.0) + row as i32 * t.row + t.row / 2);
            tap(&mut books, p, cx);
            assert!(books.selection.as_ref().unwrap().items.contains(&first));
            let all = books.selection.as_ref().unwrap().all.rect().center();
            tap(&mut books, all, cx);
            assert_eq!(books.selection.as_ref().unwrap().items.len(), 20);
            books.go_to_page(1, cx);
            assert_eq!(books.selection.as_ref().unwrap().items.len(), 20);
            let p = books.grid.cells()[0].1.center();
            tap(&mut books, p, cx);
            assert_eq!(books.selection.as_ref().unwrap().items.len(), 19);
            books.on_gesture(Gesture::Down(p), cx);
            books.on_gesture(Gesture::LongPress(p), cx);
            books.on_gesture(Gesture::End(p), cx);
            assert!(books.menu.is_none() && books.reader.is_none());
            assert_eq!(books.selection.as_ref().unwrap().items.len(), 19);
            books.navigate(None, cx);
            assert!(books.selection.is_none());
            books.start_selection(Target::Folder("/books".into()), cx);
            assert!(books.selection.is_none(), "library roots cannot be selected");
            books.navigate(Some("/books".into()), cx);
            books.start_selection(Target::Book("20".into()), cx);
            let p = books.back.rect().center();
            tap(&mut books, p, cx);
            assert!(books.selection.is_none());
            assert_eq!(books.navigation.folder, Some("/books".into()), "Back cancels before navigating");
            books.start_selection(Target::Book("20".into()), cx);
            books.navigate(Some("/books/Fiction/Nested".into()), cx);
            assert!(books.selection.is_none());
            books.start_selection(Target::Book("0".into()), cx);
            books.toggle_search(cx);
            assert!(books.selection.is_none());
            books.on_text_input(&TextInput::Submit, cx);
            books.start_selection(Target::Book("0".into()), cx);
            books.edit_search(&TextInput::Insert("Title".into()), cx);
            assert!(books.selection.is_none(), "query changes end selection");
            books.start_selection(Target::Book("0".into()), cx);
            let p = books.search.as_ref().unwrap().current.rect().center();
            tap(&mut books, p, cx);
            assert!(books.selection.is_none(), "scope changes end selection");
            books.start_selection(Target::Book("0".into()), cx);
            let all = books.selection.as_ref().unwrap().all.rect().center();
            tap(&mut books, all, cx);
            assert_eq!(books.selection.as_ref().unwrap().items.len(), 20, "Select all uses the whole scoped result");
            books.toggle_search(cx);
            books.start_selection(Target::Book("0".into()), cx);
            let p = books.selection.as_ref().unwrap().cancel.rect().center();
            tap(&mut books, p, cx);
            assert!(books.selection.is_none());
            books.home_active = true;
            books.refresh(&t);
            books.start_selection(Target::Book("0".into()), cx);
            assert!(books.selection.is_none(), "Home has no selection");
            // A folder holding a library folder can be neither deleted nor
            // moved, so it is never selectable, by menu, tap or Select all.
            books.home_active = false;
            books.navigate(Some("/books/Fiction".into()), cx);
            let nested = PathBuf::from("/books/Fiction/Nested");
            assert!(books.folders.contains(&nested));
            books.roots.push(nested.join("Deep"));
            books.open_folder_menu(nested.clone(), Point::new(190, 650), &t);
            let menu = &books.menu.as_ref().unwrap().1;
            let row = menu.items.iter().position(|(_, label)| label == "Select").unwrap();
            let p = Point::new(menu.rect().center().x, menu.rect().y + t.mm(1.0) + row as i32 * t.row + t.row / 2);
            tap(&mut books, p, cx);
            assert!(books.menu.is_some() && books.selection.is_none(), "Select is disabled");
            books.menu = None;
            books.start_selection(Target::Folder(nested.clone()), cx);
            assert!(books.selection.is_none());
            books.roots.pop();
        });
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn scans_end_selection_only_when_they_move_the_folder() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("selection-scan")?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = browsing_books(&t, screen, Rc::new(Worker::stub()), store.clone());
        with_cx(&t, screen, |cx| {
            books.navigate(Some("/books/Fiction/Nested".into()), cx);
            books.start_selection(Target::Book("0".into()), cx);
            books.toggle_selection(1);
            assert_eq!(books.selection.as_ref().unwrap().items.len(), 2);
            // A scan that drops one selected book keeps the rest selected.
            let mut index = store.library();
            index.books.retain(|b| b.id != "0");
            books.pending = Some(Snapshot { index: index.clone(), ..Default::default() });
            books.apply_pending(cx);
            assert_eq!(books.navigation.folder, Some("/books/Fiction/Nested".into()));
            assert_eq!(books.selection.as_ref().unwrap().items.len(), 1);
            // A scan that removes the folder moves to the nearest shown ancestor
            // (the emptied Fiction root is hidden too) and ends selection.
            index.books.retain(|b| !b.path.starts_with("/books/Fiction/Nested"));
            books.pending = Some(Snapshot { index, ..Default::default() });
            books.apply_pending(cx);
            assert_eq!(books.navigation.folder, Some("/books".into()));
            assert!(books.selection.is_none());
        });
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn selection_ticks_refresh_fast_without_covering_the_cover() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("selection-hints")?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = browsing_books(&t, screen, Rc::new(Worker::stub()), store);
        let mut canvas = crate::gfx::Canvas::new(1272, 1696);
        let mut text = crate::gfx::TextRenderer::new();
        let mut icons = crate::gfx::IconRenderer::default();
        books.navigation.folder = Some("/books/Fiction/Nested".into());
        books.refresh(&t);
        let cell = books.grid.cells()[0].1;
        let cover = books.cover_rect(cell, &t).center();
        let tick = Point::new(cell.right() - t.mm(3.0), cell.y + t.mm(3.0));
        let mut draw = |books: &mut Books| {
            let mut hints = Vec::new();
            books.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut hints));
            let mode = |p: Point| hints.iter().filter(|h| h.rect.contains(p)).map(|h| h.mode).max();
            (mode(cover), mode(tick))
        };
        assert_eq!(draw(&mut books).0, Some(RefreshMode::Ui));
        with_cx(&t, screen, |cx| books.start_selection(Target::Book(books.books[0].id.clone()), cx));
        // A cover that loads while selecting gets the default grayscale mode.
        assert_eq!(draw(&mut books), (None, Some(RefreshMode::Fast)));
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn selection_delete_messages_name_what_is_removed() {
        let message = |books, folders, other_files, has_folders| {
            selection::selection_delete_message(&files::Contents { books, folders, other_files }, has_folders)
        };
        assert_eq!(message(1, 0, 0, false), "1 book and its reading progress will be removed from this device.");
        assert_eq!(message(3, 0, 0, false), "3 books and their reading progress will be removed from this device.");
        assert_eq!(
            message(0, 2, 0, true),
            "The selection and everything in its folders will be removed from this device: 2 folders."
        );
        assert_eq!(
            message(1, 1, 3, true),
            "The selection and everything in its folders will be removed from this device: \
             1 book and its reading progress, 1 folder, 3 other files."
        );
    }

    #[test]
    fn mixed_selection_delete_confirms_once_and_batches_updates() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("selection-delete")?;
        let library = root.join("Library");
        let folder = library.join("Folder");
        std::fs::create_dir_all(folder.join("Empty"))?;
        std::fs::write(folder.join("nested.epub"), b"book")?;
        std::fs::write(folder.join("paper.pdf"), b"other")?;
        std::fs::write(library.join("loose.epub"), b"book")?;
        let mut nested = book(0);
        nested.path = folder.join("nested.epub");
        let mut loose = book(1);
        loose.path = library.join("loose.epub");
        store.save_library(&LibraryIndex {
            books: vec![nested, loose],
            empty_folders: vec![folder.join("Empty")],
            ..Default::default()
        })?;
        let mut settings = store.settings();
        settings.library_folders = Some(vec![library.clone()]);
        store.save_settings(&settings)?;
        for id in ["0", "1"] {
            store.save_book(id, &BookState { progress: 3500, ..Default::default() })?;
        }
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = browsing_books(&t, screen, Rc::new(Worker::stub()), store.clone());
        with_cx(&t, screen, |cx| {
            books.navigate(Some(library.clone()), cx);
            books.start_selection(Target::Folder(folder.clone()), cx);
            let all = books.selection.as_ref().unwrap().all.rect().center();
            tap(&mut books, all, cx);
            let delete = books.selection.as_ref().unwrap().delete.rect().center();
            tap(&mut books, delete, cx);
            let confirm = books.confirm.as_ref().unwrap();
            assert_eq!(
                confirm.message,
                "The selection and everything in its folders will be removed from this device: \
                 2 books and their reading progress, 2 folders, 1 other file."
            );
            let cancel = confirm.cancel.rect().center();
            tap(&mut books, cancel, cx);
            assert!(folder.exists() && books.selection.is_some());
            tap(&mut books, delete, cx);
            let delete = books.confirm.as_ref().unwrap().delete.rect().center();
            tap(&mut books, delete, cx);
            // The sheet shows progress first; deleting waits for that frame.
            let confirm = books.confirm.as_ref().unwrap();
            assert!(confirm.working && confirm.message == "Deleting 2 items…");
            assert!(books.has_pending_work() && folder.exists());
            let cancel = confirm.cancel.rect().center();
            tap(&mut books, cancel, cx);
            assert!(books.confirm.is_some(), "the sheet ignores touches while deleting");
            books.run_pending_work(cx);
            assert!(!books.has_pending_work());
            assert!(books.selection.is_none() && books.confirm.is_none());
            assert!(!folder.exists() && !library.join("loose.epub").exists());
            assert!(books.index.books.is_empty());
            assert_eq!(store.book("0").progress, 0);
            assert_eq!(store.book("1").progress, 0);
            assert!(books.index.empty_folders.contains(&library));
            let stale = store.library();
            books.pending = Some(Snapshot { index: stale, ..Default::default() });
            books.apply_pending(cx);
            assert!(books.index.books.is_empty(), "older snapshots cannot restore a batch delete");
            let blocked = library.join("blocked.epub");
            std::fs::create_dir(&blocked).unwrap();
            let mut survivor = book(2);
            survivor.path = blocked;
            books.index.books.push(survivor);
            books.collections = Arc::new(Collections::from_index(&books.index, &books.roots));
            store.save_book("2", &BookState { progress: 1700, ..Default::default() }).unwrap();
            books.refresh(&t);
            books.start_selection(Target::Book("2".into()), cx);
            books.delete_selection(vec![Target::Book("2".into())], cx);
            assert!(books.details.as_ref().unwrap().content.starts_with("Deleted 0 of 1:"));
            assert_eq!(store.book("2").progress, 1700);
            assert!(books.index.books.iter().any(|b| b.id == "2"));
        });
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn home_is_summary_only_and_defers_updates_under_touch_and_details() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("home-lazy")?;
        let index = store.library();
        for (i, b) in index.books.iter().take(6).enumerate() {
            store.save_book(
                &b.id,
                &BookState {
                    position: crate::document::Position("exact".into()),
                    last_opened: i as u64 + 1,
                    progress: 4321,
                    ..Default::default()
                },
            )?;
        }
        store.rebuild_home_summary(&index)?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = Books::with_store(&t, screen, Rc::new(Worker::stub()), store.clone());
        assert!(books.is_home());
        assert!(!books.browsing_ready);
        assert!(books.index.books.is_empty() && books.states.is_empty() && books.collections.folders.is_empty());
        assert_eq!(books.continue_id.as_deref(), Some("5"));
        assert_eq!(books.books.len(), 4);
        with_cx(&t, screen, |cx| {
            let point = books.continue_rect.center();
            books.on_gesture(Gesture::Down(point), cx);
            books.pending_home = Some(Summary::default());
            books.pending = Some(Snapshot { index: index.clone(), ..Default::default() });
            books.apply_pending(cx);
            assert_eq!(books.continue_id.as_deref(), Some("5"));
            books.on_gesture(Gesture::LongPress(point), cx);
            books.on_gesture(Gesture::End(point), cx);
            assert!(books.menu.is_some());
            books.apply_pending(cx);
            assert!(books.pending_home.is_some());
            books.menu = None;
            books.details = Some(Details::from_text(&t, screen, "Book", "details".into()));
            books.apply_pending(cx);
            assert!(books.pending_home.is_some());
            books.details = None;
            books.apply_pending(cx);
            assert!(books.books.is_empty() && books.continue_id.is_none());
            assert!(books.pending.is_some());
            assert!(!books.browsing_ready && books.states.is_empty());
            // Search from Home prepares the index once, then closes back to Home.
            books.toggle_search(cx);
            assert!(books.browsing_ready);
            assert_eq!(books.search.as_ref().unwrap().scope, Scope::All);
            assert!(!books.search.as_ref().unwrap().current.enabled);
            books.toggle_search(cx);
            assert!(books.is_home());
        });
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn browsing_takes_the_worker_hierarchy_unless_folders_or_edits_changed() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("prebuilt")?;
        let index = store.library();
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = Books::with_store(&t, screen, Rc::new(Worker::stub()), store.clone());
        // An empty marker: taken as it is, while a tree built here has folders.
        let tree = Arc::new(Collections::default());
        let snapshot = |roots: Vec<PathBuf>, cached: bool| Snapshot {
            index: index.clone(),
            collections: Some((roots, tree.clone())),
            cached,
            ..Default::default()
        };
        books.pending = Some(snapshot(books.roots.clone(), true));
        books.prepare_browsing();
        assert!(Arc::ptr_eq(&books.collections, &tree) && books.index == index);
        books.home_active = false;
        with_cx(&t, screen, |cx| {
            // Browsing has an index now; the unchanged copy is dropped.
            books.receive(snapshot(books.roots.clone(), true), cx);
            assert!(books.pending.is_none());
            // A tree built for another folder list is rebuilt for this one.
            books.receive(snapshot(vec!["/elsewhere".into()], false), cx);
            assert!(!Arc::ptr_eq(&books.collections, &tree) && !books.collections.folders.is_empty());
            books.receive(snapshot(books.roots.clone(), false), cx);
            assert!(Arc::ptr_eq(&books.collections, &tree));
            // So is one under edits made since its index was published.
            let removed = index.books[0].path.clone();
            books.changes.push((1, PathChange::Remove(removed.clone())));
            books.receive(snapshot(books.roots.clone(), false), cx);
            assert!(!Arc::ptr_eq(&books.collections, &tree));
            assert!(!books.index.books.iter().any(|b| b.path == removed));
            let folder = removed.parent().unwrap();
            assert!(books.collections.folders.get(folder).is_none_or(|f| !f.books.contains(&removed)));
        });
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn home_reader_uses_authoritative_position_and_returns_to_home() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("home-return")?;
        let mut index = store.library();
        index.books[0].metadata.language = "ru".into();
        store.save_library(&index)?;
        let book = index.books[0].clone();
        let state = BookState {
            position: crate::document::Position("authoritative-anchor".into()),
            last_opened: 7,
            progress: 1000,
            ..Default::default()
        };
        store.select_home_book(&book);
        store.save_book(&book.id, &state)?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = Books::with_store(&t, screen, Rc::new(Worker::stub()), store.clone());
        assert!(
            books.home.books.iter().any(|entry| entry.book.id == book.id && entry.book.metadata.language.is_empty())
        );
        assert_eq!(books.reading_book(&book.id).unwrap().metadata.language, "ru");
        with_cx(&t, screen, |cx| {
            let point = Point::new(t.margin + 10, books.continue_rect.y + 10);
            tap(&mut books, point, cx);
            assert!(books.reader.is_some());
            // Reader-owned overlays can push management through the outer
            // library screen, while retaining this reader and its query.
            books.reader.as_mut().unwrap().lookup("Hello", cx);
            let manage = Point::new(screen.w / 2, screen.h - t.mm(20.0));
            books.on_gesture(Gesture::Down(manage), cx);
            assert!(matches!(books.on_gesture(Gesture::Tap(manage), cx), Action::Push(_)));
            assert!(books.reader.is_some());
            books.on_background(cx);
            assert!(!books.browsing_ready && books.index.books.is_empty());
            let mut updated = state.clone();
            updated.progress = 6789;
            store.save_book(&book.id, &updated).unwrap();
            books.returned(cx);
            assert!(books.is_home());
            assert_eq!(books.home.resume().unwrap().status.progress, 6789);
            assert_eq!(books.states.get(&book.id).unwrap().progress, 6789);
            assert!(!books.browsing_ready);
            assert_eq!(store.book(&book.id).position.0, "authoritative-anchor");
            books.home_active = false;
            books.navigation.folder = Some("/books/Fiction/Nested".into());
            books.refresh(&t);
            assert_eq!(books.states.get(&book.id).unwrap().progress, 6789);
        });
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn failed_open_from_home_returns_home_and_leaves_history_unchanged() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("home-failed-open")?;
        let index = store.library();
        for (i, b) in index.books.iter().take(3).enumerate() {
            store.save_book(
                &b.id,
                &BookState {
                    position: crate::document::Position("exact".into()),
                    last_opened: i as u64 + 1,
                    ..Default::default()
                },
            )?;
        }
        store.rebuild_home_summary(&index)?;
        // A real library worker may resolve covers while the open fails.
        // Compare persisted history, which cover repair must not change.
        let history = || -> anyhow::Result<Vec<_>> {
            let summary: Summary = serde_json::from_slice(&std::fs::read(root.join("home.json"))?)?;
            Ok(summary.books.into_iter().map(|entry| (entry.book.id, entry.status)).collect())
        };
        let before = history()?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = Books::with_store(&t, screen, Rc::new(Worker::stub()), store.clone());
        assert_eq!(books.continue_id.as_deref(), Some("2"));
        with_cx(&t, screen, |cx| {
            // Open the oldest recent book, then fail before any page is shown.
            books.open("0".into(), cx);
            let reader = books.reader.as_mut().unwrap();
            reader.receive(Reply::Error { generation: books.generation, message: "fixture open failure".into() }, cx);
            assert!(reader.failed_open());
            books.returned(cx);
            assert!(books.is_home() && books.reader.is_none());
            assert_eq!(books.home.resume().unwrap().book.id, "2", "a failed open never becomes the resume book");
            assert!(!books.browsing_ready);
        });
        assert_eq!(history()?, before, "a failed open adds no history");
        assert_eq!(store.book("0").last_opened, 1);
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn library_has_no_resume_card_and_uses_responsive_grid_dimensions() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("library-grid")?;
        let id = store.library().books[0].id.clone();
        let state = BookState {
            position: crate::document::Position("exact".into()),
            last_opened: 5,
            progress: 1234,
            ..Default::default()
        };
        store.save_book(&id, &state)?;
        for (w, h, dpi, per_page, pages) in [
            (1272, 1696, 300, 12, 2), // Paperwhite portrait: 4 × 3
            (1696, 1272, 300, 12, 2), // Paperwhite landscape: 6 × 2
            (1072, 1448, 300, 9, 3),  // 6" 300 dpi portrait: 3 × 3
            (600, 800, 167, 9, 3),    // 6" 167 dpi portrait: 3 × 3
        ] {
            let t = Theme::for_dpi(dpi);
            let screen = Rect::new(0, 0, w, h);
            let mut books = browsing_books(&t, screen, Rc::new(Worker::stub()), store.clone());
            books.navigation.folder = Some("/books/Fiction/Nested".into());
            books.refresh(&t);
            assert!(books.continue_id.is_none(), "a resumable book stays on Home only");
            assert_eq!(books.continue_rect, Rect::default());
            assert_eq!(books.books.len(), 20);
            assert_eq!(books.grid.cells().len(), per_page, "{w}x{h}");
            assert_eq!(books.pager.pages, pages, "{w}x{h}");
            assert!(books.pager.compact);
            drop(books);
        }
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn page_jump_moves_browsing_and_search_pages_and_defers_updates() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("page-jump")?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = browsing_books(&t, screen, Rc::new(Worker::stub()), store.clone());
        books.navigation.folder = Some("/books/Fiction/Nested".into());
        books.refresh(&t);
        assert_eq!(books.pager.pages, 2);
        let counter = books.pager.rect().center();
        let last = |books: &Books| {
            let r = books.jump.as_ref().unwrap().slider.rect();
            Point::new(r.right() - 1, r.center().y)
        };
        with_cx(&t, screen, |cx| {
            // Tapping above the sheet closes it without moving.
            tap(&mut books, counter, cx);
            assert!(books.jump.is_some());
            assert_eq!(books.page_start(1), books.books[12].metadata.title);
            books.pending = Some(Snapshot { index: books.index.clone(), ..Default::default() });
            books.apply_pending(cx);
            assert!(books.pending.is_some(), "library updates wait for the sheet");
            tap(&mut books, Point::new(600, 400), cx);
            assert!(books.jump.is_none());
            assert_eq!(books.pager.page, 0);
            // Releasing the slider jumps and saves the page.
            tap(&mut books, counter, cx);
            let p = last(&books);
            tap(&mut books, p, cx);
            assert!(books.jump.is_none());
            assert_eq!((books.pager.page, books.grid.page), (1, 1));
            // Navigation goes to the writer once the frame showing it is presented.
            assert_ne!(store.navigation().page(), 1);
            books.after_present();
            books.navigation_writer.flush().unwrap();
            assert_eq!(store.navigation().page(), 1);
            // Search results keep their own page.
            books.toggle_search(cx);
            books.search.as_mut().unwrap().keyboard.visible = false;
            books.pager.page = 0;
            books.refresh(cx.theme);
            assert_eq!(books.pager.pages, 2);
            let search_counter = books.pager.rect().center();
            tap(&mut books, search_counter, cx);
            let p = last(&books);
            tap(&mut books, p, cx);
            assert_eq!(books.search.as_ref().unwrap().page, 1);
        });
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn sleep_and_quit_wait_for_navigation_and_failed_writes_show_a_toast() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("navigation-writer")?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = browsing_books(&t, screen, Rc::new(Worker::stub()), store.clone());
        books.pager.page = 1;
        books.save_navigation();
        // Sleep can come before the frame is presented; it still saves.
        books.on_sleep();
        assert_eq!(store.navigation().page(), 1);
        books.pager.page = 2;
        books.save_navigation();
        books.on_quit()?;
        assert_eq!(store.navigation().page(), 2);
        // A directory in place of the temporary file makes every write fail.
        let tmp = root.join("library-navigation.json.tmp");
        std::fs::create_dir(&tmp)?;
        books.pager.page = 0;
        books.save_navigation();
        books.after_present();
        assert!(books.navigation_error.is_none(), "presenting never waits for the write");
        with_cx(&t, screen, |cx| {
            let started = std::time::Instant::now();
            while books.navigation_error.is_none() && started.elapsed() < std::time::Duration::from_secs(5) {
                std::thread::yield_now();
                books.on_background(cx);
            }
            assert!(books.deadline().is_some());
            books.on_tick(cx);
            assert!(cx.toast.as_ref().unwrap().text.starts_with("Couldn't save library navigation"));
        });
        // Quitting retries the newest navigation and reports the failure once,
        // without refusing to exit.
        books.on_quit()?;
        assert!(books.navigation_error.take().is_some());
        assert_eq!(books.navigation_writer.errors().count(), 0);
        std::fs::remove_dir(&tmp)?;
        books.on_sleep();
        assert_eq!(store.navigation().page(), 0);
        assert!(books.navigation_error.is_none());
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn sleep_cover_follows_the_last_book_when_search_or_collections_hide_continue() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("sleep-cover")?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = browsing_books(&t, screen, Rc::new(Worker::stub()), store);
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/library3.epub");
        let b = &mut books.index.books[0];
        b.path = path.clone();
        b.metadata = library::epub::read(&path)?;
        let id = b.id.clone();
        books.set_book_state(id, BookState { last_opened: 10, ..Default::default() });
        books.continue_id = None;
        assert!(books.sleep_cover(600, 800).is_some());
        with_cx(&t, screen, |cx| books.toggle_search(cx));
        assert!(books.sleep_cover(600, 800).is_some());
        books.index.books[0].available = false;
        assert!(books.sleep_cover(600, 800).is_none());
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn folder_views_read_no_states_and_large_folders_list_saved_states_once() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("lazy-states")?;
        let mut index = store.library();
        index.books.extend((22..60).map(|i| {
            let mut b = book(i);
            b.path = format!("/books/Fiction/Nested/{i}.epub").into();
            b
        }));
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/library3.epub");
        index.books[5].path = path.clone();
        index.books[5].metadata = library::epub::read(&path)?;
        store.save_library(&index)?;
        store.save_book("40", &BookState { last_opened: 900, progress: 2500, ..Default::default() })?;
        store.save_book("5", &BookState { last_opened: 1000, ..Default::default() })?;
        store.rebuild_home_summary(&index)?;
        store.save_navigation(&Navigation::default())?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = browsing_books(&t, screen, Rc::new(Worker::stub()), store.clone());
        // The multi-root top level shows only folders: no book needs a state.
        assert!(!books.folders.is_empty() && books.books.is_empty());
        assert!(books.states.is_empty());
        // The Home summary holds the newest books, so the sleep cover still
        // finds the last one without any cached state.
        assert!(books.sleep_cover(600, 800).is_some());
        // Opening a large folder lists saved states once and keeps only saved records.
        let mut loaded = false;
        let (reads, listed) = load_states(&store, &mut HashMap::new(), &mut loaded, &books.index.books);
        assert_eq!((reads, listed, loaded), (2, true, true));
        books.navigation.folder = Some("/books/Fiction/Nested".into());
        books.refresh(&t);
        assert!(books.states_loaded);
        assert_eq!(books.states.len(), 2);
        assert_eq!(books.states["40"].progress, 2500);
        // Once listed, later refreshes read nothing.
        let (reads, listed) = load_states(&store, &mut books.states, &mut books.states_loaded, &books.index.books);
        assert_eq!((reads, listed), (0, false));
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn search_preserves_scope_query_result_page_and_underlying_navigation() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("search-return")?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = browsing_books(&t, screen, Rc::new(Worker::stub()), store.clone());
        with_cx(&t, screen, |cx| {
            books.navigate(Some("/books/Fiction".into()), cx);
            let before = books.navigation.clone();
            books.toggle_search(cx);
            assert_eq!(books.search.as_ref().unwrap().scope, Scope::All);
            assert_eq!(books.books.len(), 22);
            assert!(books.on_text_input(&TextInput::Insert("Title".into()), cx));
            assert!(books.on_text_input(&TextInput::Submit, cx));
            let current = books.search.as_ref().unwrap().current.rect().center();
            tap(&mut books, current, cx);
            assert_eq!(books.books.len(), 20, "folder search includes nested books");
            assert!(books.folders.is_empty());
            let next = books.pager.rect().split_right(t.target).0.center();
            tap(&mut books, next, cx);
            assert_eq!(books.pager.page, 1);
            books.after_present();
            let id = books.grid_book(books.grid.cells()[0].0).unwrap().id.clone();
            books.open(id.clone(), cx);
            store
                .save_book(
                    &id,
                    &BookState {
                        progress: 4321,
                        position: crate::document::Position("anchor".into()),
                        ..Default::default()
                    },
                )
                .unwrap();
            books.returned(cx);
            assert_eq!(books.pager.page, 1);
            let search = books.search.as_ref().unwrap();
            assert_eq!(search.field.value(), "Title");
            assert_eq!(search.scope, Scope::Collection("/books/Fiction".into()));
            assert_eq!(books.states[&id].progress, 4321);
            // The writer thread persists asynchronously; make the newest
            // snapshot durable so the file read below cannot race it.
            books.navigation_writer.flush().unwrap();
            assert_eq!(store.navigation(), before, "search paging never overwrites library navigation");
            books.open(id, cx);
            let reader = books.reader.as_mut().unwrap();
            reader.receive(Reply::Error { generation: books.generation, message: "fixture open failure".into() }, cx);
            assert!(reader.failed_open());
            books.returned(cx);
            assert_eq!(books.pager.page, 1, "failed opens preserve the search too");
            books.toggle_search(cx);
            assert_eq!(books.navigation, before);
            assert_eq!(books.pager.page, before.page());
            assert_eq!(books.folders, [PathBuf::from("/books/Fiction/Nested")]);
            // At multi-root top level, "This folder" is disabled.
            books.navigate(None, cx);
            books.toggle_search(cx);
            assert!(!books.search.as_ref().unwrap().current.enabled);
            books.toggle_search(cx);
            assert!(books.navigation.folder.is_none());
        });
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn search_snapshots_capture_touches_update_matches_and_never_broaden_missing_scope() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("search-scan")?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = browsing_books(&t, screen, Rc::new(Worker::stub()), store);
        with_cx(&t, screen, |cx| {
            books.navigate(Some("/books/Fiction".into()), cx);
            books.toggle_search(cx);
            books.edit_search(&TextInput::Insert("Needle".into()), cx);
            cx.now = books.deadline().expect("typing arms search debounce");
            books.on_tick(cx);
            assert!(books.books.is_empty());
            let mut index = books.index.clone();
            index.books[0].metadata.title = "NEEDLE".into();
            books.pending = Some(Snapshot { index: index.clone(), ..Default::default() });
            books.apply_pending(cx);
            assert_eq!(books.books.len(), 1, "an open keyboard does not block updates");
            books.edit_search(&TextInput::Clear, cx);
            assert_eq!(books.books.len(), 22);
            books.edit_search(&TextInput::Submit, cx);
            let current = books.search.as_ref().unwrap().current.rect().center();
            tap(&mut books, current, cx);
            books.pager.page = 2;
            books.save_navigation();
            let p = books.grid.cells()[0].1.center();
            books.on_gesture(Gesture::Down(p), cx);
            index.books.retain(|b| b.id == "0");
            books.pending = Some(Snapshot { index, ..Default::default() });
            books.apply_pending(cx);
            assert_eq!(books.books.len(), 20);
            books.on_gesture(Gesture::Cancel(p), cx);
            assert_eq!(books.books.len(), 1);
            assert_eq!(books.pager.page, 0);
            // Menus and sheets retain the old identity until dismissed.
            books.open_menu("0".into(), p, &t);
            books.pending = Some(Snapshot::default());
            books.apply_pending(cx);
            assert!(books.pending.is_some());
            books.menu = None;
            books.details = Some(Details::from_text(&t, screen, "Details", "body".into()));
            books.apply_pending(cx);
            assert!(books.pending.is_some());
            books.details = None;
            books.apply_pending(cx);
            assert!(books.books.is_empty());
            assert_eq!(books.search.as_ref().unwrap().scope, Scope::Collection("/books/Fiction".into()));
            assert!(books.navigation.folder.is_none());
            books.toggle_search(cx);
            assert!(books.navigation.folder.is_none());
        });
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn search_typing_debounces_and_flushes_on_clear_submit_or_dismiss() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("search-debounce")?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = browsing_books(&t, screen, Rc::new(Worker::stub()), store);
        books.working = false;
        with_cx(&t, screen, |cx| {
            books.toggle_search(cx);
            assert_eq!(books.books.len(), 22);
            assert!(books.deadline().is_none());
            assert!(!books.busy());

            // Rapid typing updates the field immediately but defers matching/sorting.
            let t0 = cx.now;
            books.edit_search(&TextInput::Insert("N".into()), cx);
            let d1 = books.deadline().expect("first keystroke arms debounce");
            assert_eq!(d1, t0 + SEARCH_DEBOUNCE);
            assert!(books.busy());
            assert_eq!(books.search.as_ref().unwrap().field.value(), "N");
            assert_eq!(books.books.len(), 22, "results wait for the debounce");

            cx.now = t0 + std::time::Duration::from_millis(50);
            books.edit_search(&TextInput::Insert("eedle".into()), cx);
            let d2 = books.deadline().expect("subsequent keystroke extends debounce");
            assert_eq!(d2, cx.now + SEARCH_DEBOUNCE);
            assert_eq!(books.books.len(), 22);

            // Ticking just before the deadline does not apply yet.
            cx.now = d2 - std::time::Duration::from_millis(1);
            books.on_tick(cx);
            assert!(books.busy());
            assert_eq!(books.books.len(), 22);

            // Ticking at the deadline applies the folded query once.
            cx.now = d2;
            books.on_tick(cx);
            assert!(books.deadline().is_none());
            assert!(!books.busy());
            assert!(books.books.is_empty());

            // Trailing whitespace does not change the folded query, so no debounce is armed.
            books.edit_search(&TextInput::Insert(" ".into()), cx);
            assert!(books.deadline().is_none());
            assert!(!books.busy());

            // Clear applies immediately without waiting for a debounce tick.
            books.edit_search(&TextInput::Clear, cx);
            assert!(books.deadline().is_none());
            assert!(!books.busy());
            assert_eq!(books.books.len(), 22);

            // Typing then backspacing back to the committed query cancels the pending debounce.
            books.edit_search(&TextInput::Insert("x".into()), cx);
            assert!(books.deadline().is_some());
            books.edit_search(&TextInput::Backspace, cx);
            assert!(books.deadline().is_none());
            assert!(!books.busy());

            // Submit flushes a pending debounced edit immediately.
            books.edit_search(&TextInput::Insert("Needle".into()), cx);
            assert!(books.deadline().is_some());
            books.edit_search(&TextInput::Submit, cx);
            assert!(books.deadline().is_none());
            assert!(!books.busy());
            assert!(!books.search.as_ref().unwrap().keyboard.visible);
            assert!(books.books.is_empty());

            // Dismiss also flushes a pending debounced edit immediately.
            books.search.as_mut().unwrap().keyboard.visible = true;
            books.search.as_mut().unwrap().field.focused = true;
            books.edit_search(&TextInput::Clear, cx);
            books.edit_search(&TextInput::Insert("Missing".into()), cx);
            assert!(books.deadline().is_some());
            books.edit_search(&TextInput::Dismiss, cx);
            assert!(books.deadline().is_none());
            assert!(!books.busy());
            assert!(books.books.is_empty());

            // Closing search drops any pending debounce.
            books.search.as_mut().unwrap().keyboard.visible = true;
            books.search.as_mut().unwrap().field.focused = true;
            books.edit_search(&TextInput::Insert("!".into()), cx);
            assert!(books.deadline().is_some());
            books.toggle_search(cx);
            assert!(books.deadline().is_none());
            assert!(!books.busy());
        });
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn book_actions_hide_inapplicable_removal_and_keep_delete_routing() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("conditional-actions")?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = browsing_books(&t, screen, Rc::new(Worker::stub()), store.clone());
        books.navigation.folder = Some("/books/Fiction/Nested".into());
        books.refresh(&t);
        // Browsing in Library never offers the removal.
        store
            .save_book(
                "0",
                &BookState { position: crate::document::Position("saved-position".into()), ..Default::default() },
            )
            .unwrap();
        books.open_menu("0".into(), Point::new(600, 600), &t);
        assert!(!books.menu.as_ref().unwrap().1.items.iter().any(|(_, l)| l == "Remove from continue reading"));
        books.menu = None;
        books.home_active = true;
        with_cx(&t, screen, |cx| {
            for (position, hidden, finished, can_remove) in [
                ("", false, false, false),
                ("saved-position", false, false, true),
                ("saved-position", true, false, false),
                ("saved-position", false, true, false),
            ] {
                store
                    .save_book(
                        "0",
                        &BookState {
                            position: crate::document::Position(position.into()),
                            hidden_from_continue: hidden,
                            finished,
                            ..Default::default()
                        },
                    )
                    .unwrap();
                books.open_menu("0".into(), Point::new(600, 600), &t);
                let menu = &books.menu.as_ref().unwrap().1;
                assert_eq!(menu.items.iter().any(|(_, label)| label == "Remove from continue reading"), can_remove);
                assert_eq!(menu.items.len(), if can_remove { 5 } else { 4 });
                let rect = menu.rect();
                let delete =
                    Point::new(rect.center().x, rect.y + t.mm(1.0) + (menu.items.len() as i32 - 1) * t.row + t.row / 2);
                tap(&mut books, delete, cx);
                assert!(books.confirm.is_some(), "the last row always asks to delete");
                assert_eq!(store.book("0").hidden_from_continue, hidden);
                books.confirm = None;
            }
            store
                .save_book(
                    "0",
                    &BookState { position: crate::document::Position("saved-position".into()), ..Default::default() },
                )
                .unwrap();
            books.open_menu("0".into(), Point::new(600, 600), &t);
            let rect = books.menu.as_ref().unwrap().1.rect();
            let remove = Point::new(rect.center().x, rect.y + t.mm(1.0) + 3 * t.row + t.row / 2);
            tap(&mut books, remove, cx);
            assert!(store.book("0").hidden_from_continue);
            assert!(books.confirm.is_none());
        });
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn delete_asks_first_then_removes_file_state_index_and_cover() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("delete")?;
        // The book lives in a library folder with its real stamp and a ready
        // cover under its real key, so the library worker's startup pass
        // finds nothing to change. Otherwise, if that pass finished before
        // the test, the book would be unavailable and gone from the view.
        let library = root.join("Library");
        std::fs::create_dir_all(&library)?;
        let mut settings = store.settings();
        settings.library_folders.get_or_insert_default().push(library.clone());
        store.save_settings(&settings)?;
        let file = library.join("doomed.epub");
        std::fs::write(&file, b"not really an epub")?;
        let mut index = store.library();
        index.books[0].path = file.clone();
        index.books[0].stamp = library::worker::stamp(&std::fs::metadata(&file)?)?;
        let cover = library::covers::key(&index.books[0]);
        index.books[0].cover = Cover::Ready(cover.clone());
        store.save_library(&index)?;
        std::fs::create_dir_all(store.cover_path("x").parent().unwrap())?;
        std::fs::write(store.cover_path(&cover), b"png")?;
        std::fs::write(store.small_cover_path(&cover), b"png")?;
        store.save_book("0", &BookState { progress: 1234, ..Default::default() })?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = browsing_books_live(&t, screen, Rc::new(Worker::stub()), store.clone());
        books.navigation.folder = Some(library.clone());
        books.refresh(&t);
        let item = |books: &Books| {
            let menu = &books.menu.as_ref().unwrap().1;
            Point::new(
                menu.rect().center().x,
                menu.rect().y + t.mm(1.0) + (menu.items.len() as i32 - 1) * t.row + t.row / 2,
            )
        };
        with_cx(&t, screen, |cx| {
            assert!(books.books.iter().any(|b| b.id == "0"));
            // Cancel keeps everything.
            books.open_menu("0".into(), Point::new(600, 600), &t);
            let p = item(&books);
            tap(&mut books, p, cx);
            assert!(books.confirm.is_some());
            let cancel = books.confirm.as_ref().unwrap().cancel.rect().center();
            tap(&mut books, cancel, cx);
            assert!(books.confirm.is_none() && file.exists());
            // Delete removes the file and reading state, and the view updates at once.
            books.open_menu("0".into(), Point::new(600, 600), &t);
            let p = item(&books);
            tap(&mut books, p, cx);
            let delete = books.confirm.as_ref().unwrap().delete.rect().center();
            tap(&mut books, delete, cx);
            assert!(books.confirm.is_none());
            assert!(!file.exists());
            assert_eq!(store.book("0").progress, 0);
            assert!(!books.books.iter().any(|b| b.id == "0"));
            assert!(!books.index.books.iter().any(|b| b.id == "0"));
            // An index published before the worker dropped the book doesn't bring it back.
            books.pending = Some(Snapshot { index: index.clone(), ..Default::default() });
            books.apply_pending(cx);
            assert!(!books.books.iter().any(|b| b.id == "0"));
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while store.library().books.iter().any(|b| b.path == file)
            || store.cover_path(&cover).exists()
            || store.small_cover_path(&cover).exists()
        {
            assert!(std::time::Instant::now() < deadline, "the library worker drops the deleted book");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn typing_uses_local_updates_and_keyboard_dismissal_consumes_the_touch() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("search-refresh")?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = browsing_books(&t, screen, Rc::new(Worker::stub()), store);
        with_cx(&t, screen, |cx| {
            books.toggle_search(cx);
            let search_button = books.search_action.rect().center();
            tap(&mut books, search_button, cx);
            assert!(books.search.is_some(), "outside tap only dismisses the keyboard");
            assert!(!books.search.as_ref().unwrap().keyboard.visible);
            books.search.as_mut().unwrap().field.focused = true;
            books.search.as_mut().unwrap().keyboard.visible = true;
        });
        let info = crate::platform::DeviceInfo { name: "test".into(), width: 1272, height: 1696, dpi: 300 };
        let mut app = crate::app::App::new(&info, t.clone(), Box::new(books));
        app.frame();
        for text in ["q", "s", "p", "v", "ГРЕЦИЯ"] {
            assert!(app.on_text_input(&TextInput::Insert(text.into())));
            assert!(!app.wants_exit());
            let updates = app.frame();
            assert!(!updates.is_empty());
            assert!(updates.iter().all(|u| u.mode != RefreshMode::Full && u.rect.y >= t.top_bar));
        }
        assert!(app.on_text_input(&TextInput::Clear));
        assert!(app.frame().iter().all(|u| u.mode != RefreshMode::Full));
        assert!(app.on_text_input(&TextInput::Submit));
        app.frame();
        assert!(!app.on_text_input(&TextInput::Insert("q".into())), "unfocused input can reach simulator shortcuts");
        app.on_sleep();
        assert!(!app.on_text_input(&TextInput::Insert("x".into())));
        drop(app);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn paging_and_folder_navigation_flash_only_the_grid() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("grid-only-refresh")?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = browsing_books(&t, screen, Rc::new(Worker::stub()), store);
        books.navigation.folder = Some("/books/Fiction/Nested".into());
        books.refresh(&t);
        let grid_rect = books.grid.rect();
        let next_pos = Point::new(books.pager.rect().right() - t.target / 2, books.pager.rect().center().y);
        let back_pos = books.back.rect().center();
        let info = crate::platform::DeviceInfo { name: "test".into(), width: 1272, height: 1696, dpi: 300 };
        let mut app = crate::app::App::new(&info, t.clone(), Box::new(books));
        app.frame();

        // Paging a folder: top bar stays untouched; Full is confined to
        // the grid and ordered last (after pager Ui cleanup).
        app.on_gesture(Gesture::Down(next_pos));
        app.frame();
        app.on_gesture(Gesture::Tap(next_pos));
        let updates = app.frame();
        assert!(!updates.is_empty());
        assert_eq!(updates.last(), Some(&crate::platform::Update { rect: grid_rect, mode: RefreshMode::Full }));
        assert!(
            updates[..updates.len() - 1].iter().all(|u| u.mode != RefreshMode::Full),
            "non-flashing updates must precede Full: {updates:?}"
        );
        assert!(updates.iter().all(|u| u.rect.y >= t.top_bar), "paging must not touch the top bar: {updates:?}");

        // Navigating Back to parent folder: top bar stays untouched, Full is confined to the grid.
        app.on_gesture(Gesture::Down(back_pos));
        app.frame();
        app.on_gesture(Gesture::Tap(back_pos));
        let updates = app.frame();
        let fulls: Vec<_> = updates.iter().filter(|u| u.mode == RefreshMode::Full).collect();
        assert_eq!(fulls.len(), 1);
        assert_eq!(updates.last().map(|u| u.mode), Some(RefreshMode::Full));
        assert!(fulls[0].rect.y >= t.top_bar, "folder flash must stay below top bar: {fulls:?}");
        assert!(updates.iter().all(|u| u.rect.y >= t.top_bar), "folder navigation must not touch top bar: {updates:?}");

        drop(app);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn collections_share_state_and_restore_folder_pages_after_reader_and_restart() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("resume")?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let worker = Rc::new(Worker::stub());
        let mut books = browsing_books(&t, screen, worker.clone(), store.clone());
        with_cx(&t, screen, |cx| {
            assert_eq!(books.folders, [PathBuf::from("/books"), PathBuf::from("/other/books")]);
            let p = books.grid.cells()[0].1.center();
            tap(&mut books, p, cx);
            assert_eq!(books.folders, [PathBuf::from("/books/Fiction")]);
            assert_eq!(books.books[0].id, "20", "loose root book stays directly in the root");
            let p = books.grid.cells()[0].1.center();
            tap(&mut books, p, cx);
            let p = books.grid.cells()[0].1.center();
            tap(&mut books, p, cx);
            assert_eq!(books.books.len(), 20);
            let next = books.pager.rect().split_right(t.target).0.center();
            tap(&mut books, next, cx);
            assert_eq!(books.pager.page, 1);
            let id = books.grid_book(books.grid.cells()[0].0).unwrap().id.clone();
            books.open(id.clone(), cx);
            assert!(books.reader.is_some());
            assert!(books.thumbnails.images.is_empty());
            let saved = BookState {
                position: crate::document::Position("same-anchor".into()),
                progress: 4321,
                ..Default::default()
            };
            store.save_book(&id, &saved).unwrap();
            books.returned(cx);
            assert_eq!(books.pager.page, 1);
            assert_eq!(books.navigation.folder, Some("/books/Fiction/Nested".into()));
            assert_eq!(books.states[&id].progress, 4321);
            let back = books.back.rect().center();
            tap(&mut books, back, cx);
            assert_eq!(books.navigation.folder, Some("/books/Fiction".into()));
            let p = books.grid.cells()[0].1.center();
            tap(&mut books, p, cx);
            assert_eq!(books.pager.page, 1, "Back and reenter retain the folder page");
            // Home icon returns to Home, and Library card reopens the exact folder and page.
            let home_btn = books.home_action.rect().center();
            tap(&mut books, home_btn, cx);
            assert!(books.is_home());
            let card = books.library_card.center();
            tap(&mut books, card, cx);
            assert!(!books.is_home());
            assert_eq!(books.navigation.folder, Some("/books/Fiction/Nested".into()));
            assert_eq!(books.pager.page, 1);
            books.after_present();
            books.navigation_writer.flush().unwrap();
        });
        drop(books);
        let restored = browsing_books(&t, screen, worker, store.clone());
        assert_eq!(restored.navigation.folder, Some("/books/Fiction/Nested".into()));
        assert_eq!(restored.pager.page, 1);
        // Reader settings saves cannot overwrite library navigation.
        store.save_settings(&store.settings())?;
        assert_eq!(store.navigation(), restored.navigation);
        drop(restored);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn collection_snapshots_defer_under_touch_and_overlays_then_fall_back_safely() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("updates")?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = browsing_books(&t, screen, Rc::new(Worker::stub()), store.clone());
        with_cx(&t, screen, |cx| {
            books.navigate(Some("/books/Fiction/Nested".into()), cx);
            books.pager.page = 1;
            books.save_navigation();
            books.after_present();
            books.navigation_writer.flush().unwrap();
            let old = books.index.clone();
            books.pending = Some(Snapshot { partial: true, ..Default::default() });
            books.apply_pending(cx);
            assert_eq!(books.navigation.folder, Some("/books/Fiction/Nested".into()));
            assert_eq!(store.navigation().page(), 1, "incomplete checkpoints cannot erase saved pages");
            books.pending = Some(Snapshot { index: old, ..Default::default() });
            books.apply_pending(cx);
            assert_eq!(books.pager.page, 1);
            let p = books.grid.cells()[0].1.center();
            books.on_gesture(Gesture::Down(p), cx);
            books.on_gesture(Gesture::LongPress(p), cx);
            books.pending = Some(Snapshot {
                index: LibraryIndex { books: vec![book(0)], ..Default::default() },
                ..Default::default()
            });
            books.apply_pending(cx);
            assert_eq!(books.books.len(), 20);
            books.on_gesture(Gesture::End(p), cx);
            books.apply_pending(cx);
            assert_eq!(books.books.len(), 20, "menu captures the old book identity");
            books.menu = None;
            books.details = Some(Details::from_text(&t, screen, "Folder", "path".into()));
            books.apply_pending(cx);
            assert!(books.pending.is_some());
            books.details = None;
            books.apply_pending(cx);
            books.after_present();
            books.navigation_writer.flush().unwrap();
            assert_eq!(books.navigation.folder, Some("/books".into()));
            assert_eq!(books.pager.page, 0);
            assert!(!store.navigation().folder_pages.contains_key(std::path::Path::new("/books/Fiction/Nested")));
            books.pending = Some(Snapshot::default());
            books.apply_pending(cx);
            assert!(books.navigation.folder.is_none());
        });
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn covers_use_tile_or_small_thumbnails_and_fall_back_to_the_full_one() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("small-covers")?;
        let mut book = store.library().books[0].clone();
        book.cover = Cover::Ready("k".into());
        std::fs::create_dir_all(store.cover_path("k").parent().unwrap())?;
        image::GrayImage::from_pixel(288, 432, image::Luma([128])).save(store.cover_path("k"))?;
        let t = Theme::for_dpi(300);
        let mut canvas = crate::gfx::Canvas::new(400, 400);
        let (mut text, mut icons) = (crate::gfx::TextRenderer::new(), crate::gfx::IconRenderer::default());
        let mut cache = Thumbnails::default();
        let (small, large) = (Rect::new(0, 0, 100, 150), Rect::new(0, 0, 200, 300));
        let mut draw = |rect: Rect, cache: &mut Thumbnails| {
            cache.clear();
            draw_cover(
                &mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut Vec::new()),
                &book,
                rect,
                &store,
                cache,
            );
            cache.images.get(&("k".to_owned(), rect.w, rect.h, Fit::Cover)).map(|e| e.cover.sample_pixel(50, 50))
        };
        // Until the worker backfills it, a small slot resizes the full thumbnail.
        assert_eq!(draw(small, &mut cache), Some(128));
        image::GrayImage::from_pixel(144, 216, image::Luma([40])).save(store.small_cover_path("k"))?;
        assert_eq!(draw(small, &mut cache), Some(40));
        assert_eq!(draw(large, &mut cache), Some(128), "larger slots keep the full thumbnail");
        // Once the worker has made a thumbnail of the slot's size, it is used as it is.
        assert_eq!(cache.take_new_sizes(), [(200, 300)], "only sizes above the small one get tiles");
        image::GrayImage::from_pixel(200, 300, image::Luma([90])).save(store.tile_cover_path("k", 200, 300))?;
        assert_eq!(draw(large, &mut cache), Some(90));
        assert!(cache.take_new_sizes().is_empty(), "each size is sent to the worker once");
        // A broken small file shows the title cover and asks for one repair.
        std::fs::write(store.small_cover_path("k"), b"corrupt")?;
        assert_eq!(draw(small, &mut cache), None);
        assert_eq!(cache.requests, vec![book.id.clone()]);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn folder_previews_pick_visible_folders_only_and_follow_a_book_just_read() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("previews")?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books =
            Books::with_library(&t, screen, Rc::new(Worker::stub()), store.clone(), Some(LibraryWorker::stub()));
        assert!(!books.previews.has_order(), "or starts clean");
        books.states.clear();
        books.home_active = false;
        books.navigation.folder = None;
        books.refresh(&t);
        let picked = |books: &Books, folder: &str| {
            books
                .previews
                .get(std::path::Path::new(folder))
                .map(|picks| picks.iter().map(|&i| books.index.books[i].id.clone()).collect::<Vec<_>>())
        };
        // Loose books before nested ones; pending covers count as art.
        assert_eq!(picked(&books, "/books").unwrap(), ["20", "0", "1", "10"]);
        assert_eq!(picked(&books, "/other/books").unwrap(), ["21"]);
        assert!(picked(&books, "/books/Fiction").is_none(), "only visible folders are picked");
        assert!(books.states.is_empty(), "previews read no reading states");
        with_cx(&t, screen, |cx| {
            books.navigate(Some("/books/Fiction/Nested".into()), cx);
            books.open("5".into(), cx);
            assert!(books.reader.is_some());
            let state =
                BookState { position: crate::document::Position("a".into()), last_opened: 50, ..Default::default() };
            store.save_book("5", &state).unwrap();
            books.returned(cx);
            assert!(!books.previews.has_order(), "the order is read again when next needed");
            books.navigate(None, cx);
        });
        assert_eq!(picked(&books, "/books").unwrap(), ["5", "20", "0", "1"]);
        assert!(books.previews.leads("5"));
        // A new index clears the picks; the visible ones are picked again.
        let index = books.index.clone();
        books.pending = Some(Snapshot { index, ..Default::default() });
        with_cx(&t, screen, |cx| books.apply_pending(cx));
        assert_eq!(picked(&books, "/books").unwrap(), ["5", "20", "0", "1"]);
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn decoded_covers_keep_nearby_pages_within_their_budget() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("covers")?;
        let mut index = store.library();
        std::fs::create_dir_all(store.cover_path("0").parent().unwrap())?;
        for b in &mut index.books {
            image::GrayImage::from_pixel(288, 432, image::Luma([128])).save(store.cover_path(&b.id))?;
            b.cover = Cover::Ready(b.id.clone());
        }
        store.save_library(&index)?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = browsing_books(&t, screen, Rc::new(Worker::stub()), store.clone());
        books.navigation.folder = Some("/books/Fiction/Nested".into());
        let mut canvas = crate::gfx::Canvas::new(1272, 1696);
        let mut text = crate::gfx::TextRenderer::new();
        let mut icons = crate::gfx::IconRenderer::default();
        let mut seen = HashSet::new();
        for page in 0..2 {
            books.pager.page = page;
            books.refresh(&t);
            books.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut Vec::new()));
            seen.extend(books.grid.cells().iter().map(|(i, _)| books.grid_book(*i).unwrap().id.clone()));
            let cached: HashSet<_> = books.thumbnails.images.keys().map(|(key, ..)| key.clone()).collect();
            assert_eq!(cached, seen, "earlier pages stay decoded");
        }
        assert!(books.thumbnails.bytes() <= Thumbnails::MAX_BYTES);
        // Every cover on a page has one size, so one tile thumbnail serves it.
        let sizes: HashSet<_> = books.thumbnails.images.keys().map(|(_, w, h, _)| (*w, *h)).collect();
        assert_eq!(sizes.len(), 1);
        // Going back a page decodes nothing, even with the files gone.
        let covers = store.cover_path("0").parent().unwrap().to_owned();
        let moved = root.join("covers-away");
        std::fs::rename(&covers, &moved)?;
        with_cx(&t, screen, |cx| books.go_to_page(0, cx));
        books.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut Vec::new()));
        assert!(books.thumbnails.failed.is_empty());
        std::fs::rename(&moved, &covers)?;
        // Clearing, as opening a book does, frees them all.
        books.thumbnails.clear();
        assert!(books.thumbnails.images.is_empty() && books.thumbnails.bytes() == 0);
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    /// Twelve folders of four books with art (ids 0–47, full covers gray 128
    /// and small ones gray 50), and a thirteenth whose books have none.
    fn preview_fixture(label: &str) -> anyhow::Result<(Store, PathBuf)> {
        let root = std::env::temp_dir().join(format!(
            "verso-previews-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let store = Store::at(root.clone());
        let mut settings = store.settings();
        settings.library_folders = Some(vec!["/books".into()]);
        store.save_settings(&settings)?;
        std::fs::create_dir_all(store.cover_path("0").parent().unwrap())?;
        let mut books: Vec<_> = (0..52)
            .map(|i| {
                let mut b = book(i);
                b.path = format!("/books/F{:02}/{i}.epub", i / 4).into();
                b.cover = Cover::Unavailable("no art".into());
                b
            })
            .collect();
        for b in &mut books[..48] {
            image::GrayImage::from_pixel(288, 432, image::Luma([128])).save(store.cover_path(&b.id))?;
            image::GrayImage::from_pixel(144, 216, image::Luma([50])).save(store.small_cover_path(&b.id))?;
            b.cover = Cover::Ready(b.id.clone());
        }
        store.save_library(&LibraryIndex { books, ..Default::default() })?;
        Ok((store, root))
    }
    /// Like `preview_fixture`, using real [`covers::key`] cache files so the
    /// live `LibraryWorker` confirms every cover instead of replacing them.
    fn warm_fixture(label: &str, extra: usize) -> anyhow::Result<(Store, PathBuf)> {
        let root = std::env::temp_dir().join(format!(
            "verso-warm-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let store = Store::at(root.clone());
        let mut settings = store.settings();
        settings.library_folders = Some(vec!["/books".into()]);
        store.save_settings(&settings)?;
        std::fs::create_dir_all(store.cover_path("0").parent().unwrap())?;
        let mut books: Vec<_> = (0..36 + extra)
            .map(|i| {
                let mut b = book(i);
                b.path = if i < 36 {
                    format!("/books/F{}/{i}.epub", i / 4).into()
                } else {
                    format!("/books/F0/extra-{i:02}.epub").into()
                };
                b.cover = Cover::Unavailable("no art".into());
                b
            })
            .collect();
        for (i, b) in books.iter_mut().enumerate() {
            if !(32..36).contains(&i) {
                let key = covers::key(b);
                image::GrayImage::from_pixel(288, 432, image::Luma([128])).save(store.cover_path(&key))?;
                image::GrayImage::from_pixel(144, 216, image::Luma([50])).save(store.small_cover_path(&key))?;
                b.cover = Cover::Ready(key);
            }
        }
        store.save_library(&LibraryIndex { books, ..Default::default() })?;
        Ok((store, root))
    }
    #[test]
    fn home_prepares_the_remembered_library_page_and_stays_as_it_was() -> anyhow::Result<()> {
        let (store, root) = warm_fixture("home", 0)?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        store.save_navigation(&Navigation { folder: Some("/books/F0".into()), ..Default::default() })?;
        let mut books = Books::with_store(&t, screen, Rc::new(Worker::stub()), store.clone());
        books.thumbnails = Thumbnails::prefetching(store.clone());
        assert!(books.is_home());
        books.refresh(&t);
        let mut canvas = crate::gfx::Canvas::new(1272, 1696);
        let mut text = crate::gfx::TextRenderer::new();
        let mut icons = crate::gfx::IconRenderer::default();
        let mut draw = |books: &mut Books| {
            let mut hints = Vec::new();
            canvas.fill(255);
            books.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut hints));
            canvas.gray_data().to_vec()
        };
        // Not before the worker's index is in, the scan is done and Home was
        // left alone for a moment.
        assert!(books.warm_at().is_none());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while books.scanning || books.pending.is_none() {
            assert!(std::time::Instant::now() < deadline, "initial scan never finished");
            std::thread::sleep(std::time::Duration::from_millis(5));
            with_cx(&t, screen, |cx| books.on_background(cx));
        }
        books.scanning = true;
        assert!(books.warm_at().is_none());
        books.scanning = false;
        let home = draw(&mut books);
        assert!(books.warm_at().is_some_and(|at| at >= books.last_input + WARM_IDLE));
        books.warm_library(&t);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let warm = loop {
            if let Some(warm) = books.library.take_warm() {
                break warm;
            }
            assert!(std::time::Instant::now() < deadline, "off-thread warm-up never finished");
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        let covers = books.apply_warm(warm, &t);
        assert!(covers > 0);
        assert!(books.warm_at().is_none(), "once per visit to Home");
        assert!(books.is_home() && !books.browsing_ready && books.books.len() <= 4);
        let after = draw(&mut books);
        let changed: Vec<_> = (0..home.len()).filter(|&i| home[i] != after[i]).collect();
        let rows = changed.first().zip(changed.last()).map(|(a, b)| (a / 1272, b / 1272));
        assert!(changed.is_empty(), "Home looks the same; rows {rows:?} changed");
        books.thumbnails.wait_prefetch();
        assert_eq!(books.thumbnails.images.len(), covers, "the first page of the remembered folder");
        // The first visit draws from memory: with the files gone, nothing fails.
        let covers = store.cover_path("0").parent().unwrap().to_owned();
        std::fs::rename(&covers, root.join("covers-away"))?;
        let card = books.library_card.center();
        with_cx(&t, screen, |cx| tap(&mut books, card, cx));
        assert_eq!(books.navigation.folder, Some("/books/F0".into()));
        draw(&mut books);
        assert!(books.thumbnails.failed.is_empty());
        // Back from a book, Home prepares it again.
        books.warm_due = false;
        with_cx(&t, screen, |cx| books.returned(cx));
        assert!(books.warm_due);
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn off_thread_warm_matches_refresh_covers_and_protects_newer_states() -> anyhow::Result<()> {
        // 36 + 40 books exercises the sparse listed-states path (> LISTED_STATES),
        // with one saved record so we verify only saved records stay in memory.
        let (store, root) = warm_fixture("match", 40)?;
        store.save_book(
            "0",
            &BookState {
                position: crate::document::Position("old".into()),
                progress: 1200,
                last_opened: 50,
                ..Default::default()
            },
        )?;
        let wait_warm = |books: &mut Books| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                if let Some(warm) = books.library.take_warm() {
                    return warm;
                }
                assert!(std::time::Instant::now() < deadline, "off-thread warm-up never finished");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        for (w, h, dpi) in [(1272, 1696, 300), (1696, 1272, 300), (600, 800, 167)] {
            let t = Theme::for_dpi(dpi);
            let screen = Rect::new(0, 0, w, h);
            for (folder, page, preview) in [
                (None, 0, FolderPreview::Stack),
                (Some(PathBuf::from("/books")), 0, FolderPreview::Stack),
                (Some(PathBuf::from("/books")), 0, FolderPreview::Latest),
                (Some(PathBuf::from("/books")), 0, FolderPreview::Icon),
                (Some(PathBuf::from("/books/F0")), 0, FolderPreview::Stack),
                (Some(PathBuf::from("/books/F0")), 2, FolderPreview::Stack),
            ] {
                let mut nav = Navigation { folder: folder.clone(), ..Default::default() };
                nav.set_page(page);
                store.save_navigation(&nav)?;
                let mut settings = store.settings();
                settings.folder_preview = preview;
                store.save_settings(&settings)?;
                let mut books = Books::with_store(&t, screen, Rc::new(Worker::stub()), store.clone());
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                while books.scanning || books.pending.is_none() {
                    assert!(std::time::Instant::now() < deadline, "initial scan never finished");
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    with_cx(&t, screen, |cx| books.on_background(cx));
                }
                books.warm_library(&t);
                let warm = wait_warm(&mut books);
                let warm_specs = books.warm_covers(&warm, &t);
                books.apply_warm(warm, &t);
                if folder.as_deref() == Some(std::path::Path::new("/books/F0")) {
                    assert!(books.states_loaded, "listed states marked complete");
                    assert_eq!(books.states.len(), 1, "unread books take no map space");
                }
                if preview != FolderPreview::Icon || folder.as_deref() == Some(std::path::Path::new("/books/F0")) {
                    assert!(!warm_specs.is_empty(), "{w}x{h} {folder:?} {preview:?}");
                }
                // Compare directly against full synchronous browsing layout.
                books.home_active = false;
                books.prepare_browsing();
                books.pager.page = books.navigation.page();
                books.refresh(&t);
                let expected = books.page_covers(&t, [books.grid.page]);
                assert_eq!(warm_specs, expected, "{w}x{h} {folder:?} {preview:?}");
            }
        }
        // Newer UI writes and deletes must never be overwritten by an in-flight warm-up.
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        store.save_navigation(&Navigation { folder: Some("/books/F0".into()), ..Default::default() })?;
        let mut books = Books::with_store(&t, screen, Rc::new(Worker::stub()), store.clone());
        books.warm_library(&t);
        let warm = wait_warm(&mut books);
        // Simulate a newer state written on the UI thread (book 0) and a deletion (book 1).
        books.set_book_state(
            "0".into(),
            BookState {
                position: crate::document::Position("newer".into()),
                progress: 8900,
                last_opened: 999,
                ..Default::default()
            },
        );
        books.remove_book_state("1");
        let mut warm_with_deleted = warm;
        if let Some(states) = &mut warm_with_deleted.states {
            states.insert("1".into(), BookState { progress: 500, ..Default::default() });
        }
        books.apply_warm(warm_with_deleted, &t);
        assert_eq!(books.states["0"].progress, 8900, "newer UI state kept");
        assert!(!books.states.contains_key("1"), "deleted book state not resurrected");
        // Opening a book bumps warm_seq and cancels prefetching stale results.
        books.warm_library(&t);
        let stale = wait_warm(&mut books);
        books.warm_seq = books.warm_seq.wrapping_add(1);
        assert_eq!(books.apply_warm(stale, &t), 0);
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn folder_previews_decode_one_page_per_mode_and_icon_decodes_nothing() -> anyhow::Result<()> {
        let (store, root) = preview_fixture("modes")?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = browsing_books(&t, screen, Rc::new(Worker::stub()), store.clone());
        // Covers is disabled in Settings but kept working; set it directly.
        books.folder_preview = FolderPreview::Covers;
        books.navigation.folder = Some("/books".into());
        books.refresh(&t);
        assert_eq!((books.folders.len(), books.grid.cells().len()), (13, 12));
        let mut canvas = crate::gfx::Canvas::new(1272, 1696);
        let mut text = crate::gfx::TextRenderer::new();
        let mut icons = crate::gfx::IconRenderer::default();
        let mut draw = |books: &mut Books| {
            let mut hints = Vec::new();
            books.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut hints));
            hints.iter().filter(|h| h.mode == RefreshMode::Ui).count()
        };
        // Pixel values tell the small file (50) from the full one (128).
        let decoded = |books: &Books, behind: bool| {
            books
                .thumbnails
                .images
                .iter()
                .filter(|((_, _, _, f), _)| matches!(f, Fit::Behind(_)) == behind)
                .map(|(_, e)| e.cover.sample_pixel(5, 5))
                .collect::<Vec<_>>()
        };
        assert_eq!(draw(&mut books), 12, "each preview tile is a grayscale area");
        assert_eq!(books.thumbnails.images.len(), 48, "four previews for each of twelve tiles");
        assert!(books.thumbnails.bytes() <= Thumbnails::MAX_BYTES);
        assert!(decoded(&books, false).iter().all(|&v| v == 50), "mosaic slots decode small files");
        // Drawing again decodes nothing new, even with the files gone.
        let covers = store.cover_path("0").parent().unwrap().to_owned();
        let moved = root.join("covers-away");
        std::fs::rename(&covers, &moved)?;
        draw(&mut books);
        assert_eq!(books.thumbnails.images.len(), 48);
        assert!(books.thumbnails.failed.is_empty());
        std::fs::rename(&moved, &covers)?;
        // The thirteenth folder has no art: its tile is the icon, with no hint.
        with_cx(&t, screen, |cx| books.go_to_page(1, cx));
        assert_eq!(draw(&mut books), 0);
        assert_eq!(books.thumbnails.images.len(), 48, "the previous page stays decoded");
        with_cx(&t, screen, |cx| books.go_to_page(0, cx));
        let set_mode = |books: &mut Books, mode| {
            let mut settings = store.settings();
            settings.folder_preview = mode;
            store.save_settings(&settings).unwrap();
            with_cx(&t, screen, |cx| books.apply_folder_changes(cx));
            assert_eq!(books.folder_preview, mode);
            assert!(books.thumbnails.images.is_empty(), "a new mode starts from an empty cache");
        };
        set_mode(&mut books, FolderPreview::Stack);
        assert_eq!(draw(&mut books), 12);
        assert_eq!(books.thumbnails.images.len(), 36);
        assert_eq!(decoded(&books, false), [128; 12], "front covers decode the full file");
        assert_eq!(decoded(&books, true), [50; 24], "covers behind decode small files");
        set_mode(&mut books, FolderPreview::Latest);
        assert_eq!(draw(&mut books), 12);
        assert_eq!(books.thumbnails.images.len(), 12);
        // The first pick is the same book in every mode.
        let latest: HashSet<_> = books.thumbnails.images.keys().map(|(key, ..)| key.clone()).collect();
        let first: HashSet<_> =
            books.visible_folders().filter_map(|f| books.preview_books(f).next()).map(|b| b.id.clone()).collect();
        assert_eq!((latest.len(), latest), (12, first));
        set_mode(&mut books, FolderPreview::Icon);
        assert_eq!(draw(&mut books), 0);
        assert!(books.thumbnails.images.is_empty());
        // Icon mode doesn't even read the opened order.
        let mut fresh = browsing_books(&t, screen, Rc::new(Worker::stub()), store.clone());
        fresh.navigation.folder = Some("/books".into());
        fresh.refresh(&t);
        assert!(!fresh.previews.has_order());
        drop((books, fresh));
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn failed_home_cover_shows_title_requests_one_repair_and_retries_after_reply() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("cover-repair")?;
        let mut index = store.library();
        std::fs::create_dir_all(store.cover_path("0").parent().unwrap())?;
        for (i, b) in index.books.iter_mut().take(3).enumerate() {
            image::GrayImage::from_pixel(288, 432, image::Luma([128])).save(store.cover_path(&b.id))?;
            b.cover = Cover::Ready(b.id.clone());
            store.save_book(
                &b.id,
                &BookState {
                    position: crate::document::Position("anchor".into()),
                    last_opened: i as u64 + 1,
                    ..Default::default()
                },
            )?;
        }
        store.rebuild_home_summary(&index)?;
        std::fs::write(store.cover_path("1"), b"corrupt")?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = Books::with_store(&t, screen, Rc::new(Worker::stub()), store.clone());
        assert!(books.is_home());
        let mut canvas = crate::gfx::Canvas::new(1272, 1696);
        let mut text = crate::gfx::TextRenderer::new();
        let mut icons = crate::gfx::IconRenderer::default();
        let mut draw = |books: &mut Books| {
            books.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut Vec::new()));
        };
        draw(&mut books);
        assert_eq!(books.thumbnails.images.len(), 2, "only Home's visible covers are decoded");
        assert!(books.thumbnails.take_new_sizes().is_empty(), "Home's covers get no tile thumbnails");
        assert!(books.thumbnails.failed.contains(&("1".into(), "1".into())));
        assert!(books.thumbnails.requests.is_empty(), "the repair request is sent after drawing");
        books.thumbnails.failed.insert(("2".into(), "missing".into()));
        draw(&mut books);
        assert!(books.thumbnails.requests.is_empty() && books.thumbnails.failed.len() == 2, "no reload loop");
        image::GrayImage::from_pixel(288, 432, image::Luma([128])).save(store.cover_path("1"))?;
        books.thumbnails.repaired(&["1".into()]);
        draw(&mut books);
        assert_eq!(books.thumbnails.images.len(), 3);
        assert_eq!(books.thumbnails.failed.len(), 1);
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn name_editor_and_folder_menu_fit_both_orientations_and_keyboards() {
        for (w, h, dpi) in [(1272, 1696, 300), (1696, 1272, 300), (600, 800, 167), (800, 600, 167)] {
            let t = Theme::for_dpi(dpi);
            let screen = Rect::new(0, 0, w, h);
            let mut sheet = NameSheet::new(&t, screen, NameOperation::Create("/books".into()));
            for _ in 0..3 {
                // The globe key is second from the left on the bottom row (1.5..2.5 of 10 units).
                let p = Point::new(screen.w / 5, screen.bottom() - t.target / 2);
                sheet.keyboard.on_gesture(&Gesture::Down(p), &t);
                sheet.keyboard.on_gesture(&Gesture::Tap(p), &t);
                assert!(
                    sheet.submit.rect().bottom() <= sheet.keyboard.rect().y,
                    "{w}x{h}: buttons {} keyboard {}",
                    sheet.submit.rect().bottom(),
                    sheet.keyboard.rect().y
                );
                assert!(sheet.cancel.rect().bottom() <= sheet.keyboard.rect().y);
                assert!(sheet.error_rect.bottom() <= sheet.keyboard.rect().y);
            }
            let area = Rect { y: t.top_bar, h: screen.h - t.top_bar, ..screen };
            let menu = PopupMenu::open(vec![(None, "Action".into()); 6], screen.center(), area, &t);
            assert!(menu.rect().y >= area.y && menu.rect().bottom() <= screen.bottom() - t.margin);
        }
    }

    #[test]
    fn folder_name_errors_root_menu_and_stale_publications() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("folder-actions")?;
        let library = root.join("library");
        std::fs::create_dir_all(&library)?;
        let mut settings = store.settings();
        settings.library_folders = Some(vec![library.clone()]);
        store.save_settings(&settings)?;
        store.save_library(&LibraryIndex { empty_folders: vec![library.clone()], ..Default::default() })?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = browsing_books(&t, screen, Rc::new(Worker::stub()), store.clone());
        with_cx(&t, screen, |cx| {
            books.navigate(Some(library.clone()), cx);
            books.open_folder_menu(library.clone(), Point::new(200, 600), &t);
            let rect = books.menu.as_ref().unwrap().1.rect();
            let rename = Point::new(rect.center().x, rect.y + t.mm(1.0) + 6 * t.row + t.row / 2);
            tap(&mut books, rename, cx);
            assert!(books.menu.is_some() && books.name_sheet.is_none(), "root rename is disabled");
            let delete = Point::new(rename.x, rename.y + t.row);
            tap(&mut books, delete, cx);
            assert!(books.menu.is_some() && books.confirm.is_none(), "root delete is disabled");
            books.menu = None;
            // A folder that only contains a library folder lies outside every
            // root: nothing there could be created and ever shown.
            let parent = root.clone();
            assert!(!books.folder_creatable(&parent) && books.folder_creatable(&library));
            books.open_folder_menu(parent.clone(), Point::new(200, 600), &t);
            let rect = books.menu.as_ref().unwrap().1.rect();
            let new_row = Point::new(rect.center().x, rect.y + t.mm(1.0) + 5 * t.row + t.row / 2);
            tap(&mut books, new_row, cx);
            assert!(books.menu.is_some() && books.name_sheet.is_none(), "New folder is disabled above a root");
            books.menu = None;
            books.start_name(NameOperation::Create(parent.clone()), &t);
            books.on_text_input(&TextInput::Insert("Lost".into()), cx);
            books.on_text_input(&TextInput::Submit, cx);
            assert!(!parent.join("Lost").exists());
            books.name_sheet = None;
            let create = books.new_folder.rect().center();
            tap(&mut books, create, cx);
            assert!(books.name_sheet.is_some());
            books.on_text_input(&TextInput::Insert(".hidden".into()), cx);
            books.on_text_input(&TextInput::Submit, cx);
            assert!(!books.name_sheet.as_ref().unwrap().error.is_empty());
            books.on_text_input(&TextInput::Clear, cx);
            books.on_text_input(&TextInput::Insert("Old".into()), cx);
            books.on_text_input(&TextInput::Submit, cx);
            assert!(books.name_sheet.is_none());
            let old = library.join("Old");
            assert!(books.folders.contains(&old));
            let before = books.index.clone();
            books.navigate(Some(old.clone()), cx);
            books.start_name(NameOperation::Rename(old.clone()), &t);
            books.on_text_input(&TextInput::Clear, cx);
            books.on_text_input(&TextInput::Insert("New".into()), cx);
            books.on_text_input(&TextInput::Submit, cx);
            let new = library.join("New");
            assert!(new.exists() && !old.exists());
            assert_eq!(books.navigation.folder, Some(new.clone()));
            books.pending = Some(Snapshot { index: before, ..Default::default() });
            books.apply_pending(cx);
            assert!(books.index.empty_folders.contains(&new));
            assert!(!books.index.empty_folders.contains(&old));
            let revision = books.changes.last().unwrap().0;
            let acknowledged = books.index.clone();
            books.pending = Some(Snapshot { index: acknowledged, revision, ..Default::default() });
            books.apply_pending(cx);
            assert_eq!(books.navigation.folder, Some(new.clone()));
            // Submitting the unchanged name closes the sheet and queues nothing.
            let queued = books.changes.len();
            books.start_name(NameOperation::Rename(new.clone()), &t);
            books.on_text_input(&TextInput::Submit, cx);
            assert!(books.name_sheet.is_none() && books.changes.len() == queued);
            // Reuse Old after rename. An acknowledged snapshot must keep it.
            books.start_name(NameOperation::Create(library.clone()), &t);
            books.on_text_input(&TextInput::Insert("Old".into()), cx);
            books.on_text_input(&TextInput::Submit, cx);
            let revision = books.changes.last().unwrap().0;
            books.pending = Some(Snapshot { index: books.index.clone(), revision, ..Default::default() });
            books.apply_pending(cx);
            assert!(books.index.empty_folders.contains(&old));
            books.ask_delete_folder(new.clone(), cx);
            let cancel = books.confirm.as_ref().unwrap().cancel.rect().center();
            tap(&mut books, cancel, cx);
            assert!(new.exists());
            books.ask_delete_folder(new.clone(), cx);
            let delete = books.confirm.as_ref().unwrap().delete.rect().center();
            tap(&mut books, delete, cx);
            assert!(!new.exists());
            assert_eq!(books.navigation.folder, Some(library.clone()));
            assert!(books.folders.contains(&old));
            // Once the index and a worker Home both carry the edits, drop them.
            let latest = books.changes.last().unwrap().0;
            books.worker_home_revision = latest - 1;
            books.pending = Some(Snapshot { index: books.index.clone(), revision: latest, ..Default::default() });
            books.apply_pending(cx);
            assert_eq!(books.changes.iter().map(|(r, _)| *r).collect::<Vec<_>>(), [latest]);
            books.worker_home_revision = latest;
            books.prune_changes();
            assert!(books.changes.is_empty());
        });
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn deleting_the_last_book_keeps_its_empty_folder() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("last-book")?;
        let folder = root.join("Only");
        std::fs::create_dir_all(&folder)?;
        let path = folder.join("book.epub");
        std::fs::write(&path, b"book")?;
        // KOReader's sidecar would otherwise keep the folder non-empty.
        std::fs::create_dir(folder.join("book.sdr"))?;
        std::fs::write(folder.join("book.sdr/metadata.epub.lua"), b"progress")?;
        let mut b = book(0);
        b.path = path;
        store.save_library(&LibraryIndex { books: vec![b], ..Default::default() })?;
        let mut settings = store.settings();
        settings.library_folders = Some(vec![root.clone()]);
        store.save_settings(&settings)?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = browsing_books(&t, screen, Rc::new(Worker::stub()), store);
        with_cx(&t, screen, |cx| {
            books.navigate(Some(folder.clone()), cx);
            books.delete("0", cx);
            assert!(!folder.join("book.sdr").exists(), "the sidecar goes with its book");
            assert_eq!(books.navigation.folder, Some(folder.clone()));
            assert_eq!(books.collections.folders[&folder].count, 0);
            assert!(books.index.empty_folders.contains(&folder));
            assert!(books.books.is_empty());
        });
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn home_delete_before_browsing_defers_the_index_and_replays_on_load() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("home-delete")?;
        let book = store.library().books.into_iter().find(|b| b.id == "20").unwrap();
        store.select_home_book(&book);
        store.save_book(&book.id, &BookState { last_opened: 7, progress: 1000, ..Default::default() })?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books =
            Books::with_library(&t, screen, Rc::new(Worker::stub()), store.clone(), Some(LibraryWorker::stub()));
        let on_home = |books: &Books| books.home.books.iter().any(|e| e.book.id == book.id);
        with_cx(&t, screen, |cx| {
            assert!(on_home(&books));
            books.delete(&book.id, cx);
            assert!(!books.browsing_ready && books.index.books.is_empty(), "Home deletes don't load the index");
            assert!(!on_home(&books));
            // The stubbed library worker never saves, like one that hasn't caught up.
            assert!(store.library().books.iter().any(|b| b.id == book.id));
            books.reload_home();
            books.apply_pending(cx);
            assert!(!on_home(&books), "a saved summary predating the delete is patched");
            books.prepare_browsing();
            assert!(books.browsing_ready);
            assert!(!books.index.books.iter().any(|b| b.id == book.id));
            assert_eq!(books.index.books.len(), 21);
        });
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn folder_delete_messages_and_subtitles_name_what_is_inside() {
        let message = |books, folders, other_files| {
            folder_delete_message("Tolkien", &files::Contents { books, folders, other_files })
        };
        assert_eq!(message(0, 0, 0), "The empty folder “Tolkien” will be removed from this device.");
        assert_eq!(message(0, 1, 0), "“Tolkien” and the empty folder in it will be removed from this device.");
        assert_eq!(message(0, 3, 0), "“Tolkien” and the 3 empty folders in it will be removed from this device.");
        assert_eq!(
            message(1, 0, 0),
            "“Tolkien” and everything in it will be removed from this device: 1 book and its reading progress."
        );
        assert_eq!(
            message(12, 2, 3),
            "“Tolkien” and everything in it will be removed from this device: \
             12 books and their reading progress, 3 other files."
        );
        assert_eq!(message(0, 1, 1), "“Tolkien” and everything in it will be removed from this device: 1 other file.");
        let mut folder = collections::Folder::default();
        assert_eq!(folder_subtitle(&folder), "Empty");
        folder.children.insert("/books/Tolkien/nested".into());
        assert_eq!(folder_subtitle(&folder), "1 folder");
        folder.count = 2;
        assert_eq!(folder_subtitle(&folder), "2 books");
    }

    #[test]
    fn details_paginate_long_unicode_paths_without_losing_text() {
        let theme = Theme::for_dpi(167);
        let mut text = crate::gfx::TextRenderer::new();
        let content = format!("Title\n\nAuthor\n\nFile: {}", "Папка 世界 / ".repeat(300));
        let area = Rect::new(0, 0, 400, 500);
        let pages = detail_pages(&mut text, &content, theme.body, area);
        assert!(pages.len() > 1);
        assert_eq!(pages.concat(), content);
        for page in pages {
            assert!(text.measure(&page, theme.body, Some(area.w as f32)).1 <= area.h as f32);
        }
    }
    #[test]
    fn touches_menus_and_sheets_defer_scan_reordering_and_clamp_pages() -> anyhow::Result<()> {
        let root = std::env::temp_dir().join(format!("verso-library-ui-{}", std::process::id()));
        std::fs::create_dir_all(root.join("books-folder"))?;
        let store = Store::at(root.join("state"));
        let mut settings = store.settings();
        settings.library_folder = Some(root.join("books-folder"));
        store.save_settings(&settings)?;
        let books_in_folder: Vec<_> = (0..14)
            .map(|i| {
                let mut b = book(i);
                b.path = root.join(format!("books-folder/{i}.epub"));
                b
            })
            .collect();
        store.save_library(&LibraryIndex { books: books_in_folder.clone(), ..Default::default() })?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = browsing_books(&t, screen, Rc::new(Worker::stub()), store);
        books.pager.page = 1;
        books.refresh(&t);
        let point = books.grid.cells()[0].1.center();
        let mut light = None;
        let mut light_request = None;
        let mut toast = None;
        let mut forced = Vec::new();
        let mut cx = EventCx {
            theme: &t,
            screen,
            now: std::time::Instant::now(),
            frontlight: &mut light,
            light_request: &mut light_request,
            toast: &mut toast,
            forced: &mut forced,
            needs_redraw: &mut false,
            battery: None,
        };
        books.on_gesture(Gesture::Down(point), &mut cx);
        books.pending = Some(Snapshot {
            index: LibraryIndex { books: vec![books_in_folder[1].clone()], ..Default::default() },
            ..Default::default()
        });
        books.apply_pending(&mut cx);
        assert_eq!(books.books.len(), 14);
        books.on_gesture(Gesture::LongPress(point), &mut cx);
        books.on_gesture(Gesture::End(point), &mut cx);
        assert!(books.menu.is_some());
        assert_eq!(books.books.len(), 14);
        let menu_rect = books.menu.as_ref().unwrap().1.rect();
        let details = Point::new(menu_rect.center().x, menu_rect.y + t.mm(1.0) + t.row + t.row / 2);
        books.on_gesture(Gesture::Down(details), &mut cx);
        books.on_gesture(Gesture::Tap(details), &mut cx);
        assert!(books.details.is_some());
        assert_eq!(books.books.len(), 14);
        let outside = Point::new(1, 1);
        books.on_gesture(Gesture::Down(outside), &mut cx);
        assert!(matches!(books.on_gesture(Gesture::Tap(outside), &mut cx), Action::None));
        assert!(books.details.is_none());
        assert_eq!(books.books.len(), 1);
        assert_eq!(books.pager.page, 0);
        assert_eq!(books.grid.cells().len(), 1);
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn a_check_finishing_waits_for_the_finger_and_moves_nothing() -> anyhow::Result<()> {
        let root = std::env::temp_dir().join(format!("verso-update-notice-{}", std::process::id()));
        let state = verso_update::check::State {
            available: Some(Offer {
                version: "99.0.0".parse()?,
                released: "2026-12-01".into(),
                notes: "Faster page turns.".into(),
                components: vec!["app".into()],
                download: 6_100_000,
                unpacked: 7_000_000,
            }),
            ..Default::default()
        };
        let path = root.join("device").join(verso_update::check::STATE_PATH);
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, serde_json::to_vec(&state)?)?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = Books::with_store(&t, screen, Rc::new(Worker::stub()), Store::at(root.clone()));
        books.refresh(&t);
        assert!(books.notice.is_some() && books.update_dot, "the notice is there from the first frame");
        let layout = (books.continue_rect, books.notice_rect);
        // The library worker reports its first scan whenever it ends, which
        // redraws the status line; let it finish so only the check changes.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while books.scanning || books.working {
            assert!(std::time::Instant::now() < deadline, "the library scan never finished");
            std::thread::sleep(std::time::Duration::from_millis(5));
            with_cx(&t, screen, |cx| books.on_background(cx));
        }
        with_cx(&t, screen, |cx| {
            let finger = Point::new(screen.w / 2, screen.h / 2);
            books.on_gesture(Gesture::Down(finger), cx);
            // A check (here: Skip from elsewhere) changes the status while
            // the finger is down: nothing on Home changes yet.
            books.updates.skip(&"99.0.0".parse().unwrap());
            *cx.needs_redraw = false;
            books.on_background(cx);
            assert!(books.notice.is_some(), "nothing changes under a finger");
            assert!(!*cx.needs_redraw);
            books.on_gesture(Gesture::Cancel(finger), cx);
            books.on_background(cx);
        });
        assert!(books.notice.is_none(), "the notice goes once the finger lifts");
        assert!(books.update_dot, "the Settings dot stays while an update exists");
        assert_eq!((books.continue_rect, books.notice_rect), layout, "its spot stays reserved");
        std::fs::remove_dir_all(root).ok();
        Ok(())
    }

    #[test]
    fn home_startup_prefetches_exact_first_draw_covers_and_draw_keeps_queue() -> anyhow::Result<()> {
        for (w, h, dpi) in [(1272, 1696, 300), (1696, 1272, 300), (600, 800, 167)] {
            let root = std::env::temp_dir().join(format!("verso-home-prefetch-{w}x{h}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let store = Store::at(root.clone());
            std::fs::create_dir_all(store.cover_path("x").parent().unwrap())?;
            let mut index = LibraryIndex::default();
            for i in 0..5 {
                let id = format!("b{i}");
                let mut b = book(i);
                b.id = id.clone();
                b.cover = Cover::Ready(id.clone());
                image::GrayImage::from_pixel(covers::WIDTH, covers::HEIGHT, image::Luma([40 + i as u8 * 20]))
                    .save(store.cover_path(&id))?;
                store.save_book(
                    &id,
                    &BookState {
                        position: if i == 0 { crate::document::Position("p1".into()) } else { Default::default() },
                        progress: if i == 0 { 3500 } else { 0 },
                        last_opened: 100 - i as u64,
                        ..Default::default()
                    },
                )?;
                index.books.push(b);
            }
            store.rebuild_home_summary(&index)?;
            let t = Theme::for_dpi(dpi);
            let screen = Rect::new(0, 0, w, h);
            let mut books =
                Books::with_library(&t, screen, Rc::new(Worker::stub()), store.clone(), Some(LibraryWorker::stub()));
            books.thumbnails = Thumbnails::prefetching(store.clone());
            let specs = books.home_covers(&t);
            let expected = 1 + books.grid.cells().len();
            assert_eq!(specs.len(), expected, "{w}x{h}: resume + visible recent covers");
            assert!(expected >= 4, "{w}x{h}: resume + recent row");
            books.thumbnails.prefetch(specs.clone());
            books.thumbnails.wait_prefetch();
            assert_eq!(books.thumbnails.images.len(), specs.len());
            // Hide cover files on disk: any size or fit mismatch in draw() fails.
            let cover_dir = store.cover_path("x").parent().unwrap().to_owned();
            std::fs::remove_dir_all(&cover_dir)?;
            let mut canvas = crate::gfx::Canvas::new(w as u32, h as u32);
            let mut text = crate::gfx::TextRenderer::new();
            let mut icons = crate::gfx::IconRenderer::default();
            let mut hints = Vec::new();
            books.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut hints));
            assert!(books.thumbnails.failed.is_empty(), "{w}x{h}: every Home cover must hit prefetch");
            assert!(books.thumbnails.requests.is_empty());
            assert_eq!(books.thumbnails.images.len(), specs.len());
            // Home's empty neighbour_covers must not wipe the startup prefetch set.
            assert!(!books.thumbnails.prefetching.is_empty());
            drop(books);
            std::fs::remove_dir_all(root).ok();
        }
        Ok(())
    }

    #[test]
    fn shared_top_bar_spacing_and_single_root_navigation() -> anyhow::Result<()> {
        for (w, h, dpi) in [(1272, 1696, 300), (1696, 1272, 300), (600, 800, 167), (1072, 1448, 300)] {
            let root = std::env::temp_dir().join(format!(
                "verso-topbar-{w}x{h}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
            ));
            let library = root.join("My Library");
            std::fs::create_dir_all(&library)?;
            let store = Store::at(root.join("data"));
            store.save_library_folders(vec![root.join("documents"), library.clone()])?;
            let mut b0 = book(0);
            b0.path = library.join("loose.epub");
            let mut b1 = book(1);
            b1.path = library.join("Tolkien/hobbit.epub");
            let index = LibraryIndex { books: vec![b0, b1], ..Default::default() };
            store.save_library(&index)?;
            store.rebuild_home_summary(&index)?;
            let t = Theme::for_dpi(dpi);
            let screen = Rect::new(0, 0, w, h);
            let mut books =
                Books::with_library(&t, screen, Rc::new(Worker::stub()), store.clone(), Some(LibraryWorker::stub()));
            // Top bar widgets never overlap, battery stays inside bounds and fixed across Home/Library.
            assert!(books.title_rect.right() <= books.battery_rect.x);
            assert!(books.battery_rect.right() <= books.search_action.rect().x);
            assert!(books.search_action.rect().right() <= books.rescan_action.rect().x);
            assert!(books.rescan_action.rect().right() < books.transfer_action.rect().x, "group gap before Wifi");
            assert!(books.transfer_action.rect().right() <= books.settings_action.rect().x);
            assert!(books.settings_action.rect().right() < books.exit.rect().x, "extra separation before Exit");
            assert!(books.home_action.rect().right() <= books.battery_rect.x);
            assert!(books.exit.rect().right() <= screen.right());
            assert_eq!(books.library_card_subtitle(), "1 folder · 1 book");
            let home_battery = books.battery_rect;
            // Tapping the Library card skips the root chooser because only My Library has books.
            with_cx(&t, screen, |cx| {
                let card = books.library_card.center();
                tap(&mut books, card, cx);
                assert!(!books.is_home());
                assert_eq!(books.navigation.folder, Some(library.clone()));
                assert!(books.is_top_level());
                assert!(!books.back.enabled, "no Back arrow at single-root top level");
                assert!(books.new_folder.enabled, "New folder enabled at single-root top level");
                assert_eq!(books.battery_rect, home_battery, "battery stays fixed across Home & Library");
                // Entering subfolder enables Back.
                books.navigate(Some(library.join("Tolkien")), cx);
                assert!(!books.is_top_level());
                assert!(books.back.enabled);
                // Tapping Home icon returns to Home.
                let home = books.home_action.rect().center();
                tap(&mut books, home, cx);
                assert!(books.is_home());
            });
            drop(books);
            std::fs::remove_dir_all(root)?;
        }
        Ok(())
    }

    #[test]
    fn manual_refresh_triggers_rescan_and_toasts_on_completion() -> anyhow::Result<()> {
        let root = std::env::temp_dir().join(format!(
            "verso-manual-refresh-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let library = root.join("books");
        std::fs::create_dir_all(&library)?;
        let store = Store::at(root.join("data"));
        store.save_library_folders(vec![library])?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = Books::with_store(&t, screen, Rc::new(Worker::stub()), store);
        // Let the initial startup scan settle without toasting.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while books.scanning || books.working {
            assert!(std::time::Instant::now() < deadline, "initial scan never finished");
            std::thread::sleep(std::time::Duration::from_millis(5));
            with_cx(&t, screen, |cx| {
                books.on_background(cx);
                assert!(cx.toast.is_none(), "startup scan never toasts");
            });
        }
        // Tapping Refresh triggers a rescan and toasts once it finishes.
        let btn = books.rescan_action.rect().center();
        with_cx(&t, screen, |cx| tap(&mut books, btn, cx));
        assert!(books.manual_rescan && books.scanning);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut toasted = None;
        while toasted.is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
            with_cx(&t, screen, |cx| {
                books.on_background(cx);
                if let Some(t) = cx.toast.take() {
                    toasted = Some(t.text);
                }
            });
        }
        assert_eq!(toasted.as_deref(), Some("Library updated"));
        assert!(!books.manual_rescan);
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn home_portrait_layout_matches_mockup_dimensions_and_cover_outlines() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("home-portrait-spec")?;
        std::fs::create_dir_all(store.cover_path("x").parent().unwrap())?;
        let mut index = store.library();
        for (i, b) in index.books.iter_mut().take(5).enumerate() {
            b.cover = Cover::Ready(b.id.clone());
            b.metadata.title = if i == 0 {
                "A Very Long Book Title That Should Wrap Cleanly Across Two Lines Without Overflowing".into()
            } else {
                format!("Recent Title {i}")
            };
            image::GrayImage::from_pixel(covers::WIDTH, covers::HEIGHT, image::Luma([200]))
                .save(store.cover_path(&b.id))?;
            store.save_book(
                &b.id,
                &BookState {
                    position: if i == 0 { crate::document::Position("p1".into()) } else { Default::default() },
                    progress: if i == 0 { 3400 } else { 0 },
                    last_opened: 100 - i as u64,
                    ..Default::default()
                },
            )?;
        }
        store.save_library(&index)?;
        store.rebuild_home_summary(&index)?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books =
            Books::with_library(&t, screen, Rc::new(Worker::stub()), store.clone(), Some(LibraryWorker::stub()));
        // Continue reading cover matches 312 × 468 at 300 dpi.
        let continue_cover = continue_cover_rect(books.continue_rect);
        assert_eq!((continue_cover.w, continue_cover.h), (312, 468));
        assert_eq!(continue_cover.x, t.margin);
        // Divider separates Continue reading from Recent books.
        let div_y = books.home_divider_y.expect("divider between Continue and Recent");
        assert!(div_y > books.continue_rect.bottom() && div_y < books.header.y);
        // 4-column flush Recent books row with 258 × 387 covers.
        let cells = books.grid.cells();
        assert_eq!(cells.len(), 4);
        assert_eq!(cells[0].1.x, t.margin);
        assert_eq!(cells[3].1.right(), screen.right() - t.margin);
        for (_, cell) in &cells {
            let cover = books.cover_rect(*cell, &t);
            assert_eq!((cover.w, cover.h), (258, 387));
        }
        // Reserved notice sits cleanly inside the gap between Recent books and the 156 px Library card.
        assert_eq!(books.library_card.h, 156);
        assert_eq!(books.library_card.bottom(), screen.bottom() - t.margin);
        assert!(books.notice_rect.y > books.grid.rect().bottom());
        assert!(books.notice_rect.bottom() < books.library_card.y);
        // home_covers matches exact cover dimensions.
        let specs = books.home_covers(&t);
        assert_eq!(specs.len(), 5);
        assert_eq!((specs[0].w, specs[0].h), (312, 468));
        assert!(specs[1..].iter().all(|s| (s.w, s.h) == (258, 387)));
        // Drawing paints 2 px INK outlines on loaded covers and 3 px black divider.
        let mut canvas = crate::gfx::Canvas::new(1272, 1696);
        let mut text = crate::gfx::TextRenderer::new();
        let mut icons = crate::gfx::IconRenderer::default();
        let mut hints = Vec::new();
        books.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut hints));
        let px = |canvas: &crate::gfx::Canvas, x: i32, y: i32| canvas.gray_data()[y as usize * 1272 + x as usize];
        assert_eq!(px(&canvas, continue_cover.x, continue_cover.y), INK);
        assert_eq!(px(&canvas, continue_cover.x + 1, continue_cover.y + 1), INK);
        assert_eq!(px(&canvas, continue_cover.x + 5, continue_cover.y + 5), 200);
        let recent_cover = books.cover_rect(cells[0].1, &t);
        assert_eq!(px(&canvas, recent_cover.x, recent_cover.y), INK);
        assert_eq!(px(&canvas, recent_cover.right() - 1, recent_cover.bottom() - 1), INK);
        assert_eq!(px(&canvas, t.margin + 100, div_y), INK);
        // Pressing the Library card inverts it with a Fast hint.
        let card_pt = books.library_card.center();
        with_cx(&t, screen, |cx| {
            books.on_gesture(Gesture::Down(card_pt), cx);
        });
        hints.clear();
        canvas.fill(PAPER);
        books.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut hints));
        assert!(hints.iter().any(|h| h.rect == books.library_card && h.mode == RefreshMode::Fast));
        assert_eq!(px(&canvas, books.library_card.x + 20, books.library_card.y + 20), INK);
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn home_empty_and_partial_states_anchor_library_card() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("home-states")?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books =
            Books::with_library(&t, screen, Rc::new(Worker::stub()), store.clone(), Some(LibraryWorker::stub()));
        // 1. Fresh launch before scan completes: empty Home, "Scanning…" subtitle, card at bottom.
        assert!(books.continue_id.is_none() && books.books.is_empty());
        assert!(books.home_divider_y.is_none());
        assert_eq!(books.library_card_subtitle(), "Scanning…");
        assert_eq!(books.library_card.bottom(), screen.bottom() - t.margin);
        let empty_card = books.library_card;
        let empty_notice = books.notice_rect;
        // Prompt draws inside the empty region above notice_rect.
        let mut canvas = crate::gfx::Canvas::new(1272, 1696);
        let mut text = crate::gfx::TextRenderer::new();
        let mut icons = crate::gfx::IconRenderer::default();
        books.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut Vec::new()));
        let gray = canvas.gray_data();
        let prompt_ink = (t.top_bar..books.notice_rect.y)
            .flat_map(|y| (t.margin..screen.right() - t.margin).map(move |x| gray[y as usize * 1272 + x as usize]))
            .any(|p| p < PAPER);
        assert!(prompt_ink, "empty Home shows the muted prompt above the Library card");
        // Scan finishes empty -> "Empty".
        books.scanning = false;
        assert_eq!(books.library_card_subtitle(), "Empty");

        // 2. Recents-only (no Continue reading): Recent books starts near the top; card stays at bottom.
        let index = store.library();
        for (i, b) in index.books.iter().take(3).enumerate() {
            store.save_book(
                &b.id,
                &BookState {
                    position: crate::document::Position("done".into()),
                    finished: true,
                    last_opened: 10 + i as u64,
                    ..Default::default()
                },
            )?;
        }
        store.rebuild_home_summary(&index)?;
        books.home = store.home_summary().unwrap();
        books.refresh(&t);
        assert!(books.continue_id.is_none() && books.home_divider_y.is_none());
        assert_eq!(books.books.len(), 3);
        assert!(books.header.y < t.top_bar + t.mm(5.0), "Recent books starts near the top without Continue reading");
        assert_eq!(books.library_card, empty_card);
        assert_eq!(books.notice_rect, empty_notice);

        // 3. Continue-only (no Recent books): Continue reading shown, no orphan divider, card stays at bottom.
        books.home.books.truncate(1);
        books.home.books[0].status.finished = false;
        books.refresh(&t);
        assert!(books.continue_id.is_some() && books.books.is_empty());
        assert!(books.home_divider_y.is_none(), "no divider without Recent books below");
        assert_eq!(books.library_card, empty_card);
        assert_eq!(books.notice_rect, empty_notice);
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn home_landscape_and_small_panels_keep_elements_in_bounds() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("home-sizes")?;
        let index = store.library();
        for (i, b) in index.books.iter().take(5).enumerate() {
            store.save_book(
                &b.id,
                &BookState {
                    position: if i == 0 { crate::document::Position("p1".into()) } else { Default::default() },
                    progress: 4500,
                    last_opened: 50 - i as u64,
                    ..Default::default()
                },
            )?;
        }
        store.rebuild_home_summary(&index)?;
        let offer = Offer {
            version: "0.9.0".parse()?,
            released: "2026-12-01".into(),
            notes: "Update notes.".into(),
            components: vec!["app".into()],
            download: 1_000_000,
            unpacked: 1_500_000,
        };
        for (w, h, dpi) in [
            (1272, 1696, 300),
            (1696, 1272, 300),
            (1072, 1448, 300),
            (1448, 1072, 300),
            (600, 800, 167),
            (800, 600, 167),
        ] {
            let t = Theme::for_dpi(dpi);
            let screen = Rect::new(0, 0, w, h);
            let mut books =
                Books::with_library(&t, screen, Rc::new(Worker::stub()), store.clone(), Some(LibraryWorker::stub()));
            books.notice = Some(offer.clone());
            let landscape = w > h;
            assert!(books.continue_rect.y >= t.top_bar, "{w}x{h}");
            assert!(books.continue_rect.bottom() <= books.library_card.y, "{w}x{h}");
            assert!(books.library_card.x == t.margin && books.library_card.right() == w - t.margin, "{w}x{h}");
            assert!(books.library_card.bottom() == h - t.margin, "{w}x{h}");
            assert!(books.notice_rect.bottom() < books.library_card.y, "{w}x{h}");
            assert!(books.grid.rect().bottom() < books.notice_rect.y, "{w}x{h}");
            if landscape {
                assert!(books.home_divider_y.is_none(), "{w}x{h}: no horizontal divider side-by-side");
                assert!(books.continue_rect.right() < books.header.x, "{w}x{h}: Continue left, Recent right");
                assert_eq!(books.notice_rect.x, books.header.x, "{w}x{h}: notice sits under Recent books");
                assert_eq!(books.notice_rect.w, books.header.w, "{w}x{h}");
                assert!((2..=3).contains(&books.books.len()), "{w}x{h}: 2-3 recent books in landscape");
            } else {
                assert!(books.home_divider_y.is_some(), "{w}x{h}");
                assert!(books.continue_rect.bottom() < books.header.y, "{w}x{h}: Continue above Recent");
                assert_eq!(books.books.len(), 4, "{w}x{h}: 4 recent books in portrait");
            }
            let mut canvas = crate::gfx::Canvas::new(w as u32, h as u32);
            let mut text = crate::gfx::TextRenderer::new();
            let mut icons = crate::gfx::IconRenderer::default();
            books.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut Vec::new()));
            drop(books);
        }
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn library_header_row_controls_spacing_and_folder_path_sheet() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("library-header-row")?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = browsing_books(&t, screen, Rc::new(Worker::stub()), store);
        books.scanning = false;
        with_cx(&t, screen, |cx| {
            // Multi-root top level: no Back, no New folder, single page -> full title width.
            assert!(books.is_top_level());
            assert!(!books.pager.visible());
            assert_eq!(books.path_rect.x, t.margin);
            assert_eq!(books.path_rect.right(), screen.right() - t.margin);
            assert!(books.path_rect.y >= t.top_bar && books.path_rect.bottom() < books.grid.rect().y);
            // Subfolder with 20 books (2 pages): Back arrow, New folder, 6 mm gap, compact pager.
            books.navigate(Some("/books/Fiction/Nested".into()), cx);
            assert!(!books.is_top_level());
            assert!(books.back.enabled && books.new_folder.enabled && books.pager.visible());
            assert!(books.back.rect().y >= t.top_bar && books.back.rect().bottom() < books.grid.rect().y);
            assert!(books.back.rect().right() <= books.path_rect.x);
            assert!(books.path_rect.right() <= books.new_folder.rect().x);
            assert!(books.new_folder.rect().right() < books.pager.rect().x);
            let visual_gap = books.pager.rect().x - (books.new_folder.rect().center().x + t.icon / 2);
            assert!((visual_gap - t.mm(6.0)).abs() <= 2, "6 mm gap between New folder and pager: {visual_gap}");
            assert_eq!(books.pager.rect().right(), screen.right() - t.margin);
            let multipage_title_w = books.path_rect.w;
            // Tapping the folder title/subtitle area opens the "Folder path" sheet.
            let p = books.path_rect.center();
            tap(&mut books, p, cx);
            assert_eq!(books.details.as_ref().unwrap().content, "/books/Fiction/Nested");
            books.details = None;
            // Single-page folder hides the pager and expands the title's max width.
            books.navigate(Some("/books/Fiction".into()), cx);
            assert!(!books.pager.visible());
            assert!(books.path_rect.w > multipage_title_w, "hiding pager expands title width");
            assert_eq!(books.new_folder.rect().center().x, screen.right() - t.margin - t.icon / 2);
        });
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn library_book_tiles_dimensions_outlines_and_progress_bars() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("library-book-tiles")?;
        std::fs::create_dir_all(store.cover_path("x").parent().unwrap())?;
        let mut index = store.library();
        for b in index.books.iter_mut().filter(|b| b.path.starts_with("/books/Fiction/Nested")) {
            b.cover = Cover::Ready(b.id.clone());
            image::GrayImage::from_pixel(covers::WIDTH, covers::HEIGHT, image::Luma([200]))
                .save(store.cover_path(&b.id))?;
        }
        store.save_library(&index)?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = browsing_books(&t, screen, Rc::new(Worker::stub()), store.clone());
        books.scanning = false;
        books.navigation.folder = Some("/books/Fiction/Nested".into());
        books.refresh(&t);
        // Set book 0 in progress (50%), book 1 finished, book 2 unread.
        let id0 = books.books[0].id.clone();
        let id1 = books.books[1].id.clone();
        let id2 = books.books[2].id.clone();
        books.set_book_state(
            id0,
            BookState { position: crate::document::Position("p".into()), progress: 5000, ..Default::default() },
        );
        books.set_book_state(
            id1,
            BookState { position: crate::document::Position("done".into()), finished: true, ..Default::default() },
        );
        books.remove_book_state(&id2);
        let cells = books.grid.cells();
        assert_eq!(cells.len(), 12);
        for (_, cell) in &cells {
            let cover = books.cover_rect(*cell, &t);
            assert_eq!(cover.x, cell.x, "book covers are left-aligned in the cell");
            assert_eq!((cover.w, cover.h), (214, 321), "Paperwhite 4 × 3 book cover dimensions");
        }
        let mut canvas = crate::gfx::Canvas::new(1272, 1696);
        canvas.fill(PAPER);
        let mut text = crate::gfx::TextRenderer::new();
        let mut icons = crate::gfx::IconRenderer::default();
        books.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut Vec::new()));
        let px = |x: i32, y: i32| canvas.gray_data()[y as usize * 1272 + x as usize];
        // Book 0 (in progress, 50%): 2 px INK outline + 6 px bar (left INK, right LINE).
        let c0 = books.cover_rect(cells[0].1, &t);
        assert_eq!(px(c0.x, c0.y), INK);
        assert_eq!(px(c0.x + 1, c0.y + 1), INK);
        assert_eq!(px(c0.x + 5, c0.y + 5), 200);
        assert_eq!(px(c0.x + 10, c0.bottom() + 2), INK, "in-progress filled portion");
        assert_eq!(px(c0.right() - 10, c0.bottom() + 2), LINE, "in-progress remaining track");
        // Book 1 (finished): 2 px white gap below cover outline, then 100% solid INK bar.
        let c1 = books.cover_rect(cells[1].1, &t);
        assert_eq!(px(c1.x + 10, c1.bottom()), PAPER, "2 px white gap above finished bar");
        assert_eq!(px(c1.x + 10, c1.bottom() + 1), PAPER);
        assert_eq!(px(c1.x + 10, c1.bottom() + 4), INK, "finished 100% solid bar");
        assert_eq!(px(c1.right() - 10, c1.bottom() + 4), INK);
        // Book 2 (unread): no bar below the cover.
        let c2 = books.cover_rect(cells[2].1, &t);
        for dy in 0..8 {
            assert_eq!(px(c2.x + 10, c2.bottom() + dy), PAPER, "unread book has no progress bar");
        }
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn search_layout_portrait_landscape_and_narrow_panels() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("search-layout")?;
        for (w, h, dpi, expected_per_page) in [
            (1272, 1696, 300, 12), // 4 × 3
            (1696, 1272, 300, 12), // 6 × 2
            (1072, 1448, 300, 9),  // 3 × 3
            (600, 800, 167, 9),    // 3 × 3
        ] {
            let t = Theme::for_dpi(dpi);
            let screen = Rect::new(0, 0, w, h);
            let mut books = browsing_books(&t, screen, Rc::new(Worker::stub()), store.clone());
            let home_battery = books.battery_rect;
            with_cx(&t, screen, |cx| {
                books.navigate(Some("/books/Fiction".into()), cx);
                books.toggle_search(cx);
                assert_eq!(books.search_action.icon, Icon::Close);
                assert_eq!(books.battery_rect, home_battery, "{w}x{h}: battery stays fixed in Search");
                assert!(books.pager.compact && books.pager.visible(), "{w}x{h}");
                assert_eq!(books.grid.cells().len(), expected_per_page, "{w}x{h}");
                assert!(books.header.right() <= books.pager.rect().x, "{w}x{h}: results header clears pager");
                assert!(books.header.bottom() < books.grid.rect().y, "{w}x{h}");
                assert!(books.grid.rect().bottom() <= screen.bottom() - t.margin, "{w}x{h}");
                // Landscape keeps row 0 completely above the open keyboard.
                if w > h {
                    let search = books.search.as_ref().unwrap();
                    assert_eq!(search.all.rect().y, t.top_bar + t.mm(2.0));
                    assert_eq!(search.current.rect().y, search.all.rect().y);
                    let row0_bottom = books.grid.cells()[0].1.bottom();
                    assert!(
                        row0_bottom <= search.keyboard.rect().y,
                        "{w}x{h}: row 0 ({row0_bottom}) must fit above keyboard ({})",
                        search.keyboard.rect().y
                    );
                }
                // Results are sorted in fixed Title order.
                let titles: Vec<_> = books.books.iter().map(|b| b.metadata.title.to_lowercase()).collect();
                let mut sorted = titles.clone();
                sorted.sort();
                assert_eq!(titles, sorted, "{w}x{h}: fixed Title order");
            });
            let mut canvas = crate::gfx::Canvas::new(w as u32, h as u32);
            let mut text = crate::gfx::TextRenderer::new();
            let mut icons = crate::gfx::IconRenderer::default();
            books.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut Vec::new()));
            drop(books);
        }
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
