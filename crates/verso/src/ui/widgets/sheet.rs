//! A panel that comes up from the bottom of the screen with a title and a
//! close button. What's on it belongs to the screen that opened it; the screen
//! passes touches to the sheet first while it's open.

use crate::geom::{Point, Rect};
use crate::gfx::{HAlign, Icon};
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK, PAPER, Theme};
use crate::ui::widget::DrawCx;
use crate::ui::widgets::button::IconButton;

pub struct Sheet {
    pub title: String,
    /// The visible panel.
    rect: Rect,
    close: IconButton,
    /// A touch started above the sheet.
    outside: bool,
}

impl Sheet {
    /// A sheet `height` pixels tall at the bottom of `screen`.
    pub fn open(title: impl Into<String>, height: i32, screen: Rect, theme: &Theme) -> Self {
        let rect = screen.split_bottom(height).0;
        let mut close = IconButton::new(Icon::Close);
        close.place_at(Point::new(rect.right() - theme.margin - theme.mm(2.0), rect.y + theme.mm(6.5)), theme);
        Self { title: title.into(), rect, close, outside: false }
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Where the sheet's contents go: below the title row.
    pub fn content(&self, theme: &Theme) -> Rect {
        self.rect.split_top(theme.mm(13.0)).1.inset_xy(theme.margin, 0)
    }

    /// True when the sheet should close: × tapped, or a tap above the sheet.
    pub fn on_gesture(&mut self, g: &Gesture) -> bool {
        if let Gesture::Down(p) = *g {
            self.outside = !self.rect.contains(p);
        }
        if self.outside {
            return matches!(g, Gesture::Tap(_));
        }
        self.close.on_gesture(g)
    }

    pub fn draw(&self, cx: &mut DrawCx) {
        let t = cx.theme;
        // Extend below the screen so only the top corners are rounded.
        let panel = Rect::new(self.rect.x, self.rect.y, self.rect.w, self.rect.h + t.radius_sheet as i32 * 2);
        cx.canvas.fill_rounded_rect(panel, t.radius_sheet, PAPER);
        cx.canvas.stroke_rounded_rect(panel, t.radius_sheet, t.border as f32, INK);
        let title =
            Rect::new(self.rect.x + t.margin, self.rect.y, self.close.rect().x - self.rect.x - t.margin, t.mm(13.0));
        cx.line(&self.title, t.heading, title, HAlign::Left);
        self.close.draw(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closes_on_x_or_tap_above() {
        let theme = Theme::for_dpi(300);
        let mut s = Sheet::open("Text", 800, Rect::new(0, 0, 1272, 1696), &theme);
        assert_eq!(s.rect(), Rect::new(0, 896, 1272, 800));
        let above = Point::new(600, 300);
        s.on_gesture(&Gesture::Down(above));
        assert!(s.on_gesture(&Gesture::Tap(above)));
        let x = s.close.rect().center();
        s.on_gesture(&Gesture::Down(x));
        assert!(s.on_gesture(&Gesture::Tap(x)));
        let inside = Point::new(600, 1400);
        s.on_gesture(&Gesture::Down(inside));
        assert!(!s.on_gesture(&Gesture::Tap(inside)));
        assert!(s.content(&theme).y > s.rect().y);
    }
}
