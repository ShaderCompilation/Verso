//! Buttons with a label (and optional icon), and icon-only buttons.
//! Pressed means inverted, refreshed with `Fast`; it acts when the finger lifts.

use crate::geom::Rect;
use crate::gfx::{HAlign, Icon};
use crate::platform::RefreshMode;
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK, INK3, LINE, PAPER, Theme};
use crate::ui::widget::{DrawCx, Press, PressEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonKind {
    /// Black fill, white text. The main action.
    Primary,
    /// Black outline.
    Secondary,
}

pub struct Button {
    pub label: String,
    pub icon: Option<Icon>,
    pub kind: ButtonKind,
    pub enabled: bool,
    rect: Rect,
    hit: Rect,
    press: Press,
}

impl Button {
    pub fn new(label: impl Into<String>, kind: ButtonKind) -> Self {
        Self {
            label: label.into(),
            icon: None,
            kind,
            enabled: true,
            rect: Rect::default(),
            hit: Rect::default(),
            press: Press::default(),
        }
    }

    pub fn with_icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    pub fn place(&mut self, rect: Rect, theme: &Theme) {
        self.rect = rect;
        self.hit = rect.at_least(theme.target, theme.target);
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    pub fn is_pressed(&self) -> bool {
        self.press.is_down()
    }

    /// True when the button was tapped (or long-pressed and released).
    pub fn on_gesture(&mut self, g: &Gesture) -> bool {
        if !self.enabled {
            return false;
        }
        matches!(self.press.on_gesture(g, self.hit), Some(PressEvent::Tap | PressEvent::LongPressEnd))
    }

    pub fn draw(&self, cx: &mut DrawCx) {
        let t = cx.theme;
        let (r, radius, border) = (self.rect, t.radius, t.border as f32);
        let pressed = self.press.is_down();
        let fg = if !self.enabled {
            cx.canvas.stroke_rounded_rect(r, radius, border, LINE);
            INK3
        } else if pressed {
            // Inverted: a primary button turns white with an outline.
            cx.hint(r, RefreshMode::Fast);
            match self.kind {
                ButtonKind::Primary => {
                    cx.canvas.stroke_rounded_rect(r, radius, border, INK);
                    INK
                }
                ButtonKind::Secondary => {
                    cx.canvas.fill_rounded_rect(r, radius, INK);
                    PAPER
                }
            }
        } else {
            match self.kind {
                ButtonKind::Primary => {
                    cx.canvas.fill_rounded_rect(r, radius, INK);
                    PAPER
                }
                ButtonKind::Secondary => {
                    cx.canvas.stroke_rounded_rect(r, radius, border, INK);
                    INK
                }
            }
        };
        let style = t.button.gray(fg);
        let Some(icon) = self.icon else {
            cx.line(&self.label, style, r.inset_xy(t.mm(2.0), 0), HAlign::Center);
            return;
        };
        // Icon and label, centred together.
        let gap = t.mm(2.0);
        let max_text = (r.w - t.icon_small - gap - 2 * t.mm(2.0)).max(0);
        let text_w = (cx.text.width(&self.label, style).ceil() as i32).min(max_text);
        let x = r.center().x - (t.icon_small + gap + text_w) / 2;
        let icon_rect = Rect::new(x, r.y, t.icon_small, r.h);
        cx.icon(icon, icon_rect, t.icon_small, fg);
        cx.line(&self.label, style, Rect::new(icon_rect.right() + gap, r.y, text_w + 2, r.h), HAlign::Left);
    }
}

/// A 5 mm icon in a 9 mm touch target.
pub struct IconButton {
    pub icon: Icon,
    pub enabled: bool,
    /// Icon size in pixels (defaults to the theme's icon size).
    pub size: Option<i32>,
    rect: Rect,
    press: Press,
}

impl IconButton {
    pub fn new(icon: Icon) -> Self {
        Self { icon, enabled: true, size: None, rect: Rect::default(), press: Press::default() }
    }

    /// `rect` is the touch target; the icon is centred in it.
    pub fn place(&mut self, rect: Rect) {
        self.rect = rect;
    }

    /// Places a target-sized button centred on `center`.
    pub fn place_at(&mut self, center: crate::geom::Point, theme: &Theme) {
        self.rect = Rect::centered(center, theme.target, theme.target);
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    pub fn on_gesture(&mut self, g: &Gesture) -> bool {
        if !self.enabled {
            return false;
        }
        matches!(self.press.on_gesture(g, self.rect), Some(PressEvent::Tap | PressEvent::LongPressEnd))
    }

    pub fn draw(&self, cx: &mut DrawCx) {
        let size = self.size.unwrap_or(cx.theme.icon);
        let fg = if !self.enabled {
            INK3
        } else if self.press.is_down() {
            cx.canvas.fill_rounded_rect(self.rect, cx.theme.radius, INK);
            cx.hint(self.rect, RefreshMode::Fast);
            PAPER
        } else {
            INK
        };
        cx.icon(self.icon, self.rect, size, fg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Point;

    #[test]
    fn button_clicks_on_lift_not_on_touch() {
        let theme = Theme::for_dpi(300);
        let mut b = Button::new("Continue", ButtonKind::Primary);
        b.place(Rect::new(0, 0, 400, 118), &theme);
        let p = Point::new(200, 50);
        assert!(!b.on_gesture(&Gesture::Down(p)));
        assert!(b.is_pressed());
        assert!(b.on_gesture(&Gesture::Tap(p)));
        assert!(!b.is_pressed());
    }

    #[test]
    fn swiping_off_a_button_doesnt_click() {
        let theme = Theme::for_dpi(300);
        let mut b = Button::new("Continue", ButtonKind::Primary);
        b.place(Rect::new(0, 0, 400, 118), &theme);
        b.on_gesture(&Gesture::Down(Point::new(200, 50)));
        b.on_gesture(&Gesture::Move(Point::new(600, 50)));
        assert!(!b.on_gesture(&Gesture::Cancel(Point::new(600, 50))));
    }

    #[test]
    fn disabled_buttons_ignore_touches() {
        let theme = Theme::for_dpi(300);
        let mut b = Button::new("Disabled", ButtonKind::Secondary).disabled();
        b.place(Rect::new(0, 0, 400, 118), &theme);
        b.on_gesture(&Gesture::Down(Point::new(200, 50)));
        assert!(!b.is_pressed());
        assert!(!b.on_gesture(&Gesture::Tap(Point::new(200, 50))));
    }

    #[test]
    fn small_buttons_get_a_full_size_touch_target() {
        let theme = Theme::for_dpi(300);
        let mut b = Button::new("Ok", ButtonKind::Secondary);
        b.place(Rect::new(100, 100, 60, 40), &theme);
        // 40 px tall, but a finger 30 px below the top edge still hits it.
        b.on_gesture(&Gesture::Down(Point::new(130, 70)));
        assert!(b.on_gesture(&Gesture::Tap(Point::new(130, 70))));
    }
}
