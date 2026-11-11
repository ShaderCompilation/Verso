//! Layout helpers: cutting rectangles into rows, columns and grids, and the
//! arithmetic for splitting long content into pages (there's no scrolling).

use std::ops::Range;

use crate::geom::Rect;

impl Rect {
    /// Splits off a strip of height `h` from the top: (strip, rest).
    pub fn split_top(&self, h: i32) -> (Rect, Rect) {
        let h = h.clamp(0, self.h);
        (Rect::new(self.x, self.y, self.w, h), Rect::new(self.x, self.y + h, self.w, self.h - h))
    }

    /// Splits off a strip of height `h` from the bottom: (strip, rest).
    pub fn split_bottom(&self, h: i32) -> (Rect, Rect) {
        let h = h.clamp(0, self.h);
        (Rect::new(self.x, self.bottom() - h, self.w, h), Rect::new(self.x, self.y, self.w, self.h - h))
    }

    /// Splits off a strip of width `w` from the left: (strip, rest).
    pub fn split_left(&self, w: i32) -> (Rect, Rect) {
        let w = w.clamp(0, self.w);
        (Rect::new(self.x, self.y, w, self.h), Rect::new(self.x + w, self.y, self.w - w, self.h))
    }

    /// Splits off a strip of width `w` from the right: (strip, rest).
    pub fn split_right(&self, w: i32) -> (Rect, Rect) {
        let w = w.clamp(0, self.w);
        (Rect::new(self.right() - w, self.y, w, self.h), Rect::new(self.x, self.y, self.w - w, self.h))
    }

    /// Shrinks by `dx` on the left and right and `dy` on the top and bottom.
    pub fn inset_xy(&self, dx: i32, dy: i32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w - 2 * dx, self.h - 2 * dy)
    }

    /// A `w` × `h` rectangle centred in this one.
    pub fn center_box(&self, w: i32, h: i32) -> Rect {
        Rect::centered(self.center(), w, h)
    }

    /// Grows around the centre to at least `w` × `h`. Used for touch targets.
    pub fn at_least(&self, w: i32, h: i32) -> Rect {
        let (nw, nh) = (self.w.max(w), self.h.max(h));
        Rect::new(self.x - (nw - self.w) / 2, self.y - (nh - self.h) / 2, nw, nh)
    }

    /// The smallest rectangle containing both.
    pub fn union(&self, other: &Rect) -> Rect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let (x, y) = (self.x.min(other.x), self.y.min(other.y));
        Rect::new(x, y, self.right().max(other.right()) - x, self.bottom().max(other.bottom()) - y)
    }

    pub fn moved(&self, dx: i32, dy: i32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }

    /// `n` equal columns with `gap` between them. Leftover pixels go to the
    /// first columns, so the columns exactly fill the width.
    pub fn columns(&self, n: usize, gap: i32) -> Vec<Rect> {
        spans(self.x, self.w, n, gap).into_iter().map(|(x, w)| Rect::new(x, self.y, w, self.h)).collect()
    }

    /// `n` equal rows with `gap` between them.
    pub fn rows(&self, n: usize, gap: i32) -> Vec<Rect> {
        spans(self.y, self.h, n, gap).into_iter().map(|(y, h)| Rect::new(self.x, y, self.w, h)).collect()
    }

    /// A `cols` × `rows` grid of cells, row by row.
    pub fn grid(&self, cols: usize, rows: usize, gap_x: i32, gap_y: i32) -> Vec<Rect> {
        let xs = spans(self.x, self.w, cols, gap_x);
        spans(self.y, self.h, rows, gap_y)
            .into_iter()
            .flat_map(|(y, h)| xs.iter().map(move |&(x, w)| Rect::new(x, y, w, h)))
            .collect()
    }
}

fn spans(start: i32, len: i32, n: usize, gap: i32) -> Vec<(i32, i32)> {
    if n == 0 {
        return Vec::new();
    }
    let n_i = n as i32;
    let free = (len - gap * (n_i - 1)).max(0);
    let (base, extra) = (free / n_i, free % n_i);
    let mut pos = start;
    (0..n_i)
        .map(|i| {
            let size = base + i32::from(i < extra);
            let span = (pos, size);
            pos += size + gap;
            span
        })
        .collect()
}

