//! First-time setup: a few pages over Home on the first launch (Welcome,
//! Library, Turning pages, Your page, From KOReader, Ready), with Back /
//! Skip / Next and "2 of 6". Every choice
//! is saved the moment it is made, through the same settings as Settings;
//! only Skip and Ready set `Settings::setup_done`, so quitting midway
//! shows setup again from Welcome with the choices in place. Settings →
//! Advanced → *Run setup again* pushes the same screen over Settings.
//!
//! Turning a page redraws the content area (a `Ui` refresh of what
//! changed); trying a zone on the sketch hints `Fast` for the sketch only.
//!
//! Your page is the Layout editor embedded with sample bar values; each
//! change is saved at once, and a reader open underneath picks it up.
//!
//! The Library page lists the folders [`discovery`] finds as it finds them
//! (the walk starts when setup opens). On a first setup, before any folder
//! list was saved, folders found with books start ticked; the list is
//! saved when the walk ends or a row is ticked, whichever comes first.
//!
//! From KOReader shows only when KOReader is installed (decided when setup
//! opens, so the page count stays). Leaving the Library page hands the
//! chosen folders to the library worker; the page waits until a scan of
//! exactly those folders has finished (the bulk scan reads `library.json`),
//! then shows the bulk import's preview and *Import*. While a book is open
//! the worker is paused, so the page reads the saved index as it is.

use std::path::{Path, PathBuf};

use crate::document::TocEntry;
use crate::geom::Rect;
use crate::gfx::{HAlign, Icon, TextRenderer};
use crate::library::discovery::{self, COUNT_CAP, Discovery, Folder};
use crate::library::worker::Worker as LibraryWorker;
use crate::platform::{DeviceModel, RefreshMode, Waker};
use crate::reader::{BOOKMARK_CORNER_MM, Zone, tap_zone};
use crate::screens::folders::{FolderPicker, display_folder};
use crate::screens::koreader_import::{self, Flow};
use crate::screens::reader::layout::{LayoutView, sample_data, sample_toc};
use crate::screens::system::{draw_zones, switch_row};
use crate::screens::transfer::TransferScreen;
use crate::screens::{Action, EventCx, RelayoutState, Screen, untested};
use crate::store::{Settings, Store, TapZones, with_folder};
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK2, Theme};
use crate::ui::widget::{DrawCx, Press, PressEvent};
use crate::ui::widgets::{Button, ButtonKind, List, ListEvent, Pager, PagerEvent, Row, Segmented, Toggle, TopBar};
use verso_update::check::CHECK_EVERY;

/// True until setup was finished or skipped once.
pub fn due(settings: &Settings) -> bool {
    !settings.setup_done
}

/// The untested-model line for Welcome, which replaces the notice: the
/// model counts as seen once setup shows it.
pub fn untested_note(store: &Store, model: &DeviceModel) -> Option<String> {
    if !untested::due(&store.settings(), model) {
        return None;
    }
    if let Err(e) = untested::mark_seen(store, &model.key) {
        log::warn!("couldn't remember the untested-model notice: {e:#}");
    }
    Some(untested::message(model))
}

/// The pages' content area: under the top bar, above Back and Next.
pub(crate) fn body_rect(t: &Theme, screen: Rect) -> Rect {
    let content = screen.split_top(t.top_bar).1.inset(t.margin);
    let top = content.y + t.mm(4.0);
    Rect::new(content.x, top, content.w, content.bottom() - t.button_h - t.mm(6.0) - top)
}

/// Where setup was before the display turned.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SetupResume {
    pub page: usize,
    /// The untested-model line on Welcome.
    pub note: Option<String>,
    /// The From KOReader page is shown.
    pub koreader: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Welcome,
    Library,
    Turning,
    Layout,
    Koreader,
    Ready,
}

impl Page {
    fn title(self) -> &'static str {
        match self {
            Page::Welcome => "Welcome",
            Page::Library => "Library",
            Page::Turning => "Turning pages",
            Page::Layout => "Your page",
            Page::Koreader => "From KOReader",
            Page::Ready => "Ready",
        }
    }
}

const GESTURES: [(Icon, &str); 4] = [
    (Icon::Menu, "Tap the middle of a page for the menu."),
    (Icon::Sun, "Swipe down from the top for light, Wi-Fi and Settings."),
    (Icon::BookOpen, "Long-press a word to look it up in the dictionary."),
    (Icon::BookmarkFilled, "Tap the top-right corner to bookmark a page."),
];

const KOREADER_INTRO: &str =
    "Bring reading progress, highlights, notes and bookmarks over from KOReader, for every book in your library.";

/// The status line under the Library page's rows: two lines of small text.
const STATUS_MM: f32 = 12.0;

/// A row of the Library page.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Listed {
    path: PathBuf,
    /// None until counted.
    books: Option<usize>,
    koreader_home: bool,
}

pub struct SetupScreen {
    store: Store,
    waker: Waker,
    screen: Rect,
    pages: Vec<Page>,
    page: usize,
    note: Option<String>,
    top: TopBar,
    skip: Button,
    back: Button,
    next: Button,
    /// The page count between Back and Next.
    count: Rect,
    /// Between the top bar and the buttons.
    body: Rect,
    // Welcome
    start: Button,
    skip_setup: Button,
    // Library: the rows, the folder list they tick (saved, plus folders
    // found with books on a first setup until `unsaved` is cleared), and
    // the walk while it runs.
    rows: Vec<Listed>,
    folders: Vec<PathBuf>,
    /// Folders found with books get ticked (no folder list saved yet).
    auto_tick: bool,
    unsaved: bool,
    discovery: Option<Discovery>,
    intro: Rect,
    list: List,
    list_rect: Rect,
    pager: Pager,
    /// Below the rows: what the walk is doing, or that there are no books.
    status: Rect,
    transfer: Button,
    add_folder: Button,
    // Turning pages: the zones, a sketch of a page with them drawn in, the
    // zone last tapped on it, and the corner switch.
    zones: Segmented,
    sketch: Rect,
    sketch_press: Press,
    tried: Option<Zone>,
    /// The sketch changed by a tap: hint Fast for it in the next frame.
    sketch_fast: bool,
    caption: Rect,
    corner: Toggle,
    /// The column of the switch rows (Turning pages and Ready).
    switches: Rect,
    // Your page, with sample bar values.
    layout: LayoutView,
    sample_toc: Vec<TocEntry>,
    // From KOReader: the library worker (only with the page), the import
    // once the library was scanned, and the folders it scanned.
    library: Option<LibraryWorker>,
    /// The book open in the reader under Settings: the import leaves it out.
    open: Option<String>,
    flow: Option<Flow>,
    flow_folders: Vec<PathBuf>,
    import: Button,
    // Ready
    auto_check: Toggle,
    finish: Button,
}

