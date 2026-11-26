//! Styled text in pages with a pager under it, for sheets: footnotes,
//! synopses and release notes. While the text fits one page it gets the
//! whole area and no pager; otherwise the pager takes the bottom of the area
//! (‹ 1 of 3 ›) and swipes anywhere turn pages too. Laid out on the first
//! draw, on the UI thread, so it is for short texts of a few pages.
use crate::{
    geom::Rect,
    gfx::{HAlign, StyledBlock, StyledPagination, TextRenderer},
    ui::{gesture::Gesture, theme::Theme, widget::DrawCx, widgets::Pager},
};

pub struct PagedText {
    blocks: Vec<StyledBlock>,
    pages: Option<StyledPagination>,
    error: Option<String>,
    /// The text and, when it needs pages, the pager under it.
    area: Rect,
    /// The text's part of `area` when the pager is shown.
    paged: Rect,
    /// Where the text is drawn: `area` for one page, else `paged`.
    body: Rect,
    pub pager: Pager,
    /// Names the text in a layout error, e.g. "note".
    what: &'static str,
}

impl PagedText {
    /// Place it with `place` before drawing.
    pub fn new(blocks: Vec<StyledBlock>, what: &'static str) -> Self {
        Self {
            blocks,
            pages: None,
            error: None,
            area: Rect::default(),
            paged: Rect::default(),
            body: Rect::default(),
            pager: Pager::new(1),
            what,
        }
    }

    /// `area` holds the text and, if it needs more than one page, the pager
    /// along its bottom. A new area lays the text out again.
    pub fn place(&mut self, area: Rect, t: &Theme) {
        let pager = Rect::new(area.x, area.bottom() - t.target, area.w, t.target);
        self.pager.place(pager, t);
        self.paged = Rect { h: (pager.y - t.mm(3.0) - area.y).max(1), ..area };
        if area != self.area {
            (self.pages, self.error) = (None, None);
            self.pager.set(0, 1);
        }
        self.area = area;
        // Until laid out, assume pages: the smaller of the two.
        self.body = if self.pages.is_some() && self.pager.pages == 1 { area } else { self.paged };
    }

    #[cfg(test)]
    pub fn blocks(&self) -> &[StyledBlock] {
        &self.blocks
    }

    /// Where the text is drawn: the whole area for one page (once laid
    /// out), else the part above the pager.
    pub fn body(&self) -> Rect {
        self.body
    }

    pub fn laid_out(&self) -> bool {
        self.pages.is_some() || self.error.is_some()
    }

    /// The height a text that fits one page needs, once laid out.
    pub fn one_page_height(&self) -> Option<i32> {
        match self.pages.as_ref()?.pages() {
            [page] => Some(page.height.ceil() as i32 + 1),
            _ => None,
        }
    }

    /// Lays the text out, once per area: at the whole area, and again above
    /// the pager if it takes more than one page.
    pub fn lay_out(&mut self, text: &mut TextRenderer) {
        if self.laid_out() {
            return;
        }
        let started = std::time::Instant::now();
        let (area, paged) = (self.area, self.paged);
        let result = StyledPagination::new(self.blocks.clone(), area.w, area.h).and_then(|mut pages| {
            text.paginate_styled_until(&mut pages, usize::MAX)?;
            if pages.pages().len() <= 1 {
                return Ok((pages, area));
            }
            pages.repage(paged.h)?;
            text.paginate_styled_until(&mut pages, usize::MAX)?;
            Ok((pages, paged))
        });
        match result {
            Ok((pages, body)) => {
                self.pager.set(0, pages.pages().len());
                (self.pages, self.body) = (Some(pages), body);
            }
            Err(e) => {
                self.body = area;
                self.error = Some(format!("Couldn't lay out this {}: {e}", self.what));
            }
        }
        log::debug!("{} laid out in {:?}", self.what, started.elapsed());
    }

    pub fn visual_state(&self) -> (usize, usize, bool, bool, bool) {
        self.pager.visual_state()
    }

    /// The pager's arrows, and swipes anywhere, turn pages.
    pub fn on_gesture(&mut self, g: &Gesture) {
        self.pager.on_gesture(g);
        if let Gesture::Swipe { dir, .. } = g {
            self.pager.on_swipe(*dir);
        }
    }

    /// Lays the text out first if it isn't yet.
    pub fn draw(&mut self, cx: &mut DrawCx) {
        self.lay_out(cx.text);
        let t = cx.theme;
        if let Some(pages) = &mut self.pages
            && self.pager.page < pages.pages().len()
        {
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
    }
}
