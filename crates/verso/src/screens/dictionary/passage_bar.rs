//! The floating bar beside a selected passage.
use super::tabs::too_long;
use crate::{
    geom::Rect,
    gfx::{HAlign, Icon, Weight},
    platform::RefreshMode,
    ui::{
        gesture::Gesture,
        theme::{INK, INK3, LINE, PAPER, Theme},
        widget::{DrawCx, Press, PressEvent},
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionAction {
    Dismiss,
    Dictionary,
    Wikipedia,
    Translate,
    Search,
    Highlight,
    Note,
    Extend,
}

/// The bar's cells, left to right; the first is the primary action.
const SELECTION_ACTIONS: [(Icon, &str, SelectionAction); 7] = [
    (Icon::Highlighter, "Highlight", SelectionAction::Highlight),
    (Icon::Pencil, "Note", SelectionAction::Note),
    (Icon::Search, "Search", SelectionAction::Search),
    (Icon::BookOpen, "Dict", SelectionAction::Dictionary),
    (Icon::Globe, "Wiki", SelectionAction::Wikipedia),
    (Icon::Transfer, "Translate", SelectionAction::Translate),
    (Icon::Extend, "Extend", SelectionAction::Extend),
];

const CELLS: usize = SELECTION_ACTIONS.len();

/// The actions for a selected passage: a floating card of seven cells beside
/// the selection, in the pop-up menu's style, leaving the page open.
pub struct SelectionActions {
    rect: Rect,
    cells: [Rect; CELLS],
    /// The passage is too long for Wikipedia: Wiki is gray and ignores taps.
    long: bool,
    pressed: Option<usize>,
    press: Press,
    /// A touch started outside the card.
    outside: bool,
}
impl SelectionActions {
    /// Below `selection` (the passage's bounding box) if there's room,
    /// otherwise above it, leaving room for the handles; a passage that
    /// fills the page gets the bar at the bottom margin.
    pub fn new(t: &Theme, screen: Rect, selection: Rect, text: &str) -> Self {
        let m = t.margin;
        let w = (screen.w - 2 * m).min(t.mm(115.0));
        let h = t.mm(11.5);
        let clearance = t.mm(4.5);
        let x = (selection.center().x - w / 2).clamp(screen.x + m, screen.right() - m - w);
        let (below, above) = (selection.bottom() + clearance, selection.y - clearance - h);
        let y = if below + h <= screen.bottom() - m {
            below
        } else if above >= screen.y + m {
            above
        } else {
            screen.bottom() - m - h
        };
        let rect = Rect::new(x, y, w, h);
        let inner = rect.inset(t.border);
        let cell_w = inner.w / CELLS as i32;
        let cells = std::array::from_fn(|i| {
            let x = inner.x + i as i32 * cell_w;
            // The last cell takes the rounding remainder.
            let w = if i + 1 == CELLS { inner.right() - x } else { cell_w };
            Rect::new(x, inner.y, w, inner.h)
        });
        Self { rect, cells, long: too_long(text), pressed: None, press: Press::default(), outside: false }
    }
    #[cfg(test)]
    pub(crate) fn rect(&self) -> Rect {
        self.rect
    }
    fn enabled(&self, i: usize) -> bool {
        !self.long || SELECTION_ACTIONS[i].2 != SelectionAction::Wikipedia
    }
    /// The area the bar covers, including its shadow.
    pub fn bounds(&self, t: &Theme) -> Rect {
        self.rect.union(&self.rect.moved(t.mm(1.0), t.mm(1.0)))
    }
    /// Where each action is, for tests.
    #[cfg(test)]
    pub(crate) fn cell(&self, action: SelectionAction) -> Rect {
        let i = SELECTION_ACTIONS.iter().position(|(_, _, a)| *a == action).expect("an action of the bar");
        self.cells[i]
    }
    fn radius(t: &Theme) -> f32 {
        t.mm(2.2) as f32
    }
    pub fn visual_state(&self) -> Option<usize> {
        self.pressed.filter(|_| self.press.is_down())
    }
    pub fn on_gesture(&mut self, g: &Gesture) -> Option<SelectionAction> {
        if let Gesture::Down(p) = *g {
            self.outside = !self.rect.contains(p);
            self.pressed = self.cells.iter().position(|r| r.contains(p)).filter(|&i| self.enabled(i));
        }
        if self.outside {
            // Any touch that starts outside dismisses the selection when it ends.
            return g.ends_touch().then_some(SelectionAction::Dismiss);
        }
        let i = self.pressed?;
        let event = match self.press.on_gesture(g, self.cells[i]) {
            Some(PressEvent::Tap | PressEvent::LongPressEnd) => Some(SELECTION_ACTIONS[i].2),
            _ => None,
        };
        if !self.press.is_down() {
            self.pressed = None;
        }
        event
    }
    /// Fills a cell, rounding only the corners that lie in the card's.
    fn fill_cell(cx: &mut DrawCx, i: usize, r: Rect, gray: u8) {
        let t = cx.theme;
        if i != 0 && i + 1 != CELLS {
            cx.fill(r, gray);
            return;
        }
        let radius = Self::radius(t) - t.border as f32;
        cx.canvas.fill_rounded_rect(r, radius, gray);
        let c = (radius.ceil() as i32).min(r.w / 2).min(r.h / 2);
        let x = if i == 0 { r.right() - c } else { r.x };
        cx.fill(Rect::new(x, r.y, c, c), gray);
        cx.fill(Rect::new(x, r.bottom() - c, c, c), gray);
    }
    pub fn draw(&self, cx: &mut DrawCx) {
        let t = cx.theme;
        let (r, radius) = (self.rect, Self::radius(t));
        cx.canvas.fill_rounded_rect(r.moved(t.mm(1.0), t.mm(1.0)), radius, cx.shadow_gray());
        cx.canvas.fill_rounded_rect(r, radius, PAPER);
        let label_style = t.caption.weight(Weight::SemiBold);
        let (icon, gap) = (t.icon_small, t.mm(0.9));
        let label_h = label_style.line_height().ceil() as i32;
        for (i, ((glyph, label, _), cell)) in SELECTION_ACTIONS.iter().zip(self.cells).enumerate() {
            let primary = i == 0;
            let pressed = self.pressed == Some(i) && self.press.is_down();
            // Pressed cells invert, like buttons.
            let fg = if primary != pressed {
                Self::fill_cell(cx, i, cell, INK);
                PAPER
            } else if self.enabled(i) {
                INK
            } else {
                INK3
            };
            if pressed {
                cx.hint(cell, RefreshMode::Fast);
            }
            if i > 1 {
                cx.fill(Rect::new(cell.x - t.hairline / 2, cell.y, t.hairline, cell.h), LINE);
            }
            let top = cell.y + (cell.h - icon - gap - label_h) / 2;
            cx.icon(*glyph, Rect::new(cell.x, top, cell.w, icon), icon, fg);
            let text = Rect::new(cell.x + t.mm(0.5), top + icon + gap, cell.w - t.mm(1.0), label_h);
            cx.line(label, label_style.gray(fg), text, HAlign::Center);
        }
        cx.canvas.stroke_rounded_rect(r, radius, t.border as f32, INK);
    }
}