impl SetupScreen {
    pub fn new(t: &Theme, screen: Rect, store: Store, waker: Waker, note: Option<String>) -> Self {
        let koreader = crate::koreader::bulk::koreader_dir().is_dir();
        Self::build(t, screen, store, waker, note, koreader)
    }

    /// The book open in the reader underneath, which the import leaves out.
    /// A scan started before (setup resumed on From KOReader) starts again.
    pub fn with_open_book(mut self, open: Option<String>) -> Self {
        self.open = open;
        if !self.importing() {
            self.flow = None;
        }
        self.sync_koreader();
        self
    }

    fn build(t: &Theme, screen: Rect, store: Store, waker: Waker, note: Option<String>, koreader: bool) -> Self {
        let settings = store.settings();
        let mut pages = vec![Page::Welcome, Page::Library, Page::Turning, Page::Layout, Page::Ready];
        if koreader {
            pages.insert(4, Page::Koreader);
        }
        let library = koreader.then(|| store.library_worker(waker.clone()));
        let mut top = TopBar::new("", false);
        top.place(screen, t);
        let content = screen.split_top(t.top_bar).1.inset(t.margin);
        let mut skip = Button::new("Skip", ButtonKind::Secondary);
        let skip_w = t.mm(20.0);
        let skip_h = t.button_h.min(t.top_bar - t.mm(3.0));
        skip.place(Rect::new(screen.right() - t.margin - skip_w, (t.top_bar - skip_h) / 2, skip_w, skip_h), t);
        top.trailing = skip_w + t.mm(2.0);

        let nav = Rect::new(content.x, content.bottom() - t.button_h, content.w, t.button_h);
        let nav_w = t.mm(34.0).min(content.w / 3);
        let mut back = Button::new("Back", ButtonKind::Secondary).with_icon(Icon::ChevronLeft);
        back.place(nav.split_left(nav_w).0, t);
        let mut next = Button::new("Next", ButtonKind::Primary);
        next.place(nav.split_right(nav_w).0, t);
        let mut finish = Button::new("Start reading", ButtonKind::Primary);
        finish.place(nav.split_right(nav_w).0, t);
        let count = Rect { x: nav.x + nav_w, w: nav.w - 2 * nav_w, ..nav };
        let body = body_rect(t, screen);
        let layout = LayoutView::embedded(t, screen, body, &settings.page_layout, &settings.typography);

        // Welcome: the two buttons at the bottom, centred.
        let w = content.w.min(t.mm(80.0));
        let x = content.x + (content.w - w) / 2;
        let mut skip_setup = Button::new("Skip setup", ButtonKind::Secondary);
        skip_setup.place(Rect::new(x, nav.y, w, t.button_h), t);
        let mut start = Button::new("Start", ButtonKind::Primary);
        start.place(Rect::new(x, nav.y - t.mm(3.0) - t.button_h, w, t.button_h), t);

        // Turning pages: the sketch below the zones in portrait, with the
        // caption and the corner switch under it; on the left in landscape.
        let landscape = screen.w > screen.h;
        let (left, right) = if landscape {
            let gap = t.mm(10.0);
            let w = (body.w - gap) / 2;
            (Rect { w, ..body }, Rect { x: body.right() - w, w, ..body })
        } else {
            (body, body)
        };
        let selected = TapZones::ALL.iter().position(|&z| z == settings.tap_zones).unwrap_or(0);
        let mut zones = Segmented::texts(&["Standard", "Left-handed", "Halves"], selected);
        // The page title names them: no section label above the zones.
        zones.place(Rect::new(right.x, right.y, right.w, t.button_h));
        let (sketch_area, caption, switches_top) = if landscape {
            let caption = Rect::new(right.x, zones.rect().bottom() + t.mm(4.0), right.w, t.mm(10.0));
            (left, caption, caption.bottom() + t.mm(4.0))
        } else {
            // The switch row: a label line and a note.
            let switches_top = body.bottom() - t.mm(14.0);
            let caption = Rect::new(body.x, switches_top - t.mm(12.0), body.w, t.mm(10.0));
            let top = zones.rect().bottom() + t.mm(5.0);
            (Rect::new(body.x, top, body.w, caption.y - t.mm(2.0) - top), caption, switches_top)
        };
        // Shaped like the screen, as large as fits, centred.
        let h = sketch_area.h.min(sketch_area.w * screen.h / screen.w.max(1)).max(0);
        let w = h * screen.w / screen.h.max(1);
        let sketch = Rect::new(sketch_area.x + (sketch_area.w - w) / 2, sketch_area.y, w, h);
        let switches = Rect { y: switches_top, h: body.bottom() - switches_top, ..right };
        let mut corner = Toggle::new(settings.bookmark_corner);
        corner.place(switches.right(), switches.y + t.mm(4.0), t);
        let mut auto_check = Toggle::new(settings.check_updates);
        auto_check.place(body.right(), body.y, t);

        // Library: the intro, the rows with a line of status under them
        // (and Wi-Fi transfer when there are no books), a pager, and the
        // add button at the bottom.
        let intro = Rect::new(body.x, body.y, body.w, t.mm(12.0));
        let mut add_folder = Button::new("Add another folder…", ButtonKind::Secondary);
        add_folder.place(Rect::new(body.x, body.bottom() - t.button_h, body.w, t.button_h), t);
        let mut pager = Pager::new(1);
        pager.place(Rect::new(body.x, add_folder.rect().y - t.mm(13.0), body.w, t.mm(11.0)), t);
        let list_top = intro.bottom() + t.mm(2.0);
        let list_rect = Rect::new(body.x, list_top, body.w, pager.rect().y - list_top);
        // Placed under the rows when drawn.
        let status = Rect::new(body.x, list_rect.bottom(), body.w, t.mm(STATUS_MM));
        let transfer = Button::new("Wi-Fi transfer", ButtonKind::Primary).with_icon(Icon::Transfer);
        // From KOReader: at the bottom of the body, or of its left column in
        // landscape (the text takes the right one).
        let mut import = Button::new("Import", ButtonKind::Primary);
        let import_w = if landscape { (body.w - t.mm(6.0)) / 3 } else { body.w };
        import.place(Rect { w: import_w, ..add_folder.rect() }, t);
        let overridden = Store::library_folders_overridden();
        let folders = store.library_folders();
        let auto_tick = !overridden && settings.library_folders.is_none() && settings.library_folder.is_none();

        let mut this = Self {
            store,
            waker,
            screen,
            pages,
            page: 0,
            note,
            top,
            skip,
            back,
            next,
            count,
            body,
            start,
            skip_setup,
            rows: Vec::new(),
            folders,
            auto_tick,
            unsaved: false,
            discovery: None,
            intro,
            list: List::new(Vec::new()),
            list_rect,
            pager,
            status,
            transfer,
            add_folder,
            zones,
            sketch,
            sketch_press: Press::default(),
            tried: None,
            sketch_fast: false,
            caption,
            corner,
            switches,
            layout,
            sample_toc: sample_toc(),
            library,
            open: None,
            flow: None,
            flow_folders: Vec::new(),
            import,
            auto_check,
            finish,
        };
        this.go(0);
        this.discover(t);
        this
    }

