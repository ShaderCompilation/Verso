//! The end-of-book sheet. A forward tap on the
//! last page opens it over the page; the book is already marked finished.
//! *Undo* unmarks it, *Next* opens the next book in the folder, *Home* closes
//! the book. A tap outside or the × closes it, and the book stays finished.
use crate::geom::Rect;
use crate::gfx::{HAlign, Icon};
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK, INK2, Theme};
use crate::ui::widget::DrawCx;
use crate::ui::widgets::{Button, ButtonKind, Sheet};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum EndEvent {
    Close,
    Undo,
    Next,
    Home,
}

pub(super) struct EndSheet {
    sheet: Sheet,
    title: String,
    undo: Button,
    home: Button,
    next: Option<Button>,
}

/// Header row, as on the reader's other sheets.
const HEADER_MM: f32 = 13.0;
/// The book title's line under the header.
const TITLE_MM: f32 = 7.0;

impl EndSheet {
    /// `title` is the finished book's; `next` the next book's, when the
    /// folder has one.
    pub fn new(t: &Theme, screen: Rect, title: &str, next: Option<&str>) -> Self {
        let (gap, pad) = (t.mm(3.0), t.mm(4.0));
        // In landscape or on a wide screen the three buttons share one row; otherwise Next
        // has a row of its own above Undo and Home.
        let one_row = next.is_none() || screen.w > screen.h || screen.w - 2 * t.margin >= t.mm(150.0);
        let rows = if one_row { 1 } else { 2 };
        let h = t.mm(HEADER_MM) + t.mm(TITLE_MM) + gap + rows * t.button_h + (rows - 1) * gap + pad;
        let sheet = Sheet::open("", h, screen, t);
        let content = sheet.content(t);
        let bottom = Rect::new(content.x, sheet.rect().bottom() - pad - t.button_h, content.w, t.button_h);
        let mut undo = Button::new("Undo", ButtonKind::Secondary);
        let mut home = Button::new("Home", if next.is_some() { ButtonKind::Secondary } else { ButtonKind::Primary });
        let mut next = next.map(|title| Button::new(format!("Next: {title}"), ButtonKind::Primary));
        // Undo and Home split what Next leaves of the bottom row.
        let pair = match &mut next {
            Some(button) if one_row => {
                let (left, right) = (bottom.w / 2 - gap / 2, bottom.w - bottom.w / 2 - gap / 2);
                button.place(Rect { x: bottom.right() - right, w: right, ..bottom }, t);
                Rect { w: left, ..bottom }
            }
            Some(button) => {
                button.place(Rect { y: bottom.y - gap - t.button_h, ..bottom }, t);
                bottom
            }
            None => bottom,
        };
        let half = (pair.w - gap) / 2;
        undo.place(Rect { w: half, ..pair }, t);
        home.place(Rect { x: pair.right() - half, w: half, ..pair }, t);
        Self { sheet, title: title.to_owned(), undo, home, next }
    }

    #[cfg(test)]
    pub fn rect(&self) -> Rect {
        self.sheet.rect()
    }

    /// Undo, Home and Next (when shown).
    #[cfg(test)]
    pub fn buttons(&self) -> (Rect, Rect, Option<Rect>) {
        (self.undo.rect(), self.home.rect(), self.next.as_ref().map(Button::rect))
    }

    pub fn visual_state(&self) -> (bool, bool, bool, bool) {
        (
            self.sheet.is_pressed(),
            self.undo.is_pressed(),
            self.home.is_pressed(),
            self.next.as_ref().is_some_and(Button::is_pressed),
        )
    }

    pub fn on_gesture(&mut self, g: &Gesture) -> Option<EndEvent> {
        if self.sheet.on_gesture(g) {
            return Some(EndEvent::Close);
        }
        if self.undo.on_gesture(g) {
            return Some(EndEvent::Undo);
        }
        if self.home.on_gesture(g) {
            return Some(EndEvent::Home);
        }
        if self.next.as_mut().is_some_and(|b| b.on_gesture(g)) {
            return Some(EndEvent::Next);
        }
        None
    }

