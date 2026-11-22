//! A sheet of styled text (bold, italic, paragraphs) with pages, laid out
//! with the dictionary's renderer: a book's footnotes and synopses. A text that
//! fits on one page gets a bottom sheet just its height.
use crate::{
    geom::Rect,
    gfx::{HAlign, StyledBlock, StyledPagination, TextRenderer},
    screens::dictionary::styled_blocks,
    ui::{
        gesture::Gesture,
        theme::Theme,
        widget::DrawCx,
        widgets::{Button, Pager, Sheet},
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
    pub(crate) blocks: Vec<StyledBlock>,
    pages: Option<StyledPagination>,
    error: Option<String>,
    pub(crate) body: Rect,
    pub(crate) pager: Pager,
    pub(crate) button: Option<Button>,
    /// Names the text in a layout error, e.g. "note".
    what: &'static str,
}

impl StyledSheet {
    /// `sheet` is the largest the sheet may be. An empty text shows `empty`
    /// instead; `what` names the text in a layout error, e.g. "note".
    pub fn new(sheet: Sheet, screen: Rect, html: &str, empty: &str, what: &'static str, t: &Theme) -> Self {
        let mut blocks: Vec<_> = styled_blocks(&crate::dictionary::html_blocks(html), t).collect();
        if blocks.iter().all(|b| !matches!(b, StyledBlock::Paragraph(p) if !p.text.trim().is_empty())) {
            blocks = styled_blocks(&[crate::dictionary::Block::plain(empty)], t).collect();
        }
        let mut result = Self {
            sheet,
            screen,
            side: false,
            blocks,
            pages: None,
            error: None,
            body: Rect::default(),
            pager: Pager::new(1),
            button: None,
            what,
        };
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
        // The line the pager (or a one-page body) stands a gap above.
        let mut floor = self.sheet.rect().bottom() - bottom + gap;
        if let Some(button) = &mut self.button {
            let rect = Rect::new(content.x, self.sheet.rect().bottom() - bottom - t.target, content.w, t.target);
            button.place(rect, t);
            floor = rect.y;
        }
        let pager = Rect::new(content.x, floor - gap - t.target, content.w, t.target);
        self.pager.place(pager, t);
        let end = if fit.is_some() { floor } else { pager.y };
        self.body = Rect::new(content.x, content.y, content.w, (end - gap - content.y).max(1));
    }

    fn lay_out(&mut self, text: &mut TextRenderer, t: &Theme) {
        let paginate = |text: &mut TextRenderer, blocks: Vec<StyledBlock>, body: Rect| {
            let mut pages = StyledPagination::new(blocks, body.w, body.h)?;
            text.paginate_styled_until(&mut pages, usize::MAX)?;
            anyhow::Ok(pages)
        };
        let result = paginate(text, self.blocks.clone(), self.body).and_then(|pages| {
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
            Err(e) => self.error = Some(format!("Couldn't lay out this {}: {e}", self.what)),
        }
    }

    pub fn visual_state(&self) -> (bool, bool, (usize, usize, bool, bool, bool)) {
        (self.sheet.is_pressed(), self.button.as_ref().is_some_and(Button::is_pressed), self.pager.visual_state())
    }

    pub fn on_gesture(&mut self, g: &Gesture) -> Option<StyledSheetEvent> {
        if self.sheet.on_gesture(g) {
            return Some(StyledSheetEvent::Close);
        }
        if self.button.as_mut().is_some_and(|b| b.on_gesture(g)) {
            return Some(StyledSheetEvent::Button);
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
            log::debug!("{} laid out in {:?}", self.what, started.elapsed());
        }
        self.sheet.draw(cx);
        if let Some(pages) = &mut self.pages {
            // Keep accents, italic overhangs and descenders in the controls' gap.
            let clip = self.body.inset(-t.mm(0.5).max(2));
            if let Err(e) = cx.text.draw_styled_page(cx.canvas, pages, self.pager.page, self.body, clip, t.hairline) {
                log::error!("{} page draw: {e:#}", self.what);
            }
        }
        if let Some(error) = &self.error {
            cx.text.draw(cx.canvas, error, t.body, self.body, HAlign::Left);
        }
        self.pager.draw(cx);
        if let Some(button) = &self.button {
            button.draw(cx);
        }
    }
}
