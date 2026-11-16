//! Paginated library, engine-independent discovery, and the existing reader flow.
use crate::document::{
    FontFace, Layout,
    thread::{Command, Reply, Worker},
};
use crate::geom::{Point, Rect};
use crate::gfx::{Family, HAlign, Icon};
use crate::library::{
    self, Book, Cover, LibraryIndex, PathChange, Sort,
    collections::{self, Collections, Navigation, Tab},
    covers, files,
    home::Summary,
    search::{self, Scope, SearchCache},
    worker::{Snapshot, Worker as LibraryWorker},
};
use crate::platform::{RefreshMode, TextInput};
use crate::screens::reader::Reader;
use crate::screens::{Action, EventCx, Screen};
use crate::store::{BookState, Store};
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK, INK2, LINE, Theme};
use crate::ui::widget::{DrawCx, Press, PressEvent};
use crate::ui::widgets::{
    Button, ButtonKind, Grid, GridEvent, IconButton, Keyboard, MenuEvent, Pager, PagerEvent, PopupMenu, Segmented,
    Sheet, Slider, SliderEvent, TextField, TopBar,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

mod moving;
mod selection;
use moving::{Carry, ClashSheet, PasteEnd};
use selection::Selection;

/// Decoded covers for the visible view only. A cache file that fails to load is
/// not retried on every redraw: the title cover is shown and the library worker
/// is asked once to repair it. A repair reply allows one more load.
#[derive(Default)]
struct Thumbnails {
    images: HashMap<(String, i32, i32), image::GrayImage>,
    /// (book id, cover key) pairs that failed to load.
    failed: HashSet<(String, String)>,
    /// Book ids to send to the library worker after drawing.
    requests: Vec<String>,
}
impl Thumbnails {
    const LIMIT: usize = 18;
    fn retain(&mut self, keys: &HashSet<String>) {
        self.images.retain(|(key, _, _), _| keys.contains(key));
    }
    fn clear(&mut self) {
        self.images.clear();
    }
    fn repaired(&mut self, ids: &[String]) {
        self.failed.retain(|(id, _)| !ids.contains(id));
    }
}
struct SearchSession {
    field: TextField,
    folded: String,
    scope: Scope,
    collection: Option<PathBuf>,
    page: usize,
    cache: SearchCache,
    all: Button,
    current: Button,
    context: Rect,
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
        if self.keyboard.visible && self.keyboard.rect().contains(g.pos()) {
            if let Some(input) = self.keyboard.on_gesture(g, t) {
                return self.edit(&input);
            }
            return None;
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
    collections: Collections,
    folders: Vec<PathBuf>,
    navigation: Navigation,
    /// Navigation changed since it was last written; see `persist_navigation`.
    navigation_unsaved: bool,
    navigation_error: Option<String>,
    tabs: Segmented,
    back: IconButton,
    new_folder: IconButton,
    path_rect: Rect,
    path_press: Press,
    states: HashMap<String, BookState>,
    fonts: Vec<FontFace>,
    top: TopBar,
    exit: IconButton,
    search_action: IconButton,
    settings_action: IconButton,
    search: Option<SearchSession>,
    rescan: Button,
    sort: Segmented,
    grid: Grid,
    pager: Pager,
    continue_id: Option<String>,
    continue_rect: Rect,
    continue_press: Press,
    header: Rect,
    status: Rect,
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
    errors: Vec<String>,
    thumbnails: Thumbnails,
    reader: Option<Reader>,
    generation: u64,
    engine_error: Option<String>,
}
impl Books {
    pub fn new(t: &Theme, screen: Rect, worker: Rc<Worker>) -> Self {
        Self::with_store(t, screen, worker, Store::new())
    }
    /// Screens sharing one Store also share its dictionary service.
    pub fn with_store(t: &Theme, screen: Rect, worker: Rc<Worker>, store: Store) -> Self {
        let roots = store.library_folders();
        let settings_seen = store.settings_saves();
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
        let tabs = Segmented::texts(&["Home", "Collections", "All books"], 0);
        let library = LibraryWorker::new(store.clone(), roots.clone(), worker.waker());
        let mut top = TopBar::new("Home", false);
        top.border = false;
        top.serif = screen.w >= t.mm(80.0);
        top.place(screen, t);
        let mut exit = IconButton::new(Icon::Exit);
        exit.place_at(Point::new(screen.right() - t.margin - t.target / 2, t.top_bar / 2), t);
        let compact = screen.w < t.mm(80.0);
        let rescan_w = t.mm(if compact { 18.0 } else { 21.0 });
        let mut rescan = Button::new(if compact { "Scan" } else { "Rescan" }, ButtonKind::Secondary);
        rescan.place(
            Rect::new(exit.rect().x - rescan_w - t.mm(2.0), (t.top_bar - t.button_h) / 2, rescan_w, t.button_h),
            t,
        );
        let mut search_action = IconButton::new(Icon::Search);
        search_action.place_at(Point::new(rescan.rect().x - t.mm(2.0) - t.target / 2, t.top_bar / 2), t);
        let mut settings_action = IconButton::new(Icon::Settings);
        settings_action.place_at(Point::new(search_action.rect().x - t.target / 2, t.top_bar / 2), t);
        let mut pager = Pager::new(1);
        pager.jump = true;
        pager.place(Rect::new(t.margin, screen.bottom() - t.mm(11.0), screen.w - 2 * t.margin, t.mm(11.0)), t);
        let sort = Segmented::texts(
            &["Recent", "Title", "Author"],
            match navigation.sort(store.settings().library_sort) {
                Sort::Recent => 0,
                Sort::Title => 1,
                Sort::Author => 2,
            },
        );
        let fonts = worker.fonts();
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
            collections: Collections::default(),
            folders: Vec::new(),
            navigation,
            navigation_unsaved: false,
            navigation_error: None,
            tabs,
            back: IconButton::new(Icon::ArrowLeft),
            new_folder: IconButton::new(Icon::FolderPlus),
            path_rect: Rect::default(),
            path_press: Press::default(),
            states: HashMap::new(),
            fonts,
            top,
            exit,
            search_action,
            settings_action,
            search: None,
            rescan,
            sort,
            grid: Grid::new(0, 4, 2),
            pager,
            continue_id: None,
            continue_rect: Rect::default(),
            continue_press: Press::default(),
            header: Rect::default(),
            status: Rect::default(),
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
            errors: Vec::new(),
            thumbnails: Thumbnails::default(),
            reader: None,
            generation: 0,
            engine_error: None,
        };
        s.pager.page = s.navigation.page();
        s.refresh(t);
        s
    }
    fn selected_sort(&self) -> Sort {
        match self.sort.selected {
            1 => Sort::Title,
            2 => Sort::Author,
            _ => Sort::Recent,
        }
    }
    fn is_home(&self) -> bool {
        self.home_active && self.search.is_none()
    }
    /// Loading the browsing model is explicitly deferred until the user asks for it.
    fn prepare_browsing(&mut self) {
        if self.browsing_ready {
            return;
        }
        let snapshot = self.pending.take();
        let complete = snapshot.as_ref().is_none_or(|s| !s.partial);
        let (mut index, revision) = snapshot.map_or_else(|| (self.store.library(), 0), |s| (s.index, s.revision));
        // Before browsing, only Home deletions can be queued. Their removals
        // are idempotent, so replaying them over the saved index is safe even
        // when the worker already saved them.
        for (r, change) in &self.changes {
            if *r > revision {
                change.apply(&mut index);
            }
        }
        self.index = index;
        self.index_revision = revision;
        let started = std::time::Instant::now();
        self.collections = Collections::from_index(&self.index, &self.roots);
        log::info!(
            "startup stage: collections books={} elapsed_us={}",
            self.index.books.len(),
            started.elapsed().as_micros()
        );
        if complete {
            self.navigation.folder = self.collections.restore(self.navigation.folder.as_deref());
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
        self.tabs.selected = if self.navigation.tab == Tab::Collections { 1 } else { 2 };
        let in_collections = self.search.is_none() && self.navigation.tab == Tab::Collections;
        self.folders =
            if in_collections { self.collections.children(self.navigation.folder.as_deref()) } else { Vec::new() };
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
                        !in_collections || folder.is_some_and(|f| f.books.contains(&collections::normalize(&b.path)))
                    }
            })
            .cloned()
            .collect();
        // Only shown books need a state: for their status and Recent order.
        // Folder-only views read none.
        let states_started = std::time::Instant::now();
        let (reads, listed) = load_states(&self.store, &mut self.states, &self.books);
        log::debug!(
            "startup stage: cached states reads={reads} listed={listed} elapsed_us={}",
            states_started.elapsed().as_micros()
        );
        let sort = self.selected_sort();
        library::sort_books(&mut self.books, &self.states, sort);
        // The resume card lives on Home only; browsing tabs are plain grids.
        self.continue_id = None;
        self.continue_rect = Rect::default();
        let y = t.top_bar + t.mm(2.0);
        self.tabs.place(Rect::new(t.margin, y, self.screen.w - 2 * t.margin, t.target));
        let landscape = self.screen.w > self.screen.h;
        let y = if let Some(search) = &mut self.search {
            search.all.kind = if search.scope == Scope::All { ButtonKind::Primary } else { ButtonKind::Secondary };
            search.current.kind = if search.scope == Scope::All { ButtonKind::Secondary } else { ButtonKind::Primary };
            let gap = t.mm(2.0);
            if landscape {
                // One row for the field and scope, and no scope note, so a row
                // of results fits above the keyboard.
                let width = self.screen.w - 2 * t.margin;
                let w = (width * 2 / 5 - gap) / 2;
                let field_w = width - 2 * (w + gap);
                search.field.place(Rect::new(t.margin, y, field_w, t.target));
                search.all.place(Rect::new(t.margin + field_w + gap, y, w, t.target), t);
                search.current.place(Rect::new(self.screen.w - t.margin - w, y, w, t.target), t);
                search.context = Rect::default();
                y + t.target + t.mm(3.0)
            } else {
                search.field.place(Rect::new(t.margin, y, self.screen.w - 2 * t.margin, t.target));
                let scope_y = y + t.target + gap;
                let w = (self.screen.w - 2 * t.margin - gap) / 2;
                search.all.place(Rect::new(t.margin, scope_y, w, t.target), t);
                search.current.place(Rect::new(t.margin + w + gap, scope_y, w, t.target), t);
                search.context =
                    Rect::new(t.margin, scope_y + t.target + t.mm(1.0), self.screen.w - 2 * t.margin, t.mm(6.0));
                search.context.bottom() + gap
            }
        } else {
            y + t.target + t.mm(3.0)
        };
        self.path_rect = if in_collections {
            self.back.place_at(Point::new(t.margin + t.target / 2, y + t.target / 2), t);
            self.new_folder.place_at(Point::new(self.screen.right() - t.margin - t.target / 2, y + t.target / 2), t);
            Rect::new(
                t.margin + t.target + t.mm(2.0),
                y,
                self.screen.w - 2 * t.margin - 2 * t.target - t.mm(2.0),
                t.mm(12.0),
            )
        } else {
            Rect::default()
        };
        let y = if in_collections { y + self.path_rect.h + t.mm(2.0) } else { y };
        self.header = Rect::new(t.margin, y, self.screen.w - 2 * t.margin, t.target);
        let sort_w = (self.header.w * 3 / 5).min(t.mm(51.0)).max(3 * t.target).min(self.header.w);
        self.sort.place(Rect::new(self.header.right() - sort_w, y, sort_w, t.target));
        self.status = Rect::new(t.margin, self.pager.rect().y - t.mm(6.0), self.screen.w - 2 * t.margin, t.mm(6.0));
        let top = self.header.bottom() + t.mm(1.5);
        // Landscape search keeps its single row of results above the
        // keyboard, whether or not the keyboard is open, so paging is the same.
        let bottom = match &self.search {
            Some(search) if landscape => self.status.y.min(search.keyboard.rect().y - t.mm(2.0)),
            _ => self.status.y,
        };
        let cols = ((self.screen.w - 2 * t.margin) / t.mm(22.0)).clamp(1, 4) as usize;
        let rows = ((bottom - top) / t.mm(32.0)).clamp(1, 2) as usize;
        self.grid = Grid::new(self.folders.len() + self.books.len(), cols, rows);
        self.grid.place(
            Rect::new(t.margin, top, self.screen.w - 2 * t.margin, (bottom - top).max(t.target)),
            t.mm(3.5),
            t.mm(3.0),
        );
        self.pager.set(self.pager.page, self.grid.paging().pages());
        self.grid.page = self.pager.page;
        if let Some(search) = &mut self.search {
            search.page = self.pager.page;
        }
        self.retain_selection();
        self.prioritize();
        // Only the visible grid plus continue card retain decoded images.
        let keys: HashSet<_> = self
            .grid
            .cells()
            .iter()
            .filter_map(|(i, _)| self.grid_book(*i))
            .chain(self.continue_id.iter().filter_map(|id| self.book(id)))
            .filter_map(|b| match &b.cover {
                Cover::Ready(key) => Some(key.clone()),
                _ => None,
            })
            .collect();
        self.thumbnails.retain(&keys);
        log::debug!("startup stage: view books={} elapsed_us={}", self.books.len(), started.elapsed().as_micros());
    }
    fn refresh_home(&mut self, t: &Theme) {
        self.top.title = "Home".into();
        self.tabs.selected = 0;
        let y = t.top_bar + t.mm(2.0);
        self.tabs.place(Rect::new(t.margin, y, self.screen.w - 2 * t.margin, t.target));
        let y = y + t.target + t.mm(5.0);
        let width = self.screen.w - 2 * t.margin;
        self.status = Rect::new(t.margin, self.screen.bottom() - t.mm(10.0), width, t.mm(6.0));
        self.continue_id = self.home.resume().map(|e| e.book.id.clone());
        // Landscape is too short to stack the resume card above the recent
        // books, so they stand side by side.
        let side = self.continue_id.is_some() && self.screen.w > self.screen.h;
        self.continue_rect = if side {
            Rect::new(t.margin, y, width / 2, (self.status.y - y - t.mm(8.0)).min(t.mm(45.0)))
        } else if self.continue_id.is_some() {
            Rect::new(t.margin, y, width, t.mm(37.0))
        } else {
            Rect::default()
        };
        self.header = if side {
            let x = self.continue_rect.right() + t.mm(6.0);
            Rect::new(x, y, self.screen.w - t.margin - x, t.mm(6.0))
        } else {
            let y = if self.continue_id.is_some() { self.continue_rect.bottom() + t.mm(6.0) } else { y };
            Rect::new(t.margin, y, width, t.mm(6.0))
        };
        self.folders.clear();
        let cols = (self.header.w / t.mm(if side { 19.0 } else { 22.0 })).clamp(1, 4) as usize;
        self.books = self.home.recents(cols).iter().map(|e| e.book.clone()).collect();
        self.grid = Grid::new(self.books.len(), cols, 1);
        self.grid.place(
            Rect::new(
                self.header.x,
                self.header.bottom() + t.mm(2.0),
                self.header.w,
                (self.status.y - self.header.bottom() - t.mm(4.0)).min(t.mm(45.0)).max(t.target),
            ),
            t.mm(3.5),
            0,
        );
        self.prioritize();
        let keys: HashSet<_> = self
            .home
            .books
            .iter()
            .filter_map(|e| match &e.book.cover {
                Cover::Ready(key) => Some(key.clone()),
                _ => None,
            })
            .collect();
        self.thumbnails.retain(&keys);
    }
    fn prioritize(&self) {
        let mut ids: Vec<_> = self.continue_id.iter().cloned().collect();
        ids.extend(self.grid.cells().iter().filter_map(|(i, _)| self.grid_book(*i)).map(|b| b.id.clone()));
        self.library.prioritize(ids);
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
        self.navigation.set_sort(self.selected_sort());
        self.navigation_unsaved = true;
    }
    /// Writes navigation once its frame is on screen. The synced write took
    /// 5–951 ms on the Kindle (measured) and used to hold that frame back.
    fn persist_navigation(&mut self) {
        if std::mem::take(&mut self.navigation_unsaved)
            && let Err(e) = self.store.save_navigation(&self.navigation)
        {
            self.navigation_error = Some(format!("Couldn't save library navigation: {e}"));
        }
    }
    /// Shows another page of the browsing grid: arrows, swipes and the page jump.
    fn go_to_page(&mut self, page: usize, cx: &mut EventCx) {
        self.pager.set(page, self.pager.pages);
        self.grid.page = self.pager.page;
        self.thumbnails.clear();
        self.prioritize();
        self.save_navigation();
        cx.refresh(self.screen, RefreshMode::Full);
    }
    /// What the jump sheet shows for `page`: the first folder or book on it,
    /// by the name the current sort uses.
    fn page_start(&self, page: usize) -> String {
        let i = page * self.grid.paging().per_page;
        if let Some(path) = self.folders.get(i) {
            return collections::name(path);
        }
        self.grid_book(i).map_or_else(String::new, |b| {
            if self.selected_sort() == Sort::Author && !b.metadata.authors.trim().is_empty() {
                b.metadata.authors.clone()
            } else {
                b.metadata.title.clone()
            }
        })
    }
    fn navigate(&mut self, folder: Option<PathBuf>, cx: &mut EventCx) {
        if !self.carrying() {
            self.end_selection(cx);
        }
        let started = std::time::Instant::now();
        self.home_active = false;
        self.prepare_browsing();
        self.navigation.set_page(self.pager.page);
        self.navigation.folder = folder;
        self.pager.page = self.navigation.page();
        self.thumbnails.clear();
        self.refresh(cx.theme);
        self.save_navigation();
        cx.refresh(self.screen, RefreshMode::Full);
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
        let roots = self.store.library_folders();
        if roots != self.roots {
            log::info!("library folders changed: {roots:?}");
            self.end_selection(cx);
            self.roots = roots.clone();
            self.library.set_roots(roots);
            self.scanning = true;
            self.working = true;
            if self.browsing_ready {
                // Folders outside the new list leave Collections at once.
                self.collections = Collections::from_index(&self.index, &self.roots);
                self.navigation.folder = self.collections.restore(self.navigation.folder.as_deref());
            }
            self.refresh(cx.theme);
            cx.redraw();
        }
    }
    fn apply_pending(&mut self, cx: &mut EventCx) {
        if self.touching
            || self.menu.is_some()
            || self.details.is_some()
            || self.confirm.is_some()
            || self.clash.is_some()
            || self.name_sheet.is_some()
            || self.jump.is_some()
            || self.reader.is_some()
        {
            return;
        }
        if let Some(mut home) = self.pending_home.take()
            && {
                for (revision, change) in &self.changes {
                    if *revision > self.pending_home_revision {
                        change.apply_home(&mut home);
                    }
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
            for (revision, change) in &self.changes {
                if *revision > snapshot.revision {
                    change.apply(&mut snapshot.index);
                }
            }
            let started = std::time::Instant::now();
            cx.redraw();
            self.index = snapshot.index;
            self.index_revision = snapshot.revision;
            self.prune_changes();
            self.collections = Collections::from_index(&self.index, &self.roots);
            if let Some(search) = &mut self.search {
                search.cache.sync(&self.index);
            }
            let old_navigation = self.navigation.clone();
            let old_folder = self.navigation.folder.clone();
            // Initial metadata checkpoints may not yet contain the saved folder.
            if !snapshot.partial {
                self.navigation.folder = self.collections.restore(self.navigation.folder.as_deref());
                self.navigation.folder_pages.retain(|p, _| self.collections.folders.contains_key(p));
            }
            if old_folder != self.navigation.folder && !self.carrying() {
                self.end_selection(cx);
            }
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
                cx.refresh(self.screen, RefreshMode::Full);
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
            self.top.title = if self.home_active { "Home" } else { "Library" }.into();
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
            let collection = (!self.home_active && self.navigation.tab == Tab::Collections)
                .then(|| self.navigation.folder.clone())
                .flatten();
            let mut current = Button::new("This collection", ButtonKind::Secondary);
            current.enabled = collection.is_some();
            let mut keyboard = Keyboard::default();
            keyboard.visible = true;
            keyboard.place(self.screen, cx.theme);
            let mut field = TextField::new("Title, author or filename");
            field.focused = true;
            self.search = Some(SearchSession {
                field,
                folded: String::new(),
                scope: Scope::All,
                collection,
                page: 0,
                cache,
                all: Button::new("All books", ButtonKind::Primary),
                current,
                context: Rect::default(),
                keyboard,
            });
            self.search_action.icon = Icon::Close;
            self.top.title = "Search".into();
            self.pager.page = 0;
            self.refresh(cx.theme);
        }
    }

    fn edit_search(&mut self, input: &TextInput, cx: &mut EventCx) {
        let Some(search) = &mut self.search else { return };
        if matches!(input, TextInput::Submit | TextInput::Dismiss) {
            search.keyboard.visible = false;
            search.field.focused = false;
        } else if search.field.edit(input) {
            self.selection = None;
            let started = std::time::Instant::now();
            search.folded = search::fold(search.field.value().trim());
            self.pager.page = 0;
            self.refresh(cx.theme);
            log::debug!("search query: {} matches prepared in {:?}", self.books.len(), started.elapsed());
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
        self.library.pause(true);
        self.thumbnails.clear();
        let settings = self.store.settings();
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
        let dpi = cx.theme.dpi;
        let margin = (7.0 * dpi / 25.4).round() as u32;
        let layout = Layout {
            width: self.screen.w as u32,
            height: self.screen.h as u32,
            margins: [margin; 4],
            font_size: cx.theme.pt(settings.typography.size_pt) as u32,
        };
        self.worker.send(Command::Open {
            generation: self.generation,
            path,
            layout,
            typography: settings.typography,
            footer: settings.footer,
            position: saved.position,
        });
    }
    fn returned(&mut self, cx: &mut EventCx) {
        self.library.pause(false);
        self.reload_home();
        if self.pending_home.as_ref().is_none_or(|summary| !summary.complete) {
            self.library.refresh_home();
        }
        // Only the reader writes reading state while it is open, so its book
        // is the only cached state that can be stale.
        if let Some(r) = self.reader.take() {
            self.generation = r.generation();
            let id = r.book_id();
            self.states.insert(id.to_owned(), self.store.book(id));
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
        if !state.position.0.is_empty() && !state.hidden_from_continue && !state.finished {
            items.push((None, "Remove from continue reading".into()));
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
                self.states.insert(id.to_owned(), state);
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
            NameOperation::Create(parent) => files::create_folder(parent, &name),
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
        for (_, change) in &changes {
            change.apply_home(&mut self.home);
            if self.browsing_ready {
                change.apply(&mut self.index);
            }
            if let PathChange::Move(from, to) = change {
                self.navigation.move_path(from, to);
            }
        }
        self.changes.extend(changes);
        if self.browsing_ready {
            self.collections = Collections::from_index(&self.index, &self.roots);
            self.navigation.folder = self.collections.restore(self.navigation.folder.as_deref());
            self.navigation.folder_pages.retain(|p, _| self.collections.folders.contains_key(p));
            if let Some(search) = &mut self.search {
                search.cache.sync(&self.index);
            }
        }
        self.refresh(cx.theme);
        self.save_navigation();
        cx.refresh(self.screen, RefreshMode::Full);
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
                    self.states.remove(id);
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
        if let Err(e) = std::fs::remove_file(&book.path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("deleting {}: {e}", book.path.display());
            cx.toast(format!("Couldn't delete the book: {e}"), None);
            return;
        }
        log::info!("deleted {}", book.path.display());
        // The confirmation promises the reading progress goes too, and a
        // leftover sidecar would hide a folder this delete just emptied.
        let sidecar_error = files::delete_sidecar(&book.path).err();
        if let Some(e) = &sidecar_error {
            log::warn!("deleting sidecar of {}: {e:#}", book.path.display());
        }
        if let Err(e) = self.store.remove_book(&book.id) {
            log::warn!("removing reading state of {}: {e}", book.path.display());
        }
        let mut changes = self.library.remove_paths(vec![book.path.clone()]);
        if let Some(parent) = book.path.parent().filter(|p| files::is_empty(p)) {
            changes.extend(self.library.add_empty_folder(parent.to_owned()));
        }
        self.states.remove(&book.id);
        self.home.books.retain(|e| e.book.id != book.id);
        self.apply_changes(changes, cx);
        cx.toast(
            match sidecar_error {
                None => format!("Deleted “{}”", book.metadata.title),
                Some(_) => format!("Deleted “{}”, but couldn't remove its KOReader data", book.metadata.title),
            },
            None,
        );
    }
}
/// Above this many uncached books, one directory listing replaces the failed
/// opens of books never read; below it, single reads are cheaper.
const LISTED_STATES: usize = 32;
/// Caches the reading states of `books`. Returns the records read and whether
/// the state directory was listed.
fn load_states(store: &Store, states: &mut HashMap<String, BookState>, books: &[Book]) -> (usize, bool) {
    let mut missing: Vec<_> = books.iter().map(|b| b.id.as_str()).filter(|id| !states.contains_key(*id)).collect();
    missing.sort_unstable();
    missing.dedup();
    let saved = if missing.len() > LISTED_STATES { store.book_state_ids() } else { None };
    let mut reads = 0;
    for id in missing {
        let state = if saved.as_ref().is_none_or(|saved| saved.contains(id)) {
            reads += 1;
            store.book(id)
        } else {
            BookState::default()
        };
        states.insert(id.to_owned(), state);
    }
    (reads, saved.is_some())
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
    if let Cover::Ready(key) = &book.cover {
        let cache_key = (key.clone(), rect.w, rect.h);
        let failure = (book.id.clone(), key.clone());
        if !cache.images.contains_key(&cache_key) && !cache.failed.contains(&failure) {
            match covers::load(&store.cover_path(key)) {
                Ok(image) => {
                    // Refresh retains only visible keys; this only guards the bound.
                    if cache.images.len() >= Thumbnails::LIMIT {
                        cache.images.clear();
                    }
                    let image = image::imageops::resize(
                        &image,
                        rect.w as u32,
                        rect.h as u32,
                        image::imageops::FilterType::Triangle,
                    );
                    cache.images.insert(cache_key.clone(), image);
                }
                Err(e) => {
                    log::warn!("cover cache for {} failed to load: {e:#}", book.id);
                    cache.failed.insert(failure);
                    cache.requests.push(book.id.clone());
                }
            }
        }
        if let Some(image) = cache.images.get(&cache_key) {
            cx.canvas.blit_gray(rect, image.as_raw(), image.width() as usize);
            return;
        }
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
        let paste_enabled = self.paste_reason().is_none();
        if let Some(selection) = &mut self.selection {
            selection.paste.enabled = paste_enabled;
            selection.draw(cx);
        } else {
            self.top.draw(cx);
            self.rescan.draw(cx);
            self.exit.draw(cx);
            self.search_action.draw(cx);
            self.settings_action.draw(cx);
        }
        if let Some(search) = &mut self.search {
            search.field.draw(cx);
            search.all.draw(cx);
            search.current.draw(cx);
            let context = match &search.scope {
                Scope::All => {
                    if search.collection.is_some() {
                        "All books · every library folder".into()
                    } else {
                        "All books · choose a collection to narrow scope".into()
                    }
                }
                Scope::Collection(folder) => format!("Collection · {} (including subfolders)", folder.display()),
            };
            if !search.context.is_empty() {
                let context = path_tail(cx.text, &context, t.caption, search.context.w);
                cx.line(&context, t.caption.gray(INK2), search.context, HAlign::Left);
            }
        } else {
            self.tabs.draw(cx);
            if self.carrying() {
                cx.canvas.fade_rect(self.tabs.rect());
            }
        }
        if !self.is_home() && self.search.is_none() && self.navigation.tab == Tab::Collections {
            if self.navigation.folder.is_some() {
                self.back.draw(cx);
                self.new_folder.draw(cx);
            }
            let title =
                self.navigation.folder.as_deref().map(collections::name).unwrap_or_else(|| "Library folders".into());
            cx.line(&title, t.title, Rect { h: t.mm(6.0), ..self.path_rect }, HAlign::Left);
            let path = self
                .navigation
                .folder
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "Choose a folder".into());
            let path = path_tail(cx.text, &path, t.caption, self.path_rect.w);
            cx.line(
                &path,
                t.caption.gray(INK2),
                Rect { y: self.path_rect.y + t.mm(6.0), h: t.mm(6.0), ..self.path_rect },
                HAlign::Left,
            );
        }
        if let Some(book) = self.continue_id.as_ref().filter(|_| self.is_home()).and_then(|id| self.book(id)).cloned() {
            let r = self.continue_rect;
            let cover = Rect::new(r.x, r.y, r.h * 2 / 3, r.h);
            draw_cover(cx, &book, cover, &self.store, &mut self.thumbnails);
            cx.hint(cover, RefreshMode::Ui);
            if self.continue_press.is_down() {
                cx.canvas.stroke_rounded_rect(r, t.radius, t.border as f32, INK);
                cx.hint(r, RefreshMode::Fast);
            }
            let x = cover.right() + t.mm(4.0);
            let w = r.right() - x;
            cx.section_label("Continue reading", Rect::new(x, r.y, w, t.mm(4.0)));
            cx.line(&book.metadata.title, t.title, Rect::new(x, r.y + t.mm(4.0), w, t.mm(7.0)), HAlign::Left);
            cx.line(
                &book.metadata.authors,
                t.body.gray(INK2),
                Rect::new(x, r.y + t.mm(11.0), w, t.mm(5.0)),
                HAlign::Left,
            );
            let progress = self.home.resume().map_or(0, |e| e.status.progress).min(10000);
            let bar = Rect::new(x, r.bottom() - t.mm(3.0), (w - t.mm(14.0)).max(1), t.hairline);
            cx.fill(bar, LINE);
            cx.fill(Rect { w: (bar.w as i64 * progress as i64 / 10000) as i32, ..bar }, INK);
            cx.line(
                &format!("{}%", progress / 100),
                t.caption.gray(INK2),
                Rect::new(bar.right() + t.mm(2.0), bar.y - t.mm(2.0), t.mm(12.0), t.mm(4.0)),
                HAlign::Right,
            );
            cx.hline(r.bottom() + t.mm(2.0), r.x, r.right(), t.hairline, LINE);
        }
        cx.line(
            &if self.is_home() {
                if self.books.is_empty() { String::new() } else { "Recent books".into() }
            } else if self.search.is_some() {
                counted(self.books.len(), "result")
            } else if self.navigation.tab == Tab::AllBooks {
                format!("All books  {}", self.books.len())
            } else {
                format!("{} · {}", counted(self.folders.len(), "folder"), counted(self.books.len(), "book"))
            },
            t.heading,
            Rect {
                w: if self.is_home() { self.header.w } else { self.sort.rect().x - self.header.x - t.mm(2.0) },
                ..self.header
            },
            HAlign::Left,
        );
        if !self.is_home() {
            self.sort.draw(cx);
        }
        for (i, cell) in self.grid.cells() {
            if let Some(path) = self.folders.get(i) {
                let folder = &self.collections.folders[path];
                let r = Rect::new(cell.x, cell.y, cell.w, (cell.h - t.mm(10.0)).min(cell.w * 3 / 2));
                let outline = Rect::new(r.x + t.mm(1.0), r.y + r.h / 4, r.w - t.mm(2.0), r.h / 2);
                cx.icons.draw(cx.canvas, Icon::Folder, outline, INK);
                cx.line(
                    &folder_subtitle(folder),
                    t.body.gray(INK2),
                    Rect::new(r.x, outline.bottom() + t.mm(1.0), r.w, t.mm(5.0)),
                    HAlign::Center,
                );
                cx.line(
                    &collections::name(path),
                    t.button,
                    Rect::new(cell.x, r.bottom() + t.mm(1.0), cell.w, t.mm(4.0)),
                    HAlign::Left,
                );
                let context = path.parent().map(|p| p.display().to_string()).unwrap_or_default();
                let context = path_tail(cx.text, &context, t.caption, cell.w);
                cx.line(
                    &context,
                    t.caption.gray(INK2),
                    Rect::new(cell.x, r.bottom() + t.mm(5.0), cell.w, t.mm(4.0)),
                    HAlign::Left,
                );
                if self.grid.pressed() == Some(i) {
                    cx.canvas.stroke_rounded_rect(r, t.radius, (2 * t.border) as f32, INK);
                    cx.hint(r, RefreshMode::Fast);
                }
                self.draw_tick(cx, Target::Folder(path.clone()), cell);
                continue;
            }
            let book = &self.books[i - self.folders.len()];
            let text_h = t.mm(10.0);
            let cover_h = (cell.h - text_h).min(cell.w * 3 / 2).max(1);
            let cover_w = (cover_h * 2 / 3).max(1);
            let cover = Rect::new(cell.x + (cell.w - cover_w) / 2, cell.y, cover_w, cover_h);
            draw_cover(cx, book, cover, &self.store, &mut self.thumbnails);
            if self.grid.pressed() == Some(i) {
                cx.canvas.stroke_rounded_rect(cover, t.radius, (2 * t.border) as f32, INK);
                cx.hint(cover, RefreshMode::Fast);
            } else if self.selection.is_none() {
                cx.hint(cover, RefreshMode::Ui);
            }
            // While selecting, the cover takes the default grayscale mode; a
            // Ui hint would override the fast tick box drawn over its corner.
            let y = cover.bottom() + t.mm(1.0);
            cx.line(&book.metadata.title, t.button, Rect::new(cell.x, y, cell.w, t.mm(4.0)), HAlign::Left);
            let state = self.states.get(&book.id).cloned().unwrap_or_default();
            let label = if self.is_home() { String::new() } else { status(&state) };
            let status_w = (cx.text.width(&label, t.caption).ceil() as i32 + t.mm(1.0)).min(cell.w * 2 / 3);
            cx.line(
                &book.metadata.authors,
                t.caption.gray(INK2),
                Rect::new(cell.x, y + t.mm(4.0), cell.w - status_w - t.mm(1.0), t.mm(4.0)),
                HAlign::Left,
            );
            cx.line(
                &label,
                t.caption.gray(INK2),
                Rect::new(cell.right() - status_w, y + t.mm(4.0), status_w, t.mm(4.0)),
                HAlign::Right,
            );
            self.draw_tick(cx, Target::Book(book.id.clone()), cell);
        }
        if !self.is_home() && self.books.is_empty() && self.folders.is_empty() {
            let message = if let Some(search) = &self.search {
                if search.folded.is_empty() {
                    "No books in this scope.".into()
                } else {
                    "No matching books. Try another title, author or filename.".into()
                }
            } else if self.navigation.tab == Tab::Collections && self.navigation.folder.is_some() {
                "This folder is empty".into()
            } else if self.scanning {
                format!(
                    "Finding books in {}…",
                    self.roots.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(" • ")
                )
            } else {
                format!(
                    "No EPUB books found in {}.\nAdd books, then tap Rescan.",
                    self.roots.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(" • ")
                )
            };
            cx.paragraph(
                &message,
                t.body.gray(INK2),
                Rect::new(t.margin, self.header.bottom() + t.mm(10.0), self.screen.w - 2 * t.margin, t.mm(30.0)),
                HAlign::Left,
            );
        }
        let message =
            self.paste_reason().or_else(|| self.errors.first().map(String::as_str)).unwrap_or(if self.scanning {
                "Scanning library…"
            } else if self.working {
                "Preparing covers…"
            } else {
                ""
            });
        let battery_w = if cx.battery.is_some() { t.mm(32.0) } else { 0 };
        cx.line(message, t.caption.gray(INK2), Rect { w: self.status.w - battery_w, ..self.status }, HAlign::Left);
        cx.battery(self.status);
        if !self.is_home() {
            self.pager.draw(cx);
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
        if !self.thumbnails.requests.is_empty() {
            self.library.repair(std::mem::take(&mut self.thumbnails.requests));
        }
    }
    fn on_gesture(&mut self, g: Gesture, cx: &mut EventCx) -> Action {
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
        if let Some(sheet) = &mut self.name_sheet {
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
                            if let Some(book) = self.book(&id) {
                                self.details = Some(Details::new(cx.theme, self.screen, book, &self.store.book(&id)));
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
            if self.selection.is_none() && self.search_action.on_gesture(&g) {
                self.toggle_search(cx);
                self.apply_pending(cx);
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
                    self.pager.page = 0;
                    self.refresh(cx.theme);
                    self.apply_pending(cx);
                    return Action::None;
                }
            }
            if self.selection.is_none() && self.exit.on_gesture(&g) {
                return Action::Pop;
            }
            if !self.carrying()
                && self.search.is_none()
                && let Some(selected) = self.tabs.on_gesture(&g)
            {
                self.end_selection(cx);
                if !self.is_home() {
                    self.navigation.set_page(self.pager.page);
                    self.navigation.set_sort(self.selected_sort());
                }
                self.home_active = selected == 0;
                if !self.home_active {
                    self.navigation.tab = if selected == 1 { Tab::Collections } else { Tab::AllBooks };
                    self.top.title = "Library".into();
                    self.sort.selected = match self.navigation.sort(self.store.settings().library_sort) {
                        Sort::Recent => 0,
                        Sort::Title => 1,
                        Sort::Author => 2,
                    };
                }
                self.pager.page = self.navigation.page();
                self.thumbnails.clear();
                self.refresh(cx.theme);
                self.save_navigation();
                cx.refresh(self.screen, RefreshMode::Full);
                self.apply_pending(cx);
                return Action::None;
            }
            if !self.is_home() && self.search.is_none() && self.navigation.tab == Tab::Collections {
                if let Some(path) = self.navigation.folder.clone()
                    && self.new_folder.on_gesture(&g)
                {
                    self.start_name(NameOperation::Create(path), cx.theme);
                    return Action::None;
                }
                if self.navigation.folder.is_some() && self.back.on_gesture(&g) {
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
            if self.selection.is_none() && self.rescan.on_gesture(&g) {
                self.library.rescan();
                self.scanning = true;
                self.working = true;
            }
            if !self.is_home() && self.sort.on_gesture(&g).is_some() {
                let mut settings = self.store.settings();
                settings.library_sort = self.selected_sort();
                if let Err(e) = self.store.save_settings(&settings) {
                    cx.toast(format!("Couldn't save sort: {e}"), None);
                }
                self.pager.page = 0;
                self.refresh(cx.theme);
                self.save_navigation();
                cx.refresh(self.screen, RefreshMode::Full);
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
    fn busy(&self) -> bool {
        self.reader.as_ref().map_or(self.working, Screen::busy)
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
        self.reader.as_ref().and_then(Screen::deadline)
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
        self.reader.as_mut().map_or(Action::None, |r| r.on_tick(cx))
    }
    fn on_background(&mut self, cx: &mut EventCx) {
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
            self.scanning = status.scanning;
            self.working = status.working;
            for error in &status.errors {
                if !self.errors.contains(error) {
                    log::warn!("{error}");
                    cx.toast(error.clone(), None);
                }
            }
            self.errors = status.errors;
        }
        // Only changed content arrives here; it waits for touches and overlays.
        if let Some(snapshot) = self.library.take() {
            self.pending = Some(snapshot);
            self.apply_pending(cx);
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
        self.persist_navigation();
        self.touching = false;
        if let Some(r) = &mut self.reader {
            r.on_sleep();
        }
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
        match covers::sleep_cover(&book, width, height) {
            Ok(image) => Some(image),
            Err(e) => {
                log::debug!("sleep cover unavailable: {e:#}");
                None
            }
        }
    }
    fn on_quit(&mut self) -> anyhow::Result<()> {
        self.persist_paste();
        self.persist_navigation();
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
            books.navigation.tab = Tab::AllBooks;
            books.refresh(&t);
            let first = Target::Book(books.books[0].id.clone());
            books.open_menu(books.books[0].id.clone(), Point::new(190, 650), &t);
            let menu = &books.menu.as_ref().unwrap().1;
            let row = menu.items.iter().position(|(_, label)| label == "Select").unwrap();
            let p = Point::new(menu.rect().center().x, menu.rect().y + t.mm(1.0) + row as i32 * t.row + t.row / 2);
            tap(&mut books, p, cx);
            assert!(books.selection.as_ref().unwrap().items.contains(&first));
            let all = books.selection.as_ref().unwrap().all.rect().center();
            tap(&mut books, all, cx);
            assert_eq!(books.selection.as_ref().unwrap().items.len(), 22);
            books.go_to_page(2, cx);
            assert_eq!(books.selection.as_ref().unwrap().items.len(), 22);
            let p = books.grid.cells()[0].1.center();
            tap(&mut books, p, cx);
            assert_eq!(books.selection.as_ref().unwrap().items.len(), 21);
            books.on_gesture(Gesture::Down(p), cx);
            books.on_gesture(Gesture::LongPress(p), cx);
            books.on_gesture(Gesture::End(p), cx);
            assert!(books.menu.is_none() && books.reader.is_none());
            assert_eq!(books.selection.as_ref().unwrap().items.len(), 21);
            let tab = Point::new(books.tabs.rect().x + books.tabs.rect().w / 2, books.tabs.rect().center().y);
            tap(&mut books, tab, cx);
            assert!(books.selection.is_none());
            books.navigate(None, cx);
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
            books.navigation.tab = Tab::Collections;
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
            books.navigation.tab = Tab::Collections;
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
        books.navigation.tab = Tab::AllBooks;
        books.refresh(&t);
        let cell = books.grid.cells()[0].1;
        let cover = Point::new(cell.center().x, cell.y + cell.h / 3);
        let tick = Point::new(cell.right() - t.mm(3.0), cell.y + t.mm(3.0));
        let mut draw = |books: &mut Books| {
            let mut hints = Vec::new();
            books.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut hints));
            let mode = |p: Point| hints.iter().filter(|h| h.rect.contains(p)).map(|h| h.mode).max();
            (mode(cover), mode(tick))
        };
        assert_eq!(draw(&mut books), (Some(RefreshMode::Ui), Some(RefreshMode::Ui)));
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
            books.navigation.tab = Tab::Collections;
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
            books.collections = Collections::from_index(&books.index, &books.roots);
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
            books.navigation.tab = Tab::AllBooks;
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
    fn all_books_has_no_resume_card_and_shows_two_full_rows() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("all-books-grid")?;
        let id = store.library().books[0].id.clone();
        let state = BookState {
            position: crate::document::Position("exact".into()),
            last_opened: 5,
            progress: 1234,
            ..Default::default()
        };
        store.save_book(&id, &state)?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = browsing_books(&t, screen, Rc::new(Worker::stub()), store);
        books.navigation.tab = Tab::AllBooks;
        books.refresh(&t);
        assert!(books.continue_id.is_none(), "a resumable book stays on Home only");
        assert_eq!(books.continue_rect, Rect::default());
        assert_eq!(books.books.len(), 22);
        assert_eq!(books.grid.cells().len(), 8);
        assert_eq!(books.pager.pages, 3);
        assert!(books.header.y < t.top_bar + t.target + t.mm(6.0), "the header follows the tabs directly");
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn page_jump_moves_browsing_and_search_pages_and_defers_updates() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("page-jump")?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = browsing_books(&t, screen, Rc::new(Worker::stub()), store.clone());
        books.navigation.tab = Tab::AllBooks;
        books.refresh(&t);
        assert_eq!(books.pager.pages, 3);
        let counter = books.pager.rect().center();
        let last = |books: &Books| {
            let r = books.jump.as_ref().unwrap().slider.rect();
            Point::new(r.right() - 1, r.center().y)
        };
        with_cx(&t, screen, |cx| {
            // Tapping above the sheet closes it without moving.
            tap(&mut books, counter, cx);
            assert!(books.jump.is_some());
            assert_eq!(books.page_start(2), books.books[16].metadata.title);
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
            assert_eq!((books.pager.page, books.grid.page), (2, 2));
            // Navigation is written once the frame showing it is presented.
            assert_ne!(store.navigation().page(), 2);
            books.after_present();
            assert_eq!(store.navigation().page(), 2);
            // Search results keep their own page.
            books.toggle_search(cx);
            books.search.as_mut().unwrap().keyboard.visible = false;
            books.pager.page = 0;
            books.refresh(cx.theme);
            assert_eq!(books.pager.pages, 3);
            tap(&mut books, counter, cx);
            let p = last(&books);
            tap(&mut books, p, cx);
            assert_eq!(books.search.as_ref().unwrap().page, 2);
        });
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
        books.states.get_mut(&id).unwrap().last_opened = 10;
        books.continue_id = None;
        books.navigation.tab = Tab::Collections;
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
    fn folder_views_read_no_states_and_recent_order_lists_saved_states_once() -> anyhow::Result<()> {
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
        store.save_navigation(&Navigation { tab: Tab::Collections, ..Default::default() })?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = browsing_books(&t, screen, Rc::new(Worker::stub()), store.clone());
        // The Collections root shows only folders: no book needs a state.
        assert!(!books.folders.is_empty() && books.books.is_empty());
        assert!(books.states.is_empty());
        // The Home summary holds the newest books, so the sleep cover still
        // finds the last one without any cached state.
        assert!(books.sleep_cover(600, 800).is_some());
        // All books sorts by recency across the whole library. The 61 states
        // come from one listing, and only the two saved records are read.
        let (reads, listed) = load_states(&store, &mut HashMap::new(), &books.index.books);
        assert_eq!((reads, listed), (2, true));
        books.navigation.tab = Tab::AllBooks;
        books.refresh(&t);
        assert_eq!(books.states.len(), books.index.books.len());
        assert_eq!(books.books.iter().take(2).map(|b| b.id.as_str()).collect::<Vec<_>>(), ["5", "40"]);
        assert_eq!(books.states["40"].progress, 2500);
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
            books.navigation.tab = Tab::Collections;
            books.tabs.selected = 1;
            books.navigate(Some("/books/Fiction".into()), cx);
            let before = books.navigation.clone();
            books.toggle_search(cx);
            assert_eq!(books.search.as_ref().unwrap().scope, Scope::All);
            assert_eq!(books.books.len(), 22);
            assert!(books.on_text_input(&TextInput::Insert("Title".into()), cx));
            assert!(books.on_text_input(&TextInput::Submit, cx));
            let current = books.search.as_ref().unwrap().current.rect().center();
            tap(&mut books, current, cx);
            assert_eq!(books.books.len(), 20, "collection search includes nested books");
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
            // A folder remembered in the other tab is not the current collection.
            books.navigation.tab = Tab::AllBooks;
            books.pager.page = 1;
            books.refresh(&t);
            books.toggle_search(cx);
            assert!(!books.search.as_ref().unwrap().current.enabled);
            books.toggle_search(cx);
            assert_eq!(books.pager.page, 1);
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
            books.navigation.tab = Tab::Collections;
            books.navigate(Some("/books/Fiction".into()), cx);
            books.toggle_search(cx);
            books.edit_search(&TextInput::Insert("Needle".into()), cx);
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
            assert_eq!(books.navigation.tab, Tab::Collections);
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
                assert_eq!(menu.items.len(), if can_remove { 7 } else { 6 });
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
        let file = root.join("doomed.epub");
        std::fs::write(&file, b"not really an epub")?;
        let mut index = store.library();
        index.books[0].path = file.clone();
        index.books[0].cover = Cover::Ready("doomedcover".into());
        store.save_library(&index)?;
        std::fs::create_dir_all(store.cover_path("x").parent().unwrap())?;
        std::fs::write(store.cover_path("doomedcover"), b"png")?;
        store.save_book("0", &BookState { progress: 1234, ..Default::default() })?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = browsing_books(&t, screen, Rc::new(Worker::stub()), store.clone());
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
        while store.library().books.iter().any(|b| b.path == file) || store.cover_path("doomedcover").exists() {
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
    fn collections_share_state_and_restore_tab_folder_pages_after_reader_and_restart() -> anyhow::Result<()> {
        let (store, root) = collection_fixture("resume")?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let worker = Rc::new(Worker::stub());
        let mut books = browsing_books(&t, screen, worker.clone(), store.clone());
        with_cx(&t, screen, |cx| {
            books.pager.page = 1;
            books.save_navigation();
            let tab = books.tabs.rect();
            tap(&mut books, Point::new(tab.x + tab.w / 2, tab.center().y), cx);
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
            let tab = books.tabs.rect();
            tap(&mut books, Point::new(tab.x + tab.w * 5 / 6, tab.center().y), cx);
            assert_eq!(books.pager.page, 1, "All books has its own page");
            assert_eq!(books.states[&id].position.0, "same-anchor");
            tap(&mut books, Point::new(tab.x + tab.w / 2, tab.center().y), cx);
            let back = books.back.rect().center();
            tap(&mut books, back, cx);
            assert_eq!(books.navigation.folder, Some("/books/Fiction".into()));
            let p = books.grid.cells()[0].1.center();
            tap(&mut books, p, cx);
            assert_eq!(books.pager.page, 1, "Back and reenter retain the folder page");
            books.after_present();
        });
        drop(books);
        let restored = browsing_books(&t, screen, worker, store.clone());
        assert_eq!(restored.navigation.tab, Tab::Collections);
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
            books.navigation.tab = Tab::Collections;
            books.tabs.selected = 1;
            books.navigate(Some("/books/Fiction/Nested".into()), cx);
            books.pager.page = 2;
            books.save_navigation();
            books.after_present();
            let old = books.index.clone();
            books.pending = Some(Snapshot { partial: true, ..Default::default() });
            books.apply_pending(cx);
            assert_eq!(books.navigation.folder, Some("/books/Fiction/Nested".into()));
            assert_eq!(store.navigation().page(), 2, "incomplete checkpoints cannot erase saved pages");
            books.pending = Some(Snapshot { index: old, ..Default::default() });
            books.apply_pending(cx);
            assert_eq!(books.pager.page, 2);
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
            assert_eq!(books.navigation.folder, Some("/books".into()));
            assert_eq!(books.pager.page, 0);
            assert!(!store.navigation().folder_pages.contains_key(std::path::Path::new("/books/Fiction/Nested")));
            books.pending = Some(Snapshot::default());
            books.apply_pending(cx);
            assert!(books.navigation.folder.is_none());
            assert_eq!(books.navigation.tab, Tab::Collections);
        });
        drop(books);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn decoded_covers_stay_on_visible_collection_pages() -> anyhow::Result<()> {
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
        let mut books = browsing_books(&t, screen, Rc::new(Worker::stub()), store);
        books.navigation.tab = Tab::Collections;
        books.navigation.folder = Some("/books/Fiction/Nested".into());
        let mut canvas = crate::gfx::Canvas::new(1272, 1696);
        let mut text = crate::gfx::TextRenderer::new();
        let mut icons = crate::gfx::IconRenderer::default();
        for page in 0..3 {
            books.pager.page = page;
            books.refresh(&t);
            books.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut Vec::new()));
            let visible: std::collections::HashSet<_> =
                books.grid.cells().iter().map(|(i, _)| books.grid_book(*i).unwrap().id.clone()).collect();
            assert_eq!(books.thumbnails.images.len(), visible.len());
            assert!(books.thumbnails.images.keys().all(|(key, _, _)| visible.contains(key)));
        }
        books.navigation.folder = None;
        books.refresh(&t);
        assert!(books.thumbnails.images.is_empty(), "folder-only pages retain no decoded covers");
        drop(books);
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
                // The language key is at the left of the last row.
                let p = Point::new(t.margin + t.target / 2, screen.bottom() - t.target / 3);
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
            books.navigation.tab = Tab::Collections;
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
            books.navigation.tab = Tab::Collections;
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
        let mut books = Books::with_store(&t, screen, Rc::new(Worker::stub()), store.clone());
        let on_home = |books: &Books| books.home.books.iter().any(|e| e.book.id == book.id);
        with_cx(&t, screen, |cx| {
            assert!(on_home(&books));
            books.delete(&book.id, cx);
            assert!(!books.browsing_ready && books.index.books.is_empty(), "Home deletes don't load the index");
            assert!(!on_home(&books));
            // The stub worker never saves, like a worker that hasn't caught up.
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
        store.save_library(&LibraryIndex { books: (0..10).map(book).collect(), ..Default::default() })?;
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
        };
        books.on_gesture(Gesture::Down(point), &mut cx);
        books.pending =
            Some(Snapshot { index: LibraryIndex { books: vec![book(1)], ..Default::default() }, ..Default::default() });
        books.apply_pending(&mut cx);
        assert_eq!(books.books.len(), 10);
        books.on_gesture(Gesture::LongPress(point), &mut cx);
        books.on_gesture(Gesture::End(point), &mut cx);
        assert!(books.menu.is_some());
        assert_eq!(books.books.len(), 10);
        let menu_rect = books.menu.as_ref().unwrap().1.rect();
        let details = Point::new(menu_rect.center().x, menu_rect.y + t.mm(1.0) + t.row + t.row / 2);
        books.on_gesture(Gesture::Down(details), &mut cx);
        books.on_gesture(Gesture::Tap(details), &mut cx);
        assert!(books.details.is_some());
        assert_eq!(books.books.len(), 10);
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
}
