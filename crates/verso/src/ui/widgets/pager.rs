//! ‹ "1 of 6" › — moves between pages of content. Swiping left or right on
//! the content does the same; screens pass swipes to `on_swipe`.

use crate::geom::{Point, Rect};
use crate::gfx::{HAlign, Icon};
use crate::ui::gesture::{Direction, Gesture};
use crate::ui::theme::{INK2, Theme};
use crate::ui::widget::DrawCx;
use crate::ui::widgets::button::IconButton;

pub struct Pager {
    pub page: usize,
    pub pages: usize,
    rect: Rect,
    prev: IconButton,
    next: IconButton,
}

impl Pager {
    pub fn new(pages: usize) -> Self {
        Self {
            page: 0,
            pages: pages.max(1),
            rect: Rect::default(),
            prev: IconButton::new(Icon::ChevronLeft),
            next: IconButton::new(Icon::ChevronRight),
        }
    }

    pub fn place(&mut self, rect: Rect, theme: &Theme) {
        self.rect = rect;
        let cy = rect.center().y;
        self.prev.place_at(Point::new(rect.x + theme.target / 2, cy), theme);
        self.next.place_at(Point::new(rect.right() - theme.target / 2, cy), theme);
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    pub fn set(&mut self, page: usize, pages: usize) {
        self.pages = pages.max(1);
        self.page = page.min(self.pages - 1);
    }

    /// Returns the new page when it changed.
    pub fn on_gesture(&mut self, g: &Gesture) -> Option<usize> {
        self.sync();
        if self.prev.on_gesture(g) {
            return self.go(-1);
        }
        if self.next.on_gesture(g) {
            return self.go(1);
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
        if self.pages <= 1 {
            return;
        }
        self.prev.draw(cx);
        self.next.draw(cx);
        let label = format!("{} of {}", self.page + 1, self.pages);
        cx.line(&label, cx.theme.small.gray(INK2), self.rect, HAlign::Center);
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
        assert_eq!(p.on_gesture(&Gesture::Tap(next)), Some(1));
        assert_eq!(p.on_swipe(Direction::Left), Some(2));
        assert_eq!(p.on_swipe(Direction::Left), None, "already on the last page");
        assert_eq!(p.on_swipe(Direction::Up), None);
        assert_eq!(p.on_swipe(Direction::Right), Some(1));
        let prev = Point::new(20, 1560);
        p.on_gesture(&Gesture::Down(prev));
        assert_eq!(p.on_gesture(&Gesture::Tap(prev)), Some(0));
        p.on_gesture(&Gesture::Down(prev));
        assert_eq!(p.on_gesture(&Gesture::Tap(prev)), None, "disabled on the first page");
    }
}
