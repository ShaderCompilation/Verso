//! Paginated library, engine-independent discovery, and the existing reader flow.
use crate::document::{
    FontFace, Layout,
    thread::{Command, Reply, Worker},
};
use crate::geom::{Point, Rect};
use crate::gfx::{Family, HAlign, Icon};
use crate::library::{
    self, Book, Cover, LibraryIndex, Sort, covers,
    worker::{Snapshot, Worker as LibraryWorker},
};
use crate::platform::RefreshMode;
use crate::screens::reader::Reader;
use crate::screens::{Action, EventCx, Screen};
use crate::store::{BookState, Store};
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK, INK2, LINE, Theme};
use crate::ui::widget::{DrawCx, Press, PressEvent};
use crate::ui::widgets::{
    Button, ButtonKind, Grid, GridEvent, IconButton, MenuEvent, Pager, PopupMenu, Segmented, Sheet, TopBar,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

type ImageCache = HashMap<(String, i32, i32), image::GrayImage>;
pub struct Books {
    screen: Rect,
    worker: Rc<Worker>,
    library: LibraryWorker,
    store: Store,
    roots: Vec<PathBuf>,
    index: LibraryIndex,
    books: Vec<Book>,
    states: HashMap<String, BookState>,
    fonts: Vec<FontFace>,
    top: TopBar,
    exit: IconButton,
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
    touching: bool,
    pending: Option<Snapshot>,
    scanning: bool,
    working: bool,
    errors: Vec<String>,
    thumbnails: ImageCache,
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
        let index = store.library();
        let library = LibraryWorker::new(store.clone(), roots.clone(), worker.waker());
        let mut top = TopBar::new("Library", false);
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
        let mut pager = Pager::new(1);
        pager.place(Rect::new(t.margin, screen.bottom() - t.mm(11.0), screen.w - 2 * t.margin, t.mm(11.0)), t);
        let sort = Segmented::texts(
            &["Recent", "Title", "Author"],
            match store.settings().library_sort {
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
            index,
            books: Vec::new(),
            states: HashMap::new(),
            fonts,
            top,
            exit,
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
            touching: false,
            pending: None,
            scanning: true,
            working: true,
            errors: Vec::new(),
            thumbnails: HashMap::new(),
            reader: None,
            generation: 0,
            engine_error: None,
        };
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
    fn refresh(&mut self, t: &Theme) {
        for b in &self.index.books {
            self.states.entry(b.id.clone()).or_insert_with(|| self.store.book(&b.id));
        }
        self.books = self.index.books.iter().filter(|b| b.available).cloned().collect();
        let sort = self.selected_sort();
        library::sort_books(&mut self.books, &self.states, sort);
        self.continue_id = library::continue_book(&self.books, &self.states).map(|b| b.id.clone());
        let y = t.top_bar + t.mm(2.0);
        self.continue_rect = if self.continue_id.is_some() {
            Rect::new(t.margin, y, self.screen.w - 2 * t.margin, t.mm(23.0))
        } else {
            Rect::new(t.margin, y, 0, 0)
        };
        let y = if self.continue_id.is_some() { self.continue_rect.bottom() + t.mm(4.0) } else { y };
        self.header = Rect::new(t.margin, y, self.screen.w - 2 * t.margin, t.button_h);
        let sort_w = (self.header.w * 3 / 5).min(t.mm(51.0)).max(3 * t.target).min(self.header.w);
        self.sort.place(Rect::new(self.header.right() - sort_w, y, sort_w, t.button_h));
        self.status = Rect::new(t.margin, self.pager.rect().y - t.mm(6.0), self.screen.w - 2 * t.margin, t.mm(6.0));
        let top = self.header.bottom() + t.mm(3.0);
        let cols = ((self.screen.w - 2 * t.margin) / t.mm(22.0)).clamp(1, 4) as usize;
        let rows = ((self.status.y - top) / t.mm(34.0)).clamp(1, 2) as usize;
        self.grid = Grid::new(self.books.len(), cols, rows);
        self.grid.place(
            Rect::new(t.margin, top, self.screen.w - 2 * t.margin, (self.status.y - top).max(t.target)),
            t.mm(3.5),
            t.mm(3.0),
        );
        self.pager.set(self.pager.page, self.grid.paging().pages());
        self.grid.page = self.pager.page;
        self.prioritize();
        // Only the visible grid plus continue card retain decoded images.
        let keys: std::collections::HashSet<_> = self
            .grid
            .cells()
            .iter()
            .map(|(i, _)| &self.books[*i])
            .chain(self.continue_id.iter().filter_map(|id| self.book(id)))
            .filter_map(|b| match &b.cover {
                Cover::Ready(key) => Some(key.clone()),
                _ => None,
            })
            .collect();
        self.thumbnails.retain(|(key, _, _), _| keys.contains(key));
    }
    fn prioritize(&self) {
        let mut ids: Vec<_> = self.continue_id.iter().cloned().collect();
        ids.extend(self.grid.cells().iter().map(|(i, _)| self.books[*i].id.clone()));
        self.library.prioritize(ids);
    }
    fn apply_pending(&mut self, cx: &mut EventCx) {
        if self.touching || self.menu.is_some() || self.details.is_some() || self.reader.is_some() {
            return;
        }
        if let Some(snapshot) = self.pending.take() {
            for error in &snapshot.errors {
                if !self.errors.contains(error) {
                    log::warn!("{error}");
                    cx.toast(error.clone(), None);
                }
            }
            self.errors = snapshot.errors;
            self.index = snapshot.index;
            let page = self.pager.page;
            self.refresh(cx.theme);
            if self.pager.page != page {
                cx.refresh(self.screen, RefreshMode::Full);
            }
        }
    }
    fn book(&self, id: &str) -> Option<&Book> {
        self.books.iter().find(|b| b.id == id)
    }
    fn open(&mut self, id: String, cx: &mut EventCx) {
        if let Some(error) = &self.engine_error {
            cx.toast(error.clone(), None);
            return;
        }
        let Some(book) = self.book(&id) else { return };
        let path = book.path.clone();
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
        if let Some(r) = self.reader.take() {
            self.generation = r.generation();
        }
        for b in &self.books {
            self.states.insert(b.id.clone(), self.store.book(&b.id));
        }
        self.apply_pending(cx);
        self.refresh(cx.theme);
        cx.refresh(self.screen, RefreshMode::Full);
    }
    fn open_menu(&mut self, id: String, anchor: Point, t: &Theme) {
        let state = self.states.get(&id).cloned().unwrap_or_default();
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
        let mut state = self.store.book(id);
        if finished {
            state.finished = true;
        } else {
            state.hidden_from_continue = true;
        }
        match self.store.save_book(id, &state) {
            Ok(()) => {
                self.states.insert(id.to_owned(), state);
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
fn draw_cover(cx: &mut DrawCx, book: &Book, rect: Rect, store: &Store, cache: &mut ImageCache) {
    if let Cover::Ready(key) = &book.cover {
        let cache_key = (key.clone(), rect.w, rect.h);
        if !cache.contains_key(&cache_key)
            && let Ok(image) = covers::load(&store.cover_path(key))
            && cache.len() < 18
        {
            cache.insert(
                cache_key.clone(),
                image::imageops::resize(&image, rect.w as u32, rect.h as u32, image::imageops::FilterType::Triangle),
            );
        }
        if let Some(image) = cache.get(&cache_key) {
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
        if let Some(book) = self.continue_id.as_ref().and_then(|id| self.book(id)).cloned() {
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
            let progress = self.states.get(&book.id).cloned().unwrap_or_default().progress.min(10000);
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
            &format!("All books  {}", self.books.len()),
            t.heading,
            Rect { w: self.sort.rect().x - self.header.x - t.mm(2.0), ..self.header },
            HAlign::Left,
        );
        self.sort.draw(cx);
        for (i, cell) in self.grid.cells() {
            let book = &self.books[i];
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
            let label = status(&state);
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
        if self.books.is_empty() {
            let message = if self.scanning {
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
        cx.line(message, t.caption.gray(INK2), self.status, HAlign::Left);
        self.pager.draw(cx);
        if let Some((_, menu)) = &self.menu {
            menu.draw(cx);
        }
        if let Some(details) = &mut self.details {
            details.draw(cx);
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
        } else {
            if self.exit.on_gesture(&g) {
                return Action::Pop;
            }
            if self.rescan.on_gesture(&g) {
                self.library.rescan();
                self.scanning = true;
                self.working = true;
            }
            if self.sort.on_gesture(&g).is_some() {
                let mut settings = self.store.settings();
                settings.library_sort = self.selected_sort();
                if let Err(e) = self.store.save_settings(&settings) {
                    cx.toast(format!("Couldn't save sort: {e}"), None);
                }
                self.pager.page = 0;
                self.refresh(cx.theme);
                cx.refresh(self.screen, RefreshMode::Full);
            }
            let old_page = self.pager.page;
            self.pager.on_gesture(&g);
            if let Gesture::Swipe { dir, .. } = g {
                self.pager.on_swipe(dir);
            }
            self.grid.page = self.pager.page;
            if old_page != self.pager.page {
                self.thumbnails.clear();
                self.prioritize();
                cx.refresh(self.screen, RefreshMode::Full);
            }
            let mut selected = self.grid.on_gesture(&g).map(|event| match event {
                GridEvent::Tapped(i) => (self.books[i].id.clone(), false),
                GridEvent::LongPressed(i) => (self.books[i].id.clone(), true),
            });
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
    fn busy(&self) -> bool {
        self.working || self.reader.as_ref().is_some_and(Screen::busy)
    }
    fn deadline(&self) -> Option<std::time::Instant> {
        self.reader.as_ref().and_then(Screen::deadline)
    }
    fn on_tick(&mut self, cx: &mut EventCx) -> Action {
        self.reader.as_mut().map_or(Action::None, |r| r.on_tick(cx))
    }
    fn on_background(&mut self, cx: &mut EventCx) {
        if let Some(snapshot) = self.library.take() {
            self.scanning = snapshot.scanning;
            self.working = snapshot.working;
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
    fn on_quit(&mut self) {
        if let Some(r) = &mut self.reader {
            r.on_quit();
        }
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
        let sheet = Sheet::open("Book details", screen.h * 3 / 4, screen, t);
        let area = sheet.content(t);
        let mut pager = Pager::new(1);
        pager.place(Rect::new(area.x, area.bottom() - t.mm(11.0), area.w, t.mm(11.0)), t);
        let text = Rect { h: area.h - t.mm(13.0), ..area };
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
        Self { sheet, pager, pages: Vec::new(), content, text }
    }
    fn draw(&mut self, cx: &mut DrawCx) {
        if self.pages.is_empty() {
            self.pages = detail_pages(cx.text, &self.content, cx.theme.body, self.text);
            self.pager.set(0, self.pages.len());
        }
        self.sheet.draw(cx);
        if let Some(page) = self.pages.get(self.pager.page) {
            cx.paragraph(page, cx.theme.body, self.text, HAlign::Left);
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
        let mut books = Books::with_store(&t, screen, Rc::new(Worker::stub()), store);
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