    /// Setup rebuilt after the display turned, on the page it showed.
    pub fn resume(t: &Theme, screen: Rect, store: Store, waker: Waker, state: &SetupResume) -> Self {
        let mut this = Self::build(t, screen, store, waker, state.note.clone(), state.koreader);
        this.go(state.page);
        this
    }

    fn current(&self) -> Page {
        self.pages[self.page]
    }

    fn go(&mut self, page: usize) {
        self.page = page.min(self.pages.len() - 1);
        self.top.title = self.current().title().into();
        self.tried = None;
        self.sync_koreader();
    }

    fn save(&self, cx: &mut EventCx, what: &str, change: impl FnOnce(&mut Settings)) -> bool {
        let mut settings = self.store.settings();
        change(&mut settings);
        match self.store.save_settings(&settings) {
            Ok(()) => true,
            Err(e) => {
                log::error!("saving {what}: {e:#}");
                cx.toast(format!("Couldn't save {what}"), None);
                false
            }
        }
    }

    /// Skip, Skip setup or Start reading: setup is done.
    fn finish(&mut self, cx: &mut EventCx) -> Action {
        self.save_folders(cx);
        if !self.store.settings().setup_done {
            self.save(cx, "settings", |s| s.setup_done = true);
        }
        Action::Pop
    }

    fn corner_px(&self, t: &Theme) -> Option<i32> {
        self.corner.on.then(|| t.mm(BOOKMARK_CORNER_MM) * self.sketch.w / self.screen.w.max(1))
    }

    fn draw_welcome(&mut self, cx: &mut DrawCx) {
        let t = cx.theme;
        let c = self.body;
        let title = Rect::new(c.x, c.y + c.h / 5, c.w, t.mm(14.0));
        cx.icon(Icon::BookOpen, Rect { y: title.y - t.mm(8.0), h: t.mm(8.0), ..title }, t.icon, crate::ui::theme::INK);
        cx.line("Verso", t.title, title, HAlign::Center);
        let text = "A few choices before you start. Each one can be changed later in Settings.";
        let w = c.w.min(t.mm(100.0));
        let x = c.x + (c.w - w) / 2;
        let h = cx.text.measure(text, t.body, Some(w as f32)).1.ceil() as i32;
        let line = Rect::new(x, title.bottom() + t.mm(3.0), w, h);
        cx.paragraph(text, t.body, line, HAlign::Center);
        if let Some(note) = &self.note {
            let style = t.small.gray(INK2);
            let h = cx.text.measure(note, style, Some(w as f32)).1.ceil() as i32;
            cx.paragraph(note, style, Rect::new(x, line.bottom() + t.mm(5.0), w, h), HAlign::Center);
        }
        self.start.draw(cx);
        self.skip_setup.draw(cx);
    }

    // ---- Library ----

    /// Starts the walk (again, after the folder picker or Wi-Fi transfer).
    /// The fixed folders show at once; counts and found folders follow.
    fn discover(&mut self, t: &Theme) {
        let sources = self.sources();
        self.discover_from(sources, t);
    }

    /// The default and saved folders, the storage root and KOReader's
    /// settings; with `VERSO_BOOKS`, its folders only.
    fn sources(&self) -> discovery::Sources {
        let overridden = Store::library_folders_overridden();
        let saved = self.store.library_folders();
        let mut fixed = if overridden { Vec::new() } else { crate::store::default_library_folders() };
        for folder in saved {
            if !fixed.contains(&folder) {
                fixed.push(folder);
            }
        }
        discovery::Sources {
            fixed,
            root: if overridden { None } else { crate::store::storage_root() },
            koreader_settings: (!overridden).then(|| crate::koreader::bulk::koreader_dir().join("settings.reader.lua")),
        }
    }

    fn discover_from(&mut self, sources: discovery::Sources, t: &Theme) {
        for path in &sources.fixed {
            self.merge(Folder { path: path.clone(), books: 0, koreader_home: false, discovered: false }, false);
        }
        self.discovery = Some(Discovery::start(sources, self.waker.clone()));
        self.refresh_list(t);
    }

    /// Adds or updates a row: a folder inside a row is left out, rows
    /// inside it go. `counted` is false for a fixed folder shown before
    /// the walk reached it.
    fn merge(&mut self, folder: Folder, counted: bool) {
        let path = folder.path.clone();
        if let Some(row) = self.rows.iter_mut().find(|r| r.path == path) {
            if counted {
                row.books = Some(folder.books);
                row.koreader_home |= folder.koreader_home;
            }
        } else if !self.rows.iter().any(|r| path.starts_with(&r.path)) {
            self.rows.retain(|r| !r.path.starts_with(&path));
            let books = counted.then_some(folder.books);
            self.rows.push(Listed { path: path.clone(), books, koreader_home: folder.koreader_home });
        } else {
            return;
        }
        if self.auto_tick
            && folder.discovered
            && folder.books > 0
            && let Some(folders) = with_folder(&self.folders, &path)
        {
            self.folders = folders;
            self.unsaved = true;
        }
    }

