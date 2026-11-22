//! The centre-tap menu. Controls use the shared toolkit.
use crate::geom::{Point, Rect};
use crate::gfx::{HAlign, Icon};
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK, INK2, PAPER, Theme};
use crate::ui::widget::{DrawCx, Press, PressEvent};
use crate::ui::widgets::{Button, ButtonKind, IconButton, Slider, SliderEvent, TopBar};

pub enum MenuEvent {
    Books,
    Close,
    Contents,
    Search,
    Highlights,
    Text,
    /// Page layout: margins and reader bars.
    Layout,
    /// The `Battery + ⌄` control: open the Top Drawer.
    QuickSettings,
    Page(usize),
    Chapter(isize),
}
pub struct ReaderMenu {
    top: TopBar,
    search: IconButton,
    /// The `Battery + ⌄` control in the top bar.
    quick: Rect,
    quick_press: Press,
    panel: Rect,
    info: Rect,
    pub progress: Slider,
    prev: IconButton,
    next: IconButton,
    contents: Button,
    highlights: Button,
    text: Button,
    layout: Button,
    outside: bool,
}
impl ReaderMenu {
    pub fn new(t: &Theme, screen: Rect, title: &str, page: usize, pages: usize) -> Self {
        let m = t.margin;
        let wide = screen.w > screen.h;
        let mut top = TopBar::new(title, true);
        top.serif = false;
        top.place(screen, t);
        let bar = top.rect();
        let quick_w = t.mm(19.5);
        let quick =
            Rect::new(screen.right() - m + t.mm(1.5) - quick_w, bar.center().y - t.target / 2, quick_w, t.target);
        let mut search = IconButton::new(Icon::Search);
        search.place_at(Point::new(quick.x - t.target / 2, bar.center().y), t);
        top.trailing = bar.right() - search.rect().x + t.mm(1.0);
        let panel = screen.split_bottom(t.mm(if wide { 35.0 } else { 36.5 })).0;
        let info = Rect::new(m, panel.y + t.mm(if wide { 2.4 } else { 2.7 }), screen.w - 2 * m, t.mm(7.0));
        let buttons_y = screen.bottom() - t.mm(if wide { 3.7 } else { 4.5 }) - t.button_h;
        // The scrubber sits midway between the info line and the buttons.
        let scrub_y = (info.bottom() + buttons_y) / 2 - t.mm(3.0);
        let mut progress = Slider::new(0.0, pages.saturating_sub(1) as f32, 1.0, page as f32);
        progress.place(Rect::new(m + t.target, scrub_y, screen.w - 2 * m - 2 * t.target, t.mm(6.0)), t);
        let mut prev = IconButton::new(Icon::ChevronLeft);
        prev.place_at(Point::new(m + t.target / 2, progress.rect().center().y), t);
        let mut next = IconButton::new(Icon::ChevronRight);
        next.place_at(Point::new(screen.w - m - t.target / 2, progress.rect().center().y), t);
        let gap = t.mm(2.4);
        let width = (screen.w - 2 * m - 3 * gap) / 4;
        let mut buttons = ["Contents", "Highlights", "Text", "Layout"].into_iter().enumerate().map(|(i, label)| {
            let mut b = Button::new(label, ButtonKind::Secondary);
            b.place(Rect::new(m + i as i32 * (width + gap), buttons_y, width, t.button_h), t);
            b
        });
        let [contents, highlights, text, layout] = std::array::from_fn(|_| buttons.next().unwrap());
        Self {
            top,
            search,
            quick,
            quick_press: Press::default(),
            panel,
            info,
            progress,
            prev,
            next,
            contents,
            highlights,
            text,
            layout,
            outside: false,
        }
    }
    #[cfg(test)]
    pub fn quick_rect(&self) -> Rect {
        self.quick
    }
    #[cfg(test)]
    pub fn search_rect(&self) -> Rect {
        self.search.rect()
    }
    #[cfg(test)]
    pub fn panel(&self) -> Rect {
        self.panel
    }
    #[cfg(test)]
    pub fn button_rects(&self) -> [Rect; 4] {
        [self.contents.rect(), self.highlights.rect(), self.text.rect(), self.layout.rect()]
    }
    pub fn draw(&mut self, cx: &mut DrawCx, chapter: &str, pages: usize, pages_ready: bool) {
        let t = cx.theme;
        cx.fill(self.top.rect(), PAPER);
        self.top.draw(cx);
        self.search.draw(cx);
        // Battery + ⌄: like the Home battery slot, no pressed state (the
        // drawer appears at once).
        let chevron =
            Rect::new(self.quick.right() - t.mm(1.5) - t.icon_small, self.quick.y, t.icon_small, self.quick.h);
        let battery = Rect::new(
            self.quick.x + t.mm(1.5),
            self.top.rect().y,
            chevron.x - t.mm(0.8) - self.quick.x - t.mm(1.5),
            t.top_bar,
        );
        cx.battery_indicator(battery);
        cx.icon(Icon::ChevronDown, chevron, t.icon_small, INK2);
        cx.fill(self.panel, PAPER);
        cx.hline(self.panel.y, 0, self.panel.right(), t.border, INK);
        let progress = progress_label(self.progress.value as usize, pages);
        cx.line(chapter, t.body, Rect { w: self.info.w - t.mm(36.0), ..self.info }, HAlign::Left);
        cx.line(&progress, t.small.gray(INK2), self.info, HAlign::Right);
        if pages_ready {
            self.progress.draw(cx);
        } else {
            cx.line("Laying out…", t.small.gray(INK2), self.progress.bounds(), HAlign::Center);
        }
        self.prev.draw(cx);
        self.next.draw(cx);
        self.contents.draw(cx);
        self.highlights.draw(cx);
        self.text.draw(cx);
        self.layout.draw(cx);
    }
    pub fn on_gesture(&mut self, g: &Gesture, pages_ready: bool) -> Option<MenuEvent> {
        if let Gesture::Down(p) = g {
            self.outside = !self.panel.contains(*p) && !self.top.rect().contains(*p);
        }
        if self.outside {
            return matches!(g, Gesture::Tap(_)).then_some(MenuEvent::Close);
        }
        if self.top.on_gesture(g) {
            return Some(MenuEvent::Books);
        }
        if self.quick_press.on_gesture(g, self.quick) == Some(PressEvent::Tap) {
            return Some(MenuEvent::QuickSettings);
        }
        if self.search.on_gesture(g) {
            return Some(MenuEvent::Search);
        }
        if self.contents.on_gesture(g) {
            return Some(MenuEvent::Contents);
        }
        if self.highlights.on_gesture(g) {
            return Some(MenuEvent::Highlights);
        }
        if self.text.on_gesture(g) {
            return Some(MenuEvent::Text);
        }
        if self.layout.on_gesture(g) {
            return Some(MenuEvent::Layout);
        }
        if self.prev.on_gesture(g) {
            return Some(MenuEvent::Chapter(-1));
        }
        if self.next.on_gesture(g) {
            return Some(MenuEvent::Chapter(1));
        }
        if pages_ready && let Some(SliderEvent::Released(v)) = self.progress.on_gesture(g) {
            return Some(MenuEvent::Page(v as usize));
        }
        None
    }
}

