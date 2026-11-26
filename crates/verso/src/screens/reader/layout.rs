//! The Page layout screen: a sketch of the page with the bars' real values,
//! and controls for the bars, the progress line and the margins. It edits a
//! copy of the layout; the reader applies it when the screen closes, so the
//! book is laid out at most once per visit.
//!
//! First-time setup shows the same editor [embedded](LayoutView::embedded)
//! in its own page: no top bar, only the bar rows (margins need the real
//! page; Reader Menu → Layout has them), with [sample values](sample_data).
use super::bars::{self, BarStyle};
use crate::clock::LocalTime;
use crate::document::{Position, TocEntry, Typography};
use crate::geom::{Point, Rect};
use crate::gfx::HAlign;
use crate::page_layout::{BAR_MM, Bar, BarItem, MAX_MARGIN_MM, MIN_MARGIN_MM, PageLayout, ProgressLine};
use crate::platform::{Battery, RefreshMode};
use crate::reader::bars::{BarData, ItemText};
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK, INK2, INK3, LINE, PAPER, Theme};
use crate::ui::widget::{DrawCx, Press, PressEvent};
use crate::ui::widgets::{MenuEvent, PopupMenu, Segmented, SideLabel, Step, Stepper, Toggle, TopBar};
use std::time::Instant;

const LABELS: [&str; 7] =
    ["Top bar", "Bottom bar", "Progress line", "Chapter marks", "Side margins", "Top margin", "Bottom margin"];
const LINES: [ProgressLine; 3] = [ProgressLine::Off, ProgressLine::Top, ProgressLine::Bottom];
/// The rows setup shows: the bars, the progress line and chapter marks.
const SETUP_ROWS: usize = 4;
/// Space between the sketch's top and bottom pieces, where the caption goes.
const SKETCH_GAP_MM: f32 = 5.0;
const CAPTION: &str = "Tap a slot to change what it shows";

/// Bar values for the editor when no book is open.
pub(crate) fn sample_toc() -> Vec<TocEntry> {
    [
        ("The Lighthouse Keeper", 4),
        ("Low Tide", 58),
        ("The Letters", 121),
        ("Storm Season", 196),
        ("The Long Way Home", 268),
    ]
    .into_iter()
    .map(|(title, page)| TocEntry { title: title.into(), level: 1, page, position: Position::default() })
    .collect()
}
pub(crate) fn sample_data(toc: &[TocEntry]) -> BarData<'_> {
    BarData {
        toc,
        title: "A Summer by the Sea",
        author: "Ada Lovelace",
        page: 11,
        pages: 340,
        progress: 400,
        clock: Some(LocalTime { hour: 9, minute: 41 }),
        battery: Some(Battery { percent: 80, charging: false }),
        ..BarData::default()
    }
}

pub(crate) enum LayoutEvent {
    Close,
}

/// An open slot picker: which bar (0 top, 1 bottom) and slot it fills.
struct Picker {
    bar: usize,
    slot: usize,
    menu: PopupMenu,
}

pub(crate) struct LayoutView {
    /// The layout being edited.
    pub layout: PageLayout,
    /// The page the sketch shows.
    screen: Rect,
    /// Where the view draws: below the top bar, or setup's page body.
    frame: Rect,
    font_px: i32,
    /// Distance between text lines on the page, in pixels.
    line_pitch: f32,
    justify: bool,
    /// None when embedded.
    top_bar: Option<TopBar>,
    /// The sketch: the top and bottom of the page, `half` tall each, with
    /// the middle left out. `scale` maps page pixels to sketch pixels.
    sketch: Rect,
    half: i32,
    scale: f32,
    target: i32,
    caption: Rect,
    /// One per label shown: all seven, or `SETUP_ROWS` embedded.
    rows: Vec<Rect>,
    label_w: i32,
    bars: [Toggle; 2],
    marks: Toggle,
    progress: Segmented,
    /// Empty when embedded.
    margins: Vec<Stepper>,
    press: Press,
    pressed: Option<(usize, usize)>,
    picker: Option<Picker>,
}

impl LayoutView {
    /// The Page layout screen over the whole of `screen`.
    pub fn new(t: &Theme, screen: Rect, layout: &PageLayout, typography: &Typography) -> Self {
        let mut top_bar = TopBar::new("Page layout", true);
        top_bar.place(screen, t);
        let body_y = top_bar.rect().bottom();
        let frame = Rect::new(screen.x, body_y, screen.w, screen.bottom() - body_y);
        Self::build(t, screen, frame, Some(top_bar), layout, typography)
    }

