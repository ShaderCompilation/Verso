//! A short message in a black pill near the bottom of the screen. It goes
//! away by itself after a few seconds, or when tapped.

use std::time::{Duration, Instant};

use crate::geom::Rect;
use crate::gfx::{HAlign, Icon};
use crate::ui::theme::{INK, PAPER, Theme};
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

    /// Where the pill goes for this text.
    pub fn rect(&self, text_w: i32, screen: Rect, theme: &Theme) -> Rect {
        let icon_w = if self.icon.is_some() { theme.icon_small + theme.mm(2.0) } else { 0 };
        let w = (text_w + icon_w + theme.mm(12.0)).min(screen.w - 2 * theme.margin);
        let h = theme.mm(10.0);
        Rect::new(screen.center().x - w / 2, screen.bottom() - theme.mm(22.0) - h, w, h)
    }

    /// Draws the toast; returns where it is.
    pub fn draw(&self, cx: &mut DrawCx) -> Rect {
        let t = cx.theme;
        let style = t.body.gray(PAPER);
        let text_w = cx.text.width(&self.text, style).ceil() as i32;
        let r = self.rect(text_w, cx.bounds(), t);
        cx.canvas.fill_rounded_rect(r, r.h as f32 / 2.0, INK);
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
