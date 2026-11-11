//! The Phase 2 spike app: a list of test books and a bare-bones reader, to
//! check crengine on the device and measure it. Tap the left side to go back a
//! page, the right side to go forward, the middle for a panel with font size
//! buttons and timings. The real screens replace this from Phase 3 on.

use std::path::PathBuf;
use std::time::Instant;

use crate::document::{self, CreDocument, CreEngine, Document, Layout, Paths};
use crate::geom::{Point, Rect};
use crate::gfx::{Canvas, HAlign, Icon, IconRenderer, TextRenderer, TextStyle, Weight};
use crate::platform::{DeviceInfo, RefreshMode, Update};
use crate::ui::gesture::{Direction, Gesture};

const BLACK: u8 = 0;
const WHITE: u8 = 255;
const MUTED: u8 = 96;
const LINE: u8 = 200;

/// A full flash every this many page turns clears e-ink ghosting.
const FLASH_EVERY: u32 = 6;
const DEFAULT_FONT_PT: f32 = 10.0;

enum Screen {
    Library { books: Vec<PathBuf>, pressed: Option<usize> },
    Reader(Box<Reader>),
}

struct Reader {
    doc: CreDocument,
    name: String,
    panel: bool,
    pressed: Option<PanelButton>,
    draw_ms: u32,
    font_change_ms: Option<u32>,
    pages_since_flash: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PanelButton {
    Smaller,
    Larger,
    Library,
}

/// Slow work to do after the next frame is on screen, so the user sees a
/// "please wait" message first.
enum Pending {
    Open(PathBuf),
    FontSize(u32),
}

pub struct App {
    canvas: Canvas,
    text: TextRenderer,
    icons: IconRenderer,
    dpi: f32,
    engine: Result<CreEngine, String>,
    paths: Paths,
    screen: Screen,
    pending: Option<Pending>,
    /// The current page as 8-bit gray, full screen.
    page: Vec<u8>,
    asleep: bool,
    exit: bool,
    updates: Vec<Update>,
}

impl App {
    pub fn new(info: &DeviceInfo) -> Self {
        let paths = Paths::for_device();
        let engine = CreEngine::new(paths.clone(), info.dpi).map_err(|e| format!("{e:#}"));
        if let Err(e) = &engine {
            log::error!("{e}");
        }
        let books = document::list_books(&paths.books);
        log::info!("{} test books in {}", books.len(), paths.books.display());
        Self {
            canvas: Canvas::new(info.width, info.height),
            text: TextRenderer::new(),
            icons: IconRenderer::default(),
            dpi: info.dpi as f32,
            engine,
            paths,
            screen: Screen::Library { books, pressed: None },
            pending: None,
            page: vec![WHITE; (info.width * info.height) as usize],
            asleep: false,
            exit: false,
            updates: Vec::new(),
        }
    }

    pub fn canvas(&self) -> &Canvas {
        &self.canvas
    }

    /// Screen areas changed since the last call, with their refresh intent.
    pub fn take_updates(&mut self) -> Vec<Update> {
        std::mem::take(&mut self.updates)
    }

    pub fn wants_exit(&self) -> bool {
        self.exit
    }

    pub fn has_pending_work(&self) -> bool {
        self.pending.is_some()
    }

    pub fn on_sleep(&mut self) {
        self.asleep = true;
    }

    pub fn on_wake(&mut self) {
        self.asleep = false;
        self.redraw_all(RefreshMode::Full);
    }

    pub fn redraw_all(&mut self, mode: RefreshMode) {
        match &self.screen {
            Screen::Library { .. } => self.draw_library(),
            Screen::Reader(_) => self.draw_reader(),
        }
        self.updates.push(Update { rect: self.canvas.bounds(), mode });
    }

    /// Does the slow work announced by the last frame.
    pub fn run_pending_work(&mut self) {
        match self.pending.take() {
            Some(Pending::Open(path)) => self.open_book(path),
            Some(Pending::FontSize(px)) => {
                if let Screen::Reader(r) = &mut self.screen {
                    let t = Instant::now();
                    r.doc.set_font_size(px);
                    let ms = t.elapsed().as_millis() as u32;
                    r.font_change_ms = Some(ms);
                    log::info!("font size {px} px: re-laid out in {ms} ms, {} pages", r.doc.page_count());
                }
                self.render_page();
                self.redraw_all(RefreshMode::Full);
            }
            None => {}
        }
    }

