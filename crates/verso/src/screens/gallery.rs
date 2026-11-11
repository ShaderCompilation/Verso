//! The widget gallery (the Phase 3 milestone): every widget, working, on three
//! pages. Page 1 follows mockup 8 in docs/design.md; page 2 has the stepper,
//! a settings-style list and the sheet; page 3 has the cover grid and its
//! long-press menu. The Phase 2 engine spike opens from page 1.

use crate::document::thread::Worker;
use crate::geom::{Point, Rect};
use crate::gfx::{Family, HAlign, Icon, TextStyle, Weight};
use crate::platform::RefreshMode;
use crate::screens::books::Books;
use crate::screens::{Action, EventCx, Screen};
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK, INK2, PAPER, Theme};
use crate::ui::widget::{DrawCx, Press, PressEvent};
use crate::ui::widgets::{
    Button, ButtonKind, Grid, GridEvent, IconButton, List, ListEvent, MenuEvent, Pager, PopupMenu, Row, Segment,
    Segmented, Sheet, SideLabel, Slider, SliderEvent, Step, Stepper, Toggle, TopBar,
};
use std::rc::Rc;

const PAGES: usize = 3;

const ICON_BUTTONS: [(Icon, &str); 6] = [
    (Icon::ArrowLeft, "Back"),
    (Icon::List, "Contents"),
    (Icon::TextSize, "Text"),
    (Icon::Settings, "Settings"),
    (Icon::Sun, "Light"),
    (Icon::Close, "Close"),
];

const MENU_ITEMS: [(Icon, &str); 4] = [
    (Icon::BookOpen, "Open"),
    (Icon::Info, "Details"),
    (Icon::Check, "Mark as finished"),
    (Icon::Close, "Remove from Continue reading"),
];

/// Stand-in books for the grid: title, author, cover gray (`None` = a plain
/// generated cover, white with a border).
const BOOKS: [(&str, &str, Option<u8>); 8] = [
    ("The Hobbit", "J. R. R. Tolkien", Some(0x33)),
    ("Граф Монте-Кристо", "Александр Дюма", Some(0xdd)),
    ("The Book of Lost Tales I", "J. R. R. Tolkien", None),
    ("Легенды и мифы Древней Греции", "Николай Кун", Some(0x99)),
    ("Middlemarch", "George Eliot", None),
    ("The Left Hand of Darkness", "Ursula K. Le Guin", Some(0x33)),
    ("Pride and Prejudice", "Jane Austen", Some(0xdd)),
    ("A Wizard of Earthsea", "Ursula K. Le Guin", Some(0x99)),
];

pub struct Gallery {
    worker: Rc<Worker>,
    screen: Rect,
    top_bar: TopBar,
    pager: Pager,
    /// Section labels: (page, rect, text).
    labels: Vec<(usize, Rect, &'static str)>,

    // Page 1
    continue_btn: Button,
    details_btn: Button,
    menu_btn: Button,
    disabled_btn: Button,
    icon_buttons: Vec<IconButton>,
    toggles: [Toggle; 2],
    sort: Segmented,
    slider_label: Rect,
    slider: Slider,
    spike_row: List,

    // Page 2
    stepper: Stepper,
    size_pt: i32,
    settings: List,
    toast_btn: Button,
    sheet_btn: Button,

    // Page 3
    grid: Grid,

    // Overlays
    popup: Option<PopupMenu>,
    sheet: Option<TextSheet>,
}

impl Gallery {
    pub fn new(t: &Theme, screen: Rect, worker: Rc<Worker>) -> Self {
        let (m, w) = (t.margin, screen.w);
        let mut top_bar = TopBar::new("Widgets", true);
        top_bar.place(screen, t);
        let mut pager = Pager::new(PAGES);
        pager.place(Rect::new(m, screen.bottom() - t.mm(11.0), w - 2 * m, t.mm(11.0)), t);
        let top = t.top_bar + t.mm(4.0);
        let mut labels = Vec::new();
        let mut section = |page: usize, y: &mut i32, text: &'static str| {
            labels.push((page, Rect::new(m, *y, w - 2 * m, t.mm(4.0)), text));
            *y += t.mm(5.0);
        };
        let col = (w - 2 * m - t.mm(4.0)) / 2;
        let left = |y: i32| Rect::new(m, y, col, t.button_h);
        let right = |y: i32| Rect::new(m + col + t.mm(4.0), y, col, t.button_h);

