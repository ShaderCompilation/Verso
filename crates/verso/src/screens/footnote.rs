//! A book's footnote, shown over the page its marker is on. The reading
//! position stays put; "Go to note" jumps to the note in the book, and the
//! reader's back pill returns from there.
use crate::{
    document::{Note, Position},
    geom::Rect,
    gfx::{HAlign, StyledBlock, StyledPagination, TextRenderer},
    screens::dictionary::styled_blocks,
    ui::{
        gesture::Gesture,
        theme::Theme,
        widget::DrawCx,
        widgets::{Button, ButtonKind, Pager, Sheet},
    },
};

pub enum FootnoteEvent {
    Close,
    GoTo(Position),
}

pub struct Footnote {
    sheet: Sheet,
    screen: Rect,
    /// The sheet stands at a side (landscape) and keeps its full height.
    side: bool,
    blocks: Vec<StyledBlock>,
    pages: Option<StyledPagination>,
    error: Option<String>,
    body: Rect,
    pager: Pager,
    go: Button,
    target: Position,
}

/// "Note 12" for a numbered marker, "Note" for a symbol or linked phrase.
fn title(label: &str) -> String {
    let numbered = !label.is_empty() && label.chars().count() <= 6 && label.chars().all(char::is_alphanumeric);
    if numbered { format!("Note {label}") } else { "Note".into() }
}