    pub fn on_gesture(&mut self, g: Gesture) {
        if self.asleep || self.pending.is_some() {
            return;
        }
        match self.screen {
            Screen::Library { .. } => self.library_gesture(g),
            Screen::Reader(_) => self.reader_gesture(g),
        }
    }

    // ---- Library ----

    fn library_rows(&self) -> (Rect, Vec<Rect>, Rect) {
        let (w, h) = (self.canvas.width() as i32, self.canvas.height() as i32);
        let margin = self.mm(7.0);
        let header = Rect::new(margin, self.mm(8.0), w - 2 * margin, self.mm(14.0));
        let row_h = self.mm(13.0);
        let count = match &self.screen {
            Screen::Library { books, .. } => books.len(),
            _ => 0,
        };
        let rows = (0..count as i32)
            .map(|i| Rect::new(margin, header.bottom() + self.mm(4.0) + i * row_h, w - 2 * margin, row_h))
            .collect();
        let exit = Rect::centered(Point::new(w / 2, h - self.mm(20.0)), self.mm(40.0), self.mm(12.0));
        (header, rows, exit)
    }

    fn draw_library(&mut self) {
        let (header, rows, exit) = self.library_rows();
        let bounds = self.canvas.bounds();
        self.canvas.fill_rect(bounds, WHITE);
        let title = TextStyle { size: self.pt(20.0), weight: Weight::SemiBold, gray: BLACK };
        self.text.draw(&mut self.canvas, "Test books", title, header, HAlign::Left);

        let Screen::Library { books, pressed } = &self.screen else { return };
        let (books, pressed) = (books.clone(), *pressed);
        if let Err(e) = &self.engine {
            let style = TextStyle { size: self.pt(10.0), weight: Weight::Regular, gray: BLACK };
            let rect = Rect::new(header.x, header.bottom(), header.w, self.mm(30.0));
            let msg = format!("The book engine didn't start: {e}");
            self.text.draw(&mut self.canvas, &msg, style, rect, HAlign::Left);
        } else if books.is_empty() {
            let style = TextStyle { size: self.pt(10.0), weight: Weight::Regular, gray: MUTED };
            let rect = Rect::new(header.x, header.bottom(), header.w, self.mm(14.0));
            let msg = format!("No EPUB files in {}", self.paths.books.display());
            self.text.draw(&mut self.canvas, &msg, style, rect, HAlign::Left);
        }
        for (i, (book, rect)) in books.iter().zip(&rows).enumerate() {
            self.draw_book_row(book, *rect, pressed == Some(i));
        }
        self.draw_button(exit, Some(Icon::Exit), "Exit", false);
    }

    fn draw_book_row(&mut self, book: &std::path::Path, rect: Rect, pressed: bool) {
        let (bg, fg) = if pressed { (BLACK, WHITE) } else { (WHITE, BLACK) };
        self.canvas.fill_rect(rect, bg);
        self.canvas.fill_rect(Rect::new(rect.x, rect.bottom() - 1, rect.w, 1), LINE);
        let name = book.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let style = TextStyle { size: self.pt(12.0), weight: Weight::Regular, gray: fg };
        let inner = Rect::new(rect.x + self.mm(2.0), rect.y, rect.w - self.mm(4.0), rect.h);
        self.text.draw(&mut self.canvas, &name, style, inner, HAlign::Left);
    }

