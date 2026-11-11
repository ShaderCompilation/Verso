//! The Phase 2 engine spike: a list of test books and a bare-bones reader, to
//! check crengine on the device and measure it. Tap the left side to go back a
//! page, the right side to go forward, the middle for a panel with font size
//! buttons and timings. Reached from the widget gallery; the real reader
//! replaces it in Phase 4.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Instant;

use crate::document::{self, CreDocument, CreEngine, Document, Layout, Paths};
use crate::geom::Rect;
use crate::gfx::HAlign;
use crate::platform::RefreshMode;
use crate::screens::{Action, EventCx, Screen};
use crate::ui::gesture::{Direction, Gesture};
use crate::ui::theme::{INK, INK2, PAPER, Theme};
use crate::ui::widget::DrawCx;
use crate::ui::widgets::{Button, ButtonKind, List, ListEvent, Pager, Row, TopBar};

/// A full flash every this many page turns clears e-ink ghosting.
const FLASH_EVERY: u32 = 6;
const DEFAULT_FONT_PT: f32 = 10.0;

/// crengine can only be started once per process.
static ENGINE: OnceLock<Result<CreEngine, String>> = OnceLock::new();

pub struct Spike {
    paths: Paths,
    dpi: u32,
    screen: Rect,
    view: View,
    top_bar: TopBar,
    books: Vec<PathBuf>,
    list: List,
    pager: Pager,
    /// Slow work to do once its "please wait" message is on screen.
    pending: Option<Pending>,
    message: Option<String>,
}

enum View {
    Books,
    Reader(Box<Reader>),
}

struct Reader {
    doc: CreDocument,
    name: String,
    /// The current page as 8-bit gray, full screen.
    page: Vec<u8>,
    panel: bool,
    smaller: Button,
    larger: Button,
    close: Button,
    /// Page number and count at the last render (the engine's getters need
    /// `&mut`, drawing only has `&`).
    page_no: usize,
    pages: usize,
    draw_ms: u32,
    font_change_ms: Option<u32>,
    pages_since_flash: u32,
}

enum Pending {
    StartEngine,
    Open(PathBuf),
    FontSize(u32),
}

impl Spike {
    pub fn new(theme: &Theme, screen: Rect) -> Self {
        let mut top_bar = TopBar::new("Test books", true);
        top_bar.place(screen, theme);
        let mut pager = Pager::new(1);
        pager.place(
            Rect::new(theme.margin, screen.bottom() - theme.mm(11.0), screen.w - 2 * theme.margin, theme.mm(11.0)),
            theme,
        );
        let started = ENGINE.get().is_some();
        Self {
            paths: Paths::for_device(),
            dpi: theme.dpi as u32,
            screen,
            view: View::Books,
            top_bar,
            books: Vec::new(),
            list: List::new(Vec::new()),
            pager,
            pending: (!started).then_some(Pending::StartEngine),
            message: (!started).then(|| "Starting the book engine…".to_owned()),
        }
        .with_books(theme)
    }

    fn with_books(mut self, theme: &Theme) -> Self {
        self.load_books(theme);
        self
    }

