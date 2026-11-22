//! [smaller] value [larger], in one outline. Holding a side repeats.

use std::time::{Duration, Instant};

use crate::geom::{Point, Rect};
use crate::gfx::{HAlign, TextStyle};
use crate::platform::RefreshMode;
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK, INK3, PAPER};
use crate::ui::widget::DrawCx;

/// Time between repeats while a side is held.
const REPEAT: Duration = Duration::from_millis(350);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Less,
    More,
}

/// What each side shows: some text in a style (e.g. a small and a big "A").
pub struct SideLabel {
    pub text: String,
    pub style: TextStyle,
}

pub struct Stepper {
    pub less: SideLabel,
    pub more: SideLabel,
    /// The current value, as shown in the middle.
    pub value: String,
    pub can_less: bool,
    pub can_more: bool,
    rect: Rect,
    side_w: i32,
    pressed: Option<Step>,
    repeat_at: Option<Instant>,
}

impl Stepper {
    pub fn new(less: SideLabel, more: SideLabel, value: impl Into<String>) -> Self {
        Self {
            less,
            more,
            value: value.into(),
            can_less: true,
            can_more: true,
            rect: Rect::default(),
            side_w: 0,
            pressed: None,
            repeat_at: None,
        }
    }

    /// `side_w` is the width of each tappable side.
    pub fn place(&mut self, rect: Rect, side_w: i32) {
        self.rect = rect;
        self.side_w = side_w;
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    pub fn pressed(&self) -> Option<Step> {
        self.pressed
    }

    /// The smaller side, the value and the larger side.
    #[cfg(test)]
    pub fn parts(&self) -> (Rect, Rect, Rect) {
        self.sides()
    }

    fn sides(&self) -> (Rect, Rect, Rect) {
        let (less, rest) = self.rect.split_left(self.side_w);
        let (more, middle) = rest.split_right(self.side_w);
        (less, middle, more)
    }

    fn side_at(&self, p: Point) -> Option<Step> {
        let (less, _, more) = self.sides();
        if less.contains(p) && self.can_less {
            Some(Step::Less)
        } else if more.contains(p) && self.can_more {
            Some(Step::More)
        } else {
            None
        }
    }

    pub fn on_gesture(&mut self, g: &Gesture) -> Option<Step> {
        match *g {
            Gesture::Down(p) => {
                self.pressed = self.side_at(p);
                None
            }
            Gesture::Tap(_) => {
                self.repeat_at = None;
                self.pressed.take()
            }
            Gesture::LongPress(_) => {
                // Step now, then keep stepping until the finger lifts.
                let step = self.pressed?;
                self.repeat_at = Some(Instant::now() + REPEAT);
                Some(step)
            }
            Gesture::End(_) | Gesture::Move(_) | Gesture::Swipe { .. } | Gesture::Cancel(_) => {
                self.pressed = None;
                self.repeat_at = None;
                None
            }
        }
    }

    /// When `on_tick` wants to be called (while a side is held).
    pub fn deadline(&self) -> Option<Instant> {
        self.repeat_at
    }

    pub fn on_tick(&mut self, now: Instant) -> Option<Step> {
        let at = self.repeat_at?;
        if now < at {
            return None;
        }
        let step = self.pressed?;
        let can = match step {
            Step::Less => self.can_less,
            Step::More => self.can_more,
        };
        if !can {
            self.repeat_at = None;
            return None;
        }
        self.repeat_at = Some(now + REPEAT);
        Some(step)
    }

    pub fn draw(&self, cx: &mut DrawCx) {
        let t = cx.theme;
        let (less, middle, more) = self.sides();
        for (side, step, label, enabled) in
            [(less, Step::Less, &self.less, self.can_less), (more, Step::More, &self.more, self.can_more)]
        {
            let fg = if self.pressed == Some(step) {
                // Rounded on the outer side only.
                cx.canvas.fill_rounded_rect(side, t.radius, INK);
                let inner = match step {
                    Step::Less => side.split_right(side.w / 2).0,
                    Step::More => side.split_left(side.w / 2).0,
                };
                cx.fill(inner, INK);
                cx.hint(side, RefreshMode::Fast);
                PAPER
            } else if enabled {
                INK
            } else {
                INK3
            };
            cx.line(&label.text, label.style.gray(fg), side, HAlign::Center);
        }
        let divider =
            |x: i32| Rect::new(x - t.hairline / 2, self.rect.y + t.mm(2.0), t.hairline, self.rect.h - t.mm(4.0));
        cx.fill(divider(less.right()), INK);
        cx.fill(divider(more.x), INK);
        cx.line(&self.value, t.button, middle, HAlign::Center);
        cx.canvas.stroke_rounded_rect(self.rect, t.radius, t.border as f32, INK);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::{Family, Weight};

    fn stepper() -> Stepper {
        let style = TextStyle::new(30.0, Weight::Regular, Family::Serif);
        let mut s = Stepper::new(
            SideLabel { text: "A".into(), style },
            SideLabel { text: "A".into(), style: style.size(45.0) },
            "10 pt",
        );
        s.place(Rect::new(0, 0, 600, 100), 150);
        s
    }

    #[test]
    fn tap_each_side() {
        let mut s = stepper();
        let (l, m) = (Point::new(20, 50), Point::new(580, 50));
        s.on_gesture(&Gesture::Down(l));
        assert_eq!(s.on_gesture(&Gesture::Tap(l)), Some(Step::Less));
        s.on_gesture(&Gesture::Down(m));
        assert_eq!(s.on_gesture(&Gesture::Tap(m)), Some(Step::More));
        let mid = Point::new(300, 50);
        s.on_gesture(&Gesture::Down(mid));
        assert_eq!(s.on_gesture(&Gesture::Tap(mid)), None, "the value in the middle isn't a button");
    }

    #[test]
    fn holding_repeats_until_lifted() {
        let mut s = stepper();
        let p = Point::new(580, 50);
        s.on_gesture(&Gesture::Down(p));
        assert_eq!(s.on_gesture(&Gesture::LongPress(p)), Some(Step::More));
        let first = s.deadline().expect("repeating");
        assert_eq!(s.on_tick(first - Duration::from_millis(1)), None);
        assert_eq!(s.on_tick(first), Some(Step::More));
        assert!(s.deadline().unwrap() > first);
        s.on_gesture(&Gesture::End(p));
        assert_eq!(s.deadline(), None);
        assert_eq!(s.on_tick(first + Duration::from_secs(5)), None);
    }

    #[test]
    fn a_disabled_side_does_nothing() {
        let mut s = stepper();
        s.can_less = false;
        let p = Point::new(20, 50);
        s.on_gesture(&Gesture::Down(p));
        assert_eq!(s.on_gesture(&Gesture::Tap(p)), None);
    }
}
