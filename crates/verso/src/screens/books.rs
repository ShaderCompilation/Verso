//! Paginated library, engine-independent discovery, and the existing reader flow.
use crate::document::{
    FontFace, Layout,
    thread::{Command, Reply, Worker},
};
use crate::geom::{Point, Rect};
use crate::gfx::{Family, HAlign, Icon};
use crate::library::{
    self, Book, Cover, LibraryIndex, Sort,
    collections::{self, Collections, Navigation, Tab},
    covers,
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

pub struct Books {
    screen: Rect,
    worker: Rc<Worker>,
    library: LibraryWorker,
    store: Store,
    roots: Vec<PathBuf>,
    index: LibraryIndex,
    browsing_ready: bool,
    home_active: bool,
    home: Summary,
    pending_home: Option<Summary>,
    books: Vec<Book>,
    collections: Collections,
    folders: Vec<PathBuf>,
    navigation: Navigation,
    tabs: Segmented,
    back: IconButton,
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
    menu: Option<(String, PopupMenu)>,
    details: Option<Details>,
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
    fn with_store(t: &Theme, screen: Rect, worker: Rc<Worker>, store: Store) -> Self {
        let roots = store.library_folders();
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
            index: LibraryIndex::default(),
            browsing_ready: false,
            home_active: true,
            home,
            pending_home: None,
            books: Vec::new(),
            collections: Collections::default(),
            folders: Vec::new(),
            navigation,
            tabs,
            back: IconButton::new(Icon::ArrowLeft),
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
            details: None,
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
        self.index = snapshot.map_or_else(|| self.store.library(), |s| s.index);
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
        let before = self.states.len();
        for b in &self.index.books {
            self.states.entry(b.id.clone()).or_insert_with(|| self.store.book(&b.id));
        }
        log::debug!(
            "startup stage: cached states reads={} elapsed_us={}",
            self.states.len() - before,
            started.elapsed().as_micros()
        );
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
        let sort = self.selected_sort();
        library::sort_books(&mut self.books, &self.states, sort);
        // The resume card lives on Home only; browsing tabs are plain grids.
        self.continue_id = None;
        self.continue_rect = Rect::default();
        let y = t.top_bar + t.mm(2.0);
        self.tabs.place(Rect::new(t.margin, y, self.screen.w - 2 * t.margin, t.target));
        let y = if let Some(search) = &mut self.search {
            search.field.place(Rect::new(t.margin, y, self.screen.w - 2 * t.margin, t.target));
            let scope_y = y + t.target + t.mm(2.0);
            let w = (self.screen.w - 2 * t.margin - t.mm(2.0)) / 2;
            search.all.place(Rect::new(t.margin, scope_y, w, t.target), t);
            search.current.place(Rect::new(t.margin + w + t.mm(2.0), scope_y, w, t.target), t);
            search.all.kind = if search.scope == Scope::All { ButtonKind::Primary } else { ButtonKind::Secondary };
            search.current.kind = if search.scope == Scope::All { ButtonKind::Secondary } else { ButtonKind::Primary };
            search.context =
                Rect::new(t.margin, scope_y + t.target + t.mm(1.0), self.screen.w - 2 * t.margin, t.mm(6.0));
            search.context.bottom() + t.mm(2.0)
        } else {
            y + t.target + t.mm(3.0)
        };
        self.path_rect = if in_collections {
            self.back.place_at(Point::new(t.margin + t.target / 2, y + t.target / 2), t);
            Rect::new(
                t.margin + t.target + t.mm(2.0),
                y,
                self.screen.w - 2 * t.margin - t.target - t.mm(2.0),
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
        let cols = ((self.screen.w - 2 * t.margin) / t.mm(22.0)).clamp(1, 4) as usize;
        let rows = ((self.status.y - top) / t.mm(32.0)).clamp(1, 2) as usize;
        self.grid = Grid::new(self.folders.len() + self.books.len(), cols, rows);
        self.grid.place(
            Rect::new(t.margin, top, self.screen.w - 2 * t.margin, (self.status.y - top).max(t.target)),
            t.mm(3.5),
            t.mm(3.0),
        );
        self.pager.set(self.pager.page, self.grid.paging().pages());
        self.grid.page = self.pager.page;
        if let Some(search) = &mut self.search {
            search.page = self.pager.page;
        }
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
        self.continue_id = self.home.resume().map(|e| e.book.id.clone());
        self.continue_rect = if self.continue_id.is_some() {
            Rect::new(t.margin, y, self.screen.w - 2 * t.margin, t.mm(37.0))
        } else {
            Rect::default()
        };
        let y = if self.continue_id.is_some() { self.continue_rect.bottom() + t.mm(6.0) } else { y };
        self.header = Rect::new(t.margin, y, self.screen.w - 2 * t.margin, t.mm(6.0));
        self.folders.clear();
        let cols = ((self.screen.w - 2 * t.margin) / t.mm(22.0)).clamp(1, 4) as usize;
        self.books = self.home.recents(cols).iter().map(|e| e.book.clone()).collect();
        self.status = Rect::new(t.margin, self.screen.bottom() - t.mm(10.0), self.screen.w - 2 * t.margin, t.mm(6.0));
        self.grid = Grid::new(self.books.len(), cols, 1);
        self.grid.place(
            Rect::new(
                t.margin,
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
    fn save_navigation(&mut self, cx: &mut EventCx) {
        if let Some(search) = &mut self.search {
            search.page = self.pager.page;
            return;
        }
        if self.is_home() {
            return;
        }
        self.navigation.set_page(self.pager.page);
        self.navigation.set_sort(self.selected_sort());
        if let Err(e) = self.store.save_navigation(&self.navigation) {
            cx.toast(format!("Couldn't save library navigation: {e}"), None);
        }
    }
    /// Shows another page of the browsing grid: arrows, swipes and the page jump.
    fn go_to_page(&mut self, page: usize, cx: &mut EventCx) {
        self.pager.set(page, self.pager.pages);
        self.grid.page = self.pager.page;
        self.thumbnails.clear();
        self.prioritize();
        self.save_navigation(cx);
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
        let started = std::time::Instant::now();
        self.home_active = false;
        self.prepare_browsing();
        self.navigation.set_page(self.pager.page);
        self.navigation.folder = folder;
        self.pager.page = self.navigation.page();
        self.thumbnails.clear();
        self.refresh(cx.theme);
        self.save_navigation(cx);
        cx.refresh(self.screen, RefreshMode::Full);
        log::info!("collection switch {:?}: prepared in {:?}", self.navigation.folder, started.elapsed());
    }
    fn apply_pending(&mut self, cx: &mut EventCx) {
        if self.touching
            || self.menu.is_some()
            || self.details.is_some()
            || self.jump.is_some()
            || self.reader.is_some()
        {
            return;
        }
        if let Some(home) = self.pending_home.take()
            && self.home != home
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
        if let Some(snapshot) = self.pending.take() {
            let started = std::time::Instant::now();
            cx.redraw();
            self.index = snapshot.index;
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
            let page = self.pager.page;
            self.pager.page = self.search.as_ref().map_or_else(|| self.navigation.page(), |s| s.page);
            self.refresh(cx.theme);
            if !snapshot.partial {
                if self.search.is_none() {
                    self.navigation.set_page(self.pager.page);
                }
                if old_navigation != self.navigation
                    && let Err(e) = self.store.save_navigation(&self.navigation)
                {
                    cx.toast(format!("Couldn't save library navigation: {e}"), None);
                }
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
        if self.search.take().is_some() {
            self.search_action.icon = Icon::Search;
            self.top.title = if self.home_active { "Home" } else { "Library" }.into();
            self.pager.page = self.navigation.page();
            self.refresh(cx.theme);
            self.save_navigation(cx);
        } else {
            self.save_navigation(cx);
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
            let mut field = TextField::default();
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
    fn open(&mut self, id: String, cx: &mut EventCx) {
        if let Some(error) = &self.engine_error {
            cx.toast(error.clone(), None);
            return;
        }
        let Some(book) = self.book(&id) else { return };
        let path = book.path.clone();
        self.store.select_home_book(book);
        self.save_navigation(cx);
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
        self.pending_home = self.store.home_summary();
        if self.pending_home.as_ref().is_none_or(|summary| !summary.complete) {
            self.library.refresh_home();
        }
        if let Some(r) = self.reader.take() {
            self.generation = r.generation();
        }
        for b in &self.books {
            self.states.insert(b.id.clone(), self.store.book(&b.id));
        }
        self.apply_pending(cx);
        self.refresh(cx.theme);
        self.save_navigation(cx);
        cx.refresh(self.screen, RefreshMode::Full);
    }
    fn open_menu(&mut self, id: String, anchor: Point, t: &Theme) {
        let state = self.store.book(&id);
        let mut menu = PopupMenu::open(
            vec![
                (Some(Icon::BookOpen), "Open".into()),
                (Some(Icon::Info), "Details".into()),
                (Some(Icon::Check), "Mark finished".into()),
                (None, "Remove from continue reading".into()),
            ],
            anchor,
            self.screen,
            t,
        );
        menu.set_enabled(2, !state.finished);
        menu.set_enabled(3, !state.position.0.is_empty() && !state.hidden_from_continue && !state.finished);
        self.menu = Some((id, menu));
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
                self.pending_home = self.store.home_summary();
                self.library.refresh_home();
                self.apply_pending(cx);
                self.refresh(cx.theme);
                cx.toast(if finished { "Marked finished" } else { "Removed from continue reading" }, None);
            }
            Err(e) => cx.toast(format!("Couldn't save book status: {e}"), None),
        }
    }
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
        self.top.draw(cx);
        self.rescan.draw(cx);
        self.exit.draw(cx);
        self.search_action.draw(cx);
        self.settings_action.draw(cx);
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
            let context = path_tail(cx.text, &context, t.caption, search.context.w);
            cx.line(&context, t.caption.gray(INK2), search.context, HAlign::Left);
        } else {
            self.tabs.draw(cx);
        }
        if !self.is_home() && self.search.is_none() && self.navigation.tab == Tab::Collections {
            if self.navigation.folder.is_some() {
                self.back.draw(cx);
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
                    &counted(folder.count, "book"),
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
            } else {
                cx.hint(cover, RefreshMode::Ui);
            }
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
        }
        if !self.is_home() && self.books.is_empty() && self.folders.is_empty() {
            let message = if let Some(search) = &self.search {
                if search.folded.is_empty() {
                    "No books in this scope.".into()
                } else {
                    "No matching books. Try another title, author or filename.".into()
                }
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
        let message = self.errors.first().map(String::as_str).unwrap_or(if self.scanning {
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
            if matches!(reader.on_gesture(g, cx), Action::Pop) {
                self.returned(cx);
            }
            return Action::None;
        }
        // Library controls own their press/capture state, including gestures
        // that return no action. Reader gestures invalidate independently.
        cx.redraw();
        if let Some(details) = &mut self.details {
            if details.on_gesture(&g, cx) {
                self.details = None;
            }
        } else if let Some((id, menu)) = &mut self.menu {
            let event = menu.on_gesture(&g, cx.theme);
            let id = id.clone();
            if let Some(event) = event {
                self.menu = None;
                match event {
                    MenuEvent::Chosen(0) => self.open(id, cx),
                    MenuEvent::Chosen(1) => {
                        if let Some(book) = self.book(&id) {
                            self.details = Some(Details::new(cx.theme, self.screen, book, &self.store.book(&id)));
                        }
                    }
                    MenuEvent::Chosen(2) => self.mark(&id, true, cx),
                    MenuEvent::Chosen(3) => self.mark(&id, false, cx),
                    _ => {}
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
            let input = self.search.as_mut().and_then(|s| s.keyboard.on_gesture(&g, cx.theme));
            if let Some(input) = input {
                self.edit_search(&input, cx);
            }
        } else {
            if self.settings_action.on_gesture(&g) {
                return Action::Push(Box::new(crate::screens::system::SystemSettings::new(
                    cx.theme,
                    self.screen,
                    self.store.clone(),
                )));
            }
            if self.search_action.on_gesture(&g) {
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
                    search.scope = scope;
                    self.pager.page = 0;
                    self.refresh(cx.theme);
                    self.apply_pending(cx);
                    return Action::None;
                }
            }
            if self.exit.on_gesture(&g) {
                return Action::Pop;
            }
            if self.search.is_none()
                && let Some(selected) = self.tabs.on_gesture(&g)
            {
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
                self.save_navigation(cx);
                cx.refresh(self.screen, RefreshMode::Full);
                self.apply_pending(cx);
                return Action::None;
            }
            if !self.is_home() && self.search.is_none() && self.navigation.tab == Tab::Collections {
                if self.navigation.folder.is_some() && self.back.on_gesture(&g) {
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
            if self.rescan.on_gesture(&g) {
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
                self.save_navigation(cx);
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
                if let Some(path) = self.folders.get(i).cloned() {
                    if long {
                        let count = self.collections.folders[&path].count;
                        self.details = Some(Details::from_text(
                            cx.theme,
                            self.screen,
                            "Collection",
                            format!(
                                "{}\n\n{count} books, including subfolders\n\n{}",
                                collections::name(&path),
                                path.display()
                            ),
                        ));
                        self.grid.on_gesture(&Gesture::Cancel(g.pos()));
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
        if self.reader.is_some() || self.search.is_none() {
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
    fn deadline(&self) -> Option<std::time::Instant> {
        self.reader.as_ref().and_then(Screen::deadline)
    }
    fn on_tick(&mut self, cx: &mut EventCx) -> Action {
        self.reader.as_mut().map_or(Action::None, |r| r.on_tick(cx))
    }
    fn on_background(&mut self, cx: &mut EventCx) {
        if let Some(home) = self.library.take_home() {
            self.pending_home = Some(self.store.home_summary().unwrap_or(home));
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
        if let Some(r) = &mut self.reader {
            r.after_present();
        }
    }
    fn on_sleep(&mut self) {
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
            self.index
                .books
                .iter()
                .filter(|b| b.available && self.states.get(&b.id).is_some_and(|s| s.last_opened > 0))
                .max_by_key(|b| self.states.get(&b.id).map_or(0, |s| s.last_opened))?
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
        if let Some(r) = &mut self.reader {
            r.on_quit()?;
        }
        Ok(())
    }
}

struct Details {
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
    fn from_text(t: &Theme, screen: Rect, title: &str, content: String) -> Self {
        let sheet = Sheet::open(title, screen.h * 3 / 4, screen, t);
        let area = sheet.content(t);
        let mut pager = Pager::new(1);
        pager.place(Rect::new(area.x, area.bottom() - t.mm(11.0), area.w, t.mm(11.0)), t);
        let text = Rect { h: area.h - t.mm(13.0), ..area };
        Self { sheet, pager, pages: Vec::new(), content, text }
    }
    fn draw(&mut self, cx: &mut DrawCx) {
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
    fn on_gesture(&mut self, g: &Gesture, cx: &mut EventCx) -> bool {
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
    fn with_cx(t: &Theme, screen: Rect, f: impl FnOnce(&mut EventCx)) {
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
    fn tap(books: &mut Books, p: Point, cx: &mut EventCx) {
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
        let book = store.library().books[0].clone();
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
        with_cx(&t, screen, |cx| {
            let point = Point::new(t.margin + 10, books.continue_rect.y + 10);
            tap(&mut books, point, cx);
            assert!(books.reader.is_some());
            assert!(!books.browsing_ready && books.index.books.is_empty());
            let mut updated = state.clone();
            updated.progress = 6789;
            store.save_book(&book.id, &updated).unwrap();
            books.returned(cx);
            assert!(books.is_home());
            assert_eq!(books.home.resume().unwrap().status.progress, 6789);
            assert!(!books.browsing_ready);
            assert_eq!(store.book(&book.id).position.0, "authoritative-anchor");
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
        let before = std::fs::read(root.join("home.json"))?;
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
        assert_eq!(std::fs::read(root.join("home.json"))?, before, "a failed open adds no history");
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
            books.save_navigation(cx);
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
            books.save_navigation(cx);
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
            books.save_navigation(cx);
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
