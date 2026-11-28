//! ‹ "1 of 6" › — moves between pages of content. Swiping left or right on
//! the content does the same; screens pass swipes to `on_swipe`. The arrows
//! wrap around (‹ on the first page goes to the last) once the page count is
//! final; swipes stop at the ends. Screens that set `jump` also get a `Jump`
//! event when the "1 of 6" counter is tapped, and the counter is outlined as a
//! pill so it reads as a button.

use crate::geom::{Point, Rect};
use crate::gfx::{HAlign, Icon, TextStyle};
use crate::platform::RefreshMode;
use crate::ui::gesture::{Direction, Gesture};
use crate::ui::theme::{INK, INK_FILL, INK2, PAPER, Theme};
use crate::ui::widget::{DrawCx, Press, PressEvent};
use crate::ui::widgets::button::IconButton;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PagerEvent {
    /// An arrow moved to this page.
    Changed(usize),
    /// The counter was tapped: the screen may offer a page picker.
    Jump,
}

pub struct Pager {
    pub page: usize,
    pub pages: usize,
    /// More pages are being laid out; the available pages remain navigable.
    pub incomplete: bool,
    /// The counter is tappable and reports `Jump`.
    pub jump: bool,
    /// Compact top-row styling (`"1 / 3"` label, compact chevrons and hit targets).
    pub compact: bool,
    rect: Rect,
    counter: Rect,
    counter_press: Press,
    prev: IconButton,
    next: IconButton,
}

impl Pager {
    pub fn new(pages: usize) -> Self {
        Self {
            page: 0,
            pages: pages.max(1),
            incomplete: false,
            jump: false,
            compact: false,
            rect: Rect::default(),
            counter: Rect::default(),
            counter_press: Press::default(),
            prev: IconButton::new(Icon::ChevronLeft),
            next: IconButton::new(Icon::ChevronRight),
        }
    }

    /// Standard width of a compact top-row pager (`~25.4 mm` / `300 px` at 300 dpi).
    pub fn compact_width(theme: &Theme) -> i32 {
        theme.mm(25.4)
    }

    /// True when the pager is drawn (more than one page, or still paginating).
    pub fn visible(&self) -> bool {
        self.pages > 1 || self.incomplete
    }

