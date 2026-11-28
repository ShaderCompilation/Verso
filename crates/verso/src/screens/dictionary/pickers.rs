//! What the lookup card's online tabs choose with: a drop-down button, and
//! the list of choices that takes the card's place until one is picked.
use crate::{
    geom::Rect,
    gfx::{HAlign, Icon},
    platform::RefreshMode,
    ui::{
        gesture::Gesture,
        theme::{INK, INK_FILL, INK2, PAPER, Theme},
        widget::{DrawCx, Press, PressEvent},
        widgets::{Button, ButtonKind, List, ListEvent, Pager, Row, button::fit_label},
    },
};

/// An outlined button with a label, a quiet note and a chevron.
#[derive(Default)]
pub struct DropDown {
    rect: Rect,
    press: Press,
}

impl DropDown {
    /// An empty rectangle hides it.
    pub fn place(&mut self, rect: Rect) {
        self.rect = rect;
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// True when it was tapped (or long-pressed and released).
    pub fn on_gesture(&mut self, g: &Gesture) -> bool {
        matches!(self.press.on_gesture(g, self.rect), Some(PressEvent::Tap | PressEvent::LongPressEnd))
    }

    pub fn draw(&self, cx: &mut DrawCx, label: &str, note: &str) {
        let t = cx.theme;
        let r = self.rect;
        if r.is_empty() {
            return;
        }
        let (fg, quiet) = if self.press.is_down() {
            cx.canvas.fill_rounded_rect(r, t.radius, INK_FILL);
            cx.hint(r, RefreshMode::Fast);
            (PAPER, PAPER)
        } else {
            cx.canvas.stroke_rounded_rect(r, t.radius, t.border as f32, INK);
            (INK, INK2)
        };
        let (pad, icon) = (t.mm(2.2), t.icon_small);
        let chevron = Rect::new(r.right() - pad - icon, r.y, icon, r.h);
        cx.icon(Icon::ChevronDown, chevron, icon, quiet);
        let mut right = chevron.x - t.mm(1.0);
        if !note.is_empty() {
            let note_style = t.small.gray(quiet);
            let note_w = (cx.text.width(note, note_style).ceil() as i32).min(r.w / 2);
            let note_rect = Rect::new(right - note_w, r.y, note_w + 1, r.h);
            cx.line(note, note_style, note_rect, HAlign::Right);
            right = note_rect.x - t.mm(1.0);
        }
        let left = r.x + pad;
        let label_rect = Rect::new(left, r.y, (right - left).max(0), r.h);
        // The label shrinks a little before it's cut short.
        let (style, _) = fit_label(cx.text, label, t.button, label_rect.w + 2 * t.mm(1.0), t);
        cx.line(label, style.gray(fg), label_rect, HAlign::Left);
    }
}

pub enum Chosen {
    /// Nothing: Back was tapped.
    Back,
    Row(usize),
}

/// Choices in a paged list, with Back below.
pub struct Chooser {
    list: List,
    pager: Pager,
    back: Button,
}

impl Chooser {
    pub fn new(rows: Vec<Row>) -> Self {
        Self { list: List::new(rows), pager: Pager::new(1), back: Button::new("Back", ButtonKind::Secondary) }
    }

    pub fn place(&mut self, area: Rect, t: &Theme) {
        // Long lists of single lines are dense, so they take fewer pages.
        let row = if self.list.rows.iter().any(|r| r.sub.is_some()) { t.row2 } else { t.target };
        self.back.place(Rect::new(area.x, area.bottom() - t.target, area.w, t.target), t);
        let full = Rect { h: self.back.rect().y - t.mm(1.0) - area.y, ..area };
        self.list.place(full, row, 0);
        if self.list.paging().pages() > 1 {
            let pager = Rect::new(area.x, full.bottom() - t.mm(7.0), area.w, t.mm(7.0));
            self.pager.place(pager, t);
            self.list.place(Rect { h: pager.y - t.mm(1.0) - area.y, ..area }, row, 0);
        }
        self.pager.set(self.list.page, self.list.paging().pages());
    }

    pub fn on_gesture(&mut self, g: &Gesture) -> Option<Chosen> {
        if self.back.on_gesture(g) {
            return Some(Chosen::Back);
        }
        if self.pager.visible() {
            self.pager.on_gesture(g);
        }
        if let Gesture::Swipe { dir, .. } = g {
            self.pager.on_swipe(*dir);
        }
        self.list.page = self.pager.page;
        match self.list.on_gesture(g) {
            Some(ListEvent::Tapped(i)) => Some(Chosen::Row(i)),
            _ => None,
        }
    }

    pub fn draw(&mut self, cx: &mut DrawCx) {
        self.list.draw(cx);
        self.pager.draw(cx);
        self.back.draw(cx);
    }
}

#[cfg(test)]
impl Chooser {
    pub fn rows(&self) -> &[Row] {
        &self.list.rows
    }

    /// The middle of the visible row `i`.
    pub fn row_point(&self, i: usize) -> crate::geom::Point {
        self.list.row_rect(i).center()
    }

    /// The visible rows and the buttons.
    pub fn targets(&self) -> Vec<Rect> {
        let rows = self.list.paging().range(self.list.page).len();
        let mut targets: Vec<Rect> = (0..rows).map(|i| self.list.row_rect(i)).collect();
        if self.pager.visible() {
            targets.push(self.pager.rect());
        }
        targets.push(self.back.rect());
        targets
    }
}
