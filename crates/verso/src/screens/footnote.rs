//! A book's footnote, shown over the page its marker is on. The reading
//! position stays put; "Go to note" jumps to the note in the book, and the
//! reader's back pill returns from there.
use crate::{
    document::{Note, Position},
    geom::Rect,
    screens::styled_sheet::{StyledSheet, StyledSheetEvent},
    ui::{
        gesture::Gesture,
        theme::Theme,
        widget::DrawCx,
        widgets::{Button, ButtonKind, Sheet},
    },
};

pub enum FootnoteEvent {
    Close,
    GoTo(Position),
}

pub struct Footnote {
    view: StyledSheet,
    target: Position,
}

/// "Note 12" for a numbered marker, "Note" for a symbol or linked phrase.
fn title(label: &str) -> String {
    let numbered = !label.is_empty() && label.chars().count() <= 6 && label.chars().all(char::is_alphanumeric);
    if numbered { format!("Note {label}") } else { "Note".into() }
}

impl Footnote {
    /// `marker` is where the tapped link is on the page. A bottom sheet as
    /// tall as the note needs, up to 3/5 of the screen; in landscape a side
    /// panel away from the marker.
    pub fn new(note: &Note, target: Position, t: &Theme, screen: Rect, marker: Rect) -> Self {
        let side = screen.w > screen.h;
        let title = title(&note.label);
        let sheet = if side {
            let right = marker.center().x < screen.center().x;
            Sheet::side(title, (screen.w * 11 / 20).max(t.mm(72.0)).min(screen.w), right, screen, t)
        } else {
            Sheet::open(title, screen.h * 3 / 5, screen, t)
        };
        let go = Button::new("Go to note", ButtonKind::Secondary);
        let mut view =
            StyledSheet::new(sheet, screen, &note.html, "This note has no text.", "note", t).with_button(go, t);
        if side {
            view = view.at_side();
        }
        Self { view, target }
    }

    #[cfg(test)]
    pub fn go_rect(&self) -> Rect {
        self.view.button.as_ref().unwrap().rect()
    }

    pub fn visual_state(&self) -> (bool, bool, (usize, usize, bool, bool, bool)) {
        self.view.visual_state()
    }

    pub fn on_gesture(&mut self, g: &Gesture) -> Option<FootnoteEvent> {
        match self.view.on_gesture(g)? {
            StyledSheetEvent::Close => Some(FootnoteEvent::Close),
            StyledSheetEvent::Button => Some(FootnoteEvent::GoTo(self.target.clone())),
        }
    }

    pub fn draw(&mut self, cx: &mut DrawCx) {
        self.view.draw(cx);
    }
}

/// A note too long for one page, for `screens/fit_tests.rs`.
#[cfg(test)]
pub(super) fn fit_samples(t: &Theme, screen: Rect) -> Vec<crate::ui::fit::Parts> {
    let note = Note { label: "12".into(), html: "<p>A long note that runs on. </p>".repeat(80) };
    let f = Footnote::new(&note, Position::default(), t, screen, Rect::new(screen.w / 4, screen.h / 3, 20, 20));
    let v = &f.view;
    let line = t.body.line_height().ceil() as i32;
    vec![crate::ui::fit::Parts::new("Footnote", v.sheet.rect(), [v.pager.rect(), f.go_rect()]).body(v.body, line)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::{StyledBlock, TextRenderer};

    #[test]
    fn notes_keep_their_text_and_styles() {
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let note = Note {
            label: "2".into(),
            html: "<div><div><b></b></div><div> Second note, with <i>italic</i> words.</div><div></div></div>".into(),
        };
        let f = Footnote::new(&note, Position("p".into()), &t, screen, Rect::new(100, 100, 20, 20));
        let texts: Vec<_> = f
            .view
            .blocks
            .iter()
            .filter_map(|b| if let StyledBlock::Paragraph(p) = b { Some(p.text.as_str()) } else { None })
            .collect();
        assert_eq!(texts, ["Second note, with italic words."]);
        let StyledBlock::Paragraph(p) = &f.view.blocks[0] else { panic!() };
        assert!(
            p.spans
                .iter()
                .any(|s| &p.text[s.bytes.clone()] == "italic" && s.style.contains(crate::gfx::SpanStyle::ITALIC))
        );
        assert_eq!(f.view.sheet.title, "Note 2");
        let empty = Footnote::new(
            &Note { label: "*".into(), html: "<div></div>".into() },
            Position("p".into()),
            &t,
            screen,
            Rect::default(),
        );
        assert!(matches!(&empty.view.blocks[0], StyledBlock::Paragraph(p) if p.text == "This note has no text."));
    }

    #[test]
    fn short_notes_get_a_short_sheet_and_landscape_a_side_panel() {
        let t = Theme::for_dpi(300);
        let (mut canvas, mut text, mut icons) =
            (crate::gfx::Canvas::new(1272, 1696), TextRenderer::new(), crate::gfx::IconRenderer::default());
        let note = Note { label: "1".into(), html: "<div>One line.</div>".into() };
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut f = Footnote::new(&note, Position("p".into()), &t, screen, Rect::default());
        let full = f.view.sheet.rect();
        let mut hints = vec![];
        f.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut hints));
        assert!(
            f.view.sheet.rect().h < full.h / 2 && f.view.sheet.rect().bottom() == screen.bottom(),
            "{:?}",
            f.view.sheet.rect()
        );
        assert!(f.go_rect().bottom() <= screen.bottom() && f.view.body.bottom() <= f.go_rect().y);
        let long = Note { label: "1".into(), html: "<p>A long note sentence goes on.</p>".repeat(300) };
        let mut f = Footnote::new(&long, Position("p".into()), &t, screen, Rect::default());
        f.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut hints));
        assert_eq!(f.view.sheet.rect(), full, "a long note keeps the tall sheet");
        assert!(f.view.pager.pages > 1);
        assert!(
            f.on_gesture(&Gesture::Swipe {
                dir: crate::ui::gesture::Direction::Left,
                from: f.view.body.center(),
                to: f.view.body.center()
            })
            .is_none()
        );
        assert_eq!(f.view.pager.page, 1);
        let wide = Rect::new(0, 0, 1696, 1272);
        let f = Footnote::new(&note, Position("p".into()), &t, wide, Rect::new(100, 100, 20, 20));
        assert_eq!(f.view.sheet.rect().right(), wide.right(), "away from a marker on the left");
    }

    #[test]
    fn titles_name_numbered_markers() {
        assert_eq!(title("12"), "Note 12");
        assert_eq!(title("iv"), "Note iv");
        assert_eq!(title("*"), "Note");
        assert_eq!(title("Gilfanon a · Davrobel"), "Note");
        assert_eq!(title(""), "Note");
    }
}