        // ---- Page 1 ----
        let mut y = top;
        section(0, &mut y, "Buttons");
        let mut continue_btn = Button::new("Continue", ButtonKind::Primary);
        let mut details_btn = Button::new("Details", ButtonKind::Secondary);
        continue_btn.place(left(y), t);
        details_btn.place(right(y), t);
        y += t.button_h + t.mm(3.0);
        let mut menu_btn = Button::new("Menu", ButtonKind::Secondary).with_icon(Icon::Menu);
        let mut disabled_btn = Button::new("Disabled", ButtonKind::Secondary).disabled();
        menu_btn.place(left(y), t);
        disabled_btn.place(right(y), t);
        y += t.button_h + t.mm(3.0);
        let icon_buttons = ICON_BUTTONS
            .iter()
            .enumerate()
            .map(|(k, (icon, _))| {
                let mut b = IconButton::new(*icon);
                b.place_at(Point::new(m + t.target / 2 + k as i32 * (t.target + t.mm(3.0)), y + t.target / 2), t);
                b
            })
            .collect();
        y += t.target + t.mm(5.0);

        section(0, &mut y, "Toggle · Segmented");
        let mut toggles = [Toggle::new(true), Toggle::new(false)];
        toggles[0].place(m + t.mm(9.0), y + t.button_h / 2, t);
        toggles[1].place(m + t.mm(21.0), y + t.button_h / 2, t);
        let mut sort = Segmented::texts(&["Recent", "Title", "Author"], 0);
        sort.place(Rect::new(m + t.mm(28.0), y, w - 2 * m - t.mm(28.0), t.button_h));
        y += t.button_h + t.mm(6.0);

        let slider_label = Rect::new(m, y, w - 2 * m, t.mm(4.0));
        y += t.mm(5.0);
        let mut slider = Slider::new(0.0, 6.0, 1.0, 2.0).with_ticks();
        slider.place(Rect::new(m, y, w - 2 * m, t.mm(6.0)), t);
        y += t.mm(10.0);

        section(0, &mut y, "List row");
        let mut spike_row = List::new(vec![Row::new("Open reader").chevron()]);
        spike_row.place(Rect::new(0, y, w, t.row), t.row, m);

        // ---- Page 2 ----
        let mut y = top;
        section(1, &mut y, "Stepper");
        let size_pt = 10;
        let mut stepper = Stepper::new(
            SideLabel { text: "A".into(), style: TextStyle::new(t.pt(7.0), Weight::Regular, Family::Serif) },
            SideLabel { text: "A".into(), style: TextStyle::new(t.pt(11.0), Weight::Regular, Family::Serif) },
            format!("{size_pt} pt"),
        );
        stepper.place(Rect::new(m, y, w - 2 * m, t.button_h), t.mm(16.0));
        y += t.button_h + t.mm(6.0);

        section(1, &mut y, "List");
        let mut settings = List::new(vec![
            Row::new("Full refresh every").value("6 pages").chevron(),
            Row::new("Progress footer").toggle(true),
            Row::new("Publisher fonts").sub("Use fonts embedded in books").toggle(false),
            Row::new("Disabled row").value("—").chevron(),
        ]);
        settings.rows[3].enabled = false;
        settings.place(Rect::new(0, y, w, 4 * t.row2), t.row2, m);
        y += 4 * t.row2 + t.mm(6.0);

        section(1, &mut y, "Toast · Sheet");
        let mut toast_btn = Button::new("Show toast", ButtonKind::Secondary);
        let mut sheet_btn = Button::new("Text settings", ButtonKind::Secondary).with_icon(Icon::TextSize);
        toast_btn.place(left(y), t);
        sheet_btn.place(right(y), t);

