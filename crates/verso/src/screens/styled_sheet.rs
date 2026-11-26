//! A sheet of styled text (bold, italic, paragraphs) with pages, laid out
//! with the dictionary's renderer: a book's footnotes and synopses. A text that
//! fits on one page gets a bottom sheet just its height.
use crate::{
    geom::Rect,
    gfx::{StyledBlock, TextRenderer},
    screens::{dictionary::styled_blocks, paged_text::PagedText},
    ui::{
        gesture::Gesture,
        theme::Theme,
        widget::DrawCx,
        widgets::{Button, Sheet},
    },
};

pub enum StyledSheetEvent {
    Close,
    /// The optional action button under the text was tapped.
    Button,
}

pub struct StyledSheet {
    pub(crate) sheet: Sheet,
    screen: Rect,
    /// The sheet stands at a side (landscape) and keeps its full height.
    side: bool,
    pub(crate) text: PagedText,
    pub(crate) button: Option<Button>,
}

impl StyledSheet {
    /// `sheet` is the largest the sheet may be. An empty text shows `empty`
    /// instead; `what` names the text in a layout error, e.g. "note".
    pub fn new(sheet: Sheet, screen: Rect, html: &str, empty: &str, what: &'static str, t: &Theme) -> Self {
        let mut blocks: Vec<_> = styled_blocks(&crate::dictionary::html_blocks(html), t).collect();
        if blocks.iter().all(|b| !matches!(b, StyledBlock::Paragraph(p) if !p.text.trim().is_empty())) {
            blocks = styled_blocks(&[crate::dictionary::Block::plain(empty)], t).collect();
        }
        let mut result = Self { sheet, screen, side: false, text: PagedText::new(blocks, what), button: None };
        result.place(t, None);
        result
    }

    /// The sheet is a side panel and keeps its full height.
    pub fn at_side(mut self) -> Self {
        self.side = true;
        self
    }

    /// Adds an action button under the text.
    pub fn with_button(mut self, button: Button, t: &Theme) -> Self {
        self.button = Some(button);
        self.place(t, None);
        self
    }

    /// Lays the controls out on the sheet; `fit` is the body height a
    /// one-page text needs, for a shorter bottom sheet.
    fn place(&mut self, t: &Theme, fit: Option<i32>) {
        let gap = t.mm(3.0);
        let bottom = t.mm(3.0);
        let button_h = if self.button.is_some() { gap + t.target } else { 0 };
        if let Some(body) = fit {
            let title = self.sheet.title.clone();
            let height = t.mm(13.0) + body + button_h + bottom;
            self.sheet = Sheet::open(title, height.min(self.sheet.rect().h), self.screen, t);
        }
        let content = self.sheet.content(t);
        // The line the text (or the pager under it) stands a gap above.
        let mut floor = self.sheet.rect().bottom() - bottom + gap;
        if let Some(button) = &mut self.button {
            let rect = Rect::new(content.x, self.sheet.rect().bottom() - bottom - t.target, content.w, t.target);
            button.place(rect, t);
            floor = rect.y;
        }
        self.text.place(Rect::new(content.x, content.y, content.w, (floor - gap - content.y).max(1)), t);
    }

    /// A bottom sheet shrinks to a text that fits one page.
    fn lay_out(&mut self, text: &mut TextRenderer, t: &Theme) {
        if self.text.laid_out() {
            return;
        }
        self.text.lay_out(text);
        if !self.side
            && let Some(needed) = self.text.one_page_height()
            && needed < self.text.body().h
        {
            self.place(t, Some(needed));
            self.text.lay_out(text);
        }
    }

    pub fn visual_state(&self) -> (bool, bool, (usize, usize, bool, bool, bool)) {
        (self.sheet.is_pressed(), self.button.as_ref().is_some_and(Button::is_pressed), self.text.visual_state())
    }

    pub fn on_gesture(&mut self, g: &Gesture) -> Option<StyledSheetEvent> {
        if self.sheet.on_gesture(g) {
            return Some(StyledSheetEvent::Close);
        }
        if self.button.as_mut().is_some_and(|b| b.on_gesture(g)) {
            return Some(StyledSheetEvent::Button);
        }
        self.text.on_gesture(g);
        None
    }

    pub fn draw(&mut self, cx: &mut DrawCx) {
        self.lay_out(cx.text, cx.theme);
        self.sheet.draw(cx);
        self.text.draw(cx);
        if let Some(button) = &self.button {
            button.draw(cx);
        }
    }
}