    /// The editor inside `frame` (already clear of the screen's margins),
    /// with no top bar and only the bar rows: setup's Your page.
    pub fn embedded(t: &Theme, screen: Rect, frame: Rect, layout: &PageLayout, typography: &Typography) -> Self {
        Self::build(t, screen, frame, None, layout, typography)
    }

    fn build(
        t: &Theme,
        screen: Rect,
        frame: Rect,
        top_bar: Option<TopBar>,
        layout: &PageLayout,
        typography: &Typography,
    ) -> Self {
        let full = top_bar.is_some();
        let (pad, row_h) = (t.mm(4.0), t.mm(11.0));
        // Embedded, the frame already keeps clear of the margins and edges.
        let (edge, inset) = if full { (pad, t.margin) } else { (0, 0) };
        let count = if full { LABELS.len() } else { SETUP_ROWS };
        // The sketch on top, the controls below: one column in portrait,
        // two columns in landscape.
        let columns = if screen.w > screen.h { 2 } else { 1 };
        let per_column = count.div_ceil(columns);
        let controls_h = per_column as i32 * row_h;
        let inner_w = frame.w - 2 * inset;
        let controls = Rect::new(frame.x + inset, frame.bottom() - edge - controls_h, inner_w, controls_h);
        let area_y = frame.y + edge;
        let area = Rect::new(controls.x, area_y, inner_w, controls.y - pad - area_y);
        // 6" landscape: a narrower gap, so each column keeps room for its
        // labels and full-size controls.
        let narrow = columns == 2 && inner_w < t.mm(125.0);
        let column_rects = controls.columns(columns, t.mm(if narrow { 4.0 } else { 8.0 }));
        let rows: Vec<Rect> = (0..count)
            .map(|i| {
                let column = column_rects[i / per_column];
                Rect::new(column.x, column.y + (i % per_column) as i32 * row_h, column.w, row_h)
            })
            .collect();
        // Near full width, so the bars show their real text and fitting. Each
        // half must hold the largest top or bottom margin with its bar.
        let half = (area.h - t.mm(SKETCH_GAP_MM)) / 2;
        let caption = Rect::new(area.x, area.y + half, inner_w, area.h - 2 * half);
        let tallest = t.mm(MAX_MARGIN_MM as f32 / 2.0 + BAR_MM + 2.0);
        let scale = (area.w as f32 / screen.w as f32).min(half as f32 / tallest as f32).min(1.0);
        let sketch_w = (screen.w as f32 * scale) as i32;
        let sketch = Rect::new(area.center().x - sketch_w / 2, area.y, sketch_w, area.h);
        // Labels are at most about 21.2 mm wide (8.5 pt "Chapter marks").
        let label_gap = t.mm(if narrow { 1.5 } else { 2.0 });
        let control_w = t.mm(50.0).min(rows[0].w - t.mm(if narrow { 23.5 } else { 26.0 }));
        let control = |i: usize, w: i32| {
            let r = rows[i];
            Rect::new(r.right() - w, r.y + (r.h - t.target) / 2, w, t.target)
        };
        // "Progress line" is shorter than the longest label, so its three
        // segments get a little more room when columns are narrow.
        let progress_w = if narrow { control_w + t.mm(2.5) } else { control_w };
        // The value ("20 mm") keeps 12 mm; the sides give way down to a
        // touch target.
        let side_w = t.mm(12.0).min((control_w - t.mm(12.0)) / 2).max(t.target.min(control_w / 3));
        let bars = [layout.top.on, layout.bottom.on].map(Toggle::new);
        let mut progress = Segmented::texts(&["Off", "Top", "Bottom"], line_index(layout.progress));
        progress.place(control(2, progress_w));
        let margins = (4..rows.len())
            .map(|i| {
                let label = |text: &str| SideLabel { text: text.into(), style: t.heading };
                let mut stepper = Stepper::new(label("−"), label("+"), "");
                stepper.place(control(i, control_w), side_w);
                stepper
            })
            .collect();
        let spacing = typography.line_spacing as f32 / 100.0;
        let font_px = typography.font_px(t.dpi);
        let mut view = Self {
            layout: layout.clone(),
            screen,
            frame,
            font_px,
            line_pitch: font_px as f32 * 1.2 * spacing,
            justify: typography.justify,
            top_bar,
            sketch,
            half,
            scale,
            target: t.target,
            caption,
            label_w: rows[0].w - control_w - label_gap,
            bars,
            marks: Toggle::new(layout.chapter_marks),
            progress,
            margins,
            rows,
            press: Press::default(),
            pressed: None,
            picker: None,
        };
        for (k, toggle) in view.bars.iter_mut().enumerate() {
            toggle.place(view.rows[k].right(), view.rows[k].center().y, t);
        }
        view.marks.place(view.rows[3].right(), view.rows[3].center().y, t);
        view.sync();
        view
    }