impl Footnote {
    /// \`marker\` is where the tapped link is on the page. A bottom sheet as
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
        let mut blocks: Vec<_> = styled_blocks(&crate::dictionary::html_blocks(&note.html), t).collect();
        if blocks.iter().all(|b| !matches!(b, StyledBlock::Paragraph(p) if !p.text.trim().is_empty())) {
            blocks = styled_blocks(&[crate::dictionary::Block::plain("This note has no text.")], t).collect();
        }
        let mut result = Self {
            sheet,
            screen,
            side,
            blocks,
            pages: None,
            error: None,
            body: Rect::default(),
            pager: Pager::new(1),
            go: Button::new("Go to note", ButtonKind::Secondary),
            target,
        };
        result.place(t, None);
        result
    }

    /// Lays the controls out on the sheet; \`fit\` is the body height a
    /// one-page note needs, for a shorter bottom sheet.
    fn place(&mut self, t: &Theme, fit: Option<i32>) {
        let gap = t.mm(3.0);
        let bottom = t.mm(3.0);
        if let Some(body) = fit {
            let title = self.sheet.title.clone();
            let height = t.mm(13.0) + body + gap + t.target + bottom;
            self.sheet = Sheet::open(title, height.min(self.sheet.rect().h), self.screen, t);
        }
        let content = self.sheet.content(t);
        let button = Rect::new(content.x, self.sheet.rect().bottom() - bottom - t.target, content.w, t.target);
        self.go.place(button, t);
        let pager = Rect::new(content.x, button.y - gap - t.target, content.w, t.target);
        self.pager.place(pager, t);
        let end = if fit.is_some() { button.y } else { pager.y };
        self.body = Rect::new(content.x, content.y, content.w, (end - gap - content.y).max(1));
    }

    fn lay_out(&mut self, text: &mut TextRenderer, t: &Theme) {
        let paginate = |text: &mut TextRenderer, blocks: Vec<StyledBlock>, body: Rect| {
            let mut pages = StyledPagination::new(blocks, body.w, body.h)?;
            text.paginate_styled_until(&mut pages, usize::MAX)?;
            anyhow::Ok(pages)
        };
        let result = paginate(text, self.blocks.clone(), self.body).and_then(|pages| {
            // A note that fits on one page gets a sheet just its height.
            if !self.side && pages.pages().len() == 1 {
                let needed = pages.pages()[0].height.ceil() as i32 + 1;
                if needed < self.body.h {
                    self.place(t, Some(needed));
                    return paginate(text, self.blocks.clone(), self.body);
                }
            }
            Ok(pages)
        });
        match result {
            Ok(pages) => {
                self.pager.set(0, pages.pages().len());
                self.pages = Some(pages);
            }
            Err(e) => self.error = Some(format!("Couldn't lay out this note: {e}")),
        }
    }

    #[cfg(test)]
    pub fn go_rect(&self) -> Rect {
        self.go.rect()
    }

    pub fn visual_state(&self) -> (bool, bool, (usize, usize, bool, bool, bool)) {
        (self.sheet.is_pressed(), self.go.is_pressed(), self.pager.visual_state())
    }

    pub fn on_gesture(&mut self, g: &Gesture) -> Option<FootnoteEvent> {
        if self.sheet.on_gesture(g) {
            return Some(FootnoteEvent::Close);
        }
        if self.go.on_gesture(g) {
            return Some(FootnoteEvent::GoTo(self.target.clone()));
        }
        self.pager.on_gesture(g);
        if let Gesture::Swipe { dir, .. } = g {
            self.pager.on_swipe(*dir);
        }
        None
    }

    pub fn draw(&mut self, cx: &mut DrawCx) {
        let t = cx.theme;
        if self.pages.is_none() && self.error.is_none() {
            let started = std::time::Instant::now();
            self.lay_out(cx.text, t);
            log::debug!("footnote laid out in {:?}", started.elapsed());
        }
        self.sheet.draw(cx);
        if let Some(pages) = &mut self.pages {
            // Keep accents, italic overhangs and descenders in the controls' gap.
            let clip = self.body.inset(-t.mm(0.5).max(2));
            if let Err(e) = cx.text.draw_styled_page(cx.canvas, pages, self.pager.page, self.body, clip, t.hairline) {
                log::error!("footnote page draw: {e:#}");
            }
        }
        if let Some(error) = &self.error {
            cx.text.draw(cx.canvas, error, t.body, self.body, HAlign::Left);
        }
        self.pager.draw(cx);
        self.go.draw(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            .blocks
            .iter()
            .filter_map(|b| if let StyledBlock::Paragraph(p) = b { Some(p.text.as_str()) } else { None })
            .collect();
        assert_eq!(texts, ["Second note, with italic words."]);
        let StyledBlock::Paragraph(p) = &f.blocks[0] else { panic!() };
        assert!(
            p.spans
                .iter()
                .any(|s| &p.text[s.bytes.clone()] == "italic" && s.style.contains(crate::gfx::SpanStyle::ITALIC))
        );
        assert_eq!(f.sheet.title, "Note 2");
        let empty = Footnote::new(
            &Note { label: "*".into(), html: "<div></div>".into() },
            Position("p".into()),
            &t,
            screen,
            Rect::default(),
        );
        assert!(matches!(&empty.blocks[0], StyledBlock::Paragraph(p) if p.text == "This note has no text."));
    }

    #[test]
    fn short_notes_get_a_short_sheet_and_landscape_a_side_panel() {
        let t = Theme::for_dpi(300);
        let (mut canvas, mut text, mut icons) =
            (crate::gfx::Canvas::new(1272, 1696), TextRenderer::new(), crate::gfx::IconRenderer::default());
        let note = Note { label: "1".into(), html: "<div>One line.</div>".into() };
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut f = Footnote::new(&note, Position("p".into()), &t, screen, Rect::default());
        let full = f.sheet.rect();
        let mut hints = vec![];
        f.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut hints));
        assert!(f.sheet.rect().h < full.h / 2 && f.sheet.rect().bottom() == screen.bottom(), "{:?}", f.sheet.rect());
        assert!(f.go.rect().bottom() <= screen.bottom() && f.body.bottom() <= f.go.rect().y);
        let long = Note { label: "1".into(), html: "<p>A long note sentence goes on.</p>".repeat(300) };
        let mut f = Footnote::new(&long, Position("p".into()), &t, screen, Rect::default());
        f.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut hints));
        assert_eq!(f.sheet.rect(), full, "a long note keeps the tall sheet");
        assert!(f.pager.pages > 1);
        assert!(
            f.on_gesture(&Gesture::Swipe {
                dir: crate::ui::gesture::Direction::Left,
                from: f.body.center(),
                to: f.body.center()
            })
            .is_none()
        );
        assert_eq!(f.pager.page, 1);
        let wide = Rect::new(0, 0, 1696, 1272);
        let f = Footnote::new(&note, Position("p".into()), &t, wide, Rect::new(100, 100, 20, 20));
        assert_eq!(f.sheet.rect().right(), wide.right(), "away from a marker on the left");
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