    fn ticked(&self, path: &Path) -> bool {
        self.folders.iter().any(|f| path.starts_with(f))
    }

    fn refresh_list(&mut self, t: &Theme) {
        let editable = !Store::library_folders_overridden();
        let rows = self
            .rows
            .iter()
            .map(|r| {
                let count = r.books.map(|n| match n {
                    0 => "No books".to_owned(),
                    1 => "1 book".to_owned(),
                    n if n >= COUNT_CAP => "1,000+ books".to_owned(),
                    n => format!("{n} books"),
                });
                let sub = match (r.koreader_home, count) {
                    (true, Some(count)) => format!("KOReader’s home folder · {count}"),
                    (true, None) => "KOReader’s home folder".to_owned(),
                    (false, count) => count.unwrap_or_default(),
                };
                let mut row = Row::new(display_folder(&r.path)).sub(sub).toggle(self.ticked(&r.path));
                row.enabled = editable;
                row
            })
            .collect();
        let page = self.list.page;
        self.list = List::new(rows);
        self.list.page = page;
        // Room for the status line, and Wi-Fi transfer, when they show.
        let mut reserve = if self.status_text().is_some() { t.mm(3.0) + self.status.h } else { 0 };
        if self.no_books() {
            reserve += t.mm(2.0) + t.button_h;
        }
        let rect = Rect { h: self.list_rect.h - reserve, ..self.list_rect };
        self.list.place(rect, t.row2, 0);
        self.pager.set(self.list.page, self.list.paging().pages());
    }

    /// No books in any listed folder, once the walk is done.
    fn no_books(&self) -> bool {
        self.discovery.is_none() && self.rows.iter().all(|r| r.books.unwrap_or(0) == 0)
    }

