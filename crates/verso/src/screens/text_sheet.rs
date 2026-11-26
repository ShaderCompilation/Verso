//! Typography and font picker, with the page left visible above them.
use crate::document::{FontFace, Paragraphs, Steps, Typography};
use crate::geom::Rect;
use crate::gfx::{Family, HAlign, Weight};
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK, INK2, PAPER, Theme};
use crate::ui::widget::{DrawCx, Press, PressEvent};
use crate::ui::widgets::{List, ListEvent, Pager, Row, Segmented, Sheet, SideLabel, Step, Stepper, Toggle};
use std::time::Instant;

pub enum TextEvent {
    Close,
    Fonts,
    Changed,
}

/// The rows of each page, in order of how often they're changed.
const PAGES: [[&str; 5]; 2] = [
    ["Font", "Size", "Weight", "Line spacing", "Hyphenation"],
    ["Contrast", "Word spacing", "Paragraphs", "Justify", "Book styles"],
];
const ROW_MM: f32 = 11.5;

/// A number stepped through a range: size, weight, contrast and spacing.
struct Number {
    stepper: Stepper,
    steps: Steps,
    get: fn(&Typography) -> i32,
    set: fn(&mut Typography, i32),
    show: fn(i32) -> String,
}
impl Number {
    fn sync(&mut self, t: &Typography) {
        let v = (self.get)(t);
        self.stepper.value = (self.show)(v);
        self.stepper.can_less = v > self.steps.min;
        self.stepper.can_more = v < self.steps.max;
    }
    fn step(&mut self, step: Step, t: &mut Typography) {
        (self.set)(t, self.steps.next((self.get)(t), step == Step::More));
        self.sync(t);
    }
}

pub struct TextSheet {
    sheet: Sheet,
    /// Page, row and label.
    rows: Vec<(usize, Rect, &'static str)>,
    font: Rect,
    press: Press,
    /// Page and stepper.
    numbers: Vec<(usize, Number)>,
    paragraphs: Segmented,
    justify: Toggle,
    hyphenate: Toggle,
    book_styles: Toggle,
    pager: Pager,
}
impl TextSheet {
    /// The sheet's height, from the bottom of the screen.
    pub const HEIGHT_MM: f32 = 83.0;

