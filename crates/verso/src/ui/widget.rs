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
    pub battery: Option<crate::platform::Battery>,
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
        Self { canvas, text, icons, theme, hints, battery: None }
    }

    /// Asks for `mode` wherever `rect` changed in this frame (see `ui::damage`).
    /// Hints are for widget-level `Fast` or `Ui` feedback; stronger modes
    /// (`Page`, `Full`) belong in `EventCx::refresh` so `App::frame` can decide
    /// full-screen bypasses before mutating the canvas.
    pub fn hint(&mut self, rect: Rect, mode: RefreshMode) {
        debug_assert!(mode <= RefreshMode::Ui, "DrawCx::hint only supports Fast or Ui; got {mode:?}");
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

    /// One line without a width limit, from the left of `rect` and clipped
    /// to it. Reuses the layout `text.width` measured.
    pub fn line_measured(&mut self, text: &str, style: TextStyle, rect: Rect) {
        self.text.line_measured(self.canvas, text, style, rect);
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

    pub fn battery(&mut self, rect: Rect) {
        if let Some(battery) = self.battery {
            let label = format!("{}%{}", battery.percent, if battery.charging { " · Charging" } else { "" });
            self.line(&label, self.theme.caption, rect, HAlign::Right);
        }
    }

    /// A battery outline filled to its level, or with a bolt while charging.
    pub fn battery_icon(&mut self, square: Rect, battery: crate::platform::Battery, gray: u8) {
        if square.is_empty() {
            return;
        }
        let icon = if battery.charging { Icon::BatteryCharging } else { Icon::Battery };
        self.icons.draw(self.canvas, icon, square, gray);
        if !battery.charging {
            // Inside the 24-unit outline (body x 2–18, y 7–17, 2-unit stroke),
            // one unit clear of the stroke: x 4–16, y 9–15.
            let unit = square.w as f32 / 24.0;
            let full = 12.0 * unit;
            let w = (full * battery.percent.min(100) as f32 / 100.0).round() as i32;
            let x = square.x + (4.0 * unit).round() as i32;
            let y = square.y + (9.0 * unit).round() as i32;
            self.fill(Rect::new(x, y, w.max(1), (6.0 * unit).round() as i32), gray);
        }
    }

    /// Dynamic battery icon (proportional fill or charging bolt) + `"82%"`,
    /// left-aligned inside `rect` and clipped to it so it never overflows.
    pub fn battery_indicator(&mut self, rect: Rect) {
        let Some(battery) = self.battery else { return };
        if rect.is_empty() {
            return;
        }
        let t = self.theme;
        let style = t.small.gray(crate::ui::theme::INK);
        let icon = t.mm(5.4).min(rect.h).min(rect.w);
        let square = Rect::new(rect.x, rect.y + (rect.h - icon) / 2, icon, icon);
        self.battery_icon(square, battery, style.gray);
        let gap = t.mm(0.8);
        let text_x = square.right() + gap;
        if text_x < rect.right() {
            let label = format!("{}%", battery.percent.min(100));
            self.line(&label, style, Rect::new(text_x, rect.y, rect.right() - text_x, rect.h), HAlign::Left);
        }
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

    #[test]
    fn battery_indicator_fills_by_level_shows_bolt_and_stays_in_bounds() {
        use crate::platform::Battery;
        use crate::ui::theme::PAPER;
        let t = Theme::for_dpi(300);
        let slot = Rect::new(460, 0, 170, t.top_bar);
        let render = |battery: Option<Battery>, rect: Rect| {
            let mut canvas = Canvas::new(800, t.top_bar as u32);
            canvas.fill(PAPER);
            let (mut text, mut icons, mut hits) = (TextRenderer::new(), IconRenderer::default(), vec![]);
            let mut cx = DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut hits);
            cx.battery = battery;
            cx.battery_indicator(rect);
            canvas.gray_data().to_vec()
        };
        let ink = |pixels: &[u8], r: Rect| {
            (r.y..r.bottom())
                .flat_map(|y| (r.x..r.right()).map(move |x| pixels[y as usize * 800 + x as usize]))
                .filter(|&p| p < PAPER)
                .count()
        };
        let full_screen = Rect::new(0, 0, 800, t.top_bar);
        let none = render(None, slot);
        assert_eq!(ink(&none, full_screen), 0);
        let low = render(Some(Battery { percent: 15, charging: false }), slot);
        let high = render(Some(Battery { percent: 95, charging: false }), slot);
        let charging = render(Some(Battery { percent: 95, charging: true }), slot);
        assert!(ink(&high, slot) > ink(&low, slot), "fill grows with charge");
        assert_ne!(high, charging, "charging draws the bolt instead of the fill");
        assert_eq!(ink(&high, slot), ink(&high, full_screen), "nothing overflows the battery slot");
        assert_eq!(ink(&charging, slot), ink(&charging, full_screen), "charging never overflows either");
        // Even a tight slot clips cleanly without drawing past its right edge.
        let tight = Rect::new(460, 0, 90, t.top_bar);
        let clipped = render(Some(Battery { percent: 100, charging: true }), tight);
        assert_eq!(ink(&clipped, tight), ink(&clipped, full_screen));
    }
}
