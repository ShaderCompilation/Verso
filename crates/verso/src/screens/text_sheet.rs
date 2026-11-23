//! Typography and font picker, with the page left visible above them.
use crate::document::{FontFace, Typography};
use crate::geom::Rect;
use crate::gfx::{Family, HAlign};
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
pub struct TextSheet {
    sheet: Sheet,
    rows: Vec<(Rect, &'static str)>,
    font: Rect,
    press: Press,
    size: Stepper,
    spacing: Segmented,
    justify: Toggle,
}
impl TextSheet {
    pub fn new(t: &Theme, screen: Rect, typography: &Typography) -> Self {
        let sheet = Sheet::open("Text", t.mm(64.0), screen, t);
        let top = sheet.content(t).y;
        let rows: Vec<_> = ["Font", "Size", "Spacing", "Justify"]
            .into_iter()
            .enumerate()
            .map(|(i, n)| (Rect::new(t.margin, top + i as i32 * t.mm(11.5), screen.w - 2 * t.margin, t.mm(11.5)), n))
            .collect();
        // Controls start after the labels, and stay at most 72 mm wide on
        // large screens (10.2", landscape) instead of stretching edge to edge.
        let x = (t.margin + t.mm(32.0)).max(screen.w - t.margin - t.mm(72.0));
        let control =
            |i: usize| Rect::new(x, rows[i].0.y + (rows[i].0.h - t.button_h) / 2, screen.w - t.margin - x, t.button_h);
        let mut size = Stepper::new(
            SideLabel { text: "A".into(), style: t.body.family(Family::Serif) },
            SideLabel { text: "A".into(), style: t.heading.family(Family::Serif) },
            format!("{} pt", typography.size_pt),
        );
        size.place(control(1), t.mm(16.0));
        let mut spacing = Segmented::texts(&["Tight", "Normal", "Loose"], typography.spacing);
        spacing.place(control(2));
        let mut justify = Toggle::new(typography.justify);
        justify.place(screen.w - t.margin, rows[3].0.center().y, t);
        let mut result = Self { font: control(0), sheet, rows, press: Press::default(), size, spacing, justify };
        result.sync(typography);
        result
    }
    fn sync(&mut self, t: &Typography) {
        self.size.value = format!("{} pt", t.size_pt);
        self.size.can_less = t.size_pt > 6.0;
        self.size.can_more = t.size_pt < 24.0;
    }
    fn step(&mut self, step: Step, t: &mut Typography) {
        t.size_pt = (t.size_pt + if step == Step::Less { -1.0 } else { 1.0 }).clamp(6.0, 24.0);
        self.sync(t);
    }
    pub fn deadline(&self) -> Option<Instant> {
        self.size.deadline()
    }
    pub fn on_tick(&mut self, now: Instant, t: &mut Typography) -> bool {
        if let Some(step) = self.size.on_tick(now) {
            self.step(step, t);
            true
        } else {
            false
        }
    }
    pub fn visual_state(&self) -> (bool, bool, Option<Step>) {
        (self.sheet.is_pressed(), self.press.is_down(), self.size.pressed())
    }

    pub fn on_gesture(&mut self, g: &Gesture, t: &mut Typography) -> Option<TextEvent> {
        if self.sheet.on_gesture(g) {
            return Some(TextEvent::Close);
        }
        if let Some(PressEvent::Tap) = self.press.on_gesture(g, self.font) {
            return Some(TextEvent::Fonts);
        }
        let before = t.clone();
        if let Some(step) = self.size.on_gesture(g) {
            self.step(step, t);
        }
        if let Some(i) = self.spacing.on_gesture(g) {
            t.spacing = i;
        }
        if self.justify.on_gesture(g).is_some() {
            t.justify = self.justify.on;
        }
        (before != *t).then_some(TextEvent::Changed)
    }
    pub fn draw(&self, cx: &mut DrawCx, t: &Typography) {
        self.sheet.draw(cx);
        let theme = cx.theme;
        for (r, label) in &self.rows {
            cx.line(label, theme.body, *r, HAlign::Left);
        }
        let color = if self.press.is_down() {
            cx.fill(self.font, INK);
            PAPER
        } else {
            INK2
        };
        cx.line(&t.font, cx.theme.body.gray(color), self.font, HAlign::Right);
        self.size.draw(cx);
        self.spacing.draw(cx);
        self.justify.draw(cx);
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
    vec![
        Parts::new("TextSheet", text.sheet.rect(), [text.font, text.size.rect(), text.spacing.rect(), text.rows[3].0]),
        // The list's rows reach the screen edges by design: only the pager is a button here.
        Parts::new("FontList", list.sheet.rect(), [list.pager.rect()]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_stay_compact_on_large_screens() {
        let t = Theme::for_dpi(300);
        for screen in [Rect::new(0, 0, 1860, 2480), Rect::new(0, 0, 2480, 1860), Rect::new(0, 0, 1696, 1272)] {
            let sheet = TextSheet::new(&t, screen, &Typography::default());
            for control in [sheet.size.rect(), sheet.spacing.rect()] {
                assert!(control.w <= t.mm(72.0), "{screen:?}: {control:?}");
                assert_eq!(control.right(), screen.w - t.margin, "{screen:?}");
            }
        }
        // 6" screens keep the labels' 32 mm and give the rest to the controls.
        let t = Theme::for_dpi(167);
        let sheet = TextSheet::new(&t, Rect::new(0, 0, 600, 800), &Typography::default());
        assert_eq!(sheet.size.rect().x, t.margin + t.mm(32.0));
    }

    #[test]
    fn text_sheet_holds_only_the_four_typography_rows() {
        let sheet = TextSheet::new(&Theme::for_dpi(300), Rect::new(0, 0, 1272, 1696), &Typography::default());
        let labels: Vec<_> = sheet.rows.iter().map(|(_, label)| *label).collect();
        assert_eq!(labels, ["Font", "Size", "Spacing", "Justify"]);
    }
}