    pub fn new(t: &Theme, screen: Rect, typography: &Typography) -> Self {
        let sheet = Sheet::open("Text", t.mm(Self::HEIGHT_MM), screen, t);
        let top = sheet.content(t).y;
        let row_h = t.mm(ROW_MM);
        let rows: Vec<_> = PAGES
            .iter()
            .enumerate()
            .flat_map(|(page, labels)| {
                labels.iter().enumerate().map(move |(i, label)| {
                    (page, Rect::new(t.margin, top + i as i32 * row_h, screen.w - 2 * t.margin, row_h), *label)
                })
            })
            .collect();
        let row = |label: &str| rows.iter().find(|r| r.2 == label).map(|r| (r.0, r.1)).unwrap();
        // Controls start after the labels, and stay at most 72 mm wide on
        // large screens (10.2", landscape) instead of stretching edge to edge.
        let x = (t.margin + t.mm(32.0)).max(screen.w - t.margin - t.mm(72.0));
        let control = |label: &str| {
            let (_, r) = row(label);
            Rect::new(x, r.y + (r.h - t.button_h) / 2, screen.w - t.margin - x, t.button_h)
        };
        let serif = t.body.family(Family::Serif);
        let sign = |text: &str| SideLabel { text: text.into(), style: t.heading };
        let number = |label: &'static str, less: SideLabel, more: SideLabel, steps, get, set, show| {
            let mut stepper = Stepper::new(less, more, "");
            stepper.place(control(label), t.mm(16.0));
            let mut n = Number { stepper, steps, get, set, show };
            n.sync(typography);
            (row(label).0, n)
        };
        let numbers = vec![
            number(
                "Size",
                SideLabel { text: "A".into(), style: serif },
                SideLabel { text: "A".into(), style: t.heading.family(Family::Serif) },
                Typography::SIZE,
                |t| t.size_pt.round() as i32,
                |t, v| t.size_pt = v as f32,
                |v| format!("{v} pt"),
            ),
            number(
                "Weight",
                SideLabel { text: "A".into(), style: serif },
                SideLabel { text: "A".into(), style: serif.weight(Weight::SemiBold) },
                Typography::WEIGHT,
                |t| t.weight,
                |t, v| t.weight = v,
                |v| v.to_string(),
            ),
            number(
                "Contrast",
                sign("−"),
                sign("+"),
                Typography::CONTRAST,
                |t| t.contrast,
                |t, v| t.contrast = v,
                |v| match v {
                    0 => "0".into(),
                    v if v < 0 => format!("−{}", -v),
                    v => format!("+{v}"),
                },
            ),
            number(
                "Line spacing",
                sign("−"),
                sign("+"),
                Typography::LINE_SPACING,
                |t| t.line_spacing,
                |t, v| t.line_spacing = v,
                |v| format!("{v}%"),
            ),
            number(
                "Word spacing",
                sign("−"),
                sign("+"),
                Typography::WORD_SPACING,
                |t| t.word_spacing,
                |t, v| t.word_spacing = v,
                |v| format!("{v}%"),
            ),
        ];
        let selected = Paragraphs::ALL.iter().position(|&p| p == typography.paragraphs).unwrap_or(0);
        let mut paragraphs = Segmented::texts(&["Book", "Indent", "Spaced"], selected);
        paragraphs.place(control("Paragraphs"));
        let toggle = |on: bool, label: &str| {
            let mut toggle = Toggle::new(on);
            toggle.place(screen.w - t.margin, row(label).1.center().y, t);
            toggle
        };
        let mut pager = Pager::new(PAGES.len());
        pager.place(Rect::new(t.margin, top + 5 * row_h, screen.w - 2 * t.margin, t.mm(11.0)), t);
        Self {
            font: control("Font"),
            justify: toggle(typography.justify, "Justify"),
            hyphenate: toggle(typography.hyphenate, "Hyphenation"),
            book_styles: toggle(typography.book_styles, "Book styles"),
            sheet,
            rows,
            press: Press::default(),
            numbers,
            paragraphs,
            pager,
        }
    }
    fn page(&self, label: &str) -> usize {
        self.rows.iter().find(|r| r.2 == label).map_or(0, |r| r.0)
    }
    pub fn deadline(&self) -> Option<Instant> {
        self.numbers.iter().filter_map(|(_, n)| n.stepper.deadline()).min()
    }
    pub fn on_tick(&mut self, now: Instant, t: &mut Typography) -> bool {
        let mut stepped = false;
        for (_, n) in &mut self.numbers {
            if let Some(step) = n.stepper.on_tick(now) {
                n.step(step, t);
                stepped = true;
            }
        }
        stepped
    }
    #[allow(clippy::type_complexity)]
    pub fn visual_state(&self) -> (bool, bool, Vec<Option<Step>>, (usize, usize, bool, bool, bool)) {
        (
            self.sheet.is_pressed(),
            self.press.is_down(),
            self.numbers.iter().map(|(_, n)| n.stepper.pressed()).collect(),
            self.pager.visual_state(),
        )
    }