    fn margin(&mut self, k: usize) -> &mut u32 {
        let m = &mut self.layout.margins;
        [&mut m.sides, &mut m.top, &mut m.bottom].into_iter().nth(k).expect("three margins")
    }

    fn bar(&mut self, k: usize) -> &mut Bar {
        if k == 0 { &mut self.layout.top } else { &mut self.layout.bottom }
    }

    /// Brings the controls in line with the layout.
    fn sync(&mut self) {
        self.bars[0].on = self.layout.top.on;
        self.bars[1].on = self.layout.bottom.on;
        self.marks.on = self.layout.chapter_marks;
        self.progress.selected = line_index(self.layout.progress);
        for k in 0..self.margins.len() {
            let value = *self.margin(k);
            let stepper = &mut self.margins[k];
            stepper.value = format!("{value} mm");
            stepper.can_less = value > MIN_MARGIN_MM;
            stepper.can_more = value < MAX_MARGIN_MM;
        }
    }

    fn step(&mut self, k: usize, step: Step) {
        let m = self.margin(k);
        *m = if step == Step::Less { m.saturating_sub(1) } else { *m + 1 }.clamp(MIN_MARGIN_MM, MAX_MARGIN_MM);
        self.sync();
    }

    /// A bar that is turned off takes the progress line with it; choosing a
    /// line for a bar that is off turns the bar on.
    fn set_bar(&mut self, k: usize, on: bool) {
        self.bar(k).on = on;
        if !on && self.layout.progress == LINES[k + 1] {
            self.layout.progress = ProgressLine::Off;
        }
        self.sync();
    }
    fn set_line(&mut self, line: ProgressLine) {
        self.layout.progress = line;
        if let Some(k) = LINES[1..].iter().position(|&l| l == line) {
            self.bar(k).on = true;
        }
        self.sync();
    }

    /// The top and bottom pieces of the sketch.
    fn pieces(&self) -> [Rect; 2] {
        let sk = self.sketch;
        [Rect::new(sk.x, sk.y, sk.w, self.half), Rect::new(sk.x, sk.bottom() - self.half, sk.w, self.half)]
    }

    /// How much of the page each piece shows, in page pixels.
    fn half_page(&self) -> i32 {
        (self.half as f32 / self.scale) as i32
    }

    /// Maps a rectangle on the page to the sketch: the upper half of the page
    /// to the top piece, the lower half to the bottom piece.
    fn map(&self, r: Rect) -> Rect {
        let s = |v: i32| (v as f32 * self.scale).round() as i32;
        let (x, right) = (self.sketch.x + s(r.x), self.sketch.x + s(r.right()));
        let (y, bottom) = if r.center().y < self.screen.h / 2 {
            (self.sketch.y + s(r.y), self.sketch.y + s(r.bottom()))
        } else {
            let from_bottom = |v: i32| self.sketch.bottom() - s(self.screen.h - v);
            (from_bottom(r.y), from_bottom(r.bottom()))
        };
        Rect::new(x, y, right - x, bottom - y)
    }

    /// The bars that are on, as drawn in the sketch: (bar, rect).
    fn sketch_bars(&self, dpi: f32) -> Vec<(usize, Rect)> {
        let g = self.layout.geometry(self.screen.w, self.screen.h, dpi, self.font_px);
        [g.top_bar, g.bottom_bar].into_iter().enumerate().filter_map(|(k, r)| Some((k, self.map(r?)))).collect()
    }

