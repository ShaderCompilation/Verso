//! A book screen. Only displayed pages are persisted; speculative renders never are.
use crate::document::{
    FontFace, Metadata, Position, TocEntry,
    thread::{Command, Page, Reply, Worker},
};
use crate::geom::Rect;
use crate::gfx::HAlign;
use crate::platform::RefreshMode;
use crate::reader::{self, PageCache, Refresh, Zone};
use crate::screens::contents::{Contents, ContentsEvent};
use crate::screens::reader_menu::{MenuEvent, ReaderMenu};
use crate::screens::text_sheet::{FontEvent, FontList, TextEvent, TextSheet};
use crate::screens::{Action, EventCx, Screen};
use crate::store::{Settings, Store};
use crate::ui::gesture::{Direction, Gesture};
use crate::ui::theme::{INK2, PAPER, Theme};
use crate::ui::widget::DrawCx;
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
    save_pending: bool,
    fresh_open: bool,
    error: Option<String>,
}
enum Overlay {
    None,
    Menu(Box<ReaderMenu>),
    Text(Box<TextSheet>),
    Fonts(Box<FontList>),
    Contents(Box<Contents>),
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
        Self {
            screen,
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
            refresh: Refresh::default(),
            overlay: Overlay::None,
            layout_at: None,
            laying_out: true,
            anchor,
            turn_pending: false,
            save_pending: false,
            fresh_open: true,
            error: None,
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
            Reply::Opened { generation, metadata, toc, pages, current } if generation == self.cache.generation => {
                self.metadata = metadata;
                self.toc = toc;
                self.pages = pages.max(1);
                self.target = current.min(self.pages - 1);
                if matches!(self.overlay, Overlay::Menu(_) | Overlay::Contents(_)) {
                    cx.redraw();
                }
            }
            Reply::Page(page) if page.generation == self.cache.generation => {
                self.cache.insert(page.clone(), self.target);
                if page.number == self.target {
                    self.display(page, cx);
                }
            }
            Reply::Error { generation, message } if generation == 0 || generation == self.cache.generation => {
                log::error!("{message}");
                self.error = Some(message.clone());
                self.laying_out = false;
                self.layout_at = None;
                self.target = self.shown.as_ref().map_or(0, |p| p.number);
                cx.toast(message, None);
            }
            _ => {}
        }
    }
    fn display(&mut self, page: Arc<Page>, cx: &mut EventCx) {
        let layout_changed = self.laying_out;
        let different = self.shown.as_ref().is_none_or(|p| p.generation != page.generation || p.number != page.number);
        if !different {
            return;
        }
        self.laying_out = false;
        self.anchor = page.position.clone();
        self.shown = Some(page.clone());
        self.save_pending = true;
        self.error = None;
        let chapter_start = self.toc.iter().any(|c| c.level <= 2 && c.page == page.number);
        let mode = if layout_changed {
            RefreshMode::Full
        } else if self.turn_pending {
            self.refresh.turn(page.images, chapter_start)
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
        self.target = page;
        self.turn_pending = true;
        if let Some(p) = self.cache.get(page) {
            self.display(p, cx);
        } else if self.cache.request(page) {
            self.worker.send(Command::Page { generation: self.cache.generation, number: page });
        }
    }
    fn jump_position(&mut self, page: usize, position: Position, cx: &mut EventCx) {
        if self.laying_out || self.layout_at.is_some() {
            return;
        }
        self.target = page.min(self.pages.saturating_sub(1));
        self.turn_pending = true;
        if let Some(p) = self.cache.get(self.target) {
            self.display(p, cx);
        } else {
            self.worker.send(Command::Position { generation: self.cache.generation, position });
        }
    }
    fn menu(&mut self, cx: &EventCx) {
        self.overlay = Overlay::Menu(Box::new(ReaderMenu::new(
            cx.theme,
            self.screen,
            &self.metadata.title,
            self.target,
            self.pages,
            *cx.frontlight,
        )));
    }
    fn text(&mut self, t: &Theme) {
        self.overlay =
            Overlay::Text(Box::new(TextSheet::new(t, self.screen, &self.settings.typography, self.settings.footer)));
    }
    fn changed(&mut self) {
        // A short debounce makes several quick taps one layout.
        self.cache.reset(self.cache.generation + 1);
        self.layout_at = Some(Instant::now() + Duration::from_millis(180));
        if let Err(e) = self.store.save_settings(&self.settings) {
            log::error!("saving typography: {e:#}");
            self.error = Some("Couldn't save text settings".into());
        }
    }
    fn save(&mut self) {
        if !self.save_pending {
            return;
        }
        let Some(page) = &self.shown else {
            return;
        };
        let mut state = self.store.book(&self.id);
        state.presented(
            page.position.clone(),
            page.number,
            page.progress,
            SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()),
            self.fresh_open,
        );
        match self.store.save_book(&self.id, &state) {
            Ok(()) => {
                self.save_pending = false;
                self.fresh_open = false;
            }
            Err(e) => {
                log::error!("saving position: {e:#}");
                self.error = Some("Couldn't save your place".into());
            }
        }
    }
    pub fn close(&mut self) {
        self.save();
        self.worker.send(Command::Close);
    }
}
impl Screen for Reader {
    fn draw(&mut self, cx: &mut DrawCx) {
        let t = cx.theme;
        if let Some(page) = &self.shown {
            cx.canvas.blit_gray(self.screen, &page.pixels, self.screen.w as usize);
            if self.settings.footer {
                let r =
                    Rect::new(t.mm(7.0), self.screen.bottom() - t.mm(9.0), self.screen.w - 2 * t.mm(7.0), t.mm(6.0));
                cx.fill(r, PAPER);
                cx.line(
                    &reader::footer(&self.toc, page.number, self.pages),
                    t.caption.gray(INK2),
                    Rect { w: r.w * 4 / 5, ..r },
                    HAlign::Left,
                );
                let progress = if page.number + 1 == self.pages { 100 } else { page.progress / 100 };
                cx.line(&format!("{progress}%"), t.caption.gray(INK2), r, HAlign::Right);
            }
        } else {
            cx.line("Opening book…", t.heading, self.screen.inset(t.margin), HAlign::Center);
        }
        match &mut self.overlay {
            Overlay::None => {}
            Overlay::Menu(menu) => {
                let title = reader::chapter(&self.toc, menu.progress.value as usize)
                    .map(|i| self.toc[i].title.as_str())
                    .unwrap_or("Book");
                menu.draw(cx, title, self.pages);
            }
            Overlay::Text(sheet) => sheet.draw(cx, &self.settings.typography),
            Overlay::Fonts(fonts) => fonts.draw(cx),
            Overlay::Contents(contents) => contents.draw(cx),
        }
        if self.laying_out || self.layout_at.is_some() || self.error.is_some() {
            let message =
                self.error.as_deref().unwrap_or(if self.shown.is_some() { "Laying out…" } else { "Opening book…" });
            let width = self.screen.w - 2 * t.margin;
            let r = Rect::new(t.margin, t.mm(16.0), width, t.mm(10.0));
            cx.fill(r, PAPER);
            cx.line(message, t.small, r, HAlign::Center);
        }
    }
    fn on_gesture(&mut self, g: Gesture, cx: &mut EventCx) -> Action {
        if !matches!(self.overlay, Overlay::None) {
            // Overlay handlers can change pressed state or slider position
            // without returning an action. Keep immediate control feedback.
            cx.redraw();
        }
        match &mut self.overlay {
            Overlay::Menu(menu) => match menu.on_gesture(&g) {
                Some(MenuEvent::Books) => {
                    self.close();
                    return Action::Pop;
                }
                Some(MenuEvent::Close) => self.overlay = Overlay::None,
                Some(MenuEvent::Contents) => {
                    self.overlay =
                        Overlay::Contents(Box::new(Contents::new(cx.theme, self.screen, &self.toc, self.target)))
                }
                Some(MenuEvent::Text) => self.text(cx.theme),
                Some(MenuEvent::Page(page)) => self.go(page, cx),
                Some(MenuEvent::Chapter(delta)) => {
                    let chapter = if delta > 0 {
                        self.toc.iter().find(|c| c.level <= 2 && c.page > self.target)
                    } else {
                        self.toc.iter().rev().find(|c| c.level <= 2 && c.page < self.target)
                    };
                    if let Some(c) = chapter {
                        let (page, position) = (c.page, c.position.clone());
                        self.jump_position(page, position, cx);
                    }
                }
                Some(MenuEvent::Light(value)) => {
                    *cx.frontlight = Some(value);
                    *cx.light_request = Some(value);
                }
                None => {}
            },
            Overlay::Text(sheet) => {
                match sheet.on_gesture(&g, &mut self.settings.typography, &mut self.settings.footer) {
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
            Overlay::Fonts(fonts) => match fonts.on_gesture(&g) {
                Some(FontEvent::Close) => self.text(cx.theme),
                Some(FontEvent::Chosen(name)) => {
                    self.settings.typography.font = name;
                    self.changed();
                    self.text(cx.theme);
                }
                None => {}
            },
            Overlay::Contents(contents) => match contents.on_gesture(&g) {
                Some(ContentsEvent::Close) => self.menu(cx),
                Some(ContentsEvent::Position(page, position)) => {
                    self.overlay = Overlay::None;
                    self.jump_position(page, position, cx);
                }
                None => {}
            },
            Overlay::None => {
                let step = match g {
                    Gesture::Tap(p) => match reader::tap_zone(self.screen, p) {
                        Zone::Menu => {
                            self.menu(cx);
                            cx.redraw();
                            return Action::None;
                        }
                        Zone::Back => -1,
                        Zone::Forward => 1,
                    },
                    Gesture::Swipe { dir: Direction::Left, .. } => 1,
                    Gesture::Swipe { dir: Direction::Right, .. } => -1,
                    _ => return Action::None,
                };
                self.go(self.target.saturating_add_signed(step), cx);
            }
        }
        Action::None
    }
    fn busy(&self) -> bool {
        self.laying_out || self.layout_at.is_some() || self.shown.as_ref().is_some_and(|p| p.number != self.target)
    }
    fn deadline(&self) -> Option<Instant> {
        let step = match &self.overlay {
            Overlay::Text(s) => s.deadline(),
            _ => None,
        };
        [self.layout_at, step].into_iter().flatten().min()
    }
    fn on_tick(&mut self, cx: &mut EventCx) -> Action {
        if let Overlay::Text(s) = &mut self.overlay
            && s.on_tick(cx.now, &mut self.settings.typography)
        {
            self.changed();
        }
        if self.layout_at.is_some_and(|t| cx.now >= t) {
            self.layout_at = None;
            self.laying_out = true;
            self.worker.send(Command::Layout {
                generation: self.cache.generation,
                typography: self.settings.typography.clone(),
                footer: self.settings.footer,
                position: self.anchor.clone(),
            });
        }
        Action::None
    }
    fn after_present(&mut self) {
        self.save();
    }
    fn on_sleep(&mut self) {
        self.save();
        // Dropping the controls cancels held steppers and slider drags.
        self.overlay = Overlay::None;
    }
    fn on_quit(&mut self) {
        self.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::BookState;
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
            frontlight: &mut None,
            light_request: &mut None,
            toast: &mut None,
            forced: &mut Vec::new(),
            needs_redraw: &mut false,
        };
        let page = |generation, number| {
            Reply::Page(Arc::new(Page {
                generation,
                number,
                pixels: Vec::new(),
                position: Position(format!("page-{number}")),
                progress: 2500,
                images: false,
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
        let mut light = None;
        let mut request = None;
        let mut toast = None;
        let mut forced = Vec::new();
        let mut cx = EventCx {
            theme: &theme,
            screen,
            now: Instant::now(),
            frontlight: &mut light,
            light_request: &mut request,
            toast: &mut toast,
            forced: &mut forced,
            needs_redraw: &mut false,
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
                pixels: vec![255; 400],
                position: Position(format!("page-{number}")),
                progress: 2500,
                images: false,
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
        let mut state = store.book("book");
        assert!(!state.finished && !state.hidden_from_continue);
        assert!(state.last_opened > 10);
        state.hidden_from_continue = true;
        store.save_book("book", &state)?;
        r.target = 1;
        r.receive(Reply::Page(page(1)), &mut cx);
        r.after_present();
        assert!(store.book("book").hidden_from_continue);
        assert_eq!(store.book("book").position.0, "page-1");
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
