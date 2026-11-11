//! An on/off switch: a 9 × 5 mm pill. Tap flips it.

use crate::geom::Rect;
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK, INK3, PAPER, Theme};
use crate::ui::widget::{DrawCx, Press, PressEvent};

pub struct Toggle {
    pub on: bool,
    pub enabled: bool,
    rect: Rect,
    hit: Rect,
    press: Press,
}

impl Toggle {
    pub fn new(on: bool) -> Self {
        Self { on, enabled: true, rect: Rect::default(), hit: Rect::default(), press: Press::default() }
    }

    /// The pill's size for a theme.
    pub fn size(theme: &Theme) -> (i32, i32) {
        (theme.mm(9.0), theme.mm(5.0))
    }

    /// Places the pill with its right edge at `right`, centred on `cy`.
    pub fn place(&mut self, right: i32, cy: i32, theme: &Theme) {
        let (w, h) = Self::size(theme);
        self.rect = Rect::new(right - w, cy - h / 2, w, h);
        self.hit = self.rect.at_least(theme.target, theme.target);
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Returns the new value when tapped.
    pub fn on_gesture(&mut self, g: &Gesture) -> Option<bool> {
        if !self.enabled {
            return None;
        }
        match self.press.on_gesture(g, self.hit) {
            Some(PressEvent::Tap | PressEvent::LongPressEnd) => {
                self.on = !self.on;
                Some(self.on)
            }
            _ => None,
        }
    }

    pub fn draw(&self, cx: &mut DrawCx) {
        draw_pill(cx, self.rect, self.on, self.enabled);
    }
}

/// Draws a toggle pill in `rect` (also used by list rows).
pub fn draw_pill(cx: &mut DrawCx, rect: Rect, on: bool, enabled: bool) {
    let ink = if enabled { INK } else { INK3 };
    let h = rect.h as f32;
    let knob = h - 2.0 * cx.theme.mm(0.8) as f32;
    let cy = rect.y as f32 + h / 2.0;
    if on {
        cx.canvas.fill_rounded_rect(rect, h / 2.0, ink);
        cx.canvas.fill_circle(rect.right() as f32 - h / 2.0, cy, knob / 2.0, PAPER);
    } else {
        cx.canvas.stroke_rounded_rect(rect, h / 2.0, cx.theme.border as f32, ink);
        cx.canvas.fill_circle(rect.x as f32 + h / 2.0, cy, knob / 2.0 - 2.0, ink);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Point;

    #[test]
    fn tap_flips() {
        let theme = Theme::for_dpi(300);
        let mut t = Toggle::new(false);
        t.place(500, 300, &theme);
        let p = t.rect().center();
        t.on_gesture(&Gesture::Down(p));
        assert_eq!(t.on_gesture(&Gesture::Tap(p)), Some(true));
        t.on_gesture(&Gesture::Down(p));
        assert_eq!(t.on_gesture(&Gesture::Tap(p)), Some(false));
        // The touch area is bigger than the 5 mm tall pill.
        let near = Point::new(p.x, t.rect().y - 20);
        t.on_gesture(&Gesture::Down(near));
        assert_eq!(t.on_gesture(&Gesture::Tap(near)), Some(true));
    }
}