/// Splitting a list of `items` into pages of `per_page`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Paging {
    pub items: usize,
    pub per_page: usize,
}

impl Paging {
    pub fn new(items: usize, per_page: usize) -> Self {
        Self { items, per_page: per_page.max(1) }
    }

    /// Number of pages; an empty list still has one (empty) page.
    pub fn pages(&self) -> usize {
        self.items.div_ceil(self.per_page).max(1)
    }

    /// The items shown on `page` (0-based).
    pub fn range(&self, page: usize) -> Range<usize> {
        let start = (page * self.per_page).min(self.items);
        start..(start + self.per_page).min(self.items)
    }

    /// The page that shows item `index`.
    pub fn page_of(&self, index: usize) -> usize {
        (index / self.per_page).min(self.pages() - 1)
    }

    pub fn clamp(&self, page: usize) -> usize {
        page.min(self.pages() - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits() {
        let r = Rect::new(10, 20, 100, 200);
        assert_eq!(r.split_top(30), (Rect::new(10, 20, 100, 30), Rect::new(10, 50, 100, 170)));
        assert_eq!(r.split_bottom(30), (Rect::new(10, 190, 100, 30), Rect::new(10, 20, 100, 170)));
        assert_eq!(r.split_left(40), (Rect::new(10, 20, 40, 200), Rect::new(50, 20, 60, 200)));
        assert_eq!(r.split_right(40), (Rect::new(70, 20, 40, 200), Rect::new(10, 20, 60, 200)));
        // Asking for more than there is takes everything.
        assert_eq!(r.split_top(500).0, r);
        assert!(r.split_top(500).1.is_empty());
    }

    #[test]
    fn columns_fill_the_width_exactly() {
        let cols = Rect::new(0, 0, 103, 10).columns(4, 5);
        assert_eq!(cols.len(), 4);
        assert_eq!(cols[0].x, 0);
        assert_eq!(cols[3].right(), 103);
        let widths: Vec<i32> = cols.iter().map(|c| c.w).collect();
        assert_eq!(widths, vec![22, 22, 22, 22]);
        for pair in cols.windows(2) {
            assert_eq!(pair[1].x - pair[0].right(), 5);
        }
        let uneven = Rect::new(0, 0, 10, 1).columns(3, 0);
        assert_eq!(uneven.iter().map(|c| c.w).collect::<Vec<_>>(), vec![4, 3, 3]);
    }

    #[test]
    fn grid_is_row_major() {
        let cells = Rect::new(0, 0, 100, 50).grid(4, 2, 0, 0);
        assert_eq!(cells.len(), 8);
        assert_eq!(cells[1], Rect::new(25, 0, 25, 25));
        assert_eq!(cells[4], Rect::new(0, 25, 25, 25));
    }

    #[test]
    fn centring_and_touch_targets() {
        let r = Rect::new(0, 0, 100, 60);
        assert_eq!(r.center_box(20, 10), Rect::new(40, 25, 20, 10));
        let small = Rect::new(50, 50, 20, 10);
        assert_eq!(small.at_least(40, 40), Rect::new(40, 35, 40, 40));
        assert_eq!(r.at_least(10, 10), r, "never shrinks");
    }

    #[test]
    fn union() {
        let a = Rect::new(0, 0, 10, 10);
        assert_eq!(a.union(&Rect::new(20, 5, 5, 20)), Rect::new(0, 0, 25, 25));
        assert_eq!(a.union(&Rect::default()), a);
    }

    #[test]
    fn paging() {
        let p = Paging::new(17, 8);
        assert_eq!(p.pages(), 3);
        assert_eq!(p.range(0), 0..8);
        assert_eq!(p.range(2), 16..17);
        assert_eq!(p.range(5), 17..17);
        assert_eq!(p.page_of(8), 1);
        assert_eq!(p.clamp(9), 2);
        assert_eq!(Paging::new(0, 8).pages(), 1);
        assert_eq!(Paging::new(16, 8).pages(), 2);
        assert_eq!(Paging::new(3, 0).pages(), 3, "per_page is at least 1");
    }
}
