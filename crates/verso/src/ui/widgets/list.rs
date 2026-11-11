//! A list of rows split into pages (no scrolling on e-ink). Rows span the
//! full width of the list; their text is inset by the screen margin.

use crate::geom::{Point, Rect};
use crate::gfx::{HAlign, Icon};
use crate::platform::RefreshMode;
use crate::ui::gesture::Gesture;
use crate::ui::layout::Paging;
use crate::ui::theme::{INK, INK2, INK3, LINE, PAPER};
use crate::ui::widget::{DrawCx, Press, PressEvent};
use crate::ui::widgets::toggle::{Toggle, draw_pill};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Accessory {
    None,
    Chevron,
    Toggle(bool),
}

#[derive(Clone, Debug)]
pub struct Row {
    pub label: String,
    /// A second, smaller line under the label.
    pub sub: Option<String>,
    /// Shown on the right, in gray.
    pub value: Option<String>,
    pub accessory: Accessory,
    pub enabled: bool,
}

impl Row {
    pub fn new(label: impl Into<String>) -> Self {
        Self { label: label.into(), sub: None, value: None, accessory: Accessory::None, enabled: true }
    }

    pub fn sub(mut self, sub: impl Into<String>) -> Self {
        self.sub = Some(sub.into());
        self
    }

    pub fn value(mut self, value: impl Into<String>) -> Self {
        self.value = Some(value.into());
        self
    }

    pub fn chevron(mut self) -> Self {
        self.accessory = Accessory::Chevron;
        self
    }

