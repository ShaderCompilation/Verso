//! A short message in a black pill near the bottom of the screen. It goes
//! away by itself after a few seconds, or when tapped.

use std::time::{Duration, Instant};

use crate::geom::Rect;
use crate::gfx::{HAlign, Icon};
use crate::ui::theme::{INK_FILL, PAPER, Theme};
use crate::ui::widget::DrawCx;

pub const TOAST_TIME: Duration = Duration::from_millis(2500);

pub struct Toast {
    pub text: String,
    pub icon: Option<Icon>,
    pub until: Instant,
}

impl Toast {
    pub fn new(text: impl Into<String>, icon: Option<Icon>, now: Instant) -> Self {
        Self { text: text.into(), icon, until: now + TOAST_TIME }
    }

    /// Where the pill goes for this text, its bottom edge at `bottom`.
    pub fn rect(&self, text_w: i32, screen: Rect, bottom: i32, theme: &Theme) -> Rect {
        let icon_w = if self.icon.is_some() { theme.icon_small + theme.mm(2.0) } else { 0 };
        let w = (text_w + icon_w + theme.mm(12.0)).min(screen.w - 2 * theme.margin);
        let h = theme.mm(10.0);
        Rect::new(screen.center().x - w / 2, bottom - h, w, h)
    }

    /// Draws the toast; returns where it is.
    pub fn draw(&self, cx: &mut DrawCx) -> Rect {
        let bottom = cx.bounds().bottom() - cx.theme.mm(22.0);
        self.draw_above(cx, bottom)
    }

    /// Draws the pill with its bottom edge at `bottom`; returns where it is.
    pub fn draw_above(&self, cx: &mut DrawCx, bottom: i32) -> Rect {
        let t = cx.theme;
        let style = t.body.gray(PAPER);
        let text_w = cx.text.width(&self.text, style).ceil() as i32;
        let r = self.rect(text_w, cx.bounds(), bottom, t);
        cx.canvas.fill_rounded_rect(r, r.h as f32 / 2.0, INK_FILL);
        let mut inner = r.inset_xy(t.mm(6.0), 0);
        if let Some(icon) = self.icon {
            let (icon_rect, rest) = inner.split_left(t.icon_small);
            cx.icon(icon, icon_rect, t.icon_small, PAPER);
            inner = rest.split_left(t.mm(2.0)).1;
        }
        cx.line(&self.text, style, inner, HAlign::Center);
        r
    }
}
