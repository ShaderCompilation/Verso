//! A value on a track: tap to jump, or drag. Snaps to steps.
//! A touch that sets off mostly vertically is a swipe across the slider, not a
//! drag: the value goes back to what it was and the slider lets the touch go.
//! Refreshed with `Fast` while the finger is down, which sends only black and
//! white: the gray track and ticks are drawn as thin black lines meanwhile.

use crate::geom::{Point, Rect};
use crate::platform::RefreshMode;
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK, INK3, LINE, PAPER, Theme};
use crate::ui::widget::DrawCx;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SliderEvent {
    /// The value changed while the finger is down, or went back because the
    /// touch turned out to be a vertical swipe.
    Moved(f32),
    /// The finger lifted; this is the final value.
    Released(f32),
}

/// The touch a slider is following.
#[derive(Clone, Copy)]
struct Drag {
    start: Point,
    /// The value before the touch.
    before: f32,
    /// Set off sideways, or held still: a drag wherever it goes next.
    locked: bool,
}

pub struct Slider {
    pub min: f32,
    pub max: f32,
    /// Values snap to multiples of this from `min` (0 = no snapping).
    pub step: f32,
    pub value: f32,
    /// Draw a tick at every step.
    pub ticks: bool,
    rect: Rect,
    hit: Rect,
    knob_r: i32,
    drag: Option<Drag>,
}

impl Slider {
    pub fn new(min: f32, max: f32, step: f32, value: f32) -> Self {
        Self { min, max, step, value, ticks: false, rect: Rect::default(), hit: Rect::default(), knob_r: 0, drag: None }
    }

    pub fn with_ticks(mut self) -> Self {
        self.ticks = true;
        self
    }