    /// The line under the rows: what the walk is doing, or why there is
    /// nothing to choose.
    fn status_text(&self) -> Option<&'static str> {
        if Store::library_folders_overridden() {
            Some("Library folders are set by VERSO_BOOKS.")
        } else if self.discovery.is_some() {
            Some("Looking for books on this Kindle…")
        } else if self.no_books() {
            Some("No books on this Kindle yet. Send some from a computer or phone over Wi-Fi.")
        } else {
            None
        }
    }

    /// Saves the ticked folders if they changed.
    fn save_folders(&mut self, cx: &mut EventCx) {
        if !std::mem::take(&mut self.unsaved) || Store::library_folders_overridden() {
            return;
        }
        if let Err(e) = self.store.save_library_folders(self.folders.clone()) {
            log::error!("saving library folders: {e:#}");
            cx.toast("Couldn't save library folders", None);
            self.folders = self.store.library_folders();
        }
    }

    fn toggle_folder(&mut self, i: usize, cx: &mut EventCx) {
        let path = self.rows[i].path.clone();
        let folders = if self.ticked(&path) {
            let left: Vec<PathBuf> = self.folders.iter().filter(|f| !f.starts_with(&path)).cloned().collect();
            if left.is_empty() {
                cx.toast("Keep at least one folder", None);
                return;
            }
            left
        } else {
            with_folder(&self.folders, &path).unwrap_or_else(|| self.folders.clone())
        };
        // A choice of the user's own: the walk ticks nothing more.
        self.auto_tick = false;
        self.folders = folders;
        self.unsaved = true;
        self.save_folders(cx);
        self.refresh_list(cx.theme);
    }

    fn draw_library(&mut self, cx: &mut DrawCx) {
        let t = cx.theme;
        let intro = "Verso shows EPUB and Markdown books in the folders switched on here, and in their subfolders.";
        cx.paragraph(intro, t.body, self.intro, HAlign::Left);
        self.list.draw(cx);
        if self.pager.pages > 1 {
            self.pager.draw(cx);
        }
        let status = self.status_text();
        // Under the last row on the page.
        let shown = self.list.paging().range(self.list.page).len() as i32;
        let y = self.list_rect.y + shown * t.row2 + t.mm(3.0);
        self.status = Rect { y, ..self.status };
        if let Some(status) = status {
            cx.paragraph(status, t.small.gray(INK2), self.status, HAlign::Left);
        }
        if self.no_books() {
            let w = self.status.w.min(t.mm(70.0));
            let rect = Rect::new(self.status.x, self.status.bottom() + t.mm(2.0), w, t.button_h);
            self.transfer.place(rect, t);
            self.transfer.draw(cx);
        }
        self.add_folder.enabled = !Store::library_folders_overridden();
        self.add_folder.draw(cx);
    }

    fn library_gesture(&mut self, g: &Gesture, cx: &mut EventCx) -> Action {
        if let Gesture::Swipe { dir, from, .. } = *g
            && self.list_rect.contains(from)
            && let Some(page) = self.pager.on_swipe(dir)
        {
            self.list.page = page;
            return Action::None;
        }
        if let Some(PagerEvent::Changed(page)) = self.pager.on_gesture(g) {
            self.list.page = page;
            return Action::None;
        }
        if let Some(ListEvent::Tapped(i)) = self.list.on_gesture(g) {
            self.toggle_folder(i, cx);
            return Action::None;
        }
        if self.no_books() && self.transfer.on_gesture(g) {
            self.save_folders(cx);
            return Action::Push(Box::new(TransferScreen::new(
                cx.theme,
                self.screen,
                self.store.clone(),
                self.waker.clone(),
            )));
        }
        if self.add_folder.on_gesture(g) {
            self.save_folders(cx);
            return Action::Push(Box::new(FolderPicker::new(
                cx.theme,
                self.screen,
                self.store.clone(),
                self.folders.clone(),
            )));
        }
        Action::None
    }

    fn draw_turning(&mut self, cx: &mut DrawCx) {
        let t = cx.theme;
        self.zones.draw(cx);
        let zones = TapZones::ALL[self.zones.selected];
        draw_zones(cx, self.sketch, zones, self.corner_px(t), self.tried);
        if std::mem::take(&mut self.sketch_fast) {
            cx.hint(self.sketch, RefreshMode::Fast);
        }
        let caption = match self.tried {
            None => "Tap the page above to try it. Swiping left or right also turns pages.",
            Some(Zone::Back) => "Back: the previous page.",
            Some(Zone::Forward) => "Next: the next page.",
            Some(Zone::Menu) => "Menu: contents, notes, text size and more.",
            Some(Zone::Bookmark) => "Bookmark: marks the page, or removes its mark.",
        };
        let caption =
            if self.screen.w > self.screen.h { caption.replace("above", "on the left") } else { caption.into() };
        cx.paragraph(&caption, t.small.gray(INK2), self.caption, HAlign::Left);
        let s = self.switches;
        let corner = ("Tap the top-right corner to bookmark", "Tap it again to remove the bookmark.");
        switch_row(cx, &mut self.corner, corner, s.x, s.y, s.w);
    }

    fn draw_ready(&mut self, cx: &mut DrawCx) {
        let t = cx.theme;
        let c = self.body;
        let mut y = c.y;
        let text_x = c.x + t.icon + t.mm(4.0);
        let text_w = c.right() - text_x;
        // Each icon centred on its text's first line.
        let line_h = cx.text.measure("Tap", t.body, None).1.ceil() as i32;
        for (icon, text) in GESTURES {
            let h = (cx.text.measure(text, t.body, Some(text_w as f32)).1.ceil() as i32).max(t.icon);
            cx.icon(icon, Rect::new(c.x, y, t.icon, line_h), t.icon, crate::ui::theme::INK);
            cx.paragraph(text, t.body, Rect::new(text_x, y, text_w, h), HAlign::Left);
            y += h + t.mm(5.0);
        }
        y += t.mm(3.0);
        let every = format!("Every {} hours, only when Wi-Fi is already on.", CHECK_EVERY / 3600);
        let updates = ("Check for updates automatically", every.as_str());
        y = switch_row(cx, &mut self.auto_check, updates, c.x, y, c.w);
        cx.paragraph(
            "Run setup again: Settings → Advanced.",
            t.small.gray(INK2),
            Rect::new(c.x, y, c.w, t.mm(6.0)),
            HAlign::Left,
        );
    }

    // ---- From KOReader ----

    /// Hands the chosen folders to the library and, on the page, starts the
    /// KOReader scan once the library scanned them. A preview for other
    /// folders (the user went back and changed them) starts again.
    fn sync_koreader(&mut self) {
        let Some(library) = &self.library else { return };
        let folders = self.store.library_folders();
        library.set_roots(folders.clone());
        if self.current() != Page::Koreader {
            return;
        }
        if self.flow.as_ref().is_some_and(|f| !f.importing()) && self.flow_folders != folders {
            self.flow = None;
        }
        if self.flow.is_none() && (library.scanned().as_ref() == Some(&folders) || library.paused()) {
            self.flow = Some(Flow::scan(self.store.clone(), self.waker.clone(), self.open.clone()));
            self.flow_folders = folders;
        }
    }

    fn importing(&self) -> bool {
        self.flow.as_ref().is_some_and(Flow::importing)
    }

    fn sync_nav_enabled(&mut self) {
        let enabled = !self.importing();
        self.skip.enabled = enabled;
        self.back.enabled = enabled;
        self.next.enabled = enabled;
    }

    /// From KOReader's intro, and the area for the import's text under it.
    /// In landscape the intro and *Import* share the left column.
    fn koreader_areas(&self, text: &mut TextRenderer, t: &Theme) -> (Rect, Rect) {
        let c = self.body;
        let w = self.import.rect().w;
        let h = text.measure(KOREADER_INTRO, t.body, Some(w as f32)).1.ceil() as i32;
        let intro = Rect { w, h, ..c };
        if w < c.w {
            // No button under the text: it may run to just above Back / Next.
            let x = c.x + w + t.mm(6.0);
            return (intro, Rect { x, w: c.right() - x, h: self.back.rect().y - t.mm(2.0) - c.y, ..c });
        }
        let y = c.y + h + t.mm(6.0);
        (intro, Rect::new(c.x, y, c.w, self.import.rect().y - t.mm(3.0) - y))
    }

    fn draw_koreader(&mut self, cx: &mut DrawCx) {
        let t = cx.theme;
        let (intro, text) = self.koreader_areas(cx.text, t);
        cx.paragraph(KOREADER_INTRO, t.body, intro, HAlign::Left);
        match &self.flow {
            None => koreader_import::draw_text(cx, "Waiting for the library scan…", "", text),
            Some(flow) => {
                let (main, note) = flow.text();
                koreader_import::draw_text(cx, &main, note, text);
            }
        }
        if self.flow.as_ref().is_some_and(Flow::importable) {
            self.import.draw(cx);
        }
    }

    fn koreader_gesture(&mut self, g: &Gesture) {
        if let Some(flow) = &mut self.flow
            && flow.importable()
            && self.import.on_gesture(g)
        {
            flow.import();
            self.sync_nav_enabled();
        }
    }

    /// Your page: the editor takes the gesture; a change is saved at once.
    fn layout_gesture(&mut self, g: &Gesture, cx: &mut EventCx) {
        self.layout.on_gesture(g, cx.theme, &sample_data(&self.sample_toc));
        let layout = self.layout.layout.clone();
        let saved = self.store.settings().page_layout;
        if layout != saved && !self.save(cx, "the page layout", |s| s.page_layout = layout) {
            self.layout.reset(&saved);
        }
    }

    fn turning_gesture(&mut self, g: &Gesture, cx: &mut EventCx) {
        if let Some(i) = self.zones.on_gesture(g) {
            if self.save(cx, "tap zones", |s| s.tap_zones = TapZones::ALL[i]) {
                self.tried = None;
            } else {
                let current = self.store.settings().tap_zones;
                self.zones.selected = TapZones::ALL.iter().position(|&z| z == current).unwrap_or(0);
            }
        }
        if let Some(on) = self.corner.on_gesture(g)
            && !self.save(cx, "the corner tap", |s| s.bookmark_corner = on)
        {
            self.corner.on = !on;
        }
        if let Some(PressEvent::Tap) = self.sketch_press.on_gesture(g, self.sketch) {
            let inner = self.sketch.inset(cx.theme.border);
            let zones = TapZones::ALL[self.zones.selected];
            self.tried = Some(tap_zone(inner, g.pos(), zones, self.corner_px(cx.theme)));
            self.sketch_fast = true;
        }
    }
}

