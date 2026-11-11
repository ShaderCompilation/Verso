//! A row of choices in one outline; the selected one is filled black.

use crate::geom::Rect;
use crate::gfx::{HAlign, Icon};
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK, PAPER};
use crate::ui::widget::DrawCx;

pub enum Segment {
    Text(String),
    Icon(Icon),
    /// Draws its own picture in the segment, in the given gray.
    Glyph(fn(&mut DrawCx, Rect, u8)),
}

pub struct Segmented {
    pub items: Vec<Segment>,
    pub selected: usize,
    rect: Rect,
    /// The segment the current touch started on.
    down_on: Option<usize>,
}

impl Segmented {
    pub fn new(items: Vec<Segment>, selected: usize) -> Self {
        Self { items, selected, rect: Rect::default(), down_on: None }
    }

    pub fn texts(labels: &[&str], selected: usize) -> Self {
        Self::new(labels.iter().map(|s| Segment::Text((*s).to_owned())).collect(), selected)
    }

    pub fn place(&mut self, rect: Rect) {
        self.rect = rect;
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    fn segments(&self) -> Vec<Rect> {
        self.rect.columns(self.items.len(), 0)
    }

    fn segment_at(&self, p: crate::geom::Point) -> Option<usize> {
        self.segments().iter().position(|r| r.contains(p))
    }

    /// Returns the newly selected index when a different segment is tapped.
    pub fn on_gesture(&mut self, g: &Gesture) -> Option<usize> {
        match *g {
            Gesture::Down(p) => self.down_on = self.segment_at(p),
            Gesture::Tap(p) | Gesture::End(p) => {
                let (start, end) = (self.down_on.take(), self.segment_at(p));
                if let Some(i) = start.filter(|&i| Some(i) == end && i != self.selected) {
                    self.selected = i;
                    return Some(i);
                }
            }
            Gesture::Move(_) | Gesture::Swipe { .. } | Gesture::Cancel(_) => self.down_on = None,
            Gesture::LongPress(_) => {}
        }
        None
    }

    pub fn draw(&self, cx: &mut DrawCx) {
        let t = cx.theme;
        let segs = self.segments();
        let n = segs.len();
        for (i, (seg, item)) in segs.iter().zip(&self.items).enumerate() {
            let selected = i == self.selected;
            let fg = if selected {
                // Rounded on the outside only: fill rounded, then square off inner sides.
                cx.canvas.fill_rounded_rect(*seg, t.radius, INK);
                if i > 0 {
                    cx.fill(seg.split_left(seg.w / 2).0, INK);
                }
                if i + 1 < n {
                    cx.fill(seg.split_right(seg.w / 2).0, INK);
                }
                PAPER
            } else {
                // A divider, unless the neighbour on the left is selected.
                if i > 0 && i != self.selected + 1 {
                    cx.fill(Rect::new(seg.x - t.hairline / 2, seg.y + t.mm(2.0), t.hairline, seg.h - t.mm(4.0)), INK);
                }
                INK
            };
            match item {
                Segment::Text(s) => cx.line(s, t.button.gray(fg), seg.inset_xy(t.mm(1.0), 0), HAlign::Center),
                Segment::Icon(icon) => cx.icon(*icon, *seg, t.icon_small, fg),
                Segment::Glyph(draw) => draw(cx, *seg, fg),
            }
        }
        cx.canvas.stroke_rounded_rect(self.rect, t.radius, t.border as f32, INK);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Point;

    fn control() -> Segmented {
        let mut s = Segmented::texts(&["Recent", "Title", "Author"], 0);
        s.place(Rect::new(0, 0, 300, 100));
        s
    }

    #[test]
    fn tapping_a_segment_selects_it() {
        let mut s = control();
        let p = Point::new(150, 50);
        s.on_gesture(&Gesture::Down(p));
        assert_eq!(s.on_gesture(&Gesture::Tap(p)), Some(1));
        assert_eq!(s.selected, 1);
        // Tapping the selected one again reports nothing.
        s.on_gesture(&Gesture::Down(p));
        assert_eq!(s.on_gesture(&Gesture::Tap(p)), None);
    }

    #[test]
    fn hits_the_right_segment() {
        let mut s = control();
        for (x, want) in [(5, None), (120, Some(1)), (299, Some(2))] {
            let mut s2 = control();
            let p = Point::new(x, 50);
            s2.on_gesture(&Gesture::Down(p));
            assert_eq!(s2.on_gesture(&Gesture::Tap(p)), want, "x = {x}");
        }
        s.on_gesture(&Gesture::Down(Point::new(400, 50)));
        assert_eq!(s.on_gesture(&Gesture::Tap(Point::new(400, 50))), None, "outside");
    }
}