    /// `rect` spans the whole track including the knob at both ends.
    pub fn place(&mut self, rect: Rect, theme: &Theme) {
        self.rect = rect;
        self.hit = rect.at_least(0, theme.target);
        self.knob_r = theme.mm(2.2);
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// The area the slider may draw in (the knob and ticks stick out of the track).
    pub fn bounds(&self) -> Rect {
        self.hit
    }

    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// The track runs between the knob's centre at either end.
    fn track(&self) -> (i32, i32) {
        (self.rect.x + self.knob_r, self.rect.right() - self.knob_r)
    }

    fn value_at(&self, x: i32) -> f32 {
        let (x0, x1) = self.track();
        let frac = ((x - x0) as f32 / (x1 - x0).max(1) as f32).clamp(0.0, 1.0);
        let raw = self.min + frac * (self.max - self.min);
        if self.step > 0.0 {
            (self.min + ((raw - self.min) / self.step).round() * self.step).clamp(self.min, self.max)
        } else {
            raw
        }
    }

    fn frac(&self) -> f32 {
        if self.max > self.min { ((self.value - self.min) / (self.max - self.min)).clamp(0.0, 1.0) } else { 0.0 }
    }

    pub fn on_gesture(&mut self, g: &Gesture) -> Option<SliderEvent> {
        if let Gesture::Down(p) = *g {
            self.drag = self.hit.contains(p).then_some(Drag { start: p, before: self.value, locked: false });
            return self.drag.and_then(|_| self.set(p.x)).map(SliderEvent::Moved);
        }
        let drag = self.drag.as_mut()?;
        if matches!(g, Gesture::LongPress(_)) {
            drag.locked = true;
            return None;
        }
        let p = g.pos();
        if !drag.locked {
            if (p.y - drag.start.y).abs() > (p.x - drag.start.x).abs() {
                let before = drag.before;
                self.drag = None;
                return (self.value != before).then(|| {
                    self.value = before;
                    SliderEvent::Moved(before)
                });
            }
            drag.locked = true;
        }
        if g.ends_touch() {
            self.drag = None;
            self.set(p.x);
            return Some(SliderEvent::Released(self.value));
        }
        self.set(p.x).map(SliderEvent::Moved)
    }

    /// Moves the knob to `x`; returns the new value if it changed.
    fn set(&mut self, x: i32) -> Option<f32> {
        let v = self.value_at(x);
        (v != self.value).then(|| {
            self.value = v;
            v
        })
    }

    pub fn draw(&self, cx: &mut DrawCx) {
        let t = cx.theme;
        let (x0, x1) = self.track();
        let cy = self.rect.center().y;
        let dragging = self.drag.is_some();
        if dragging {
            let h = t.hairline.max(1);
            cx.fill(Rect::new(x0, cy - h / 2, x1 - x0, h), INK);
        } else {
            let track_h = t.hairline + 2;
            cx.canvas.fill_rounded_rect(Rect::new(x0, cy - track_h / 2, x1 - x0, track_h), 2.0, LINE);
        }
        let kx = x0 + ((x1 - x0) as f32 * self.frac()).round() as i32;
        let filled_h = t.mm(1.0);
        if kx > x0 {
            cx.canvas.fill_rounded_rect(
                Rect::new(x0, cy - filled_h / 2, kx - x0, filled_h),
                filled_h as f32 / 2.0,
                INK,
            );
        }
        if self.ticks && self.step > 0.0 {
            let n = ((self.max - self.min) / self.step).round() as i32;
            for i in 1..n {
                let tx = x0 + (x1 - x0) * i / n;
                let (w, gray) = if dragging { (t.hairline.max(1), INK) } else { (3, INK3) };
                cx.fill(Rect::new(tx - w / 2, cy + t.mm(1.6), w, t.mm(1.0)), gray);
            }
        }
        let r = self.knob_r as f32;
        cx.canvas.fill_circle(kx as f32, cy as f32, r, PAPER);
        cx.canvas.stroke_circle(kx as f32, cy as f32, r, t.border as f32, INK);
        if dragging {
            cx.hint(self.hit, RefreshMode::Fast);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::gesture::Direction;

    fn slider() -> Slider {
        let theme = Theme::for_dpi(300);
        let mut s = Slider::new(0.0, 24.0, 1.0, 8.0);
        // Knob radius 26 px: the track runs from x = 126 to 1074 (948 px).
        s.place(Rect::new(100, 100, 1000, 60), &theme);
        s
    }

    #[test]
    fn tap_jumps_and_snaps() {
        let mut s = slider();
        let p = Point::new(600, 130); // exactly halfway
        assert_eq!(s.on_gesture(&Gesture::Down(p)), Some(SliderEvent::Moved(12.0)));
        assert!(s.is_dragging());
        assert_eq!(s.on_gesture(&Gesture::Tap(p)), Some(SliderEvent::Released(12.0)));
        assert!(!s.is_dragging());
    }

    #[test]
    fn drag_reports_each_change_and_clamps() {
        let mut s = slider();
        s.on_gesture(&Gesture::Down(Point::new(600, 130)));
        assert_eq!(s.on_gesture(&Gesture::Move(Point::new(610, 130))), None, "less than a step");
        assert_eq!(s.on_gesture(&Gesture::Move(Point::new(1500, 130))), Some(SliderEvent::Moved(24.0)));
        // Lifting far below the slider still finishes the drag.
        assert_eq!(
            s.on_gesture(&Gesture::Swipe { dir: Direction::Left, from: Point::new(600, 130), to: Point::new(0, 900) }),
            Some(SliderEvent::Released(0.0))
        );
    }

    #[test]
    fn a_vertical_swipe_puts_the_value_back() {
        let from = Point::new(600, 130);
        let swipe = |to| Gesture::Swipe { dir: Direction::Up, from, to };
        // Wherever it ends up, and also when no movement was reported.
        for rest in [
            vec![Gesture::Move(Point::new(610, 90)), swipe(Point::new(900, 20))],
            vec![Gesture::Move(Point::new(610, 170)), Gesture::Cancel(Point::new(900, 170))],
            vec![swipe(Point::new(620, 20))],
        ] {
            let mut s = slider();
            assert_eq!(s.on_gesture(&Gesture::Down(from)), Some(SliderEvent::Moved(12.0)));
            assert_eq!(s.on_gesture(&rest[0]), Some(SliderEvent::Moved(8.0)), "{rest:?}");
            assert!(!s.is_dragging());
            assert!(rest[1..].iter().all(|g| s.on_gesture(g).is_none()), "{rest:?}");
            assert_eq!(s.value, 8.0);
        }
        // A finger held on the slider is dragging it, however it moves next.
        let mut s = slider();
        s.on_gesture(&Gesture::Down(from));
        s.on_gesture(&Gesture::LongPress(from));
        assert_eq!(s.on_gesture(&Gesture::Move(Point::new(600, 120))), None);
        assert_eq!(s.on_gesture(&Gesture::End(Point::new(600, 120))), Some(SliderEvent::Released(12.0)));
    }

    #[test]
    fn dragging_draws_the_gray_track_black_for_fast_refreshes() {
        let theme = Theme::for_dpi(300);
        let (mut canvas, mut text, mut icons) =
            (crate::gfx::Canvas::new(1200, 300), crate::gfx::TextRenderer::new(), crate::gfx::IconRenderer::default());
        let mut s = slider();
        // A point on the unfilled track, right of the knob at 8 of 24.
        let at = |c: &crate::gfx::Canvas| c.gray_data()[130 * 1200 + 900];
        let mut draw = |s: &Slider| {
            let mut hints = Vec::new();
            s.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &theme, &mut hints));
            at(&canvas)
        };
        assert_eq!(draw(&s), LINE);
        s.on_gesture(&Gesture::Down(Point::new(400, 130)));
        assert_eq!(draw(&s), INK);
    }

    #[test]
    fn touches_elsewhere_are_ignored() {
        let mut s = slider();
        assert_eq!(s.on_gesture(&Gesture::Down(Point::new(600, 400))), None);
        assert_eq!(s.on_gesture(&Gesture::Tap(Point::new(600, 400))), None);
        assert_eq!(s.value, 8.0);
    }
}