    fn library_gesture(&mut self, g: Gesture) {
        let (_, rows, exit) = self.library_rows();
        let hit = |p: Point| rows.iter().position(|r| r.contains(p));
        match g {
            Gesture::Down(p) => {
                if let Some(i) = hit(p) {
                    self.set_library_pressed(Some(i), &rows);
                }
            }
            Gesture::Tap(p) => {
                let was = self.set_library_pressed(None, &rows);
                if exit.inset(-self.mm(2.0)).contains(p) {
                    self.exit = true;
                } else if let (Some(i), Some(j)) = (was, hit(p))
                    && i == j
                    && self.engine.is_ok()
                {
                    let Screen::Library { books, .. } = &self.screen else { return };
                    let path = books[i].clone();
                    self.show_message(&format!(
                        "Opening {}…",
                        path.file_stem().unwrap_or_default().to_string_lossy()
                    ));
                    self.pending = Some(Pending::Open(path));
                }
            }
            _ => {
                self.set_library_pressed(None, &rows);
            }
        }
    }

    /// Highlights a row (or none), redrawing what changed. Returns the old one.
    fn set_library_pressed(&mut self, new: Option<usize>, rows: &[Rect]) -> Option<usize> {
        let Screen::Library { books, pressed } = &mut self.screen else { return None };
        let old = std::mem::replace(pressed, new);
        if old != new {
            let books = books.clone();
            for i in [old, new].into_iter().flatten() {
                self.draw_book_row(&books[i], rows[i], new == Some(i));
                self.updates.push(Update { rect: rows[i], mode: RefreshMode::Fast });
            }
        }
        old
    }

    // ---- Reader ----

    fn open_book(&mut self, path: PathBuf) {
        let Ok(engine) = &self.engine else { return };
        let (w, h) = (self.canvas.width(), self.canvas.height());
        let layout = Layout {
            width: w,
            height: h,
            margins: [self.mm(7.0), self.mm(8.0), self.mm(7.0), self.mm(12.0)].map(|m| m as u32),
            font_size: self.pt(DEFAULT_FONT_PT) as u32,
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
                self.screen = Screen::Reader(Box::new(Reader {
                    doc,
                    name,
                    panel: false,
                    pressed: None,
                    draw_ms: 0,
                    font_change_ms: None,
                    pages_since_flash: 0,
                }));
                self.render_page();
                self.redraw_all(RefreshMode::Full);
            }
            Err(e) => {
                log::error!("{e:#}");
                self.show_message(&format!("Couldn't open {name}"));
                self.go_to_library();
            }
        }
    }

    fn go_to_library(&mut self) {
        let t = Instant::now();
        let books = document::list_books(&self.paths.books);
        // Dropping the reader closes the book and writes crengine's cache.
        self.screen = Screen::Library { books, pressed: None };
        log::info!("closed the book in {:?}, memory {:.1} MB", t.elapsed(), rss_mb());
        self.redraw_all(RefreshMode::Full);
    }

    /// Asks the engine for the current page and keeps it in `self.page`.
    fn render_page(&mut self) {
        let Screen::Reader(r) = &mut self.screen else { return };
        let stride = self.canvas.width() as usize;
        let t = Instant::now();
        if let Err(e) = r.doc.draw(&mut self.page, stride) {
            log::error!("{e:#}");
        }
        r.draw_ms = t.elapsed().as_millis() as u32;
        log::info!("page {}/{} drawn in {} ms", r.doc.current_page() + 1, r.doc.page_count(), r.draw_ms);
    }

    fn draw_reader(&mut self) {
        let bounds = self.canvas.bounds();
        self.canvas.blit_gray(bounds, &self.page, bounds.w as usize);
        self.draw_footer();
        if matches!(&self.screen, Screen::Reader(r) if r.panel) {
            self.draw_panel();
        }
    }

    fn footer_rect(&self) -> Rect {
        let (w, h) = (self.canvas.width() as i32, self.canvas.height() as i32);
        Rect::new(self.mm(7.0), h - self.mm(9.0), w - 2 * self.mm(7.0), self.mm(6.0))
    }

    fn draw_footer(&mut self) {
        let Screen::Reader(r) = &mut self.screen else { return };
        let (page, count) = (r.doc.current_page() + 1, r.doc.page_count());
        let rect = self.footer_rect();
        let style = TextStyle { size: self.pt(8.0), weight: Weight::Regular, gray: MUTED };
        self.canvas.fill_rect(rect, WHITE);
        self.text.draw(&mut self.canvas, &format!("{page} / {count}"), style, rect, HAlign::Right);
    }