    /// Tap areas for the slots of the bars that are on: the bar's thirds,
    /// grown to a full touch target.
    fn slot_rects(&self, dpi: f32) -> Vec<(usize, usize, Rect)> {
        self.sketch_bars(dpi)
            .into_iter()
            .flat_map(|(k, r)| {
                let target = self.target;
                r.columns(3, 0).into_iter().enumerate().map(move |(slot, c)| (k, slot, c.at_least(0, target)))
            })
            .collect()
    }

    fn open_picker(&mut self, bar: usize, slot: usize, anchor: Point, t: &Theme, data: &BarData) {
        let mut items = vec![(None, "Nothing".to_owned())];
        items.extend(BarItem::ALL.iter().map(|&item| {
            let label = match data.text(item) {
                Some(text) => format!("{} — {}{}", item.label(), text.head, text.tail),
                None => item.label().to_owned(),
            };
            (None, label)
        }));
        let mut menu = PopupMenu::open(items, anchor, self.screen, t);
        let current = self.bar(bar).slots[slot];
        menu.set_checked(Some(current.and_then(|c| BarItem::ALL.iter().position(|&i| i == c)).map_or(0, |i| i + 1)));
        self.picker = Some(Picker { bar, slot, menu });
    }

    /// Shows `layout` (setup couldn't save a change).
    pub fn reset(&mut self, layout: &PageLayout) {
        self.layout = layout.clone();
        self.sync();
    }

    pub fn picker_open(&self) -> bool {
        self.picker.is_some()
    }

    #[allow(clippy::type_complexity)]
    pub fn visual_state(
        &self,
    ) -> (Option<(usize, usize, Option<usize>)>, bool, Option<(usize, usize)>, PageLayout, Vec<Option<Step>>) {
        (
            self.picker.as_ref().map(|p| (p.bar, p.slot, p.menu.is_pressed())),
            self.top_bar.as_ref().is_some_and(TopBar::is_pressed),
            self.pressed,
            self.layout.clone(),
            self.margins.iter().map(Stepper::pressed).collect(),
        )
    }

    pub fn on_gesture(&mut self, g: &Gesture, t: &Theme, data: &BarData) -> Option<LayoutEvent> {
        if let Some(picker) = &mut self.picker {
            match picker.menu.on_gesture(g, t) {
                Some(MenuEvent::Chosen(i)) => {
                    let (bar, slot) = (picker.bar, picker.slot);
                    self.picker = None;
                    self.bar(bar).slots[slot] = i.checked_sub(1).map(|k| BarItem::ALL[k]);
                }
                Some(MenuEvent::Dismissed) => self.picker = None,
                None => {}
            }
            return None;
        }
        if let Some(top_bar) = &mut self.top_bar
            && top_bar.on_gesture(g)
        {
            return Some(LayoutEvent::Close);
        }
        let slots = self.slot_rects(t.dpi);
        if let Gesture::Down(p) = *g {
            self.pressed = slots.iter().find(|s| s.2.contains(p)).map(|s| (s.0, s.1));
        }
        if let Some((bar, slot)) = self.pressed
            && let Some(&(_, _, rect)) = slots.iter().find(|s| (s.0, s.1) == (bar, slot))
        {
            let event = self.press.on_gesture(g, rect);
            if !self.press.is_down() {
                self.pressed = None;
            }
            if let Some(PressEvent::Tap | PressEvent::LongPressEnd) = event {
                self.open_picker(bar, slot, rect.center(), t, data);
            }
            return None;
        }
        for k in 0..2 {
            if let Some(on) = self.bars[k].on_gesture(g) {
                self.set_bar(k, on);
            }
        }
        if let Some(on) = self.marks.on_gesture(g) {
            self.layout.chapter_marks = on;
            self.sync();
        }
        if let Some(i) = self.progress.on_gesture(g) {
            self.set_line(LINES[i.min(2)]);
        }
        for k in 0..self.margins.len() {
            if let Some(step) = self.margins[k].on_gesture(g) {
                self.step(k, step);
            }
        }
        None
    }

    /// When a held stepper repeats next.
    pub fn deadline(&self) -> Option<Instant> {
        self.margins.iter().filter_map(Stepper::deadline).min()
    }
    /// Repeats held steppers; true when a value changed.
    pub fn on_tick(&mut self, now: Instant) -> bool {
        let mut changed = false;
        for k in 0..self.margins.len() {
            if let Some(step) = self.margins[k].on_tick(now) {
                self.step(k, step);
                changed = true;
            }
        }
        changed
    }

