//! Pieces every widget shares: the drawing context and press tracking.
//!
//! Widgets are plain structs owned by their screen (see docs/design.md §6).
//! A screen places them (`place`), draws them, and passes each gesture to
//! them; a widget answers with a typed result (clicked, new value, row…).
//! Every widget gets every gesture, but only reacts to a touch that started
//! on it, so hit areas of different widgets must not overlap.

use crate::geom::Rect;
use crate::gfx::{Canvas, HAlign, Icon, IconRenderer, TextRenderer, TextStyle};
use crate::platform::{RefreshMode, Update};
use crate::ui::gesture::Gesture;
use crate::ui::theme::Theme;

/// Everything needed to draw a frame.
pub struct DrawCx<'a> {
    pub canvas: &'a mut Canvas,
    pub text: &'a mut TextRenderer,
    pub icons: &'a mut IconRenderer,
    pub theme: &'a Theme,
    hints: &'a mut Vec<Update>,
}

impl<'a> DrawCx<'a> {
    pub fn new(
        canvas: &'a mut Canvas,
        text: &'a mut TextRenderer,
        icons: &'a mut IconRenderer,
        theme: &'a Theme,
        hints: &'a mut Vec<Update>,
    ) -> Self {
        Self { canvas, text, icons, theme, hints }
    }

    /// Asks for `mode` wherever `rect` changed in this frame (see `ui::damage`).
    pub fn hint(&mut self, rect: Rect, mode: RefreshMode) {
        self.hints.push(Update { rect, mode });
    }

    pub fn bounds(&self) -> Rect {
        self.canvas.bounds()
    }

    pub fn fill(&mut self, rect: Rect, gray: u8) {
        self.canvas.fill_rect(rect, gray);
    }

    /// One line of text, cut short with "…" if needed.
    pub fn line(&mut self, text: &str, style: TextStyle, rect: Rect, align: HAlign) {
        self.text.line(self.canvas, text, style, rect, align);
    }

    /// Wrapped text, as many lines as fit.
    pub fn paragraph(&mut self, text: &str, style: TextStyle, rect: Rect, align: HAlign) {
        self.text.draw(self.canvas, text, style, rect, align);
    }

    /// An icon of `size` centred in `rect`.
    pub fn icon(&mut self, icon: Icon, rect: Rect, size: i32, gray: u8) {
        self.icons.draw(self.canvas, icon, rect.center_box(size, size), gray);
    }

    /// A horizontal line of the given thickness, centred on `y`.
    pub fn hline(&mut self, y: i32, x0: i32, x1: i32, thickness: i32, gray: u8) {
        self.canvas.fill_rect(Rect::new(x0, y - thickness / 2, x1 - x0, thickness), gray);
    }

    /// A small uppercase section label ("BUTTONS"), left-aligned.
    pub fn section_label(&mut self, text: &str, rect: Rect) {
        let style = self.theme.section;
        self.text.line(self.canvas, &text.to_uppercase(), style, rect, HAlign::Left);
    }
}

/// Tracks a finger on something you can tap.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Press {
    down: bool,
}

/// How a touch on a pressable thing ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PressEvent {
    Tap,
    /// Held still; the finger is still down.
    LongPress,
    /// Lifted after a long-press.
    LongPressEnd,
}

impl Press {
    /// True while a finger that started on the widget is down and hasn't
    /// moved away. The widget shows this as pressed (inverted).
    pub fn is_down(&self) -> bool {
        self.down
    }

    /// Feeds a gesture. `area` is the touch area.
    pub fn on_gesture(&mut self, g: &Gesture, area: Rect) -> Option<PressEvent> {
        match *g {
            Gesture::Down(p) => {
                self.down = area.contains(p);
                None
            }
            // Moving away means it's no longer a tap.
            Gesture::Move(_) | Gesture::Swipe { .. } | Gesture::Cancel(_) => {
                self.down = false;
                None
            }
            Gesture::Tap(_) if self.down => {
                self.down = false;
                Some(PressEvent::Tap)
            }
            Gesture::LongPress(_) if self.down => Some(PressEvent::LongPress),
            Gesture::End(_) if self.down => {
                self.down = false;
                Some(PressEvent::LongPressEnd)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Point;

    const AREA: Rect = Rect::new(0, 0, 100, 100);
    const IN: Point = Point::new(50, 50);
    const OUT: Point = Point::new(150, 50);

    #[test]
    fn tap_on_it() {
        let mut p = Press::default();
        assert_eq!(p.on_gesture(&Gesture::Down(IN), AREA), None);
        assert!(p.is_down());
        assert_eq!(p.on_gesture(&Gesture::Tap(IN), AREA), Some(PressEvent::Tap));
        assert!(!p.is_down());
    }

    #[test]
    fn touches_elsewhere_are_ignored() {
        let mut p = Press::default();
        p.on_gesture(&Gesture::Down(OUT), AREA);
        assert!(!p.is_down());
        assert_eq!(p.on_gesture(&Gesture::Tap(OUT), AREA), None);
    }

    #[test]
    fn moving_away_cancels() {
        let mut p = Press::default();
        p.on_gesture(&Gesture::Down(IN), AREA);
        p.on_gesture(&Gesture::Move(OUT), AREA);
        assert!(!p.is_down());
        assert_eq!(p.on_gesture(&Gesture::Cancel(OUT), AREA), None);
    }

    #[test]
    fn long_press_then_end() {
        let mut p = Press::default();
        p.on_gesture(&Gesture::Down(IN), AREA);
        assert_eq!(p.on_gesture(&Gesture::LongPress(IN), AREA), Some(PressEvent::LongPress));
        assert!(p.is_down(), "still pressed while held");
        assert_eq!(p.on_gesture(&Gesture::End(IN), AREA), Some(PressEvent::LongPressEnd));
        assert!(!p.is_down());
    }
}