impl Screen for SetupScreen {
    fn allows_top_drawer(&self) -> bool {
        true
    }

    /// Takes the folders the walk found and saves the ticked ones when it
    /// ends; follows the library scan and the KOReader import.
    fn on_background(&mut self, cx: &mut EventCx) {
        let waiting = self.flow.is_none();
        self.sync_koreader();
        let (moved, _) = self.flow.as_mut().map_or((false, None), Flow::on_background);
        self.sync_nav_enabled();
        if self.current() == Page::Koreader && (moved || waiting != self.flow.is_none()) {
            cx.redraw();
        }
        let Some(walk) = &self.discovery else { return };
        let finished = walk.finished();
        let found = walk.take();
        let changed = !found.is_empty() || finished;
        for folder in found {
            self.merge(folder, true);
        }
        if finished {
            self.discovery = None;
            self.save_folders(cx);
        }
        if changed {
            self.refresh_list(cx.theme);
            if self.current() == Page::Library {
                cx.redraw();
            }
        }
    }

    /// Back from the folder picker or Wi-Fi transfer: count again.
    fn on_return(&mut self, cx: &mut EventCx) {
        self.folders = self.store.library_folders();
        self.discover(cx.theme);
        cx.redraw();
    }

    /// Scripted screenshots wait for the walk, and on From KOReader for the
    /// library scan and the KOReader scan.
    fn busy(&self) -> bool {
        let koreader = self.current() == Page::Koreader && self.flow.as_ref().is_none_or(Flow::busy);
        self.discovery.is_some() || koreader
    }

    fn relayout_state(&self, state: &mut RelayoutState) {
        let koreader = self.library.is_some();
        state.setup = Some(SetupResume { page: self.page, note: self.note.clone(), koreader });
    }

    fn draw(&mut self, cx: &mut DrawCx) {
        let page = self.current();
        if page == Page::Welcome {
            self.draw_welcome(cx);
            return;
        }
        self.sync_nav_enabled();
        self.top.draw(cx);
        if page != Page::Ready {
            self.skip.draw(cx);
        }
        match page {
            Page::Welcome => {}
            Page::Library => self.draw_library(cx),
            Page::Turning => self.draw_turning(cx),
            // Below: its slot picker opens over the buttons.
            Page::Layout => {}
            Page::Koreader => self.draw_koreader(cx),
            Page::Ready => self.draw_ready(cx),
        }
        self.back.draw(cx);
        let count = format!("{} of {}", self.page + 1, self.pages.len());
        cx.line(&count, cx.theme.small.gray(INK2), self.count, HAlign::Center);
        if page == Page::Ready {
            self.finish.draw(cx)
        } else {
            self.next.draw(cx)
        }
        if page == Page::Layout {
            self.layout.draw(cx, &sample_data(&self.sample_toc));
        }
    }

    fn on_gesture(&mut self, g: Gesture, cx: &mut EventCx) -> Action {
        cx.redraw();
        let page = self.current();
        if page == Page::Welcome {
            if self.start.on_gesture(&g) {
                self.go(self.page + 1);
            }
            if self.skip_setup.on_gesture(&g) {
                return self.finish(cx);
            }
            return Action::None;
        }
        // Nothing leaves the page while the import writes (it takes seconds).
        if self.importing() {
            return Action::None;
        }
        // An open slot picker takes every gesture, also over the buttons.
        if page == Page::Layout && self.layout.picker_open() {
            self.layout_gesture(&g, cx);
            return Action::None;
        }
        if page != Page::Ready && self.skip.on_gesture(&g) {
            return self.finish(cx);
        }
        if self.back.on_gesture(&g) {
            self.go(self.page.saturating_sub(1));
            return Action::None;
        }
        if page == Page::Ready {
            if self.finish.on_gesture(&g) {
                return self.finish(cx);
            }
            if let Some(on) = self.auto_check.on_gesture(&g)
                && !self.save(cx, "update checks", |s| s.check_updates = on)
            {
                self.auto_check.on = !on;
            }
            return Action::None;
        }
        if self.next.on_gesture(&g) {
            self.go(self.page + 1);
            return Action::None;
        }
        match page {
            Page::Library => return self.library_gesture(&g, cx),
            Page::Turning => self.turning_gesture(&g, cx),
            Page::Layout => self.layout_gesture(&g, cx),
            Page::Koreader => self.koreader_gesture(&g),
            _ => {}
        }
        Action::None
    }
}