        // ---- Page 3 ----
        let mut y = top;
        section(2, &mut y, "Grid · long-press for a menu");
        let mut grid = Grid::new(BOOKS.len(), 4, 2);
        // Cells are a 2:3 cover plus two lines of text under it.
        let (gap_x, gap_y) = (t.mm(3.5), t.mm(4.0));
        let cell_w = (w - 2 * m - 3 * gap_x) / 4;
        let cell_h = cell_w * 3 / 2 + t.mm(8.5);
        grid.place(Rect::new(m, y, w - 2 * m, 2 * cell_h + gap_y), gap_x, gap_y);

        Self {
            screen,
            worker,
            top_bar,
            pager,
            labels,
            continue_btn,
            details_btn,
            menu_btn,
            disabled_btn,
            icon_buttons,
            toggles,
            sort,
            slider_label,
            slider,
            spike_row,
            stepper,
            size_pt,
            settings,
            toast_btn,
            sheet_btn,
            grid,
            popup: None,
            sheet: None,
        }
    }

    fn page(&self) -> usize {
        self.pager.page
    }

    fn open_menu(&mut self, anchor: Point, t: &Theme) {
        let items = MENU_ITEMS.iter().map(|(icon, label)| (Some(*icon), (*label).to_owned())).collect();
        self.popup = Some(PopupMenu::open(items, anchor, self.screen, t));
    }

    fn page1_gesture(&mut self, g: &Gesture, cx: &mut EventCx) -> Action {
        if self.continue_btn.on_gesture(g) {
            cx.toast("Position saved", Some(Icon::Check));
        }
        if self.details_btn.on_gesture(g) {
            cx.toast("Details", None);
        }
        if self.menu_btn.on_gesture(g) {
            let r = self.menu_btn.rect();
            self.open_menu(Point::new(r.center().x, r.bottom()), cx.theme);
        }
        self.disabled_btn.on_gesture(g);
        for (b, (_, name)) in self.icon_buttons.iter_mut().zip(ICON_BUTTONS) {
            if b.on_gesture(g) {
                cx.toast(name, None);
            }
        }
        for toggle in &mut self.toggles {
            toggle.on_gesture(g);
        }
        if let Some(i) = self.sort.on_gesture(g) {
            log::debug!("sort by segment {i}");
        }
        if let Some(SliderEvent::Released(v)) = self.slider.on_gesture(g) {
            log::debug!("slider released at {v}");
        }
        if let Some(ListEvent::Tapped(_)) = self.spike_row.on_gesture(g) {
            return Action::Push(Box::new(Books::new(cx.theme, cx.screen, self.worker.clone())));
        }
        Action::None
    }

    fn page2_gesture(&mut self, g: &Gesture, cx: &mut EventCx) {
        if let Some(step) = self.stepper.on_gesture(g) {
            self.step_size(step);
        }
        if let Some(ListEvent::Tapped(i)) = self.settings.on_gesture(g) {
            let row = &mut self.settings.rows[i];
            match &mut row.accessory {
                crate::ui::widgets::list::Accessory::Toggle(on) => *on = !*on,
                _ => cx.toast(row.label.clone(), None),
            }
        }
        if self.toast_btn.on_gesture(g) {
            cx.toast("Position saved", Some(Icon::Check));
        }
        if self.sheet_btn.on_gesture(g) {
            self.sheet = Some(TextSheet::new(cx.theme, self.screen));
        }
    }

    fn page3_gesture(&mut self, g: &Gesture, cx: &mut EventCx) {
        match self.grid.on_gesture(g) {
            Some(GridEvent::Tapped(i)) => cx.toast(format!("Open {}", BOOKS[i].0), Some(Icon::BookOpen)),
            Some(GridEvent::LongPressed(i)) => {
                if let Some((_, r)) = self.grid.cells().into_iter().find(|(j, _)| *j == i) {
                    self.open_menu(r.center(), cx.theme);
                }
            }
            None => {}
        }
    }

    fn step_size(&mut self, step: Step) {
        self.size_pt = match step {
            Step::Less => (self.size_pt - 1).max(6),
            Step::More => (self.size_pt + 1).min(20),
        };
        self.stepper.value = format!("{} pt", self.size_pt);
        self.stepper.can_less = self.size_pt > 6;
        self.stepper.can_more = self.size_pt < 20;
    }