    pub fn draw(&self, cx: &mut DrawCx, data: &BarData) {
        let t = cx.theme;
        cx.fill(if self.top_bar.is_some() { self.screen } else { self.frame }, PAPER);
        if let Some(top_bar) = &self.top_bar {
            top_bar.draw(cx);
        }
        self.draw_sketch(cx, data);
        for (row, label) in self.rows.iter().zip(LABELS) {
            cx.line(label, t.body, Rect::new(row.x, row.y, self.label_w, row.h), HAlign::Left);
        }
        self.bars.iter().for_each(|toggle| toggle.draw(cx));
        self.progress.draw(cx);
        self.marks.draw(cx);
        self.margins.iter().for_each(|stepper| stepper.draw(cx));
        if let Some(picker) = &self.picker {
            if let Some(&(_, _, rect)) = self.slot_rects(t.dpi).iter().find(|s| (s.0, s.1) == (picker.bar, picker.slot))
            {
                cx.canvas.stroke_rounded_rect(rect.inset_xy(0, t.mm(1.5)), t.radius, t.border as f32, INK);
            }
            picker.menu.draw(cx);
        }
    }

    /// The page's top and bottom at near real size: margins to scale, grey
    /// lines for text, and the bars with their real values. Empty slots show
    /// "+" in a thin outline; the middle of the page is left out, and the
    /// caption goes there.
    fn draw_sketch(&self, cx: &mut DrawCx, data: &BarData) {
        let t = cx.theme;
        let s = self.scale;
        let [top, bottom] = self.pieces();
        // Two cards, open where the page is cut.
        let open = t.radius_sheet as i32 + 2 * t.border;
        cx.canvas.stroke_rounded_rect(
            top.inset(-t.border).union(&top.moved(0, open)),
            t.radius_sheet,
            t.border as f32,
            INK,
        );
        cx.canvas.stroke_rounded_rect(
            bottom.inset(-t.border).union(&bottom.moved(0, -open)),
            t.radius_sheet,
            t.border as f32,
            INK,
        );
        let gap = Rect::new(top.x - 2 * t.border, top.bottom(), top.w + 4 * t.border, bottom.y - top.bottom());
        cx.fill(gap, PAPER);
        cx.line(CAPTION, t.small.gray(INK2), self.caption, HAlign::Center);
        let g = self.layout.geometry(self.screen.w, self.screen.h, t.dpi, self.font_px);
        let (width, height) = (self.screen.w, self.screen.h);
        let half_page = self.half_page();
        let mut y = g.top as f32;
        for k in 0.. {
            if y + self.line_pitch > (height - g.bottom) as f32 + 0.5 {
                break;
            }
            let (line_top, line_bottom) = (y as i32, (y + self.line_pitch) as i32);
            y += self.line_pitch;
            if line_bottom > half_page && line_top < height - half_page {
                continue; // The middle of the page isn't shown.
            }
            // Paragraphs of seven lines: an indent, and a short last line.
            let indent = if k % 7 == 0 { (self.line_pitch * 1.5) as i32 } else { 0 };
            let column = width - 2 * g.side;
            let share = if k % 7 == 6 {
                0.55
            } else if self.justify {
                1.0
            } else {
                [1.0, 0.93, 0.97, 0.9, 0.95, 0.98][k % 6]
            };
            // About an x-height of ink in the middle of the line.
            let ink = (self.line_pitch * 0.25) as i32;
            let line = Rect::new(
                g.side + indent,
                line_top + (line_bottom - line_top - ink) / 2,
                (column as f32 * share) as i32 - indent,
                ink,
            );
            let mut r = self.map(line);
            r.h = r.h.max(2);
            cx.fill(r, LINE);
        }
        let style = BarStyle::scaled(t, s, t.caption.size((t.caption.size * s).round()).gray(INK2));
        let pressed = self.pressed.filter(|_| self.press.is_down());
        for (k, rect) in self.sketch_bars(t.dpi) {
            let bar = if k == 0 { &self.layout.top } else { &self.layout.bottom };
            cx.fill(rect, PAPER);
            if self.layout.progress == LINES[k + 1] {
                bars::progress(cx, rect, k == 1, self.layout.chapter_marks, data, &style);
            }
            let thirds = rect.columns(3, 0);
            let texts: [Option<(ItemText, u8)>; 3] = std::array::from_fn(|slot| {
                let down = pressed == Some((k, slot));
                if down {
                    let r = thirds[slot];
                    cx.canvas.fill_rounded_rect(r, t.radius, INK);
                    cx.hint(r, RefreshMode::Fast);
                }
                let (text, gray) = match bar.slots[slot] {
                    None => {
                        // A small "+" box where the slot's text would start.
                        let h = (rect.h * 4 / 5).max(t.mm(2.5));
                        let w = 2 * h;
                        let x = [rect.x, rect.center().x - w / 2, rect.right() - w][slot];
                        let r = Rect::new(x, rect.center().y - h / 2, w, h);
                        let ink = if down { PAPER } else { INK3 };
                        cx.canvas.stroke_rounded_rect(r, h as f32 / 3.0, (t.border as f32 / 2.0).max(1.0), ink);
                        cx.line("+", style.text.gray(if down { PAPER } else { INK2 }), r, HAlign::Center);
                        return None;
                    }
                    Some(item) => match data.text(item) {
                        Some(text) => (text, style.text.gray),
                        // Items without a value yet show their name.
                        None => (ItemText::short(item.label().into()), INK3),
                    },
                };
                Some((text, if down { PAPER } else { gray }))
            });
            bars::slots(cx, &texts, rect, &style);
        }
    }
}