    fn panel_layout(&self) -> (Rect, Rect, [(PanelButton, Rect); 3]) {
        let (w, h) = (self.canvas.width() as i32, self.canvas.height() as i32);
        let panel = Rect::new(0, h - self.mm(48.0), w, self.mm(48.0));
        let margin = self.mm(7.0);
        let info = Rect::new(margin, panel.y + self.mm(3.0), w - 2 * margin, self.mm(22.0));
        let (bw, bh) = (self.mm(22.0), self.mm(12.0));
        let y = info.bottom() + self.mm(4.0);
        let buttons = [
            (PanelButton::Smaller, Rect::new(margin, y, bw, bh)),
            (PanelButton::Larger, Rect::new(margin + bw + self.mm(4.0), y, bw, bh)),
            (PanelButton::Library, Rect::new(w - margin - self.mm(34.0), y, self.mm(34.0), bh)),
        ];
        (panel, info, buttons)
    }

    fn draw_panel(&mut self) {
        let (panel, info, buttons) = self.panel_layout();
        let Screen::Reader(r) = &mut self.screen else { return };
        let stats = r.doc.stats;
        let text = format!(
            "{}\nOpen {} ms · layout {} ms · page {} ms{} · {} pt · memory {:.1} MB",
            r.name,
            stats.load_ms,
            stats.layout_ms,
            r.draw_ms,
            r.font_change_ms.map(|ms| format!(" · font change {ms} ms")).unwrap_or_default(),
            (r.doc.font_size() as f32 * 72.0 / self.dpi).round(),
            rss_mb(),
        );
        let pressed = r.pressed;
        self.canvas.fill_rect(panel, WHITE);
        self.canvas.fill_rect(Rect::new(panel.x, panel.y, panel.w, self.mm(0.4)), BLACK);
        let style = TextStyle { size: self.pt(9.0), weight: Weight::Regular, gray: BLACK };
        self.text.draw(&mut self.canvas, &text, style, info, HAlign::Left);
        for (b, rect) in buttons {
            let label = match b {
                PanelButton::Smaller => "A−",
                PanelButton::Larger => "A+",
                PanelButton::Library => "Books",
            };
            self.draw_button(rect, None, label, pressed == Some(b));
        }
    }

    fn reader_gesture(&mut self, g: Gesture) {
        let Screen::Reader(r) = &self.screen else { return };
        if r.panel {
            return self.panel_gesture(g);
        }
        let (w, h) = (self.canvas.width() as i32, self.canvas.height() as i32);
        let step = match g {
            Gesture::Tap(p) => {
                let middle = Rect::new(w * 3 / 10, h * 3 / 10, w * 4 / 10, h * 4 / 10);
                if middle.contains(p) {
                    return self.set_panel(true);
                }
                if p.x < w * 3 / 10 { -1 } else { 1 }
            }
            Gesture::Swipe { dir: Direction::Left, .. } => 1,
            Gesture::Swipe { dir: Direction::Right, .. } => -1,
            _ => return,
        };
        self.turn_page(step);
    }

    fn panel_gesture(&mut self, g: Gesture) {
        let (panel, _, buttons) = self.panel_layout();
        let pad = self.mm(2.0);
        let hit = |p: Point| buttons.iter().find(|(_, rect)| rect.inset(-pad).contains(p)).map(|(b, _)| *b);
        let Screen::Reader(r) = &mut self.screen else { return };
        let was = r.pressed.take();
        match g {
            Gesture::Down(p) => r.pressed = hit(p),
            Gesture::Tap(p) if was.is_some() && was == hit(p) => return self.panel_action(was.unwrap()),
            Gesture::Tap(p) if !panel.contains(p) => return self.set_panel(false),
            _ => {}
        }
        if r.pressed != was {
            self.draw_panel();
            self.updates.push(Update { rect: panel, mode: RefreshMode::Fast });
        }
    }

    fn set_panel(&mut self, open: bool) {
        let Screen::Reader(r) = &mut self.screen else { return };
        r.panel = open;
        let (panel, ..) = self.panel_layout();
        self.draw_reader();
        self.updates.push(Update { rect: panel, mode: RefreshMode::Ui });
    }

