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
    rows: usize,
    pressed: Option<usize>,
    press: Press,
    /// A touch started outside the card.
    outside: bool,
    enabled: Vec<bool>,
    /// The current choice, marked with a check at the right.
    checked: Option<usize>,
}

impl PopupMenu {
    /// Opens the menu just below `anchor`, or above it if there's no room.
    /// If it fits neither (a short landscape screen), it opens beside the
    /// anchor instead. Always kept inside the screen margins.
    pub fn open(items: Vec<(Option<Icon>, String)>, anchor: Point, screen: Rect, theme: &Theme) -> Self {
        let m = theme.margin;
        let gap = theme.mm(2.0);
        let available_h = screen.h - 2 * m - 2 * theme.mm(1.0);
        let columns = if available_h / (items.len().max(1) as i32) < theme.target { 2 } else { 1 };
        let rows = items.len().max(1).div_ceil(columns);
        let w = theme.mm(70.0 * columns as f32).min(screen.w - 2 * m);
        let row_h = theme.row.min(available_h / rows as i32);
        let h = row_h * rows as i32 + 2 * theme.mm(1.0);
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
            rows,
            pressed: None,
            press: Press::default(),
            outside: false,
            enabled,
            checked: None,
        }
    }

    /// Marks `item` as the current choice.
    pub fn set_checked(&mut self, item: Option<usize>) {
        self.checked = item;
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

    fn columns(&self) -> usize {
        self.items.len().max(1).div_ceil(self.rows)
    }
    /// Which of item `k`'s corners (top-left, top-right, bottom-left,
    /// bottom-right) lie in a corner of the card. Items fill column by column.
    fn card_corners(&self, k: usize) -> [bool; 4] {
        let (row, column) = (k % self.rows, k / self.rows);
        let (first, last) = (column == 0, column + 1 == self.columns());
        let (top, bottom) = (row == 0, row + 1 == self.rows);
        [top && first, top && last, bottom && first, bottom && last]
    }

    /// Where each item is, for routing taps (and tests).
    pub fn item_rects(&self, theme: &Theme) -> Vec<Rect> {
        let top = self.rect.y + theme.mm(1.0);
        let columns = self.columns();
        let width = (self.rect.w - 2 * theme.border) / columns as i32;
        (0..self.items.len())
            .map(|k| {
                Rect::new(
                    self.rect.x + theme.border + (k / self.rows) as i32 * width,
                    top + (k % self.rows) as i32 * self.row_h,
                    width,
                    self.row_h,
                )
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
        for (k, (item, ir)) in self.items.iter().zip(self.item_rects(t)).enumerate() {
            let fg = if !self.enabled[k] {
                crate::ui::theme::INK3
            } else if self.pressed == Some(k) && self.press.is_down() {
                // Round only the corners that sit in a corner of the card.
                cx.canvas.fill_rounded_rect(ir, t.radius_sheet - t.border as f32, INK);
                let [top_left, top_right, bottom_left, bottom_right] = self.card_corners(k);
                let c = (t.radius_sheet.ceil() as i32).min(ir.w / 2).min(ir.h / 2);
                for (rounded, x, y) in [
                    (top_left, ir.x, ir.y),
                    (top_right, ir.right() - c, ir.y),
                    (bottom_left, ir.x, ir.bottom() - c),
                    (bottom_right, ir.right() - c, ir.bottom() - c),
                ] {
                    if !rounded {
                        cx.fill(Rect::new(x, y, c, c), INK);
                    }
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
            let mut right = ir.right() - t.mm(3.0);
            if self.checked == Some(k) {
                cx.icon(Icon::Check, Rect::new(right - t.icon_small, ir.y, t.icon_small, ir.h), t.icon_small, fg);
                right -= t.icon_small + t.mm(2.0);
            }
            let text = Rect::new(text_x, ir.y, right - text_x, ir.h);
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
    fn eight_actions_keep_touch_targets_and_route_both_columns() {
        for (w, h, dpi) in [(1272, 1696, 300), (1696, 1272, 300), (600, 800, 167), (800, 600, 167)] {
            let t = Theme::for_dpi(dpi);
            let area = Rect::new(0, t.top_bar, w, h - t.top_bar);
            let mut menu = PopupMenu::open(vec![(None, "Action".into()); 8], area.center(), area, &t);
            assert!(menu.row_h >= t.target, "{w}x{h}: {} < {}", menu.row_h, t.target);
            assert!(menu.rect().y >= area.y && menu.rect().bottom() <= area.bottom() - t.margin);
            for (i, rect) in menu.item_rects(&t).into_iter().enumerate() {
                assert!(rect.w >= t.target);
                let p = rect.center();
                menu.on_gesture(&Gesture::Down(p), &t);
                assert_eq!(menu.on_gesture(&Gesture::Tap(p), &t), Some(MenuEvent::Chosen(i)));
                menu.set_enabled(i, false);
                menu.on_gesture(&Gesture::Down(p), &t);
                assert_eq!(menu.on_gesture(&Gesture::Tap(p), &t), None);
            }
        }
    }

    #[test]
    fn pressed_items_round_only_the_cards_corners() {
        let t = Theme::for_dpi(300);
        let mut menu =
            PopupMenu::open(vec![(None, "Action".into()); 3], Point::new(600, 400), Rect::new(0, 0, 1272, 1696), &t);
        assert_eq!(menu.card_corners(0), [true, true, false, false]);
        assert_eq!(menu.card_corners(1), [false; 4]);
        assert_eq!(menu.card_corners(2), [false, false, true, true]);
        // Two columns of four: corners go to the outer items of each column.
        menu.items = vec![(None, "Action".into()); 8];
        menu.rows = 4;
        let corners: Vec<_> = (0..8).map(|k| menu.card_corners(k)).collect();
        assert_eq!(corners[0], [true, false, false, false]);
        assert_eq!(corners[3], [false, false, true, false]);
        assert_eq!(corners[4], [false, true, false, false]);
        assert_eq!(corners[7], [false, false, false, true]);
        assert!([1, 2, 5, 6].iter().all(|&k| corners[k] == [false; 4]));
        // A shorter last column leaves the bottom-right corner empty.
        menu.items.pop();
        assert_eq!(menu.card_corners(6), [false; 4]);
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
