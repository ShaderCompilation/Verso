//! A small menu that pops up next to what was long-pressed: a white card with
//! a black border and a flat gray offset. Tap an item, or tap outside to close.

use crate::geom::{Point, Rect};
use crate::gfx::{HAlign, Icon};
use crate::platform::RefreshMode;
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK, PAPER, SHADOW, Theme};
use crate::ui::widget::{DrawCx, Press, PressEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuEvent {
    Chosen(usize),
    Dismissed,
}

pub struct PopupMenu {
    pub items: Vec<(Option<Icon>, String)>,
    rect: Rect,
    row_h: i32,
    pressed: Option<usize>,
    press: Press,
    /// A touch started outside the card.
    outside: bool,
    enabled: Vec<bool>,
}

impl PopupMenu {
    /// Opens the menu just below `anchor`, or above it if there's no room.
    /// If it fits neither (a short landscape screen), it opens beside the
    /// anchor instead. Always kept inside the screen margins.
    pub fn open(items: Vec<(Option<Icon>, String)>, anchor: Point, screen: Rect, theme: &Theme) -> Self {
        let m = theme.margin;
        let gap = theme.mm(2.0);
        let w = theme.mm(70.0).min(screen.w - 2 * m);
        let row_h = theme.row.min((screen.h - 2 * m - 2 * theme.mm(1.0)) / items.len().max(1) as i32);
        let h = row_h * items.len() as i32 + 2 * theme.mm(1.0);
        let centered = (anchor.x - w / 2).clamp(screen.x + m, screen.right() - m - w);
        let (below, above) = (anchor.y + gap, anchor.y - gap - h);
        let (x, y) = if below + h <= screen.bottom() - m {
            (centered, below)
        } else if above >= screen.y + m {
            (centered, above)
        } else {
            let right = anchor.x + gap;
            let x = if right + w <= screen.right() - m { right } else { anchor.x - gap - w };
            let y = (anchor.y - h / 2).clamp(screen.y + m, (screen.bottom() - m - h).max(screen.y + m));
            (x.clamp(screen.x + m, screen.right() - m - w), y)
        };
        let enabled = vec![true; items.len()];
        Self {
            items,
            rect: Rect::new(x, y, w, h),
            row_h,
            pressed: None,
            press: Press::default(),
            outside: false,
            enabled,
        }
    }

    pub fn set_enabled(&mut self, item: usize, enabled: bool) {
        if let Some(value) = self.enabled.get_mut(item) {
            *value = enabled;
        }
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// The area the menu covers, including its shadow.
    pub fn bounds(&self, theme: &Theme) -> Rect {
        self.rect.union(&self.rect.moved(theme.mm(1.0), theme.mm(1.0)))
    }

    fn item_rects(&self, theme: &Theme) -> Vec<Rect> {
        let top = self.rect.y + theme.mm(1.0);
        (0..self.items.len() as i32)
            .map(|k| {
                Rect::new(self.rect.x + theme.border, top + k * self.row_h, self.rect.w - 2 * theme.border, self.row_h)
            })
            .collect()
    }

    pub fn on_gesture(&mut self, g: &Gesture, theme: &Theme) -> Option<MenuEvent> {
        let rects = self.item_rects(theme);
        if let Gesture::Down(p) = *g {
            self.outside = !self.rect.contains(p);
            self.pressed = rects.iter().position(|r| r.contains(p));
        }
        if self.outside {
            // Any touch that starts outside closes the menu when it ends.
            return g.ends_touch().then_some(MenuEvent::Dismissed);
        }
        let i = self.pressed?;
        if !self.enabled[i] {
            if g.ends_touch() {
                self.pressed = None;
            }
            return None;
        }
        let event = match self.press.on_gesture(g, rects[i]) {
            Some(PressEvent::Tap | PressEvent::LongPressEnd) => Some(MenuEvent::Chosen(i)),
            _ => None,
        };
        if !self.press.is_down() {
            self.pressed = None;
        }
        event
    }

    pub fn draw(&self, cx: &mut DrawCx) {
        let t = cx.theme;
        let r = self.rect;
        cx.canvas.fill_rounded_rect(r.moved(t.mm(1.0), t.mm(1.0)), t.radius_sheet, SHADOW);
        cx.canvas.fill_rounded_rect(r, t.radius_sheet, PAPER);
        cx.canvas.stroke_rounded_rect(r, t.radius_sheet, t.border as f32, INK);
        let n = self.items.len();
        for (k, (item, ir)) in self.items.iter().zip(self.item_rects(t)).enumerate() {
            let fg = if !self.enabled[k] {
                crate::ui::theme::INK3
            } else if self.pressed == Some(k) && self.press.is_down() {
                // Follow the card's rounded corners on the first and last item.
                cx.canvas.fill_rounded_rect(ir, t.radius_sheet - t.border as f32, INK);
                if k > 0 {
                    cx.fill(ir.split_top(ir.h / 2).0, INK);
                }
                if k + 1 < n {
                    cx.fill(ir.split_bottom(ir.h / 2).0, INK);
                }
                cx.hint(ir, RefreshMode::Fast);
                PAPER
            } else {
                INK
            };
            let mut text_x = ir.x + t.mm(4.0) - t.border;
            if let Some(icon) = item.0 {
                cx.icon(icon, Rect::new(text_x, ir.y, t.icon_small, ir.h), t.icon_small, fg);
                text_x += t.icon_small + t.mm(3.0);
            }
            let text = Rect::new(text_x, ir.y, ir.right() - text_x - t.mm(3.0), ir.h);
            cx.line(&item.1, t.body.gray(fg), text, HAlign::Left);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu(theme: &Theme) -> PopupMenu {
        let items = vec![(Some(Icon::BookOpen), "Open".into()), (None, "Details".into())];
        PopupMenu::open(items, Point::new(600, 400), Rect::new(0, 0, 1272, 1696), theme)
    }

    #[test]
    fn choose_an_item() {
        let theme = Theme::for_dpi(300);
        let mut m = menu(&theme);
        let second = m.item_rects(&theme)[1].center();
        m.on_gesture(&Gesture::Down(second), &theme);
        assert_eq!(m.on_gesture(&Gesture::Tap(second), &theme), Some(MenuEvent::Chosen(1)));
    }

    #[test]
    fn tapping_outside_dismisses() {
        let theme = Theme::for_dpi(300);
        let mut m = menu(&theme);
        let outside = Point::new(10, 10);
        assert_eq!(m.on_gesture(&Gesture::Down(outside), &theme), None);
        assert_eq!(m.on_gesture(&Gesture::Tap(outside), &theme), Some(MenuEvent::Dismissed));
    }

    #[test]
    fn stays_on_screen() {
        let theme = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let items = vec![(None, "A".to_string()); 4];
        let m = PopupMenu::open(items, Point::new(1260, 1650), screen, &theme);
        assert!(m.rect().right() <= screen.right() - theme.margin);
        assert!(m.rect().bottom() < 1650, "opens above the finger when there's no room below");
    }

    #[test]
    fn opens_beside_the_finger_on_a_short_screen() {
        let theme = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1696, 1272);
        let items = vec![(None, "A".to_string()); 5];
        for finger in [Point::new(240, 700), Point::new(1500, 700)] {
            let m = PopupMenu::open(items.clone(), finger, screen, &theme);
            assert!(!m.rect().contains(finger), "the long-pressed book stays visible");
            assert!(m.rect().y >= theme.margin && m.rect().bottom() <= screen.bottom() - theme.margin);
            assert!(m.rect().x >= theme.margin && m.rect().right() <= screen.right() - theme.margin);
        }
    }
}
