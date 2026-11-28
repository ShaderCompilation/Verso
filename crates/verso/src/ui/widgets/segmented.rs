//! A row of choices in one outline; the selected one is filled black.

use crate::geom::Rect;
use crate::gfx::{HAlign, Icon};
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK, INK_FILL, INK3, PAPER};
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
    /// Optional dot indicator on an unselected segment (e.g. an available update).
    pub dot: Option<usize>,
    /// Segments drawn gray that ignore taps.
    pub disabled: Vec<usize>,
    rect: Rect,
    /// The segment the current touch started on.
    down_on: Option<usize>,
}

impl Segmented {
    pub fn new(items: Vec<Segment>, selected: usize) -> Self {
        Self { items, selected, dot: None, disabled: Vec::new(), rect: Rect::default(), down_on: None }
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
        self.segments().iter().position(|r| r.contains(p)).filter(|i| !self.disabled.contains(i))
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
        // Labels shrink together, down to 80%, rather than being cut short
        // (five tabs on a 600 px wide screen).
        let pad = t.mm(1.0);
        let mut style = t.button;
        let fit = segs
            .iter()
            .zip(&self.items)
            .filter_map(|(seg, item)| match item {
                Segment::Text(s) => Some((seg.w - 2 * pad) as f32 / cx.text.width(s, style).max(1.0)),
                _ => None,
            })
            .fold(1.0f32, f32::min);
        if fit < 1.0 {
            style.size = (style.size * fit.max(0.8)).floor();
        }
        for (i, (seg, item)) in segs.iter().zip(&self.items).enumerate() {
            let selected = i == self.selected;
            let fg = if selected {
                // Rounded on the outside only: fill rounded, then square off inner sides.
                cx.canvas.fill_rounded_rect(*seg, t.radius, INK_FILL);
                if i > 0 {
                    cx.fill(seg.split_left(seg.w / 2).0, INK_FILL);
                }
                if i + 1 < n {
                    cx.fill(seg.split_right(seg.w / 2).0, INK_FILL);
                }
                PAPER
            } else {
                // A divider, unless the neighbour on the left is selected.
                if i > 0 && i != self.selected + 1 {
                    cx.fill(Rect::new(seg.x - t.hairline / 2, seg.y + t.mm(2.0), t.hairline, seg.h - t.mm(4.0)), INK);
                }
                if self.disabled.contains(&i) { INK3 } else { INK }
            };
            match item {
                Segment::Text(s) => cx.line(s, style.gray(fg), seg.inset_xy(pad, 0), HAlign::Center),
                Segment::Icon(icon) => cx.icon(*icon, *seg, t.icon_small, fg),
                Segment::Glyph(draw) => draw(cx, *seg, fg),
            }
            if self.dot == Some(i) && !selected {
                let r = t.mm(0.8) as f32;
                let inset = t.mm(2.0) as f32;
                cx.canvas.fill_circle(seg.right() as f32 - inset, seg.y as f32 + inset, r, INK);
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
