//! A list of rows split into pages (no scrolling on e-ink). Rows span the
//! full width of the list; their text is inset by the screen margin.

use crate::geom::{Point, Rect};
use crate::gfx::{HAlign, Icon, Weight};
use crate::platform::RefreshMode;
use crate::ui::gesture::Gesture;
use crate::ui::layout::Paging;
use crate::ui::theme::{INK, INK_FILL, INK2, INK3, LINE, PAPER};
use crate::ui::widget::{DrawCx, Press, PressEvent};
use crate::ui::widgets::toggle::{Toggle, draw_pill};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Accessory {
    None,
    Chevron,
    Check,
    Toggle(bool),
    /// Up and down buttons for reordering; a false side is drawn faint and
    /// ignores taps. Needs `List::slot`.
    Reorder {
        up: bool,
        down: bool,
    },
}

/// Something tappable before the label. Needs `List::slot`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lead {
    None,
    /// A filled circle with a 1-based position, or an empty circle for an
    /// item without one.
    Rank(Option<usize>),
}

#[derive(Clone, Debug)]
pub struct Row {
    pub label: String,
    /// A second, smaller line under the label.
    pub sub: Option<String>,
    /// Shown on the right, in gray.
    pub value: Option<String>,
    pub accessory: Accessory,
    pub lead: Lead,
    /// A small picture before the label (not tappable), on the label's line.
    pub icon: Option<Icon>,
    /// Grayer text for a row that is still tappable (unlike `enabled`).
    pub dim: bool,
    pub enabled: bool,
}