    fn engine(&self) -> Option<&'static CreEngine> {
        ENGINE.get().and_then(|e| e.as_ref().ok())
    }

    fn load_books(&mut self, theme: &Theme) {
        self.books = document::list_books(&self.paths.books);
        log::info!("{} test books in {}", self.books.len(), self.paths.books.display());
        let rows = self
            .books
            .iter()
            .map(|b| Row::new(b.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()))
            .collect();
        self.list = List::new(rows);
        let area = Rect::new(0, self.top_bar.rect().bottom() + theme.mm(2.0), self.screen.w, 0);
        let area = Rect { h: self.pager.rect().y - area.y - theme.mm(2.0), ..area };
        self.list.place(area, theme.row, theme.margin);
        self.pager.set(0, self.list.paging().pages());
    }

    fn mm(&self, v: f32) -> i32 {
        (v * self.dpi as f32 / 25.4).round() as i32
    }

    // ---- Reader ----

    fn open_book(&mut self, path: PathBuf, cx: &mut EventCx) {
        let Some(engine) = self.engine() else { return };
        let layout = Layout {
            width: self.screen.w as u32,
            height: self.screen.h as u32,
            margins: [self.mm(7.0), self.mm(8.0), self.mm(7.0), self.mm(12.0)].map(|m| m as u32),
            font_size: cx.theme.pt(DEFAULT_FONT_PT) as u32,
        };
        let name = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let rss_before = rss_mb();
        match engine.open(&path, &layout) {
            Ok(mut doc) => {
                let (stats, pages, lang) = (doc.stats, doc.page_count(), doc.language());
                let title = doc.title();
                let name = if title.trim().is_empty() { name } else { title };
                log::info!(
                    "opened {name}: load {} ms, layout {} ms, {pages} pages, language {lang:?}, \
                     memory {rss_before:.1} → {:.1} MB",
                    stats.load_ms,
                    stats.layout_ms,
                    rss_mb()
                );
                let mut reader = Reader {
                    doc,
                    name,
                    page: vec![PAPER; self.screen.w as usize * self.screen.h as usize],
                    panel: false,
                    smaller: Button::new("A−", ButtonKind::Secondary),
                    larger: Button::new("A+", ButtonKind::Secondary),
                    close: Button::new("Books", ButtonKind::Secondary),
                    page_no: 0,
                    pages: 0,
                    draw_ms: 0,
                    font_change_ms: None,
                    pages_since_flash: 0,
                };
                let (_, _, buttons) = self.panel_layout();
                reader.smaller.place(buttons[0], cx.theme);
                reader.larger.place(buttons[1], cx.theme);
                reader.close.place(buttons[2], cx.theme);
                render_page(&mut reader, self.screen.w as usize);
                self.view = View::Reader(Box::new(reader));
                cx.refresh(self.screen, RefreshMode::Full);
            }
            Err(e) => {
                log::error!("{e:#}");
                cx.toast(format!("Couldn't open {name}"), None);
            }
        }
    }

    fn close_book(&mut self, cx: &mut EventCx) {
        let t = Instant::now();
        // Dropping the reader closes the book and writes crengine's cache.
        self.view = View::Books;
        log::info!("closed the book in {:?}, memory {:.1} MB", t.elapsed(), rss_mb());
        cx.refresh(self.screen, RefreshMode::Full);
    }

    fn footer_rect(&self) -> Rect {
        Rect::new(self.mm(7.0), self.screen.h - self.mm(9.0), self.screen.w - 2 * self.mm(7.0), self.mm(6.0))
    }

    /// (panel, info text, [A−, A+, Books]).
    fn panel_layout(&self) -> (Rect, Rect, [Rect; 3]) {
        let (w, h) = (self.screen.w, self.screen.h);
        let panel = Rect::new(0, h - self.mm(48.0), w, self.mm(48.0));
        let margin = self.mm(5.0);
        let info = Rect::new(margin, panel.y + self.mm(3.0), w - 2 * margin, self.mm(22.0));
        let (bw, bh) = (self.mm(22.0), self.mm(10.0));
        let y = info.bottom() + self.mm(5.0);
        let buttons = [
            Rect::new(margin, y, bw, bh),
            Rect::new(margin + bw + self.mm(4.0), y, bw, bh),
            Rect::new(w - margin - self.mm(34.0), y, self.mm(34.0), bh),
        ];
        (panel, info, buttons)
    }

    fn reader_gesture(&mut self, g: Gesture, cx: &mut EventCx) -> Action {
        let (w, h) = (self.screen.w, self.screen.h);
        let View::Reader(r) = &mut self.view else { return Action::None };
        if r.panel {
            let (panel, ..) = self.panel_layout();
            let View::Reader(r) = &mut self.view else { return Action::None };
            let pt = (r.doc.font_size() as f32 * 72.0 / cx.theme.dpi).round();
            let new_pt = if r.smaller.on_gesture(&g) {
                (pt - 1.0).max(6.0)
            } else if r.larger.on_gesture(&g) {
                (pt + 1.0).min(24.0)
            } else if r.close.on_gesture(&g) {
                self.close_book(cx);
                return Action::None;
            } else {
                if matches!(g, Gesture::Tap(p) if !panel.contains(p)) {
                    r.panel = false;
                }
                return Action::None;
            };
            if new_pt != pt {
                self.message = Some(format!("Laying out at {new_pt} pt…"));
                self.pending = Some(Pending::FontSize(cx.theme.pt(new_pt) as u32));
            }
            return Action::None;
        }
        let step = match g {
            Gesture::Tap(p) => {
                let middle = Rect::new(w * 3 / 10, h * 3 / 10, w * 4 / 10, h * 4 / 10);
                if middle.contains(p) {
                    r.panel = true;
                    return Action::None;
                }
                if p.x < w * 3 / 10 { -1 } else { 1 }
            }
            Gesture::Swipe { dir: Direction::Left, .. } => 1,
            Gesture::Swipe { dir: Direction::Right, .. } => -1,
            _ => return Action::None,
        };
        let (cur, count) = (r.doc.current_page() as i32, r.doc.page_count() as i32);
        let next = (cur + step).clamp(0, count - 1);
        if next != cur {
            r.doc.go_to_page(next as usize);
            render_page(r, w as usize);
            r.pages_since_flash += 1;
            let mode = if r.pages_since_flash >= FLASH_EVERY {
                r.pages_since_flash = 0;
                RefreshMode::Full
            } else {
                RefreshMode::Page
            };
            cx.refresh(self.screen, mode);
        }
        Action::None
    }

    fn draw_reader(&self, cx: &mut DrawCx, r: &Reader) {
        let t = cx.theme;
        cx.canvas.blit_gray(self.screen, &r.page, self.screen.w as usize);
        let footer = self.footer_rect();
        cx.fill(footer, PAPER);
        let label = format!("{} / {}", r.page_no + 1, r.pages);
        cx.line(&label, t.caption.gray(INK2), footer, HAlign::Right);
        if !r.panel {
            return;
        }
        let (panel, info, _) = self.panel_layout();
        cx.fill(panel, PAPER);
        cx.hline(panel.y, 0, panel.w, t.border, INK);
        let stats = r.doc.stats;
        let text = format!(
            "{}\nOpen {} ms · layout {} ms · page {} ms{} · {} pt · memory {:.1} MB",
            r.name,
            stats.load_ms,
            stats.layout_ms,
            r.draw_ms,
            r.font_change_ms.map(|ms| format!(" · font change {ms} ms")).unwrap_or_default(),
            (r.doc.font_size() as f32 * 72.0 / t.dpi).round(),
            rss_mb(),
        );
        cx.paragraph(&text, t.body, info, HAlign::Left);
        r.smaller.draw(cx);
        r.larger.draw(cx);
        r.close.draw(cx);
    }
}

