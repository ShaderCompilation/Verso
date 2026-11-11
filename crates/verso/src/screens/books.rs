//! Temporary themed book list; the metadata/cover library arrives in Phase 5.
use crate::document::{
    self, FontFace, Layout, Paths,
    thread::{Command, Reply, Worker},
};
use crate::geom::Rect;
use crate::gfx::HAlign;
use crate::platform::RefreshMode;
use crate::screens::reader::Reader;
use crate::screens::{Action, EventCx, Screen};
use crate::store::{Store, book_id};
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK2, Theme};
use crate::ui::widget::DrawCx;
use crate::ui::widgets::{List, ListEvent, Pager, Row, TopBar};
use std::path::PathBuf;
use std::rc::Rc;

pub struct Books {
    screen: Rect,
    worker: Rc<Worker>,
    store: Store,
    paths: Paths,
    books: Vec<PathBuf>,
    fonts: Vec<FontFace>,
    top: TopBar,
    list: List,
    pager: Pager,
    reader: Option<Reader>,
    generation: u64,
    error: Option<String>,
}
impl Books {
    pub fn new(t: &Theme, screen: Rect, worker: Rc<Worker>) -> Self {
        let mut top = TopBar::new("Books", true);
        top.place(screen, t);
        let mut pager = Pager::new(1);
        pager.place(Rect::new(t.margin, screen.bottom() - t.mm(11.0), screen.w - 2 * t.margin, t.mm(11.0)), t);
        let paths = Paths::for_device();
        let books = document::list_books(&paths.books);
        let mut s = Self {
            screen,
            worker,
            store: Store::new(),
            paths,
            books,
            fonts: Vec::new(),
            top,
            list: List::new(Vec::new()),
            pager,
            reader: None,
            generation: 0,
            error: None,
        };
        s.refresh(t);
        s
    }
    fn refresh(&mut self, t: &Theme) {
        let page = self.pager.page;
        self.list = List::new(
            self.books
                .iter()
                .map(|p| {
                    let state = book_id(p).ok().map(|id| self.store.book(&id));
                    let progress = state
                        .filter(|s| !s.position.0.is_empty())
                        .map_or_else(|| "New".to_owned(), |s| format!("{}%", s.progress / 100));
                    Row::new(p.file_stem().unwrap_or_default().to_string_lossy()).value(progress).chevron()
                })
                .collect(),
        );
        let y = t.top_bar + t.mm(3.0);
        self.list.place(Rect::new(0, y, self.screen.w, self.pager.rect().y - y), t.row, t.margin);
        self.pager.set(page, self.list.paging().pages());
        self.list.page = self.pager.page;
    }
    fn open(&mut self, i: usize, cx: &mut EventCx) {
        let path = self.books[i].clone();
        let id = match book_id(&path) {
            Ok(id) => id,
            Err(e) => {
                cx.toast(format!("Couldn't open book: {e}"), None);
                return;
            }
        };
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
}
impl Screen for Books {
    fn draw(&mut self, cx: &mut DrawCx) {
        if let Some(reader) = &mut self.reader {
            reader.draw(cx);
            return;
        }
        self.top.draw(cx);
        self.list.draw(cx);
        self.pager.draw(cx);
        if self.books.is_empty() || self.error.is_some() {
            let message =
                self.error.clone().unwrap_or_else(|| format!("No EPUB files in {}", self.paths.books.display()));
            cx.paragraph(
                &message,
                cx.theme.body.gray(INK2),
                Rect::new(cx.theme.margin, cx.theme.top_bar, self.screen.w - 2 * cx.theme.margin, cx.theme.mm(25.0)),
                HAlign::Left,
            );
        }
    }
    fn on_gesture(&mut self, g: Gesture, cx: &mut EventCx) -> Action {
        if let Some(r) = &mut self.reader {
            if matches!(r.on_gesture(g, cx), Action::Pop) {
                self.generation = r.generation();
                self.reader = None;
                self.refresh(cx.theme);
                cx.refresh(self.screen, RefreshMode::Full);
            }
            return Action::None;
        }
        if self.top.on_gesture(&g) {
            return Action::Pop;
        }
        self.pager.on_gesture(&g);
        if let Gesture::Swipe { dir, .. } = g {
            self.pager.on_swipe(dir);
        }
        self.list.page = self.pager.page;
        if let Some(ListEvent::Tapped(i)) = self.list.on_gesture(&g) {
            self.open(i, cx);
        }
        Action::None
    }
    fn busy(&self) -> bool {
        self.reader.as_ref().is_some_and(Screen::busy)
    }
    fn deadline(&self) -> Option<std::time::Instant> {
        self.reader.as_ref().and_then(Screen::deadline)
    }
    fn on_tick(&mut self, cx: &mut EventCx) -> Action {
        self.reader.as_mut().map_or(Action::None, |r| r.on_tick(cx))
    }
    fn on_background(&mut self, cx: &mut EventCx) {
        for reply in self.worker.drain() {
            match reply {
                Reply::Fonts(fonts) => {
                    self.fonts = fonts.clone();
                    if let Some(r) = &mut self.reader {
                        r.fonts = fonts;
                    }
                }
                reply => {
                    if let Some(r) = &mut self.reader {
                        r.receive(reply, cx);
                    } else if let Reply::Error { message, .. } = reply {
                        self.error = Some(message);
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
