//! A paged grid of cells (book covers). The screen draws each cell; the grid
//! does the layout, paging and touch handling.

use crate::geom::{Point, Rect};
use crate::ui::gesture::Gesture;
use crate::ui::layout::Paging;
use crate::ui::widget::{DrawCx, Press, PressEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridEvent {
    Tapped(usize),
    LongPressed(usize),
}

pub struct Grid {
    pub count: usize,
    pub page: usize,
    cols: usize,
    rows: usize,
    rect: Rect,
    gap_x: i32,
    gap_y: i32,
    pressed: Option<usize>,
    press: Press,
}

impl Grid {
    pub fn new(count: usize, cols: usize, rows: usize) -> Self {
        Self {
            count,
            page: 0,
            cols: cols.max(1),
            rows: rows.max(1),
            rect: Rect::default(),
            gap_x: 0,
            gap_y: 0,
            pressed: None,
            press: Press::default(),
        }
    }

    pub fn place(&mut self, rect: Rect, gap_x: i32, gap_y: i32) {
        self.rect = rect;
        self.gap_x = gap_x;
        self.gap_y = gap_y;
        self.page = self.paging().clamp(self.page);
    }

    pub fn paging(&self) -> Paging {
        Paging::new(self.count, self.cols * self.rows)
    }

    /// The cells on `page`: (item index, rect).
    pub fn cells_for_page(&self, page: usize) -> Vec<(usize, Rect)> {
        let slots = self.rect.grid(self.cols, self.rows, self.gap_x, self.gap_y);
        self.paging().range(page).zip(slots).collect()
    }

    /// The cells on the current page: (item index, rect).
    pub fn cells(&self) -> Vec<(usize, Rect)> {
        self.cells_for_page(self.page)
    }

    /// The uniform base cell size `(width, height)`, ignoring 1 px remainder
    /// pixels distributed to the first columns/rows so every tile on a page
    /// shares one exact cover size.
    pub fn base_cell_size(&self) -> (i32, i32) {
        let w = (self.rect.w - self.gap_x * (self.cols as i32 - 1)).max(0) / self.cols as i32;
        let h = (self.rect.h - self.gap_y * (self.rows as i32 - 1)).max(0) / self.rows as i32;
        (w, h)
    }

    fn cell_at(&self, p: Point) -> Option<usize> {
        self.cells().into_iter().find(|(_, r)| r.contains(p)).map(|(i, _)| i)
    }

    /// The item a finger is on, if any (draw it pressed).
    pub fn pressed(&self) -> Option<usize> {
        self.pressed.filter(|_| self.press.is_down())
    }

    pub fn on_gesture(&mut self, g: &Gesture) -> Option<GridEvent> {
        if let Gesture::Down(p) = *g {
            self.pressed = self.cell_at(p);
        }
        let i = self.pressed?;
        let area = self.cells().into_iter().find(|(j, _)| *j == i).map(|(_, r)| r).unwrap_or_default();
        let event = match self.press.on_gesture(g, area) {
            Some(PressEvent::Tap) => Some(GridEvent::Tapped(i)),
            Some(PressEvent::LongPress) => Some(GridEvent::LongPressed(i)),
            _ => None,
        };
        if !self.press.is_down() {
            self.pressed = None;
        }
        event
    }

    /// Calls `cell(cx, index, rect, pressed)` for each cell on the page.
    pub fn draw(&self, cx: &mut DrawCx, mut cell: impl FnMut(&mut DrawCx, usize, Rect, bool)) {
        let pressed = self.pressed();
        for (i, r) in self.cells() {
            cell(cx, i, r, pressed == Some(i));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_and_hits() {
        let mut g = Grid::new(11, 4, 2);
        g.place(Rect::new(0, 0, 400, 200), 0, 0);
        assert_eq!(g.paging().pages(), 2);
        g.page = 1;
        assert_eq!(g.cells().len(), 3);
        let p = Point::new(150, 50); // second cell → item 9
        g.on_gesture(&Gesture::Down(p));
        assert_eq!(g.pressed(), Some(9));
        assert_eq!(g.on_gesture(&Gesture::Tap(p)), Some(GridEvent::Tapped(9)));
        assert_eq!(g.pressed(), None);
        let empty = Point::new(350, 150);
        g.on_gesture(&Gesture::Down(empty));
        assert_eq!(g.on_gesture(&Gesture::Tap(empty)), None);
    }
}