/// `"{page} / {pages} · {percent}%"` for the zero-based `page`.
fn progress_label(page: usize, pages: usize) -> String {
    let pages = pages.max(1);
    let page = (page + 1).min(pages);
    format!("{page} / {pages} · {}%", (page * 100 + pages / 2) / pages)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tap(menu: &mut ReaderMenu, p: Point) -> Option<MenuEvent> {
        menu.on_gesture(&Gesture::Down(p), true);
        menu.on_gesture(&Gesture::Tap(p), true)
    }

    #[test]
    fn panel_is_slimmer_and_holds_four_equal_buttons_in_both_orientations() {
        let t = Theme::for_dpi(300);
        for (screen, mm) in [(Rect::new(0, 0, 1272, 1696), 36.5), (Rect::new(0, 0, 1696, 1272), 35.0)] {
            let menu = ReaderMenu::new(&t, screen, "Book", 10, 100);
            let panel = menu.panel();
            assert_eq!(panel.h, t.mm(mm), "{screen:?}");
            assert_eq!(panel.bottom(), screen.bottom());
            let buttons = menu.button_rects();
            for b in buttons {
                assert!(b.y > panel.y && b.bottom() <= panel.bottom(), "{screen:?}: {b:?}");
                assert_eq!((b.y, b.w, b.h), (buttons[0].y, buttons[0].w, t.button_h));
            }
            for pair in buttons.windows(2) {
                assert!(pair[0].right() < pair[1].x);
            }
            let slider = menu.progress.rect();
            assert!(slider.y > panel.y + t.mm(9.0) && slider.bottom() < buttons[0].y, "{screen:?}: {slider:?}");
            // Search and Battery + ⌄ sit in the top bar, apart, inside the screen.
            let (search, quick) = (menu.search_rect(), menu.quick_rect());
            assert!(search.right() <= quick.x && quick.right() <= screen.right());
            assert!(quick.y >= 0 && quick.bottom() <= t.top_bar);
        }
    }

    #[test]
    fn top_bar_and_buttons_emit_their_events() {
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut menu = ReaderMenu::new(&t, screen, "Book", 10, 100);
        let (quick, search) = (menu.quick_rect().center(), menu.search_rect().center());
        assert!(matches!(tap(&mut menu, quick), Some(MenuEvent::QuickSettings)));
        assert!(matches!(tap(&mut menu, search), Some(MenuEvent::Search)));
        let [contents, highlights, text, layout] = menu.button_rects();
        assert!(matches!(tap(&mut menu, contents.center()), Some(MenuEvent::Contents)));
        assert!(matches!(tap(&mut menu, highlights.center()), Some(MenuEvent::Highlights)));
        assert!(matches!(tap(&mut menu, text.center()), Some(MenuEvent::Text)));
        assert!(matches!(tap(&mut menu, layout.center()), Some(MenuEvent::Layout)));
        // The page between the bars closes the menu.
        assert!(matches!(tap(&mut menu, Point::new(636, 800)), Some(MenuEvent::Close)));
    }

    #[test]
    fn progress_label_shows_page_count_and_rounded_percent() {
        assert_eq!(progress_label(111, 438), "112 / 438 · 26%");
        assert_eq!(progress_label(0, 438), "1 / 438 · 0%");
        assert_eq!(progress_label(437, 438), "438 / 438 · 100%");
        assert_eq!(progress_label(0, 0), "1 / 1 · 100%");
    }
}
