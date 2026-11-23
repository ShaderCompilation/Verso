//! Basic geometry in screen pixels. The origin is the top-left corner.

/// A point on the screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    /// Straight-line distance to another point.
    pub fn distance(self, other: Point) -> f32 {
        let dx = (other.x - self.x) as f32;
        let dy = (other.y - self.y) as f32;
        (dx * dx + dy * dy).sqrt()
    }
}

/// An axis-aligned rectangle. `w` and `h` are never negative.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w: if w > 0 { w } else { 0 }, h: if h > 0 { h } else { 0 } }
    }

    /// A rectangle of the given size, centred on `center`.
    pub fn centered(center: Point, w: i32, h: i32) -> Self {
        Self::new(center.x - w / 2, center.y - h / 2, w, h)
    }

    pub const fn right(&self) -> i32 {
        self.x + self.w
    }

    pub const fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub const fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    pub const fn center(&self) -> Point {
        Point::new(self.x + self.w / 2, self.y + self.h / 2)
    }

    pub const fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.right() && p.y >= self.y && p.y < self.bottom()
    }

    /// True when `other` lies wholly inside this rectangle.
    pub const fn contains_rect(&self, other: &Rect) -> bool {
        other.x >= self.x && other.y >= self.y && other.right() <= self.right() && other.bottom() <= self.bottom()
    }

    /// The overlapping part of two rectangles, if any.
    pub fn intersect(&self, other: &Rect) -> Option<Rect> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let r = Rect::new(x, y, self.right().min(other.right()) - x, self.bottom().min(other.bottom()) - y);
        (!r.is_empty()).then_some(r)
    }

    /// Shrinks the rectangle by `d` on every side (grows it if `d` is negative).
    pub fn inset(&self, d: i32) -> Rect {
        Rect::new(self.x + d, self.y + d, self.w - 2 * d, self.h - 2 * d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_is_half_open() {
        let r = Rect::new(10, 10, 5, 5);
        assert!(r.contains(Point::new(10, 10)));
        assert!(r.contains(Point::new(14, 14)));
        assert!(!r.contains(Point::new(15, 10)));
        assert!(!r.contains(Point::new(9, 12)));
    }

    #[test]
    fn intersect() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(5, 5, 10, 10);
        assert_eq!(a.intersect(&b), Some(Rect::new(5, 5, 5, 5)));
        assert_eq!(a.intersect(&Rect::new(10, 0, 5, 5)), None);
    }

    #[test]
    fn negative_sizes_clamp_to_empty() {
        assert!(Rect::new(0, 0, -3, 4).is_empty());
        assert!(Rect::new(0, 0, 4, 4).inset(3).is_empty());
    }
}