/// From KOReader with the fullest preview, for `screens/fit_tests.rs`.
#[cfg(test)]
pub(super) fn fit_samples(t: &Theme, screen: Rect, text: &mut TextRenderer) -> Vec<crate::ui::fit::Parts> {
    use crate::ui::fit::text_h;
    let store = Store::at(std::env::temp_dir().join("verso-setup-fit"));
    let s = SetupScreen::build(t, screen, store, Waker::new(|| {}), None, false);
    let (_, area) = s.koreader_areas(text, t);
    let (main, note) = koreader_import::fullest_text();
    let needed = text_h(text, &main, t.body, area.w) + t.mm(3.0) + text_h(text, note, t.small, area.w);
    let buttons = [s.import.rect(), s.back.rect(), s.next.rect(), s.skip.rect()];
    vec![crate::ui::fit::Parts::new("Setup: From KOReader", screen, buttons).body(area, needed)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Point;
    use std::time::Instant;

    const SCREEN: Rect = Rect { x: 0, y: 0, w: 1272, h: 1696 };

    fn setup(store: &Store) -> (SetupScreen, Theme) {
        let t = Theme::for_dpi(300);
        (SetupScreen::new(&t, SCREEN, store.clone(), Waker::new(|| {}), None), t)
    }

    fn draw(s: &mut SetupScreen, t: &Theme) {
        use crate::gfx::{Canvas, IconRenderer, TextRenderer};
        let (mut canvas, mut text, mut icons) =
            (Canvas::new(SCREEN.w as u32, SCREEN.h as u32), TextRenderer::new(), IconRenderer::default());
        s.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, t, &mut vec![]));
    }

    fn with_cx<R>(t: &Theme, f: impl FnOnce(&mut EventCx) -> R) -> R {
        let mut cx = EventCx {
            theme: t,
            screen: SCREEN,
            now: Instant::now(),
            battery: None,
            toast: &mut None,
            forced: &mut vec![],
            needs_redraw: &mut false,
        };
        f(&mut cx)
    }

    /// Draws (which places the switches), then taps the middle of \`rect\`.
    fn tap(s: &mut SetupScreen, t: &Theme, rect: impl Fn(&SetupScreen) -> Rect) -> Action {
        draw(s, t);
        let p = rect(s).center();
        let p = Point::new(p.x, p.y);
        with_cx(t, |cx| {
            s.on_gesture(Gesture::Down(p), cx);
            s.on_gesture(Gesture::Tap(p), cx)
        })
    }

    /// The Library page walking \`fixed\` and \`root\` to the end, with
    /// \`fixed\` as the folder list (as the defaults are before any save).
    fn walk(s: &mut SetupScreen, t: &Theme, fixed: Vec<PathBuf>, root: &Path) {
        s.discovery = None;
        s.rows.clear();
        if s.auto_tick {
            s.folders = fixed.clone();
        }
        let sources = discovery::Sources { fixed, root: Some(root.to_path_buf()), koreader_settings: None };
        s.discover_from(sources, t);
        s.go(1);
        while s.discovery.is_some() {
            std::thread::sleep(std::time::Duration::from_millis(5));
            with_cx(t, |cx| s.on_background(cx));
        }
    }

    fn row_of(s: &SetupScreen, name: &str) -> usize {
        s.rows.iter().position(|r| r.path.ends_with(name)).unwrap()
    }

    #[test]
    fn choices_save_at_once_and_only_finishing_ends_setup() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::at(dir.path().to_path_buf());
        assert!(due(&store.settings()));
        let (mut s, t) = setup(&store);
        tap(&mut s, &t, |s| s.start.rect());
        assert_eq!(s.current(), Page::Library);
        tap(&mut s, &t, |s| s.next.rect());
        assert_eq!(s.current(), Page::Turning);
        tap(&mut s, &t, |s| s.zones.rect().columns(3, 0)[2]);
        tap(&mut s, &t, |s| s.corner.rect());
        tap(&mut s, &t, |s| s.next.rect());
        assert_eq!(s.current(), Page::Layout);
        tap(&mut s, &t, |s| s.next.rect());
        tap(&mut s, &t, |s| s.auto_check.rect());
        let settings = store.settings();
        assert_eq!(
            (settings.tap_zones, settings.bookmark_corner, settings.check_updates),
            (TapZones::Halves, false, false),
            "saved before setup ends"
        );
        // Quitting midway: setup shows again next time.
        s.on_quit()?;
        assert!(due(&store.settings()));
        assert!(matches!(tap(&mut s, &t, |s| s.finish.rect()), Action::Pop));
        assert!(!due(&store.settings()), "Ready ends setup");

        // Skip setup on Welcome, and Skip on a later page, end it too.
        for page in [0, 1] {
            let mut settings = store.settings();
            settings.setup_done = false;
            store.save_settings(&settings)?;
            let (mut s, t) = setup(&store);
            s.go(page);
            let action = tap(&mut s, &t, |s| if page == 0 { s.skip_setup.rect() } else { s.skip.rect() });
            assert!(matches!(action, Action::Pop) && !due(&store.settings()), "page {page}");
        }
        Ok(())
    }

    #[test]
    fn run_again_shows_current_values_and_stays_done() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::at(dir.path().to_path_buf());
        let mut settings = store.settings();
        (settings.setup_done, settings.tap_zones, settings.bookmark_corner, settings.check_updates) =
            (true, TapZones::Mirrored, false, false);
        store.save_settings(&settings)?;
        let (mut s, t) = setup(&store);
        assert_eq!((s.zones.selected, s.corner.on, s.auto_check.on), (1, false, false));
        s.go(4);
        assert!(matches!(tap(&mut s, &t, |s| s.back.rect()), Action::None));
        assert!(matches!(tap(&mut s, &t, |s| s.skip.rect()), Action::Pop));
        assert!(store.settings().setup_done);
        Ok(())
    }

    #[test]
    fn your_page_saves_each_change_and_its_picker_takes_every_tap() -> anyhow::Result<()> {
        use crate::page_layout::BarItem;
        let dir = tempfile::tempdir()?;
        let store = Store::at(dir.path().to_path_buf());
        let (mut s, t) = setup(&store);
        s.go(3);
        assert_eq!(s.current(), Page::Layout);
        tap(&mut s, &t, |s| s.layout.bar_toggle(0));
        assert!(store.settings().page_layout.top.on, "saved at once");
        // The top bar's empty centre slot opens the picker; a tap on Next
        // while it is open only closes it.
        tap(&mut s, &t, |s| s.layout.slot(&t, 0, 1));
        assert!(s.layout.picker_open());
        tap(&mut s, &t, |s| s.next.rect());
        assert!(!s.layout.picker_open() && s.current() == Page::Layout);
        tap(&mut s, &t, |s| s.layout.slot(&t, 0, 1));
        tap(&mut s, &t, |s| s.layout.picker_item(&t, BarItem::Author));
        assert_eq!(store.settings().page_layout.top.slots[1], Some(BarItem::Author));
        Ok(())
    }

    #[test]
    fn library_ticks_found_folders_once_and_saves_each_tick() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().join("us");
        for (folder, n) in [("documents", 1), ("Books", 2), ("Books/Nested", 1), ("Empty", 0)] {
            std::fs::create_dir_all(root.join(folder))?;
            for i in 0..n {
                std::fs::write(root.join(folder).join(format!("{i}.epub")), b"")?;
            }
        }
        let store = Store::at(dir.path().join("data"));
        let (mut s, t) = setup(&store);
        assert!(s.auto_tick, "no folder list saved yet");
        let documents = root.join("documents");
        walk(&mut s, &t, vec![documents.clone()], &root);
        let rows: Vec<_> = s.rows.iter().map(|r| (r.path.clone(), r.books)).collect();
        assert_eq!(rows, [(documents.clone(), Some(1)), (root.join("Books"), Some(3))]);
        // Found with books: ticked, and saved when the walk ended.
        let saved = || store.settings().library_folders.unwrap_or_default();
        assert_eq!(saved(), [documents.clone(), root.join("Books")]);
        assert!(s.list.rows.iter().all(|r| r.accessory == crate::ui::widgets::list::Accessory::Toggle(true)));

        // Each tap saves; the last folder stays.
        let row_h = t.row2;
        let row = |s: &SetupScreen, name| {
            let r = s.list_rect;
            Rect::new(r.x, r.y + row_of(s, name) as i32 * row_h, r.w, row_h)
        };
        tap(&mut s, &t, |s| row(s, "Books"));
        assert_eq!(saved(), std::slice::from_ref(&documents));
        tap(&mut s, &t, |s| row(s, "documents"));
        assert_eq!(saved(), std::slice::from_ref(&documents), "keeps at least one folder");
        tap(&mut s, &t, |s| row(s, "Books"));
        assert_eq!(saved(), [documents.clone(), root.join("Books")]);

        // Once a list was saved, the walk ticks nothing new.
        std::fs::create_dir_all(root.join("More"))?;
        std::fs::write(root.join("More/a.epub"), b"")?;
        let (mut s, t) = setup(&store);
        assert!(!s.auto_tick);
        walk(&mut s, &t, vec![documents.clone(), root.join("Books")], &root);
        assert!(s.rows.iter().any(|r| r.path.ends_with("More")) && !s.ticked(&root.join("More")));
        assert_eq!(saved(), [documents, root.join("Books")]);
        Ok(())
    }

    #[test]
    fn library_without_books_offers_wifi_transfer() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().join("us");
        std::fs::create_dir_all(root.join("documents"))?;
        std::fs::create_dir_all(root.join("Other"))?;
        let store = Store::at(dir.path().join("data"));
        let (mut s, t) = setup(&store);
        walk(&mut s, &t, vec![root.join("documents")], &root);
        assert!(s.no_books());
        std::fs::write(root.join("Other/a.epub"), b"")?;
        walk(&mut s, &t, vec![root.join("documents")], &root);
        assert!(!s.no_books());
        // Markdown files also count as books during folder discovery.
        std::fs::remove_file(root.join("Other/a.epub"))?;
        std::fs::write(root.join("Other/notes.md"), b"# Notes")?;
        walk(&mut s, &t, vec![root.join("documents")], &root);
        assert!(!s.no_books());
        assert_eq!(s.rows.iter().find(|r| r.path == root.join("Other")).and_then(|r| r.books), Some(1));
        Ok(())
    }

    #[test]
    fn from_koreader_shows_with_koreader_and_waits_for_the_chosen_folders() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let t = Theme::for_dpi(300);
        let store = Store::at(dir.path().join("data"));
        let without = SetupScreen::build(&t, SCREEN, store.clone(), Waker::new(|| {}), None, false);
        assert_eq!(without.pages.len(), 5, "no KOReader, no page");

        // A book in each folder; only the second has KOReader progress.
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let (first, second) = (dir.path().join("us/First"), dir.path().join("us/Second"));
        for (folder, name) in [(&first, "library2.epub"), (&second, "library3.epub")] {
            std::fs::create_dir_all(folder)?;
            std::fs::copy(fixtures.join(name), folder.join(name))?;
        }
        let sidecar = crate::koreader::metadata_path(&second.join("library3.epub")).unwrap();
        std::fs::create_dir_all(sidecar.parent().unwrap())?;
        std::fs::write(&sidecar, "return { last_xpointer = '/k', percent_finished = 0.5 }")?;
        store.save_library_folders(vec![first.clone()])?;

        let mut s = SetupScreen::build(&t, SCREEN, store.clone(), Waker::new(|| {}), None, true);
        assert_eq!(s.pages[4], Page::Koreader);
        // The Library page added the second folder.
        store.save_library_folders(vec![first, second])?;
        s.go(4);
        assert!(s.flow.is_none() && s.busy(), "waits for the library scan");
        let started = Instant::now();
        while s.flow.as_ref().is_none_or(|f| f.preview().is_none()) {
            assert!(started.elapsed().as_secs() < 20, "no preview");
            std::thread::sleep(std::time::Duration::from_millis(10));
            with_cx(&t, |cx| s.on_background(cx));
        }
        let preview = s.flow.as_ref().and_then(Flow::preview).unwrap();
        assert_eq!(preview.not_started, 1, "the second folder's book is in the scan");

        // Starting the import disables Skip, Back and Next until it finishes.
        assert!(s.skip.enabled && s.back.enabled && s.next.enabled);
        tap(&mut s, &t, |s| s.import.rect());
        assert!(s.importing());
        assert!(!s.skip.enabled && !s.back.enabled && !s.next.enabled);
        assert!(matches!(tap(&mut s, &t, |s| s.skip.rect()), Action::None));
        assert!(matches!(tap(&mut s, &t, |s| s.back.rect()), Action::None));
        assert!(matches!(tap(&mut s, &t, |s| s.next.rect()), Action::None));
        assert_eq!(s.current(), Page::Koreader);

        let started = Instant::now();
        while s.importing() {
            assert!(started.elapsed().as_secs() < 20, "import did not finish");
            std::thread::sleep(std::time::Duration::from_millis(10));
            with_cx(&t, |cx| s.on_background(cx));
        }
        assert!(s.skip.enabled && s.back.enabled && s.next.enabled);
        tap(&mut s, &t, |s| s.next.rect());
        assert_eq!(s.current(), Page::Ready);
        Ok(())
    }

    #[test]
    fn welcome_takes_the_untested_notice_and_marks_it_seen() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::at(dir.path().to_path_buf());
        let model =
            DeviceModel { name: "Kindle X".into(), key: "Kindle X".into(), untested: true, ..Default::default() };
        let note = untested_note(&store, &model);
        assert_eq!(note, Some(untested::message(&model)));
        assert!(!untested::due(&store.settings(), &model), "no notice after setup");
        assert_eq!(untested_note(&store, &model), None);
        let tested = DeviceModel { untested: false, ..model };
        assert_eq!(untested_note(&store, &tested), None);
        Ok(())
    }
}
