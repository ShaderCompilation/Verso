//! The bar at the top of a screen: back arrow, title, and a line under it.

use crate::geom::{Point, Rect};
use crate::gfx::{HAlign, Icon};
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK, Theme};
use crate::ui::widget::DrawCx;
use crate::ui::widgets::button::IconButton;

pub struct TopBar {
    pub title: String,
    /// Literata title (screen titles) or Inter SemiBold (book titles in menus).
    pub serif: bool,
    pub border: bool,
    pub show_battery: bool,
    /// Width kept clear at the right end for the caller's own controls.
    pub trailing: i32,
    back: Option<IconButton>,
    rect: Rect,
}

impl TopBar {
    pub fn new(title: impl Into<String>, back: bool) -> Self {
        Self {
            title: title.into(),
            serif: true,
            border: true,
            show_battery: false,
            trailing: 0,
            back: back.then(|| IconButton::new(Icon::ArrowLeft)),
            rect: Rect::default(),
        }
    }

    /// Places the bar across the top of `screen`.
    pub fn place(&mut self, screen: Rect, theme: &Theme) {
        self.rect = screen.split_top(theme.top_bar).0;
        if let Some(b) = &mut self.back {
            b.place_at(
                Point::new(screen.x + theme.margin + theme.target / 2 - theme.mm(2.0), self.rect.center().y),
                theme,
            );
        }
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    pub fn is_pressed(&self) -> bool {
        self.back.as_ref().is_some_and(IconButton::is_pressed)
    }

    /// True when the back arrow was tapped.
    pub fn on_gesture(&mut self, g: &Gesture) -> bool {
        self.back.as_mut().is_some_and(|b| b.on_gesture(g))
    }

    pub fn draw(&self, cx: &mut DrawCx) {
        let t = cx.theme;
        let x = match &self.back {
            Some(b) => {
                b.draw(cx);
                self.rect.x + t.margin + t.target - t.mm(1.0)
            }
            None => self.rect.x + t.margin,
        };
        let style = if self.serif { t.title } else { t.button };
        let battery_w = if self.show_battery && cx.battery.is_some() { t.mm(32.0) } else { 0 }.max(self.trailing);
        let title = Rect::new(x, self.rect.y, self.rect.right() - t.margin - x - battery_w, self.rect.h);
        cx.line(&self.title, style, title, HAlign::Left);
        if self.show_battery {
            cx.battery(self.rect.inset_xy(t.margin, 0));
        }
        if self.border {
            cx.hline(self.rect.bottom(), self.rect.x, self.rect.right(), t.border, INK);
        }
    }
}