    fn draw_cover(cx: &mut DrawCx, index: usize, cell: Rect, pressed: bool) {
        let t = cx.theme;
        let (title, author, gray) = BOOKS[index];
        let cover = Rect::new(cell.x, cell.y, cell.w, (cell.w as f32 * 1.5) as i32);
        let (bg, fg) = match (pressed, gray) {
            (true, _) => (INK, PAPER),
            (false, Some(g)) => (g, if g < 0x80 { PAPER } else { INK }),
            (false, None) => (PAPER, INK),
        };
        let radius = t.mm(0.6) as f32;
        cx.canvas.fill_rounded_rect(cover, radius, bg);
        if gray.is_none() && !pressed {
            cx.canvas.stroke_rounded_rect(cover, radius, t.hairline as f32, INK);
        }
        if pressed {
            cx.hint(cell, RefreshMode::Fast);
        }
        // Title in the upper part, a short rule, the author below.
        let pad = (cover.w as f32 * 0.12) as i32;
        let mut title_style = TextStyle::new((cover.w as f32 * 0.13).round(), Weight::SemiBold, Family::Serif).gray(fg);
        let title_box = Rect::new(cover.x + pad, cover.y + cover.h / 10, cover.w - 2 * pad, cover.h * 2 / 5);
        // Shrink the title until its longest word fits on a line.
        let longest = title.split_whitespace().map(|word| cx.text.width(word, title_style)).fold(0.0, f32::max);
        if longest > title_box.w as f32 {
            title_style.size = (title_style.size * title_box.w as f32 / longest).floor();
        }
        cx.text.draw_lines(cx.canvas, title, title_style, title_box, HAlign::Center, 4);
        let rule_w = cover.w / 5;
        cx.fill(Rect::new(cover.center().x - rule_w / 2, cover.y + cover.h * 62 / 100, rule_w, 3), fg);
        let author_style =
            TextStyle::new((cover.w as f32 * 0.065).round(), Weight::Regular, Family::Sans).gray(fg).tracking(0.08);
        let author_box = Rect::new(cover.x + pad, cover.y + cover.h * 72 / 100, cover.w - 2 * pad, cover.h / 8);
        cx.line(&author.to_uppercase(), author_style, author_box, HAlign::Center);

        // Under the cover: title and author.
        let below = Rect::new(cell.x, cover.bottom() + t.mm(1.0), cell.w, t.mm(3.5));
        cx.line(title, t.small.weight(Weight::SemiBold), below, HAlign::Left);
        cx.line(author, t.caption.gray(INK2), below.moved(0, t.mm(3.4)), HAlign::Left);
    }
}

impl Screen for Gallery {
    fn draw(&mut self, cx: &mut DrawCx) {
        let page = self.page();
        self.top_bar.draw(cx);
        for (_, rect, text) in self.labels.iter().filter(|(p, ..)| *p == page) {
            cx.section_label(text, *rect);
        }
        match page {
            0 => {
                self.continue_btn.draw(cx);
                self.details_btn.draw(cx);
                self.menu_btn.draw(cx);
                self.disabled_btn.draw(cx);
                for b in &self.icon_buttons {
                    b.draw(cx);
                }
                for toggle in &self.toggles {
                    toggle.draw(cx);
                }
                self.sort.draw(cx);
                cx.section_label(&format!("Slider · {}", self.slider.value), self.slider_label);
                self.slider.draw(cx);
                self.spike_row.draw(cx);
            }
            1 => {
                self.stepper.draw(cx);
                self.settings.draw(cx);
                self.toast_btn.draw(cx);
                self.sheet_btn.draw(cx);
            }
            _ => self.grid.draw(cx, Self::draw_cover),
        }
        self.pager.draw(cx);
        if let Some(sheet) = &self.sheet {
            sheet.draw(cx);
        }
        if let Some(popup) = &self.popup {
            popup.draw(cx);
        }
    }