    pub fn on_gesture(&mut self, g: &Gesture, t: &mut Typography) -> Option<TextEvent> {
        if self.sheet.on_gesture(g) {
            return Some(TextEvent::Close);
        }
        self.pager.on_gesture(g);
        if let Gesture::Swipe { dir, .. } = g {
            self.pager.on_swipe(*dir);
        }
        let page = self.pager.page;
        if page == self.page("Font")
            && let Some(PressEvent::Tap) = self.press.on_gesture(g, self.font)
        {
            return Some(TextEvent::Fonts);
        }
        let before = t.clone();
        for (p, n) in &mut self.numbers {
            if *p == page
                && let Some(step) = n.stepper.on_gesture(g)
            {
                n.step(step, t);
            }
        }
        if page == self.page("Paragraphs")
            && let Some(i) = self.paragraphs.on_gesture(g)
        {
            t.paragraphs = Paragraphs::ALL[i];
        }
        if page == self.page("Justify") && self.justify.on_gesture(g).is_some() {
            t.justify = self.justify.on;
        }
        if page == self.page("Hyphenation") && self.hyphenate.on_gesture(g).is_some() {
            t.hyphenate = self.hyphenate.on;
        }
        if page == self.page("Book styles") && self.book_styles.on_gesture(g).is_some() {
            t.book_styles = self.book_styles.on;
        }
        (before != *t).then_some(TextEvent::Changed)
    }
    pub fn draw(&mut self, cx: &mut DrawCx, t: &Typography) {
        self.sheet.draw(cx);
        let theme = cx.theme;
        let page = self.pager.page;
        for (_, r, label) in self.rows.iter().filter(|r| r.0 == page) {
            cx.line(label, theme.body, *r, HAlign::Left);
        }
        if page == self.page("Font") {
            let color = if self.press.is_down() {
                cx.fill(self.font, INK);
                PAPER
            } else {
                INK2
            };
            cx.line(&t.font, cx.theme.body.gray(color), self.font, HAlign::Right);
        }
        for (_, n) in self.numbers.iter().filter(|(p, _)| *p == page) {
            n.stepper.draw(cx);
        }
        if page == self.page("Paragraphs") {
            self.paragraphs.draw(cx);
        }
        for (label, toggle) in
            [("Justify", &self.justify), ("Hyphenation", &self.hyphenate), ("Book styles", &self.book_styles)]
        {
            if page == self.page(label) {
                toggle.draw(cx);
            }
        }
        self.pager.draw(cx);
    }
}

pub struct FontList {
    sheet: Sheet,
    list: List,
    pager: Pager,
    fonts: Vec<FontFace>,
    area: Rect,
}
pub enum FontEvent {
    Close,
    Chosen(String),
}
impl FontList {
    pub fn new(t: &Theme, screen: Rect, fonts: &[FontFace], publisher: bool, current: &str) -> Self {
        let sheet = Sheet::open("Font", t.mm(96.0), screen, t);
        let mut fonts: Vec<_> = fonts
            .iter()
            .filter(|f| {
                let lower = f.name.to_ascii_lowercase();
                ![
                    "symbol",
                    "emoji",
                    "cjk",
                    "chinese",
                    "japanese",
                    "korean",
                    " ui",
                    "heisei",
                    "code2000",
                    "hans",
                    "hant",
                    "droid sans fallback",
                    "kindle",
                    "amazon ember",
                ]
                .iter()
                .any(|n| lower.contains(n))
            })
            .cloned()
            .collect();
        fonts.sort_by(|a, b| a.name.cmp(&b.name));
        fonts.dedup_by(|a, b| a.name == b.name);
        if publisher {
            fonts.insert(0, FontFace { name: "Publisher font".into(), file: None });
        }
        let mut pager = Pager::new(1);
        pager.place(Rect::new(t.margin, screen.bottom() - t.mm(11.0), screen.w - 2 * t.margin, t.mm(11.0)), t);
        let c = sheet.content(t);
        let area = Rect::new(0, c.y, screen.w, pager.rect().y - c.y);
        let mut list =
            List::new(fonts.iter().map(|f| Row::new("").value(if f.name == current { "✓" } else { "" })).collect());
        list.place(area, t.row, t.margin);
        let index = fonts.iter().position(|f| f.name == current).unwrap_or(0);
        let per = (area.h / t.row).max(1) as usize;
        pager.set(index / per, list.paging().pages());
        list.page = pager.page;
        Self { sheet, list, pager, fonts, area }
    }
    pub fn draw(&mut self, cx: &mut DrawCx) {
        self.sheet.draw(cx);
        self.list.draw(cx);
        let t = cx.theme;
        let per = (self.area.h / t.row).max(1) as usize;
        for (k, i) in self.list.paging().range(self.list.page).enumerate() {
            let f = &self.fonts[i];
            let family = cx.text.book_family(&f.name, f.file.as_deref());
            let rect = Rect::new(
                t.margin,
                self.area.y + (k % per) as i32 * t.row,
                self.area.w - 2 * t.margin - t.mm(8.0),
                t.row,
            );
            let gray = if self.list.is_pressed(i) { PAPER } else { INK };
            cx.line(&f.name, t.body.family(family).gray(gray), rect, HAlign::Left);
        }
        self.pager.draw(cx);
    }
    #[allow(clippy::type_complexity)]
    pub fn visual_state(&self) -> (bool, (usize, usize, bool, bool, bool), (usize, Option<usize>, bool)) {
        (self.sheet.is_pressed(), self.pager.visual_state(), self.list.visual_state())
    }

