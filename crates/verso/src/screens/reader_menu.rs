//! The centre-tap menu. Controls use the shared toolkit.
use crate::geom::{Point, Rect};
use crate::gfx::{HAlign, Icon};
use crate::ui::gesture::{Direction, Gesture};
use crate::ui::theme::{INK, INK2, LINE, PAPER, Theme};
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
    /// The `☀` button, battery indicator, or pulling down the bar: open the Top Drawer.
    QuickSettings,
    Page(usize),
    Chapter(isize),
}
pub struct ReaderMenu {
    top_panel: Rect,
    handle: Rect,
    top: TopBar,
    search: IconButton,
    light: IconButton,
    /// The battery indicator in the top bar (also opens the Top Drawer).
    battery: Rect,
    battery_press: Press,
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
        let top_panel = screen.split_top(t.mm(14.8)).0;
        let handle_w = t.mm(12.2);
        let handle_h = t.mm(0.8).max(2);
        let handle = Rect::new(
            screen.x + (screen.w - handle_w) / 2,
            top_panel.bottom() - t.mm(1.7) - handle_h,
            handle_w,
            handle_h,
        );
        let mut top = TopBar::new(title, true);
        top.serif = false;
        top.border = false;
        top.place(screen, t);
        let bar = top.rect();
        let battery_w = t.mm(13.5);
        let battery = Rect::new(screen.right() - m - battery_w, bar.center().y - t.target / 2, battery_w, t.target);
        let mut light = IconButton::new(Icon::Sun);
        light.place_at(Point::new(battery.x - t.target / 2, bar.center().y), t);
        let mut search = IconButton::new(Icon::Search);
        search.place_at(Point::new(light.rect().x - t.target / 2, bar.center().y), t);
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
            top_panel,
            handle,
            top,
            search,
            light,
            battery,
            battery_press: Press::default(),
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
    pub fn top_panel(&self) -> Rect {
        self.top_panel
    }
    #[cfg(test)]
    pub fn handle_rect(&self) -> Rect {
        self.handle
    }
    #[cfg(test)]
    pub fn light_rect(&self) -> Rect {
        self.light.rect()
    }
    #[cfg(test)]
    pub fn battery_rect(&self) -> Rect {
        self.battery
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
    /// `percent` is the book percentage of the slider's page, once that page
    /// is on screen (while scrubbing elsewhere, only the page number shows).
    pub fn draw(&mut self, cx: &mut DrawCx, chapter: &str, pages: usize, percent: Option<u32>, pages_ready: bool) {
        let t = cx.theme;
        // Same pull-down visual language as TopDrawer: rounded bottom corners,
        // side borders just off-screen, and a centered handle pill.
        let r = self.top_panel;
        let extra = t.radius_sheet as i32 * 2;
        let shape = Rect::new(r.x - t.border, r.y - extra, r.w + 2 * t.border, r.h + extra);
        cx.canvas.fill_rounded_rect(shape, t.radius_sheet, PAPER);
        cx.canvas.stroke_rounded_rect(shape, t.radius_sheet, t.border as f32, INK);
        self.top.draw(cx);
        self.search.draw(cx);
        self.light.draw(cx);
        if let Some(battery) = cx.battery {
            let label = format!("{}%", battery.percent.min(100));
            let w = t.mm(5.4) + t.mm(0.8) + cx.text.width(&label, t.small).ceil() as i32 + 2;
            let bar = self.top.rect();
            let rect = Rect::new(self.battery.right() - w, bar.y, w, bar.h);
            cx.battery_indicator(rect);
        }
        let h = self.handle;
        cx.canvas.fill_rounded_rect(h, h.h as f32 / 2.0, LINE);
        cx.fill(self.panel, PAPER);
        cx.hline(self.panel.y, 0, self.panel.right(), t.border, INK);
        let progress = progress_label(self.progress.value as usize, pages, percent);
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
            self.outside = !self.panel.contains(*p) && !self.top_panel.contains(*p);
        }
        if self.outside {
            return matches!(g, Gesture::Tap(_)).then_some(MenuEvent::Close);
        }
        if let Gesture::Swipe { dir: Direction::Down, from, .. } = g
            && self.top_panel.contains(*from)
        {
            return Some(MenuEvent::QuickSettings);
        }
        if self.top.on_gesture(g) {
            return Some(MenuEvent::Books);
        }
        if self.light.on_gesture(g) {
            return Some(MenuEvent::QuickSettings);
        }
        if self.battery_press.on_gesture(g, self.battery) == Some(PressEvent::Tap) {
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

/// `"{page} / {pages} · {percent}%"` for the zero-based `page`, or just
/// `"{page} / {pages}"` while its percentage isn't known.
fn progress_label(page: usize, pages: usize, percent: Option<u32>) -> String {
    let pages = pages.max(1);
    let page = (page + 1).min(pages);
    match percent {
        Some(percent) => format!("{page} / {pages} · {percent}%"),
        None => format!("{page} / {pages}"),
    }
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
            // Search, Sun, and Battery sit in the top bar, apart, inside the screen.
            let (search, light, battery) = (menu.search_rect(), menu.light_rect(), menu.battery_rect());
            assert!(search.right() <= light.x && light.right() <= battery.x && battery.right() <= screen.right());
            assert!(battery.y >= 0 && battery.bottom() <= t.top_bar);
            let top_panel = menu.top_panel();
            let handle = menu.handle_rect();
            assert!(top_panel.h > t.top_bar);
            assert!(handle.y > battery.bottom() && handle.bottom() < top_panel.bottom());
            assert_eq!(handle.center().x, screen.center().x);
        }
    }

    #[test]
    fn top_bar_and_buttons_emit_their_events() {
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut menu = ReaderMenu::new(&t, screen, "Book", 10, 100);
        let (light, battery, search) =
            (menu.light_rect().center(), menu.battery_rect().center(), menu.search_rect().center());
        assert!(matches!(tap(&mut menu, light), Some(MenuEvent::QuickSettings)));
        assert!(matches!(tap(&mut menu, battery), Some(MenuEvent::QuickSettings)));
        assert!(matches!(tap(&mut menu, search), Some(MenuEvent::Search)));
        // Swiping down on the pull-handle area opens the Top Drawer.
        let handle = menu.handle_rect().center();
        menu.on_gesture(&Gesture::Down(handle), true);
        let swipe = Gesture::Swipe { dir: Direction::Down, from: handle, to: Point::new(handle.x, 500) };
        assert!(matches!(menu.on_gesture(&swipe, true), Some(MenuEvent::QuickSettings)));
        // Tapping the handle area doesn't count as outside.
        assert!(tap(&mut menu, handle).is_none());
        let [contents, highlights, text, layout] = menu.button_rects();
        assert!(matches!(tap(&mut menu, contents.center()), Some(MenuEvent::Contents)));
        assert!(matches!(tap(&mut menu, highlights.center()), Some(MenuEvent::Highlights)));
        assert!(matches!(tap(&mut menu, text.center()), Some(MenuEvent::Text)));
        assert!(matches!(tap(&mut menu, layout.center()), Some(MenuEvent::Layout)));
        // The page between the bars closes the menu.
        assert!(matches!(tap(&mut menu, Point::new(636, 800)), Some(MenuEvent::Close)));
    }

    #[test]
    fn progress_label_shows_page_count_and_the_book_percent_once_known() {
        assert_eq!(progress_label(111, 438, Some(26)), "112 / 438 · 26%");
        assert_eq!(progress_label(0, 438, Some(0)), "1 / 438 · 0%");
        assert_eq!(progress_label(437, 438, Some(100)), "438 / 438 · 100%");
        assert_eq!(progress_label(0, 0, Some(100)), "1 / 1 · 100%");
        assert_eq!(progress_label(111, 438, None), "112 / 438");
    }
}
