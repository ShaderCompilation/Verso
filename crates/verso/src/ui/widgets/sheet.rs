//! A panel that comes up from the bottom of the screen (or comes down from
//! the top, or, in landscape, stands at one side) with a title and a close
//! button. What's on it belongs to the screen that opened it; the screen
//! passes touches to the sheet first while it's open. A headerless panel
//! keeps only the shell (border, corners, tap-outside closing) for screens
//! that draw their own header, like Lookup.

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
    /// None for a headerless panel.
    close: Option<IconButton>,
    /// A touch started outside the sheet.
    outside: bool,
}

impl Sheet {
    /// A sheet `height` pixels tall at the bottom of `screen`.
    pub fn open(title: impl Into<String>, height: i32, screen: Rect, theme: &Theme) -> Self {
        Self::at(title, screen.split_bottom(height).0, screen, theme)
    }

    /// A sheet `height` pixels tall at the top of `screen`, flush with its
    /// top, left and right edges: like a bottom sheet turned over, its border
    /// shows only along the free bottom edge. For content that must leave
    /// something in the lower half visible.
    pub fn top(title: impl Into<String>, height: i32, screen: Rect, theme: &Theme) -> Self {
        Self::at(title, screen.split_top(height).0, screen, theme)
    }

    /// A full-height panel `width` pixels wide at the left or right edge of
    /// `screen`, for landscape, where a bottom sheet would be too short.
    pub fn side(title: impl Into<String>, width: i32, right: bool, screen: Rect, theme: &Theme) -> Self {
        let rect = if right { screen.split_right(width).0 } else { screen.split_left(width).0 };
        Self::at(title, rect, screen, theme)
    }

    /// Only the shell at `rect`: no title row and no ×. The screen draws its
    /// own header inside `content()` and closes it itself; the sheet still
    /// closes on a tap outside it.
    pub fn panel(rect: Rect, screen: Rect) -> Self {
        Self { title: String::new(), rect, screen, close: None, outside: false }
    }