    pub fn draw(&self, cx: &mut DrawCx) {
        let t = cx.theme;
        self.sheet.draw(cx);
        let content = self.sheet.content(t);
        let top = self.sheet.rect().y;
        let icon = t.mm(5.0);
        cx.icon(Icon::Check, Rect::new(content.x, top, icon, t.mm(HEADER_MM)), icon, INK);
        let heading = content.x + icon + t.mm(2.0);
        // Short of the sheet's × at the right.
        let heading_w = content.right() - t.mm(10.0) - heading;
        cx.line("Finished", t.heading, Rect::new(heading, top, heading_w, t.mm(HEADER_MM)), HAlign::Left);
        let title = Rect::new(content.x, top + t.mm(HEADER_MM) - t.mm(1.5), content.w, t.mm(TITLE_MM));
        cx.line(&self.title, t.body.gray(INK2), title, HAlign::Left);
        self.undo.draw(cx);
        self.home.draw(cx);
        if let Some(next) = &self.next {
            next.draw(cx);
        }
    }
}

/// The sheet with and without Next, for `screens/fit_tests.rs`.
#[cfg(test)]
pub(super) fn fit_samples(t: &Theme, screen: Rect) -> Vec<crate::ui::fit::Parts> {
    [Some("The Second Book, with a Long Subtitle"), None]
        .into_iter()
        .map(|next| {
            let sheet = EndSheet::new(t, screen, "The First Book", next);
            let (undo, home, next) = sheet.buttons();
            let content = sheet.sheet.content(t);
            let title = Rect::new(content.x, sheet.rect().y + t.mm(HEADER_MM) - t.mm(1.5), content.w, t.mm(TITLE_MM));
            crate::ui::fit::Parts::new("EndSheet", sheet.rect(), [Some(undo), Some(home), next].into_iter().flatten())
                .body(title, t.body.line_height().ceil() as i32)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Point;

    fn tap(sheet: &mut EndSheet, p: Point) -> Option<EndEvent> {
        sheet.on_gesture(&Gesture::Down(p));
        sheet.on_gesture(&Gesture::Tap(p))
    }

    #[test]
    fn the_buttons_answer_with_next_stacked_or_in_a_row() {
        // Next has a row of its own at 600x800 and shares the row at 1696x1272.
        for (screen, dpi, stacked) in
            [(Rect::new(0, 0, 600, 800), 167, true), (Rect::new(0, 0, 1696, 1272), 300, false)]
        {
            let t = Theme::for_dpi(dpi);
            for next in [Some("The Second Book"), None] {
                let mut sheet = EndSheet::new(&t, screen, "The First Book", next);
                let r = sheet.rect();
                assert_eq!(r.bottom(), screen.bottom());
                assert!(r.h < screen.h / 2, "a small sheet: {r:?}");
                let (undo, home, next_rect) = sheet.buttons();
                assert_eq!(next_rect.is_some(), next.is_some());
                assert!(undo.right() < home.x);
                if let Some(n) = next_rect {
                    assert_eq!(n.bottom() <= undo.y, stacked, "Next above: {n:?}");
                    assert_eq!(n.x > home.right(), !stacked, "Next beside: {n:?}");
                    assert_eq!(tap(&mut sheet, n.center()), Some(EndEvent::Next));
                }
                assert_eq!(tap(&mut sheet, undo.center()), Some(EndEvent::Undo));
                assert_eq!(tap(&mut sheet, home.center()), Some(EndEvent::Home));
                assert_eq!(tap(&mut sheet, Point::new(r.center().x, r.y + t.mm(HEADER_MM + 2.0))), None);
                assert_eq!(tap(&mut sheet, Point::new(screen.w / 2, screen.h / 4)), Some(EndEvent::Close));
            }
        }
    }
}