    pub fn place(&mut self, rect: Rect, theme: &Theme) {
        self.rect = rect;
        let cy = rect.center().y;
        if self.compact {
            let icon = theme.mm(3.7);
            self.prev.size = Some(icon);
            self.next.size = Some(icon);
            let btn_w = theme.mm(7.5).min(rect.w / 3).max(1);
            let hit_h = theme.target.max(rect.h);
            self.prev.place(Rect::new(rect.x, cy - hit_h / 2, btn_w, hit_h));
            self.next.place(Rect::new(rect.right() - btn_w, cy - hit_h / 2, btn_w, hit_h));
            let cx_x = self.prev.rect().right();
            let cx_w = (self.next.rect().x - cx_x).max(0);
            self.counter = Rect::new(cx_x, cy - hit_h / 2, cx_w, hit_h);
        } else {
            self.prev.size = None;
            self.next.size = None;
            self.prev.place_at(Point::new(rect.x + theme.target / 2, cy), theme);
            self.next.place_at(Point::new(rect.right() - theme.target / 2, cy), theme);
            let w = theme.mm(32.0).min(rect.w - 2 * theme.target).max(0);
            let h = theme.target.min(rect.h.max(theme.target));
            self.counter = Rect::new(rect.center().x - w / 2, cy - h / 2, w, h);
        }
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Snapshot of visible state that gestures can change without emitting an event.
    pub fn visual_state(&self) -> (usize, usize, bool, bool, bool) {
        (
            self.page,
            self.pages,
            self.prev.is_pressed(),
            self.next.is_pressed(),
            self.jump && self.counter_press.is_down(),
        )
    }

    pub fn set(&mut self, page: usize, pages: usize) {
        self.pages = pages.max(1);
        self.page = page.min(self.pages - 1);
    }

    /// Reports a page change from the arrows, or a tap on the counter.
    pub fn on_gesture(&mut self, g: &Gesture) -> Option<PagerEvent> {
        self.sync();
        let wrap = !self.incomplete;
        if self.prev.on_gesture(g) {
            return self.go(-1, wrap).map(PagerEvent::Changed);
        }
        if self.next.on_gesture(g) {
            return self.go(1, wrap).map(PagerEvent::Changed);
        }
        if !(self.jump && self.pages > 1) {
            // Not tappable now: don't keep a press from when it was.
            self.counter_press = Press::default();
            return None;
        }
        if self.counter_press.on_gesture(g, self.counter) == Some(PressEvent::Tap) {
            return Some(PagerEvent::Jump);
        }
        None
    }

    /// Swipe left for the next page, right for the previous one.
    pub fn on_swipe(&mut self, dir: Direction) -> Option<usize> {
        match dir {
            Direction::Left => self.go(1, false),
            Direction::Right => self.go(-1, false),
            _ => None,
        }
    }

    /// Moves one page; `wrap` steps past either end to the other one.
    fn go(&mut self, delta: isize, wrap: bool) -> Option<usize> {
        let new = match self.page.checked_add_signed(delta).filter(|&p| p < self.pages) {
            Some(p) => p,
            None if wrap && self.pages > 1 => (self.page as isize + delta).rem_euclid(self.pages as isize) as usize,
            None => return None,
        };
        self.page = new;
        self.sync();
        Some(new)
    }

    fn sync(&mut self) {
        // While pages are still being counted, "the last page" isn't known yet.
        let wrap = !self.incomplete && self.pages > 1;
        self.prev.enabled = wrap || self.page > 0;
        self.next.enabled = wrap || self.page + 1 < self.pages;
    }

    /// The outline around a tappable counter, sized for the widest label so it
    /// doesn't change size (and grow the refreshed area) as pages turn.
    fn pill(&self, cx: &mut DrawCx, style: TextStyle, sep: &str) -> Rect {
        let t = cx.theme;
        let widest = format!("{} {sep} {}", self.pages, self.pages);
        // The arrows' hit areas have room around their chevrons: the pill may use it.
        let room = if self.compact {
            let chevron = self.prev.size.unwrap_or(t.icon);
            self.next.rect().center().x - self.prev.rect().center().x - chevron - t.mm(1.5)
        } else {
            self.counter.w
        };
        let w = (cx.text.width(&widest, style).ceil() as i32 + 2 * t.mm(2.2)).min(room);
        let h = t.mm(6.5).min(self.rect.h).min(self.counter.h);
        let c = self.counter.center();
        Rect::new(c.x - w / 2, c.y - h / 2, w, h)
    }

    pub fn draw(&mut self, cx: &mut DrawCx) {
        self.sync();
        if !self.visible() {
            return;
        }
        self.prev.draw(cx);
        self.next.draw(cx);
        let label = match (self.compact, self.incomplete) {
            (true, true) => format!("{} / …", self.page + 1),
            (true, false) => format!("{} / {}", self.page + 1, self.pages),
            (false, true) => format!("{} of …", self.page + 1),
            (false, false) => format!("{} of {}", self.page + 1, self.pages),
        };
        let t = cx.theme;
        let (style, text_rect) = if self.compact {
            (t.body.gray(INK), Rect::new(self.counter.x, self.rect.y, self.counter.w, self.rect.h))
        } else {
            (t.small.gray(INK2), self.rect)
        };
        if !(self.jump && self.pages > 1) {
            cx.line(&label, style, text_rect, HAlign::Center);
            return;
        }
        let pill = self.pill(cx, style, if self.compact { "/" } else { "of" });
        let radius = pill.h as f32 / 2.0;
        if self.counter_press.is_down() {
            cx.canvas.fill_rounded_rect(pill, radius, INK_FILL);
            cx.line(&label, style.gray(PAPER), text_rect, HAlign::Center);
            cx.hint(pill, RefreshMode::Fast);
        } else {
            cx.canvas.stroke_rounded_rect(pill, radius, t.hairline as f32, INK2);
            cx.line(&label, style, text_rect, HAlign::Center);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_pager_navigates_jumps_and_hides_on_single_page() {
        use crate::gfx::{Canvas, IconRenderer, TextRenderer};
        let theme = Theme::for_dpi(300);
        let mut p = Pager::new(3);
        p.compact = true;
        p.jump = true;
        let w = Pager::compact_width(&theme);
        let rect = Rect::new(912, 224, w, 48);
        p.place(rect, &theme);
        assert!(p.visible());
        assert!(p.prev.rect().right() <= p.counter.x && p.counter.right() <= p.next.rect().x);
        // Next arrow
        let next = p.next.rect().center();
        p.on_gesture(&Gesture::Down(next));
        assert_eq!(p.on_gesture(&Gesture::Tap(next)), Some(PagerEvent::Changed(1)));
        // Prev arrow
        let prev = p.prev.rect().center();
        p.on_gesture(&Gesture::Down(prev));
        assert_eq!(p.on_gesture(&Gesture::Tap(prev)), Some(PagerEvent::Changed(0)));
        // Counter tap opens PageJump
        let mid = p.counter.center();
        p.on_gesture(&Gesture::Down(mid));
        assert_eq!(p.on_gesture(&Gesture::Tap(mid)), Some(PagerEvent::Jump));
        // Single-page list hides and draws nothing
        p.set(0, 1);
        assert!(!p.visible());
        let mut canvas = Canvas::new(1272, 400);
        canvas.fill(PAPER);
        let (mut text, mut icons) = (TextRenderer::new(), IconRenderer::default());
        p.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &theme, &mut vec![]));
        assert!(canvas.gray_data().iter().all(|&px| px == PAPER));
        p.on_gesture(&Gesture::Down(mid));
        assert_eq!(p.on_gesture(&Gesture::Tap(mid)), None);
    }

    #[test]
    fn arrows_wrap_around_and_swipes_stop_at_the_ends() {
        let theme = Theme::for_dpi(300);
        let mut p = Pager::new(3);
        p.place(Rect::new(0, 1500, 1272, 120), &theme);
        let next = Point::new(1272 - 20, 1560);
        p.on_gesture(&Gesture::Down(next));
        assert_eq!(p.on_gesture(&Gesture::Tap(next)), Some(PagerEvent::Changed(1)));
        assert_eq!(p.on_swipe(Direction::Left), Some(2));
        assert_eq!(p.on_swipe(Direction::Left), None, "already on the last page");
        assert_eq!(p.on_swipe(Direction::Up), None);
        assert_eq!(p.on_swipe(Direction::Right), Some(1));
        let prev = Point::new(20, 1560);
        p.on_gesture(&Gesture::Down(prev));
        assert_eq!(p.on_gesture(&Gesture::Tap(prev)), Some(PagerEvent::Changed(0)));
        assert_eq!(p.on_swipe(Direction::Right), None, "swipes stop on the first page");
        p.on_gesture(&Gesture::Down(prev));
        assert_eq!(p.on_gesture(&Gesture::Tap(prev)), Some(PagerEvent::Changed(2)), "wraps to the last");
        p.on_gesture(&Gesture::Down(next));
        assert_eq!(p.on_gesture(&Gesture::Tap(next)), Some(PagerEvent::Changed(0)), "wraps to the first");
        p.incomplete = true;
        p.on_gesture(&Gesture::Down(prev));
        assert_eq!(p.on_gesture(&Gesture::Tap(prev)), None, "no last page to wrap to yet");
        p.incomplete = false;
        p.set(0, 1);
        p.on_gesture(&Gesture::Down(next));
        assert_eq!(p.on_gesture(&Gesture::Tap(next)), None, "one page doesn't wrap onto itself");
    }

    #[test]
    fn counter_tap_asks_for_a_jump_only_when_enabled_with_pages() {
        let theme = Theme::for_dpi(300);
        let mut p = Pager::new(3);
        p.place(Rect::new(0, 1500, 1272, 120), &theme);
        let counter = Point::new(636, 1560);
        p.on_gesture(&Gesture::Down(counter));
        assert_eq!(p.on_gesture(&Gesture::Tap(counter)), None, "screens opt in");
        p.jump = true;
        p.on_gesture(&Gesture::Down(counter));
        assert_eq!(p.on_gesture(&Gesture::Tap(counter)), Some(PagerEvent::Jump));
        p.on_gesture(&Gesture::Down(counter));
        p.on_gesture(&Gesture::Move(Point::new(636, 1400)));
        assert_eq!(p.on_gesture(&Gesture::Tap(counter)), None, "a drag away is not a tap");
        p.set(0, 1);
        p.on_gesture(&Gesture::Down(counter));
        assert_eq!(p.on_gesture(&Gesture::Tap(counter)), None, "nothing to jump to");
    }

    #[test]
    fn counter_press_does_not_outlive_being_disabled() {
        let theme = Theme::for_dpi(300);
        let mut p = Pager::new(3);
        p.place(Rect::new(0, 1500, 1272, 120), &theme);
        p.jump = true;
        let counter = Point::new(636, 1560);
        p.on_gesture(&Gesture::Down(counter));
        p.set(0, 1);
        p.on_gesture(&Gesture::Down(Point::new(636, 100)));
        p.set(0, 3);
        assert_eq!(p.on_gesture(&Gesture::Tap(Point::new(636, 100))), None, "no ghost jump");
    }
}