    fn on_gesture(&mut self, g: Gesture, cx: &mut EventCx) -> Action {
        if let Some(sheet) = &mut self.sheet {
            if sheet.on_gesture(&g, cx) {
                self.sheet = None;
            }
            return Action::None;
        }
        if let Some(popup) = &mut self.popup {
            match popup.on_gesture(&g, cx.theme) {
                Some(MenuEvent::Chosen(i)) => {
                    cx.toast(MENU_ITEMS[i].1, Some(MENU_ITEMS[i].0));
                    self.popup = None;
                }
                Some(MenuEvent::Dismissed) => self.popup = None,
                None => {}
            }
            return Action::None;
        }
        if self.top_bar.on_gesture(&g) {
            return Action::Exit;
        }
        self.pager.on_gesture(&g);
        if let Gesture::Swipe { dir, from, .. } = g {
            let on_slider = self.page() == 0 && self.slider.bounds().contains(from);
            if !on_slider {
                self.pager.on_swipe(dir);
            }
        }
        match self.page() {
            0 => return self.page1_gesture(&g, cx),
            1 => self.page2_gesture(&g, cx),
            _ => self.page3_gesture(&g, cx),
        }
        Action::None
    }

    fn deadline(&self) -> Option<std::time::Instant> {
        let sheet = self.sheet.as_ref().and_then(|s| s.size.deadline());
        [self.stepper.deadline(), sheet].into_iter().flatten().min()
    }

    fn on_tick(&mut self, cx: &mut EventCx) -> Action {
        if let Some(step) = self.stepper.on_tick(cx.now) {
            self.step_size(step);
        }
        if let Some(sheet) = &mut self.sheet {
            sheet.on_tick(cx.now);
        }
        Action::None
    }
}

/// The text settings sheet from mockup 6. In the gallery its controls only
/// change themselves; the reader (Phase 4) applies them to the book.
struct TextSheet {
    sheet: Sheet,
    rows: Vec<(Rect, &'static str)>,
    font: Rect,
    font_press: Press,
    size: Stepper,
    size_pt: i32,
    spacing: Segmented,
    margins: Segmented,
    justify: Toggle,
}

impl TextSheet {
    fn new(t: &Theme, screen: Rect) -> Self {
        let sheet = Sheet::open("Text", t.mm(76.0), screen, t);
        let m = t.margin;
        let row_h = t.mm(11.5);
        let top = sheet.rect().y + t.mm(13.0);
        let names = ["Font", "Size", "Spacing", "Margins", "Justify"];
        let rows: Vec<(Rect, &str)> = names
            .iter()
            .enumerate()
            .map(|(k, name)| (Rect::new(m, top + k as i32 * row_h, screen.w - 2 * m, row_h), *name))
            .collect();
        let ctrl_x = m + t.mm(30.0);
        let control = |k: usize| {
            let r = rows[k].0;
            Rect::new(ctrl_x, r.y + (r.h - t.button_h) / 2, screen.w - m - ctrl_x, t.button_h)
        };
        let size_pt = 10;
        let mut size = Stepper::new(
            SideLabel { text: "A".into(), style: TextStyle::new(t.pt(7.0), Weight::Regular, Family::Serif) },
            SideLabel { text: "A".into(), style: TextStyle::new(t.pt(11.0), Weight::Regular, Family::Serif) },
            format!("{size_pt} pt"),
        );
        size.place(control(1), t.mm(16.0));
        let mut spacing = Segmented::new(
            vec![Segment::Glyph(spacing_tight), Segment::Glyph(spacing_normal), Segment::Glyph(spacing_loose)],
            1,
        );
        spacing.place(control(2));
        let mut margins = Segmented::new(
            vec![Segment::Glyph(margins_narrow), Segment::Glyph(margins_normal), Segment::Glyph(margins_wide)],
            1,
        );
        margins.place(control(3));
        let mut justify = Toggle::new(true);
        justify.place(screen.w - m, rows[4].0.center().y, t);
        Self { font: control(0), sheet, rows, font_press: Press::default(), size, size_pt, spacing, margins, justify }
    }