    pub fn on_gesture(&mut self, g: &Gesture) -> Option<FontEvent> {
        if self.sheet.on_gesture(g) {
            return Some(FontEvent::Close);
        }
        self.pager.on_gesture(g);
        if let Gesture::Swipe { dir, .. } = g {
            self.pager.on_swipe(*dir);
        }
        self.list.page = self.pager.page;
        if let Some(ListEvent::Tapped(i)) = self.list.on_gesture(g) {
            return Some(FontEvent::Chosen(self.fonts[i].name.clone()));
        }
        None
    }
}

/// The Text sheet and a font list of several pages, for `screens/fit_tests.rs`.
#[cfg(test)]
pub(super) fn fit_samples(t: &Theme, screen: Rect) -> Vec<crate::ui::fit::Parts> {
    use crate::ui::fit::Parts;
    let text = TextSheet::new(t, screen, &Typography::default());
    let fonts: Vec<_> = (0..40).map(|i| FontFace { name: format!("Font {i:02}"), file: None }).collect();
    let list = FontList::new(t, screen, &fonts, true, "Font 20");
    // One sample per page: the pages share the same rows.
    let mut pages = vec![vec![text.font, text.pager.rect()], vec![text.paragraphs.rect(), text.pager.rect()]];
    for (page, n) in &text.numbers {
        pages[*page].push(n.stepper.rect());
    }
    for r in text.rows.iter().filter(|r| ["Justify", "Hyphenation", "Book styles"].contains(&r.2)) {
        pages[r.0].push(r.1);
    }
    let [first, second] = <[_; 2]>::try_from(pages).unwrap();
    vec![
        Parts::new("TextSheet", text.sheet.rect(), first),
        Parts::new("TextSheet page 2", text.sheet.rect(), second),
        // The list's rows reach the screen edges by design: only the pager is a button here.
        Parts::new("FontList", list.sheet.rect(), [list.pager.rect()]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Point;

    #[test]
    fn controls_stay_compact_on_large_screens() {
        let t = Theme::for_dpi(300);
        for screen in [Rect::new(0, 0, 1860, 2480), Rect::new(0, 0, 2480, 1860), Rect::new(0, 0, 1696, 1272)] {
            let sheet = TextSheet::new(&t, screen, &Typography::default());
            let controls = sheet.numbers.iter().map(|(_, n)| n.stepper.rect()).chain([sheet.paragraphs.rect()]);
            for control in controls {
                assert!(control.w <= t.mm(72.0), "{screen:?}: {control:?}");
                assert_eq!(control.right(), screen.w - t.margin, "{screen:?}");
            }
        }
        // 6" screens keep the labels' 32 mm and give the rest to the controls.
        let t = Theme::for_dpi(167);
        let sheet = TextSheet::new(&t, Rect::new(0, 0, 600, 800), &Typography::default());
        assert_eq!(sheet.numbers[0].1.stepper.rect().x, t.margin + t.mm(32.0));
    }

    #[test]
    fn only_the_shown_page_takes_touches() {
        let t = Theme::for_dpi(300);
        let mut sheet = TextSheet::new(&t, Rect::new(0, 0, 1272, 1696), &Typography::default());
        let mut typography = Typography::default();
        let tap = |sheet: &mut TextSheet, typography: &mut Typography, p: Point| {
            sheet.on_gesture(&Gesture::Down(p), typography);
            sheet.on_gesture(&Gesture::Tap(p), typography)
        };
        // Both pages end in a switch: Hyphenation, then Book styles in its place.
        let last = sheet.hyphenate.rect().center();
        assert!(matches!(tap(&mut sheet, &mut typography, last), Some(TextEvent::Changed)));
        assert_eq!((typography.hyphenate, typography.book_styles), (false, true));
        sheet.pager.page = 1;
        assert!(matches!(tap(&mut sheet, &mut typography, last), Some(TextEvent::Changed)));
        assert_eq!((typography.hyphenate, typography.book_styles), (false, false));
    }
}
