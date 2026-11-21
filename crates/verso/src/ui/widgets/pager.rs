//! ‹ "1 of 6" › — moves between pages of content. Swiping left or right on
//! the content does the same; screens pass swipes to `on_swipe`. Screens that
//! set `jump` also get a `Jump` event when the "1 of 6" counter is tapped.

use crate::geom::{Point, Rect};
use crate::gfx::{HAlign, Icon};
use crate::platform::RefreshMode;
use crate::ui::gesture::{Direction, Gesture};
use crate::ui::theme::{INK, INK2, PAPER, Theme};
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
            rect: Rect::default(),
            counter: Rect::default(),
            counter_press: Press::default(),
            prev: IconButton::new(Icon::ChevronLeft),
            next: IconButton::new(Icon::ChevronRight),
        }
    }

    pub fn place(&mut self, rect: Rect, theme: &Theme) {
        self.rect = rect;
        let cy = rect.center().y;
        self.prev.place_at(Point::new(rect.x + theme.target / 2, cy), theme);
        self.next.place_at(Point::new(rect.right() - theme.target / 2, cy), theme);
        let w = theme.mm(32.0).min(rect.w - 2 * theme.target).max(0);
        let h = theme.target.min(rect.h.max(theme.target));
        self.counter = Rect::new(rect.center().x - w / 2, cy - h / 2, w, h);
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    pub fn set(&mut self, page: usize, pages: usize) {
        self.pages = pages.max(1);
        self.page = page.min(self.pages - 1);
    }

    /// Reports a page change from the arrows, or a tap on the counter.
    pub fn on_gesture(&mut self, g: &Gesture) -> Option<PagerEvent> {
        self.sync();
        if self.prev.on_gesture(g) {
            return self.go(-1).map(PagerEvent::Changed);
        }
        if self.next.on_gesture(g) {
            return self.go(1).map(PagerEvent::Changed);
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
            Direction::Left => self.go(1),
            Direction::Right => self.go(-1),
            _ => None,
        }
    }

    fn go(&mut self, delta: isize) -> Option<usize> {
        let new = self.page.checked_add_signed(delta).filter(|&p| p < self.pages)?;
        self.page = new;
        self.sync();
        Some(new)
    }

    fn sync(&mut self) {
        self.prev.enabled = self.page > 0;
        self.next.enabled = self.page + 1 < self.pages;
    }

    pub fn draw(&mut self, cx: &mut DrawCx) {
        self.sync();
        if self.pages <= 1 && !self.incomplete {
            return;
        }
        self.prev.draw(cx);
        self.next.draw(cx);
        let label = if self.incomplete {
            format!("{} of …", self.page + 1)
        } else {
            format!("{} of {}", self.page + 1, self.pages)
        };
        let t = cx.theme;
        if self.jump && self.counter_press.is_down() {
            cx.canvas.fill_rounded_rect(self.counter, t.radius, INK);
            cx.line(&label, t.small.gray(PAPER), self.rect, HAlign::Center);
            cx.hint(self.counter, RefreshMode::Fast);
        } else {
            cx.line(&label, t.small.gray(INK2), self.rect, HAlign::Center);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrows_and_swipes_stay_in_range() {
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
        p.on_gesture(&Gesture::Down(prev));
        assert_eq!(p.on_gesture(&Gesture::Tap(prev)), None, "disabled on the first page");
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