impl Row {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            sub: None,
            value: None,
            accessory: Accessory::None,
            lead: Lead::None,
            icon: None,
            dim: false,
            enabled: true,
        }
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

    pub fn check(mut self) -> Self {
        self.accessory = Accessory::Check;
        self
    }

    pub fn toggle(mut self, on: bool) -> Self {
        self.accessory = Accessory::Toggle(on);
        self
    }

    pub fn reorder(mut self, up: bool, down: bool) -> Self {
        self.accessory = Accessory::Reorder { up, down };
        self
    }

    pub fn rank(mut self, rank: Option<usize>) -> Self {
        self.lead = Lead::Rank(rank);
        self
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn dim(mut self) -> Self {
        self.dim = true;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListEvent {
    Tapped(usize),
    LongPressed(usize),
    /// The row's lead (e.g. its rank circle) was tapped.
    Lead(usize),
    Up(usize),
    Down(usize),
}

/// The part of a row a touch started on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Row,
    Lead,
    Up,
    Down,
}

pub struct List {
    pub rows: Vec<Row>,
    pub page: usize,
    /// Touch width of a lead or of each reorder button, at least the touch
    /// target; the lead circle and button boxes are drawn inside it.
    pub slot: i32,
    rect: Rect,
    row_h: i32,
    /// Text inset from the list's left and right edge.
    pad: i32,
    pressed: Option<(usize, Part)>,
    press: Press,
}

impl List {
    pub fn new(rows: Vec<Row>) -> Self {
        Self { rows, page: 0, slot: 0, rect: Rect::default(), row_h: 1, pad: 0, pressed: None, press: Press::default() }
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

    /// Whether even one row fits where the list is placed.
    pub fn fits(&self) -> bool {
        self.rect.h >= self.row_h
    }

    /// Where the visible row `i` is.
    #[cfg(test)]
    pub fn row_rect(&self, i: usize) -> Rect {
        Rect::new(self.rect.x, self.rect.y + i as i32 * self.row_h, self.rect.w, self.row_h)
    }

    /// The lead's touch area: from the row's left edge to a slot past the text inset.
    fn lead_rect(&self, r: Rect) -> Rect {
        Rect::new(r.x, r.y, self.pad + self.slot, r.h)
    }

    /// The up and down buttons' touch areas, at the row's right text edge.
    fn reorder_rects(&self, r: Rect) -> (Rect, Rect) {
        let right = r.right() - self.pad;
        (Rect::new(right - 2 * self.slot, r.y, self.slot, r.h), Rect::new(right - self.slot, r.y, self.slot, r.h))
    }

    /// The enabled row part under `p` and its touch area. A faint reorder
    /// button is inert, not part of the row.
    fn part_at(&self, p: Point) -> Option<(usize, Part, Rect)> {
        let (i, r) = self.visible().find(|(i, r)| r.contains(p) && self.rows[*i].enabled)?;
        let row = &self.rows[i];
        if row.lead != Lead::None && self.lead_rect(r).contains(p) {
            return Some((i, Part::Lead, self.lead_rect(r)));
        }
        if let Accessory::Reorder { up, down } = row.accessory {
            let (up_rect, down_rect) = self.reorder_rects(r);
            if up_rect.contains(p) {
                return up.then_some((i, Part::Up, up_rect));
            }
            if down_rect.contains(p) {
                return down.then_some((i, Part::Down, down_rect));
            }
        }
        Some((i, Part::Row, r))
    }

    pub fn on_gesture(&mut self, g: &Gesture) -> Option<ListEvent> {
        if let Gesture::Down(p) = *g {
            self.pressed = self.part_at(p).map(|(i, part, _)| (i, part));
        }
        let (i, part) = self.pressed?;
        let area = self
            .visible()
            .find(|(j, _)| *j == i)
            .map(|(_, r)| match part {
                Part::Row => r,
                Part::Lead => self.lead_rect(r),
                Part::Up => self.reorder_rects(r).0,
                Part::Down => self.reorder_rects(r).1,
            })
            .unwrap_or_default();
        let event = match (self.press.on_gesture(g, area), part) {
            (Some(PressEvent::Tap), Part::Row) => Some(ListEvent::Tapped(i)),
            (Some(PressEvent::LongPress), Part::Row) => Some(ListEvent::LongPressed(i)),
            (Some(PressEvent::Tap | PressEvent::LongPressEnd), Part::Lead) => Some(ListEvent::Lead(i)),
            (Some(PressEvent::Tap | PressEvent::LongPressEnd), Part::Up) => Some(ListEvent::Up(i)),
            (Some(PressEvent::Tap | PressEvent::LongPressEnd), Part::Down) => Some(ListEvent::Down(i)),
            _ => None,
        };
        if !self.press.is_down() {
            self.pressed = None;
        }
        event
    }

    pub fn is_pressed(&self, index: usize) -> bool {
        self.pressed.is_some_and(|(i, _)| i == index) && self.press.is_down()
    }

    /// Snapshot of visible state that gestures can change without emitting an event.
    pub fn visual_state(&self) -> (usize, Option<usize>, bool) {
        (self.page, self.pressed.map(|(i, _)| i), self.press.is_down())
    }

    fn pressed_part(&self, index: usize) -> Option<Part> {
        self.pressed.filter(|(i, _)| *i == index && self.press.is_down()).map(|(_, part)| part)
    }

    pub fn draw(&self, cx: &mut DrawCx) {
        let t = cx.theme;
        for (i, r) in self.visible() {
            let row = &self.rows[i];
            let pressed = self.pressed_part(i);
            let (fg, fg2) = if pressed == Some(Part::Row) {
                cx.fill(r, INK_FILL);
                cx.hint(r, RefreshMode::Fast);
                (PAPER, PAPER)
            } else if !row.enabled {
                (INK3, INK3)
            } else if row.dim {
                (INK2, INK3)
            } else {
                (INK, INK2)
            };
            let inner = r.inset_xy(self.pad, 0);
            let mut left = inner.x;
            if let Lead::Rank(rank) = row.lead {
                let d = t.mm(6.5);
                let circle = Rect::new(inner.x, r.center().y - d / 2, d, d);
                draw_rank(cx, circle, rank, row.enabled, pressed == Some(Part::Lead), pressed == Some(Part::Row));
                left = inner.x + self.slot;
            }
            let mut right = inner.right();
            match row.accessory {
                Accessory::None => {}
                Accessory::Chevron => {
                    let icon = Rect::new(right - t.icon_small + t.mm(0.6), r.y, t.icon_small, r.h);
                    cx.icon(Icon::ChevronRight, icon, t.icon_small, fg2);
                    right -= t.icon_small + t.mm(1.0);
                }
                Accessory::Check => {
                    let icon = Rect::new(right - t.icon_small, r.y, t.icon_small, r.h);
                    cx.icon(Icon::Check, icon, t.icon_small, fg);
                    right -= t.icon_small + t.mm(1.5);
                }
                Accessory::Toggle(on) => {
                    let (w, h) = Toggle::size(t);
                    let pill = Rect::new(right - w, r.center().y - h / 2, w, h);
                    if pressed == Some(Part::Row) {
                        cx.canvas.fill_rounded_rect(pill.inset(-2), h as f32 / 2.0 + 2.0, PAPER);
                    }
                    draw_pill(cx, pill, on, row.enabled);
                    right -= w + t.mm(2.0);
                }
                Accessory::Reorder { up, down } => {
                    let (up_rect, down_rect) = self.reorder_rects(r);
                    for (rect, icon, active, part) in
                        [(up_rect, Icon::ChevronUp, up, Part::Up), (down_rect, Icon::ChevronDown, down, Part::Down)]
                    {
                        let on_ink = pressed == Some(Part::Row);
                        draw_step(cx, rect, icon, active && row.enabled, pressed == Some(part), on_ink);
                    }
                    right = up_rect.x - t.mm(1.5);
                }
            }
            let mut label_right = right;
            if let Some(value) = &row.value {
                let w = (cx.text.width(value, t.body).ceil() as i32).min(inner.w / 2);
                cx.line(value, t.body.gray(fg2), Rect::new(right - w, r.y, w, r.h), HAlign::Right);
                label_right = right - w - t.mm(3.0);
            }
            let mut text = Rect::new(left, r.y, label_right - left, r.h);
            let label_line =
                if row.sub.is_some() { text.split_top(r.h / 2 + t.mm(0.4)).0.moved(0, t.mm(1.2)) } else { text };
            if let Some(icon) = row.icon {
                cx.icon(icon, Rect::new(left, label_line.y, t.icon_small, label_line.h), t.icon_small, fg);
                let shift = t.icon_small + t.mm(2.0);
                text = Rect::new(text.x + shift, text.y, text.w - shift, text.h);
            }
            match &row.sub {
                None => cx.line(&row.label, t.body.gray(fg), text, HAlign::Left),
                Some(sub) => {
                    let (top, bottom) = text.split_top(r.h / 2 + t.mm(0.4));
                    cx.line(&row.label, t.body.gray(fg), top.moved(0, t.mm(1.2)), HAlign::Left);
                    cx.line(sub, t.small.gray(fg2), bottom.moved(0, -t.mm(1.0)), HAlign::Left);
                }
            }
            if pressed != Some(Part::Row) {
                cx.hline(r.bottom() - t.hairline / 2 - 1, inner.x, inner.right(), t.hairline, LINE);
            }
        }
    }
}

/// A rank circle: filled with the number, or an empty outline. Pressed, it
/// sits on a black disc; on a pressed row it is drawn in paper.
fn draw_rank(cx: &mut DrawCx, circle: Rect, rank: Option<usize>, enabled: bool, pressed: bool, on_ink: bool) {
    let t = cx.theme;
    let (cxp, cyp, radius) = (circle.center().x as f32, circle.center().y as f32, circle.w as f32 / 2.0);
    let (ink, paper) = if pressed || on_ink {
        (PAPER, INK)
    } else if enabled {
        (INK, PAPER)
    } else {
        (INK3, PAPER)
    };
    if pressed {
        let halo = circle.inset(-t.mm(1.0));
        cx.canvas.fill_circle(cxp, cyp, halo.w as f32 / 2.0, INK_FILL);
        cx.hint(halo, RefreshMode::Fast);
    }
    match rank {
        Some(n) => {
            cx.canvas.fill_circle(cxp, cyp, radius, ink);
            cx.line(&n.to_string(), t.small.weight(Weight::SemiBold).gray(paper), circle, HAlign::Center);
        }
        None => {
            let outline = if pressed || on_ink || !enabled { ink } else { INK2 };
            cx.canvas.stroke_circle(cxp, cyp, radius - t.border as f32 / 2.0, t.border as f32, outline);
        }
    }
}

/// One reorder button: an outlined box with a chevron, faint when inactive
/// and inverted while pressed; on a pressed row it is drawn in paper.
fn draw_step(cx: &mut DrawCx, area: Rect, icon: Icon, active: bool, pressed: bool, on_ink: bool) {
    let t = cx.theme;
    let size = (area.w - t.mm(1.5)).min(area.h - t.mm(1.5));
    let rect = Rect::new(area.center().x - size / 2, area.center().y - size / 2, size, size);
    let icon_rect =
        Rect::new(rect.center().x - t.icon_small / 2, rect.center().y - t.icon_small / 2, t.icon_small, t.icon_small);
    if pressed {
        cx.canvas.fill_rounded_rect(rect, t.radius, INK_FILL);
        cx.hint(rect, RefreshMode::Fast);
        cx.icon(icon, icon_rect, t.icon_small, PAPER);
    } else {
        let (border, ink) = match (on_ink, active) {
            (true, true) => (PAPER, PAPER),
            (true, false) => (INK2, INK2),
            (false, true) => (INK, INK),
            (false, false) => (LINE, INK3),
        };
        cx.canvas.stroke_rounded_rect(rect, t.radius, t.border as f32, border);
        cx.icon(icon, icon_rect, t.icon_small, ink);
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

    #[test]
    fn lead_and_reorder_buttons_report_their_own_events() {
        let mut l = list();
        l.slot = 60;
        l.rows[0] = Row::new("First").rank(Some(1)).reorder(false, true);
        let tap = |l: &mut List, p: Point| {
            l.on_gesture(&Gesture::Down(p));
            l.on_gesture(&Gesture::Tap(p))
        };
        // Lead: the row's left edge to a slot past the text inset (50 + 60).
        assert_eq!(tap(&mut l, Point::new(5, 150)), Some(ListEvent::Lead(0)));
        assert_eq!(tap(&mut l, Point::new(105, 150)), Some(ListEvent::Lead(0)));
        assert_eq!(tap(&mut l, Point::new(115, 150)), Some(ListEvent::Tapped(0)));
        // Buttons end at the right text inset: up 430..490, down 490..550.
        assert_eq!(tap(&mut l, Point::new(520, 150)), Some(ListEvent::Down(0)));
        assert_eq!(tap(&mut l, Point::new(460, 150)), None, "a faint button is inert");
        assert_eq!(tap(&mut l, Point::new(580, 150)), Some(ListEvent::Tapped(0)));
        l.rows[0].accessory = Accessory::Reorder { up: true, down: false };
        assert_eq!(tap(&mut l, Point::new(460, 150)), Some(ListEvent::Up(0)));
        assert_eq!(tap(&mut l, Point::new(520, 150)), None);
        // Holding a part still taps it; dragging off it does not.
        let p = Point::new(460, 150);
        l.on_gesture(&Gesture::Down(p));
        assert_eq!(l.on_gesture(&Gesture::LongPress(p)), None);
        assert_eq!(l.on_gesture(&Gesture::End(p)), Some(ListEvent::Up(0)));
        l.on_gesture(&Gesture::Down(p));
        l.on_gesture(&Gesture::Move(Point::new(300, 150)));
        assert_eq!(l.on_gesture(&Gesture::Tap(Point::new(300, 150))), None);
        // A disabled row ignores its parts too.
        l.rows[0].enabled = false;
        assert_eq!(tap(&mut l, Point::new(5, 150)), None);
        assert_eq!(tap(&mut l, Point::new(460, 150)), None);
    }
}