    fn turn_page(&mut self, step: i32) {
        let Screen::Reader(r) = &mut self.screen else { return };
        let (cur, count) = (r.doc.current_page() as i32, r.doc.page_count() as i32);
        let next = (cur + step).clamp(0, count - 1);
        if next == cur {
            return;
        }
        r.doc.go_to_page(next as usize);
        r.pages_since_flash += 1;
        let mode = if r.pages_since_flash >= FLASH_EVERY {
            r.pages_since_flash = 0;
            RefreshMode::Full
        } else {
            RefreshMode::Page
        };
        self.render_page();
        self.redraw_all(mode);
    }

    fn panel_action(&mut self, b: PanelButton) {
        let Screen::Reader(r) = &mut self.screen else { return };
        let pt = (r.doc.font_size() as f32 * 72.0 / self.dpi).round();
        let new_pt = match b {
            PanelButton::Smaller => (pt - 1.0).max(6.0),
            PanelButton::Larger => (pt + 1.0).min(24.0),
            PanelButton::Library => return self.go_to_library(),
        };
        if new_pt != pt {
            let (panel, info, _) = self.panel_layout();
            self.draw_panel();
            let style = TextStyle { size: self.pt(9.0), weight: Weight::SemiBold, gray: BLACK };
            self.canvas.fill_rect(info, WHITE);
            self.text.draw(&mut self.canvas, &format!("Laying out at {new_pt} pt…"), style, info, HAlign::Left);
            self.updates.push(Update { rect: panel, mode: RefreshMode::Ui });
            self.pending = Some(Pending::FontSize(self.pt(new_pt) as u32));
        } else {
            let (panel, ..) = self.panel_layout();
            self.draw_panel();
            self.updates.push(Update { rect: panel, mode: RefreshMode::Fast });
        }
    }

    // ---- Shared ----

    /// A full-screen "please wait" message.
    fn show_message(&mut self, msg: &str) {
        let bounds = self.canvas.bounds();
        self.canvas.fill_rect(bounds, WHITE);
        let style = TextStyle { size: self.pt(14.0), weight: Weight::Regular, gray: BLACK };
        let rect = Rect::new(self.mm(7.0), 0, bounds.w - 2 * self.mm(7.0), bounds.h);
        self.text.draw(&mut self.canvas, msg, style, rect, HAlign::Center);
        self.updates.push(Update { rect: bounds, mode: RefreshMode::Ui });
    }

    fn draw_button(&mut self, rect: Rect, icon: Option<Icon>, label: &str, pressed: bool) {
        let radius = self.mm(3.0) as f32;
        self.canvas.fill_rect(rect, WHITE);
        let fg = if pressed {
            self.canvas.fill_rounded_rect(rect, radius, BLACK);
            WHITE
        } else {
            self.canvas.stroke_rounded_rect(rect, radius, self.mm(0.5) as f32, BLACK);
            BLACK
        };
        let style = TextStyle { size: self.pt(12.0), weight: Weight::SemiBold, gray: fg };
        let Some(icon) = icon else {
            self.text.draw(&mut self.canvas, label, style, rect, HAlign::Center);
            return;
        };
        // Icon and label, centred together.
        let (tw, _) = self.text.measure(label, style, None);
        let icon_size = self.pt(13.0) as i32;
        let gap = self.mm(2.0);
        let x = rect.center().x - (icon_size + gap + tw.ceil() as i32) / 2;
        let icon_rect = Rect::new(x, rect.center().y - icon_size / 2, icon_size, icon_size);
        self.icons.draw(&mut self.canvas, icon, icon_rect, fg);
        let label_rect = Rect::new(icon_rect.right() + gap, rect.y, rect.right() - icon_rect.right() - gap, rect.h);
        self.text.draw(&mut self.canvas, label, style, label_rect, HAlign::Left);
    }

    /// Millimetres to pixels.
    fn mm(&self, v: f32) -> i32 {
        (v * self.dpi / 25.4).round() as i32
    }

    /// Typographic points to pixels.
    fn pt(&self, v: f32) -> f32 {
        (v * self.dpi / 72.0).round()
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