    fn at(title: impl Into<String>, rect: Rect, screen: Rect, theme: &Theme) -> Self {
        let mut close = IconButton::new(Icon::Close);
        close.place_at(Point::new(rect.right() - theme.margin - theme.mm(2.0), rect.y + theme.mm(6.5)), theme);
        Self { title: title.into(), rect, screen, close: Some(close), outside: false }
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Where the sheet's contents go: below the title row, if there is one.
    pub fn content(&self, theme: &Theme) -> Rect {
        let header = if self.close.is_some() { theme.mm(13.0) } else { 0 };
        self.rect.split_top(header).1.inset_xy(theme.margin, 0)
    }

    /// True when the sheet should close: × tapped, or a tap outside the sheet.
    pub fn on_gesture(&mut self, g: &Gesture) -> bool {
        if let Gesture::Down(p) = *g {
            self.outside = !self.rect.contains(p);
        }
        if self.outside {
            return matches!(g, Gesture::Tap(_));
        }
        self.close.as_mut().is_some_and(|close| close.on_gesture(g))
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
        if let Some(close) = &self.close {
            let title =
                Rect::new(self.rect.x + t.margin, self.rect.y, close.rect().x - self.rect.x - t.margin, t.mm(13.0));
            cx.line(&self.title, t.heading, title, HAlign::Left);
            close.draw(cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::{Canvas, IconRenderer, TextRenderer};

    const SCREEN: Rect = Rect::new(0, 0, 1272, 1696);

    fn draw(sheet: &Sheet, theme: &Theme) -> Canvas {
        let (mut canvas, mut text, mut icons) = (Canvas::new(1272, 1696), TextRenderer::new(), IconRenderer::default());
        canvas.fill(0x80);
        sheet.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, theme, &mut Vec::new()));
        canvas
    }

    #[test]
    fn closes_on_x_or_tap_above() {
        let theme = Theme::for_dpi(300);
        let mut s = Sheet::open("Text", 800, SCREEN, &theme);
        assert_eq!(s.rect(), Rect::new(0, 896, 1272, 800));
        let above = Point::new(600, 300);
        s.on_gesture(&Gesture::Down(above));
        assert!(s.on_gesture(&Gesture::Tap(above)));
        let x = s.close.as_ref().unwrap().rect().center();
        s.on_gesture(&Gesture::Down(x));
        assert!(s.on_gesture(&Gesture::Tap(x)));
        let inside = Point::new(600, 1400);
        s.on_gesture(&Gesture::Down(inside));
        assert!(!s.on_gesture(&Gesture::Tap(inside)));
        assert!(s.content(&theme).y > s.rect().y);
    }

    #[test]
    fn top_sheet_is_flush_with_the_top_and_borders_only_its_bottom_edge() {
        let theme = Theme::for_dpi(300);
        let mut s = Sheet::top("Dictionary", 780, SCREEN, &theme);
        assert_eq!(s.rect(), Rect::new(0, 0, 1272, 780));
        let x = s.close.as_ref().unwrap().rect();
        assert!(s.rect().contains(x.center()) && x.y < theme.mm(13.0));
        s.on_gesture(&Gesture::Down(x.center()));
        assert!(s.on_gesture(&Gesture::Tap(x.center())));
        let below = Point::new(600, 1200);
        s.on_gesture(&Gesture::Down(below));
        assert!(s.on_gesture(&Gesture::Tap(below)));
        let inside = Point::new(600, 500);
        s.on_gesture(&Gesture::Down(inside));
        assert!(!s.on_gesture(&Gesture::Tap(inside)));
        let content = s.content(&theme);
        assert_eq!((content.y, content.bottom()), (theme.mm(13.0), 780));

        let canvas = draw(&s, &theme);
        let at = |x: i32, y: i32| canvas.gray_data()[(y * 1272 + x) as usize];
        // The border and corners at the screen edges run off the screen, so
        // only the free bottom edge has a border, right across.
        assert_eq!((at(0, 0), at(636, 0), at(1271, 0)), (PAPER, PAPER, PAPER));
        assert_eq!((at(0, 400), at(1271, 400)), (PAPER, PAPER));
        assert_eq!((at(0, 778), at(636, 778), at(1271, 778)), (INK, INK, INK));
        assert_eq!(at(636, 780), 0x80, "nothing drawn below the sheet");
    }

    #[test]
    fn headerless_panel_keeps_the_shell_and_tap_outside_closing() {
        let theme = Theme::for_dpi(300);
        let rect = Rect::new(0, 916, 1272, 780);
        let mut s = Sheet::panel(rect, SCREEN);
        assert_eq!(s.rect(), rect);
        assert_eq!(s.content(&theme), rect.inset_xy(theme.margin, 0));
        let outside = Point::new(600, 300);
        s.on_gesture(&Gesture::Down(outside));
        assert!(s.on_gesture(&Gesture::Tap(outside)));
        // No ×: touches anywhere inside, even its usual place, stay with the screen.
        for p in [Point::new(1272 - theme.margin - theme.mm(2.0), 916 + theme.mm(6.5)), Point::new(600, 1400)] {
            s.on_gesture(&Gesture::Down(p));
            assert!(!s.on_gesture(&Gesture::Tap(p)));
        }
        let canvas = draw(&s, &theme);
        let at = |x: i32, y: i32| canvas.gray_data()[(y * 1272 + x) as usize];
        assert_eq!(at(636, 915), 0x80, "nothing drawn above the panel");
        assert_eq!((at(0, 917), at(636, 917), at(1271, 917)), (INK, INK, INK), "top border");
        assert_eq!((at(0, 1300), at(636, 1695)), (PAPER, PAPER), "no border at the screen edges");
        // Nothing but paper inside: no title and no × are drawn.
        let inner = rect.inset(theme.border + 2);
        let inner = Rect::new(inner.x + theme.mm(3.0), inner.y, inner.w - 2 * theme.mm(3.0), theme.mm(13.0));
        for y in inner.y..inner.bottom() {
            for x in inner.x..inner.right() {
                assert_eq!(at(x, y), PAPER);
            }
        }
    }
}
