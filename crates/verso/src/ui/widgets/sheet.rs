//! A panel that comes up from the bottom of the screen (or, in landscape,
//! can stand at one side) with a title and a close button. What's on it belongs to the screen that opened it; the screen
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
    screen: Rect,
    close: IconButton,
    /// A touch started above the sheet.
    outside: bool,
}

impl Sheet {
    /// A sheet `height` pixels tall at the bottom of `screen`.
    pub fn open(title: impl Into<String>, height: i32, screen: Rect, theme: &Theme) -> Self {
        Self::at(title, screen.split_bottom(height).0, screen, theme)
    }

    /// A full-height panel `width` pixels wide at the left or right edge of
    /// `screen`, for landscape, where a bottom sheet would be too short.
    pub fn side(title: impl Into<String>, width: i32, right: bool, screen: Rect, theme: &Theme) -> Self {
        let rect = if right { screen.split_right(width).0 } else { screen.split_left(width).0 };
        Self::at(title, rect, screen, theme)
    }

    fn at(title: impl Into<String>, rect: Rect, screen: Rect, theme: &Theme) -> Self {
        let mut close = IconButton::new(Icon::Close);
        close.place_at(Point::new(rect.right() - theme.margin - theme.mm(2.0), rect.y + theme.mm(6.5)), theme);
        Self { title: title.into(), rect, screen, close, outside: false }
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
        // Extend past the screen edges the panel touches, so only its free
        // corners are rounded (the top ones, for a bottom sheet).
        let r = self.rect;
        let extra = t.radius_sheet as i32 * 2;
        let grow = |touches: bool| if touches { extra } else { 0 };
        let (left, top) = (grow(r.x <= self.screen.x), grow(r.y <= self.screen.y));
        let (right, bottom) = (grow(r.right() >= self.screen.right()), grow(r.bottom() >= self.screen.bottom()));
        let panel = Rect::new(r.x - left, r.y - top, r.w + left + right, r.h + top + bottom);
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