    /// True when the sheet should close.
    fn on_gesture(&mut self, g: &Gesture, cx: &mut EventCx) -> bool {
        if self.sheet.on_gesture(g) {
            return true;
        }
        if let Some(PressEvent::Tap) = self.font_press.on_gesture(g, self.font) {
            cx.toast("The font list comes with the reader", None);
        }
        if let Some(step) = self.size.on_gesture(g) {
            self.step(step);
        }
        self.spacing.on_gesture(g);
        self.margins.on_gesture(g);
        self.justify.on_gesture(g);
        false
    }

    fn on_tick(&mut self, now: std::time::Instant) {
        if let Some(step) = self.size.on_tick(now) {
            self.step(step);
        }
    }

    fn step(&mut self, step: Step) {
        self.size_pt = match step {
            Step::Less => (self.size_pt - 1).max(6),
            Step::More => (self.size_pt + 1).min(20),
        };
        self.size.value = format!("{} pt", self.size_pt);
        self.size.can_less = self.size_pt > 6;
        self.size.can_more = self.size_pt < 20;
    }

    fn draw(&self, cx: &mut DrawCx) {
        let t = cx.theme;
        self.sheet.draw(cx);
        for (r, name) in &self.rows {
            cx.line(name, t.body.gray(INK2), *r, HAlign::Left);
        }
        // The font field: the current font, in itself, and a chevron.
        let (fg, fg2) = if self.font_press.is_down() {
            cx.canvas.fill_rounded_rect(self.font, t.radius, INK);
            cx.hint(self.font, RefreshMode::Fast);
            (PAPER, PAPER)
        } else {
            cx.canvas.stroke_rounded_rect(self.font, t.radius, t.border as f32, INK);
            (INK, INK2)
        };
        let inner = self.font.inset_xy(t.mm(4.0), 0);
        let style = TextStyle::new((t.body.size * 1.15).round(), Weight::Regular, Family::Serif).gray(fg);
        cx.line("Literata", style, inner, HAlign::Left);
        let chevron = inner.split_right(t.icon_small).0;
        cx.icon(Icon::ChevronRight, chevron, t.icon_small, fg2);
        self.size.draw(cx);
        self.spacing.draw(cx);
        self.margins.draw(cx);
        self.justify.draw(cx);
    }
}

/// Three lines, `gap` apart: the line spacing choices.
fn spacing_glyph(cx: &mut DrawCx, seg: Rect, gray: u8, gap_mm: f32) {
    let (w, gap) = (cx.theme.mm(5.0), cx.theme.mm(gap_mm));
    let c = seg.center();
    for k in -1..=1 {
        cx.canvas.fill_rounded_rect(Rect::new(c.x - w / 2, c.y + k * gap - 2, w, 4), 2.0, gray);
    }
}

fn spacing_tight(cx: &mut DrawCx, seg: Rect, gray: u8) {
    spacing_glyph(cx, seg, gray, 1.1);
}

fn spacing_normal(cx: &mut DrawCx, seg: Rect, gray: u8) {
    spacing_glyph(cx, seg, gray, 1.5);
}

fn spacing_loose(cx: &mut DrawCx, seg: Rect, gray: u8) {
    spacing_glyph(cx, seg, gray, 1.9);
}

/// A page outline with text lines inset by `inset`: the margin choices.
fn margins_glyph(cx: &mut DrawCx, seg: Rect, gray: u8, inset_mm: f32) {
    let t = cx.theme;
    let page = seg.center_box(t.mm(5.0), t.mm(6.0));
    cx.canvas.stroke_rounded_rect(page, 4.0, 3.0, gray);
    let inset = t.mm(inset_mm);
    for k in 0..4 {
        let y = page.y + t.mm(1.3) + k * t.mm(1.1);
        cx.fill(Rect::new(page.x + inset, y, page.w - 2 * inset, 3), gray);
    }
}

fn margins_narrow(cx: &mut DrawCx, seg: Rect, gray: u8) {
    margins_glyph(cx, seg, gray, 0.6);
}

fn margins_normal(cx: &mut DrawCx, seg: Rect, gray: u8) {
    margins_glyph(cx, seg, gray, 1.1);
}

fn margins_wide(cx: &mut DrawCx, seg: Rect, gray: u8) {
    margins_glyph(cx, seg, gray, 1.6);
}