/// Tap points for setup's tests.
#[cfg(test)]
impl LayoutView {
    pub(crate) fn bar_toggle(&self, k: usize) -> Rect {
        self.bars[k].rect()
    }
    pub(crate) fn slot(&self, t: &Theme, bar: usize, slot: usize) -> Rect {
        self.slot_rects(t.dpi).into_iter().find(|s| (s.0, s.1) == (bar, slot)).expect("the bar is on").2
    }
    /// Item `i` of the open picker, after "Nothing".
    pub(crate) fn picker_item(&self, t: &Theme, item: BarItem) -> Rect {
        let i = BarItem::ALL.iter().position(|&b| b == item).unwrap() + 1;
        self.picker.as_ref().expect("a picker is open").menu.item_rects(t)[i]
    }
}

fn line_index(line: ProgressLine) -> usize {
    LINES.iter().position(|&l| l == line).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Position, TocEntry};

    fn data(toc: &[TocEntry]) -> BarData<'_> {
        BarData {
            toc,
            title: "Dune",
            author: "Frank Herbert",
            page: 4,
            pages: 20,
            progress: 2500,
            ..BarData::default()
        }
    }
    fn tap(view: &mut LayoutView, p: Point, t: &Theme, data: &BarData) -> Option<LayoutEvent> {
        view.on_gesture(&Gesture::Down(p), t, data);
        view.on_gesture(&Gesture::Tap(p), t, data)
    }
    /// The Page layout screen and setup's embedded editor on each screen.
    fn views() -> Vec<(Theme, LayoutView)> {
        [
            (1272, 1696, 300),
            (1696, 1272, 300),
            (600, 800, 167),
            (800, 600, 167),
            (1448, 1072, 300),
            (1860, 2480, 300),
            (2480, 1860, 300),
        ]
        .into_iter()
        .flat_map(|(w, h, dpi)| {
            let screen = Rect::new(0, 0, w, h);
            let t = Theme::for_dpi(dpi);
            let full = like(None, &t, screen, &PageLayout::default());
            let embedded = like(Some(&full), &t, screen, &PageLayout::default());
            [(t.clone(), full), (t, embedded)]
        })
        .collect()
    }
    /// A full view, or with `embedded_like` an embedded one in setup's body.
    fn like(embedded_like: Option<&LayoutView>, t: &Theme, screen: Rect, layout: &PageLayout) -> LayoutView {
        match embedded_like {
            None => LayoutView::new(t, screen, layout, &Typography::default()),
            Some(_) => {
                let body = crate::screens::setup::body_rect(t, screen);
                LayoutView::embedded(t, screen, body, layout, &Typography::default())
            }
        }
    }

    #[test]
    fn controls_hold_their_text_on_every_screen() {
        let mut text = crate::gfx::TextRenderer::new();
        for (t, view) in views() {
            let size = (view.screen.w, view.screen.h);
            for label in LABELS {
                assert!(text.width(label, t.body) <= view.label_w as f32, "{size:?}: {label} cut short");
            }
            for stepper in &view.margins {
                let (less, value, more) = stepper.parts();
                assert!(less.w >= t.target && more.w >= t.target, "{size:?}: stepper sides {less:?} {more:?}");
                assert!(text.width("20 mm", t.button) <= value.w as f32, "{size:?}: value {value:?}");
                assert!(stepper.rect().x > view.rows[4].x + view.label_w, "{size:?}: stepper over its label");
            }
            // Segment labels may shrink to 80% before they're cut.
            let segment = view.progress.rect().w / 3 - 2 * t.mm(1.0);
            assert!(text.width("Bottom", t.button) * 0.8 <= segment as f32, "{size:?}: progress {segment}");
            assert!(view.progress.rect().right() <= view.rows[2].right());
        }
    }

    #[test]
    fn controls_fit_the_screen_without_overlapping() {
        for (t, view) in views() {
            let screen = view.screen;
            let size = (screen.w, screen.h);
            let frame = view.frame;
            let last = *view.rows.last().unwrap();
            assert!(view.sketch.y >= frame.y && last.bottom() <= frame.bottom() && view.rows[0].y >= frame.y);
            assert!(view.sketch.w >= t.mm(20.0), "{size:?}: the sketch is big enough to read ({:?})", view.sketch);
            let controls = view.rows[0].union(&last);
            assert!(view.sketch.intersect(&controls).is_none(), "{size:?}");
            let [top, bottom] = view.pieces();
            assert!(view.caption.y >= top.bottom() && view.caption.bottom() <= bottom.y, "{size:?}");
            // Each half of the sketch shows a bar and its margin legibly.
            assert!(view.half >= t.mm(5.0), "{size:?}: half {}", view.half);
            // Slot taps are full targets and stay off the top bar and the controls.
            let mut layout = PageLayout::default();
            layout.top.on = true;
            let embedded = view.top_bar.is_none().then_some(&view);
            let view = like(embedded, &t, screen, &layout);
            // Embedded, a slot may reach into the gap under setup's top bar.
            let top_limit = if embedded.is_some() { screen.y + t.top_bar } else { frame.y };
            let slots = view.slot_rects(t.dpi);
            assert_eq!(slots.len(), 6);
            for (_, _, r) in &slots {
                assert!(r.h >= t.target, "{size:?}");
                assert!(r.y >= top_limit, "{size:?}: {r:?}");
                assert!(r.intersect(&controls).is_none(), "{size:?}: {r:?}");
                assert!(r.intersect(&view.bars[0].rect()).is_none());
            }
        }
    }

    #[test]
    fn picking_a_slot_sets_its_item_or_nothing() {
        let toc =
            vec![TocEntry { title: "The Desert Planet".into(), level: 1, page: 2, position: Position::default() }];
        let data = data(&toc);
        for (t, mut view) in views() {
            // The bottom bar's centre slot.
            let (_, _, centre) = view.slot_rects(t.dpi).into_iter().find(|s| (s.0, s.1) == (1, 1)).unwrap();
            assert!(tap(&mut view, centre.center(), &t, &data).is_none());
            let picker = view.picker.as_ref().expect("a tap opens the picker");
            assert_eq!(picker.menu.items.len(), BarItem::ALL.len() + 1);
            assert_eq!(picker.menu.items[0].1, "Nothing");
            assert_eq!(picker.menu.items[8].1, "Percent — 25%");
            assert!(picker.menu.items[2].1.ends_with("— The Desert Planet · 15 pages left"));
            assert_eq!(picker.menu.items[11].1, "Clock (24 h)", "items without a value yet show just the name");
            // Choose Page / total (item 9: after "Nothing").
            let item = item_center(&view, &t, 9);
            tap(&mut view, item, &t, &data);
            assert!(view.picker.is_none());
            assert_eq!(view.layout.bottom.slots[1], Some(BarItem::PageTotal));
            // Now clear the left slot.
            let (_, _, left) = view.slot_rects(t.dpi).into_iter().find(|s| (s.0, s.1) == (1, 0)).unwrap();
            tap(&mut view, left.center(), &t, &data);
            let nothing = item_center(&view, &t, 0);
            tap(&mut view, nothing, &t, &data);
            assert_eq!(view.layout.bottom.slots, [None, Some(BarItem::PageTotal), Some(BarItem::Percent)]);
            // An empty slot opens the picker too; tapping outside closes it unchanged.
            tap(&mut view, left.center(), &t, &data);
            assert!(view.picker.is_some());
            let outside = Point::new(view.screen.x + 1, view.screen.y + 1);
            tap(&mut view, outside, &t, &data);
            assert!(view.picker.is_none());
            assert_eq!(view.layout.bottom.slots[0], None);
            assert!(!view.layout.moves_text(&PageLayout::default()), "slot picks never re-lay out");
        }
    }

    fn item_center(view: &LayoutView, t: &Theme, i: usize) -> Point {
        view.picker.as_ref().unwrap().menu.item_rects(t)[i].center()
    }

    #[test]
    fn a_bar_that_is_off_has_no_slots() {
        let (t, mut view) = views().remove(0);
        assert!(view.slot_rects(t.dpi).iter().all(|s| s.0 == 1), "the top bar is off by default");
        let toggle = view.bars[0].rect().center();
        tap(&mut view, toggle, &t, &data(&[]));
        assert!(view.layout.top.on);
        assert_eq!(view.slot_rects(t.dpi).len(), 6);
    }

    #[test]
    fn steppers_toggles_and_the_line_edit_the_copy() {
        let (t, mut view) = views().remove(0);
        let d = data(&[]);
        let less = |v: &LayoutView, k: usize| {
            let r = v.margins[k].rect();
            Point::new(r.x + t.mm(3.0), r.center().y)
        };
        let more = |v: &LayoutView, k: usize| {
            let r = v.margins[k].rect();
            Point::new(r.right() - t.mm(3.0), r.center().y)
        };
        let p = more(&view, 0);
        tap(&mut view, p, &t, &d);
        let p = more(&view, 1);
        tap(&mut view, p, &t, &d);
        let p = more(&view, 2);
        tap(&mut view, p, &t, &d);
        assert_eq!((view.layout.margins.sides, view.layout.margins.top, view.layout.margins.bottom), (4, 3, 3));
        assert_eq!(view.margins[0].value, "4 mm");
        // Limits hold, and the stepper side greys out there.
        for _ in 0..30 {
            let p = less(&view, 1);
            tap(&mut view, p, &t, &d);
        }
        assert_eq!(view.layout.margins.top, MIN_MARGIN_MM);
        assert!(!view.margins[1].can_less);
        // Choosing a line for a bar that is off turns it on; turning the bar
        // off takes the line with it.
        let seg = view.progress.rect().columns(3, 0);
        tap(&mut view, seg[1].center(), &t, &d);
        assert_eq!(view.layout.progress, ProgressLine::Top);
        assert!(view.layout.top.on && view.bars[0].on);
        let toggle = view.bars[0].rect().center();
        tap(&mut view, toggle, &t, &d);
        assert!(!view.layout.top.on);
        assert_eq!((view.layout.progress, view.progress.selected), (ProgressLine::Off, 0));
        // The chapter marks toggle flips the layout.
        assert!(view.layout.chapter_marks);
        let marks = view.marks.rect().center();
        tap(&mut view, marks, &t, &d);
        assert!(!view.layout.chapter_marks && !view.marks.on);
        // The back arrow closes.
        let back = Point::new(t.margin + t.target / 2 - t.mm(2.0), view.top_bar.as_ref().unwrap().rect().center().y);
        assert!(matches!(tap(&mut view, back, &t, &d), Some(LayoutEvent::Close)));
    }

    #[test]
    fn holding_a_stepper_repeats() {
        let (t, mut view) = views().remove(0);
        let d = data(&[]);
        let r = view.margins[0].rect();
        let p = Point::new(r.right() - t.mm(3.0), r.center().y);
        view.on_gesture(&Gesture::Down(p), &t, &d);
        view.on_gesture(&Gesture::LongPress(p), &t, &d);
        assert_eq!(view.layout.margins.sides, 4);
        let at = view.deadline().expect("repeating");
        assert!(view.on_tick(at));
        assert_eq!(view.layout.margins.sides, 5);
        view.on_gesture(&Gesture::End(p), &t, &d);
        assert!(view.deadline().is_none());
    }
}
