//! First-time setup: a few pages over Home on the first launch (Welcome,
//! Turning pages, Ready), with Back / Skip / Next and "2 of 3". Every choice
//! is saved the moment it is made, through the same settings as Settings;
//! only Skip and Ready set `Settings::setup_done`, so quitting midway
//! shows setup again from Welcome with the choices in place. Settings →
//! Device → *Run setup again* pushes the same screen over Settings.
//!
//! Turning a page redraws the content area (a `Ui` refresh of what
//! changed); trying a zone on the sketch hints `Fast` for the sketch only.

use crate::geom::Rect;
use crate::gfx::{HAlign, Icon};
use crate::platform::{DeviceModel, RefreshMode};
use crate::reader::{BOOKMARK_CORNER_MM, Zone, tap_zone};
use crate::screens::system::{draw_zones, switch_row};
use crate::screens::{Action, EventCx, RelayoutState, Screen, untested};
use crate::store::{Settings, Store, TapZones};
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK2, Theme};
use crate::ui::widget::{DrawCx, Press, PressEvent};
use crate::ui::widgets::{Button, ButtonKind, Segmented, Toggle, TopBar};

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

/// Where setup was before the display turned.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SetupResume {
    pub page: usize,
    /// The untested-model line on Welcome.
    pub note: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Welcome,
    Turning,
    Ready,
}

impl Page {
    fn title(self) -> &'static str {
        match self {
            Page::Welcome => "Welcome",
            Page::Turning => "Turning pages",
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

pub struct SetupScreen {
    store: Store,
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
    // Ready
    auto_check: Toggle,
    finish: Button,
}

impl SetupScreen {
    pub fn new(t: &Theme, screen: Rect, store: Store, note: Option<String>) -> Self {
        let settings = store.settings();
        let pages = vec![Page::Welcome, Page::Turning, Page::Ready];
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
        let body_top = content.y + t.mm(4.0);
        let body = Rect::new(content.x, body_top, content.w, nav.y - t.mm(6.0) - body_top);

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

        let mut this = Self {
            store,
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
            zones,
            sketch,
            sketch_press: Press::default(),
            tried: None,
            sketch_fast: false,
            caption,
            corner,
            switches,
            auto_check,
            finish,
        };
        this.go(0);
        this
    }

    /// Setup rebuilt after the display turned, on the page it showed.
    pub fn resume(t: &Theme, screen: Rect, store: Store, state: &SetupResume) -> Self {
        let mut this = Self::new(t, screen, store, state.note.clone());
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
    fn finish(&self, cx: &mut EventCx) -> Action {
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
        let updates = ("Check for updates automatically", "Once a day, only when Wi-Fi is already on.");
        y = switch_row(cx, &mut self.auto_check, updates, c.x, y, c.w);
        cx.paragraph(
            "Run setup again: Settings → Advanced.",
            t.small.gray(INK2),
            Rect::new(c.x, y, c.w, t.mm(6.0)),
            HAlign::Left,
        );
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

    fn relayout_state(&self, state: &mut RelayoutState) {
        state.setup = Some(SetupResume { page: self.page, note: self.note.clone() });
    }

    fn draw(&mut self, cx: &mut DrawCx) {
        let page = self.current();
        if page == Page::Welcome {
            self.draw_welcome(cx);
            return;
        }
        self.top.draw(cx);
        if page != Page::Ready {
            self.skip.draw(cx);
        }
        match page {
            Page::Welcome => {}
            Page::Turning => self.draw_turning(cx),
            Page::Ready => self.draw_ready(cx),
        }
        self.back.draw(cx);
        let count = format!("{} of {}", self.page + 1, self.pages.len());
        cx.line(&count, cx.theme.small.gray(INK2), self.count, HAlign::Center);
        if page == Page::Ready { self.finish.draw(cx) } else { self.next.draw(cx) }
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
        if page == Page::Turning {
            self.turning_gesture(&g, cx);
        }
        Action::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Point;
    use std::time::Instant;

    const SCREEN: Rect = Rect { x: 0, y: 0, w: 1272, h: 1696 };

    fn setup(store: &Store) -> (SetupScreen, Theme) {
        let t = Theme::for_dpi(300);
        (SetupScreen::new(&t, SCREEN, store.clone(), None), t)
    }

    fn draw(s: &mut SetupScreen, t: &Theme) {
        use crate::gfx::{Canvas, IconRenderer, TextRenderer};
        let (mut canvas, mut text, mut icons) =
            (Canvas::new(SCREEN.w as u32, SCREEN.h as u32), TextRenderer::new(), IconRenderer::default());
        s.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, t, &mut vec![]));
    }

    /// Draws (which places the switches), then taps the middle of \`rect\`.
    fn tap(s: &mut SetupScreen, t: &Theme, rect: impl Fn(&SetupScreen) -> Rect) -> Action {
        draw(s, t);
        let p = rect(s).center();
        let p = Point::new(p.x, p.y);
        let mut cx = EventCx {
            theme: t,
            screen: SCREEN,
            now: Instant::now(),
            battery: None,
            toast: &mut None,
            forced: &mut vec![],
            needs_redraw: &mut false,
        };
        s.on_gesture(Gesture::Down(p), &mut cx);
        s.on_gesture(Gesture::Tap(p), &mut cx)
    }

    #[test]
    fn choices_save_at_once_and_only_finishing_ends_setup() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::at(dir.path().to_path_buf());
        assert!(due(&store.settings()));
        let (mut s, t) = setup(&store);
        tap(&mut s, &t, |s| s.start.rect());
        assert_eq!(s.current(), Page::Turning);
        tap(&mut s, &t, |s| s.zones.rect().columns(3, 0)[2]);
        tap(&mut s, &t, |s| s.corner.rect());
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
        s.go(2);
        assert!(matches!(tap(&mut s, &t, |s| s.back.rect()), Action::None));
        assert!(matches!(tap(&mut s, &t, |s| s.skip.rect()), Action::Pop));
        assert!(store.settings().setup_done);
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