    pub fn toggle(mut self, on: bool) -> Self {
        self.accessory = Accessory::Toggle(on);
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListEvent {
    Tapped(usize),
    LongPressed(usize),
}

pub struct List {
    pub rows: Vec<Row>,
    pub page: usize,
    rect: Rect,
    row_h: i32,
    /// Text inset from the list's left and right edge.
    pad: i32,
    pressed: Option<usize>,
    press: Press,
}

impl List {
    pub fn new(rows: Vec<Row>) -> Self {
        Self { rows, page: 0, rect: Rect::default(), row_h: 1, pad: 0, pressed: None, press: Press::default() }
    }

    pub fn place(&mut self, rect: Rect, row_h: i32, pad: i32) {
        self.rect = rect;
        self.row_h = row_h.max(1);
        self.pad = pad;
        self.page = self.paging().clamp(self.page);
    }

    pub fn paging(&self) -> Paging {
        Paging::new(self.rows.len(), (self.rect.h / self.row_h).max(1) as usize)
    }

    /// The visible rows: (index, rect).
    fn visible(&self) -> impl Iterator<Item = (usize, Rect)> + '_ {
        self.paging()
            .range(self.page)
            .enumerate()
            .map(|(k, i)| (i, Rect::new(self.rect.x, self.rect.y + k as i32 * self.row_h, self.rect.w, self.row_h)))
    }

    fn row_at(&self, p: Point) -> Option<usize> {
        self.visible().find(|(i, r)| r.contains(p) && self.rows[*i].enabled).map(|(i, _)| i)
    }

    pub fn on_gesture(&mut self, g: &Gesture) -> Option<ListEvent> {
        if let Gesture::Down(p) = *g {
            self.pressed = self.row_at(p);
        }
        let i = self.pressed?;
        let area = self.visible().find(|(j, _)| *j == i).map(|(_, r)| r).unwrap_or_default();
        let event = match self.press.on_gesture(g, area) {
            Some(PressEvent::Tap) => Some(ListEvent::Tapped(i)),
            Some(PressEvent::LongPress) => Some(ListEvent::LongPressed(i)),
            _ => None,
        };
        if !self.press.is_down() {
            self.pressed = None;
        }
        event
    }

    pub fn draw(&self, cx: &mut DrawCx) {
        let t = cx.theme;
        for (i, r) in self.visible() {
            let row = &self.rows[i];
            let pressed = self.pressed == Some(i) && self.press.is_down();
            let (fg, fg2) = if pressed {
                cx.fill(r, INK);
                cx.hint(r, RefreshMode::Fast);
                (PAPER, PAPER)
            } else if row.enabled {
                (INK, INK2)
            } else {
                (INK3, INK3)
            };
            let inner = r.inset_xy(self.pad, 0);
            let mut right = inner.right();
            match row.accessory {
                Accessory::None => {}
                Accessory::Chevron => {
                    let icon = Rect::new(right - t.icon_small + t.mm(0.6), r.y, t.icon_small, r.h);
                    cx.icon(Icon::ChevronRight, icon, t.icon_small, fg2);
                    right -= t.icon_small + t.mm(1.0);
                }
                Accessory::Toggle(on) => {
                    let (w, h) = Toggle::size(t);
                    let pill = Rect::new(right - w, r.center().y - h / 2, w, h);
                    if pressed {
                        cx.canvas.fill_rounded_rect(pill.inset(-2), h as f32 / 2.0 + 2.0, PAPER);
                    }
                    draw_pill(cx, pill, on, row.enabled);
                    right -= w + t.mm(2.0);
                }
            }
            let mut label_right = right;
            if let Some(value) = &row.value {
                let w = (cx.text.width(value, t.body).ceil() as i32).min(inner.w / 2);
                cx.line(value, t.body.gray(fg2), Rect::new(right - w, r.y, w, r.h), HAlign::Right);
                label_right = right - w - t.mm(3.0);
            }
            let text = Rect::new(inner.x, r.y, label_right - inner.x, r.h);
            match &row.sub {
                None => cx.line(&row.label, t.body.gray(fg), text, HAlign::Left),
                Some(sub) => {
                    let (top, bottom) = text.split_top(r.h / 2 + t.mm(0.4));
                    cx.line(&row.label, t.body.gray(fg), top.moved(0, t.mm(1.2)), HAlign::Left);
                    cx.line(sub, t.small.gray(fg2), bottom.moved(0, -t.mm(1.0)), HAlign::Left);
                }
            }
            if !pressed {
                cx.hline(r.bottom() - t.hairline / 2 - 1, inner.x, inner.right(), t.hairline, LINE);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list() -> List {
        let rows = (0..7).map(|i| Row::new(format!("Row {i}"))).collect();
        let mut l = List::new(rows);
        l.place(Rect::new(0, 100, 600, 300), 100, 50);
        l
    }

    #[test]
    fn rows_are_paged() {
        let mut l = list();
        assert_eq!(l.paging().pages(), 3);
        l.page = 2;
        assert_eq!(l.visible().map(|(i, _)| i).collect::<Vec<_>>(), vec![6]);
    }

    #[test]
    fn tap_and_long_press_report_the_row_on_the_current_page() {
        let mut l = list();
        l.page = 1;
        let p = Point::new(300, 250); // second visible row → index 4
        l.on_gesture(&Gesture::Down(p));
        assert_eq!(l.on_gesture(&Gesture::Tap(p)), Some(ListEvent::Tapped(4)));
        l.on_gesture(&Gesture::Down(p));
        assert_eq!(l.on_gesture(&Gesture::LongPress(p)), Some(ListEvent::LongPressed(4)));
        assert_eq!(l.on_gesture(&Gesture::End(p)), None);
    }

    #[test]
    fn disabled_rows_and_gaps_dont_react() {
        let mut l = list();
        l.rows[0].enabled = false;
        let p = Point::new(300, 150);
        l.on_gesture(&Gesture::Down(p));
        assert_eq!(l.on_gesture(&Gesture::Tap(p)), None);
        l.page = 2;
        let below = Point::new(300, 250); // no row there on the last page
        l.on_gesture(&Gesture::Down(below));
        assert_eq!(l.on_gesture(&Gesture::Tap(below)), None);
    }
}