/// Asks the engine for the current page and keeps it in `r.page`.
fn render_page(r: &mut Reader, stride: usize) {
    let t = Instant::now();
    if let Err(e) = r.doc.draw(&mut r.page, stride) {
        log::error!("{e:#}");
    }
    r.draw_ms = t.elapsed().as_millis() as u32;
    (r.page_no, r.pages) = (r.doc.current_page(), r.doc.page_count());
    log::info!("page {}/{} drawn in {} ms", r.page_no + 1, r.pages, r.draw_ms);
}

impl Screen for Spike {
    fn draw(&mut self, cx: &mut DrawCx) {
        let t = cx.theme;
        if let Some(msg) = &self.message {
            let rect = self.screen.inset_xy(t.margin, 0);
            cx.paragraph(msg, t.heading.weight(crate::gfx::Weight::Regular), rect, HAlign::Center);
            return;
        }
        if let View::Reader(r) = &self.view {
            self.draw_reader(cx, r);
            return;
        }
        self.top_bar.draw(cx);
        match ENGINE.get() {
            Some(Err(e)) => {
                let rect = Rect::new(t.margin, self.top_bar.rect().bottom(), self.screen.w - 2 * t.margin, t.mm(30.0));
                cx.paragraph(&format!("The book engine didn't start: {e}"), t.body, rect, HAlign::Left);
            }
            _ if self.books.is_empty() => {
                let rect = Rect::new(t.margin, self.top_bar.rect().bottom(), self.screen.w - 2 * t.margin, t.mm(14.0));
                let msg = format!("No EPUB files in {}", self.paths.books.display());
                cx.paragraph(&msg, t.body.gray(INK2), rect, HAlign::Left);
            }
            _ => {}
        }
        self.list.draw(cx);
        self.pager.draw(cx);
    }

    fn on_gesture(&mut self, g: Gesture, cx: &mut EventCx) -> Action {
        if self.pending.is_some() {
            return Action::None;
        }
        if matches!(self.view, View::Reader(_)) {
            return self.reader_gesture(g, cx);
        }
        if self.top_bar.on_gesture(&g) {
            return Action::Pop;
        }
        if let Some(page) = self.pager.on_gesture(&g) {
            self.list.page = page;
        }
        if let Gesture::Swipe { dir, .. } = g
            && let Some(page) = self.pager.on_swipe(dir)
        {
            self.list.page = page;
        }
        if let Some(ListEvent::Tapped(i)) = self.list.on_gesture(&g)
            && self.engine().is_some()
        {
            let path = self.books[i].clone();
            self.message = Some(format!("Opening {}…", path.file_stem().unwrap_or_default().to_string_lossy()));
            self.pending = Some(Pending::Open(path));
        }
        Action::None
    }

    fn has_pending_work(&self) -> bool {
        self.pending.is_some()
    }

    fn run_pending_work(&mut self, cx: &mut EventCx) -> Action {
        self.message = None;
        match self.pending.take() {
            Some(Pending::StartEngine) => {
                let engine = CreEngine::new(self.paths.clone(), self.dpi).map_err(|e| format!("{e:#}"));
                if let Err(e) = &engine {
                    log::error!("{e}");
                }
                let _ = ENGINE.set(engine);
            }
            Some(Pending::Open(path)) => self.open_book(path, cx),
            Some(Pending::FontSize(px)) => {
                if let View::Reader(r) = &mut self.view {
                    let t = Instant::now();
                    r.doc.set_font_size(px);
                    let ms = t.elapsed().as_millis() as u32;
                    r.font_change_ms = Some(ms);
                    log::info!("font size {px} px: re-laid out in {ms} ms, {} pages", r.doc.page_count());
                    render_page(r, self.screen.w as usize);
                }
                cx.refresh(self.screen, RefreshMode::Full);
            }
            None => {}
        }
        Action::None
    }
}

/// Resident memory of this process, in MB (0 if unknown).
fn rss_mb() -> f32 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            let line = s.lines().find(|l| l.starts_with("VmRSS:"))?;
            line.split_whitespace().nth(1)?.parse::<f32>().ok()
        })
        .map_or(0.0, |kb| kb / 1024.0)
}
