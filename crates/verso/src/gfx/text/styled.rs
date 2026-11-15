//! Bounded rich-text layout, independent of dictionary content and screen state.
//!
//! Pagination owns its input blocks and keeps only lightweight line records,
//! never layouts. It runs incrementally, one piece of at most 16 KiB at a time,
//! so the first page can be drawn before a long entry is finished. Only
//! drawing populates its separate four-layout cache.

use std::collections::{BTreeSet, VecDeque};
use std::ops::Range;

use anyhow::{Result, ensure};
use cosmic_text::{BidiParagraphs, LayoutRun};

use super::*;

pub const MAX_PARAGRAPH_BYTES: usize = 16 * 1024;
const DRAW_CACHE_LIMIT: usize = 4;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpanStyle(u8);

impl SpanStyle {
    pub const BOLD: Self = Self(1);
    pub const ITALIC: Self = Self(2);
    pub const MUTED: Self = Self(4);
    pub const PHONETIC: Self = Self(8);
    pub const UNDERLINE: Self = Self(16);

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

#[derive(Clone, Debug)]
pub struct StyledSpan {
    /// UTF-8 byte range; spans must be sorted, disjoint and nonempty.
    pub bytes: Range<usize>,
    pub style: SpanStyle,
}

#[derive(Clone, Debug)]
pub struct StyledParagraph {
    pub text: String,
    pub spans: Vec<StyledSpan>,
    pub base: TextStyle,
    /// Gray of `MUTED` spans (the theme's `ink2`); other spans use `base.gray`.
    pub muted: u8,
    /// Pixels from the left edge, applied to every wrapped line.
    pub indent: f32,
}

#[derive(Clone, Debug)]
pub enum StyledBlock {
    Paragraph(StyledParagraph),
    /// Pixels of vertical space. Gaps collapse at page edges.
    Gap(f32),
    /// Divider with equal space above and below; collapses at page edges.
    Rule {
        space: f32,
        thickness: i32,
        gray: u8,
    },
}

#[derive(Clone, Debug)]
pub struct StyledLine {
    /// Bytes in the piece's text, including intervening whitespace and hard
    /// breaks. These ranges partition its text exactly.
    pub bytes: Range<usize>,
    pub height: f32,
}

/// The result of laying out one bounded paragraph. Drop it after extracting
/// line records, or keep it briefly for drawing; never cache a whole entry.
pub struct ParagraphLayout {
    buffer: Buffer,
    pub lines: Vec<StyledLine>,
}

/// One bounded piece of a source paragraph, as laid out during pagination.
/// The text itself stays in the pagination's blocks.
pub struct PaginatedParagraph {
    /// Index in the original list of blocks.
    pub block: usize,
    /// Bytes in that paragraph. Pieces of one paragraph are contiguous and
    /// meet only at wrapped line boundaries.
    pub source_bytes: Range<usize>,
    /// Lines of the piece, relative to `source_bytes.start`.
    pub lines: Vec<StyledLine>,
}

#[derive(Debug)]
pub struct PageParagraph {
    /// Index into `StyledPagination::paragraphs()`.
    pub paragraph: usize,
    /// Wrapped line range in that piece.
    pub lines: Range<usize>,
    pub y: f32,
}

#[derive(Debug, Default)]
pub struct StyledPage {
    pub paragraphs: Vec<PageParagraph>,
    pub height: f32,
    rules: Vec<(f32, i32, u8)>,
}

pub struct StyledPagination {
    blocks: Vec<StyledBlock>,
    paragraphs: Vec<PaginatedParagraph>,
    /// Finished pages only; the one being filled is `page`.
    pages: Vec<StyledPage>,
    width: i32,
    height: i32,
    // Where pagination resumes: the next block, the byte offset inside it,
    // the merged gap still pending and the page being filled.
    next_block: usize,
    next_byte: usize,
    gap: f32,
    rule: Option<(i32, u8)>,
    page: StyledPage,
    complete: bool,
    // Belongs to this immutable input so layouts cannot outlive or refer to
    // changed source text. Independent of TextRenderer's UI cache.
    layouts: VecDeque<(usize, Buffer)>,
    missing_logged: Vec<bool>,
}

impl StyledPagination {
    /// Validate the whole input up front, without shaping anything. Pages
    /// are made by `TextRenderer::paginate_styled_step` and `_until`.
    pub fn new(blocks: Vec<StyledBlock>, width: i32, height: i32) -> Result<Self> {
        ensure!(width > 0 && height > 0, "page dimensions must be positive");
        for block in &blocks {
            match block {
                StyledBlock::Gap(space) => ensure!(space.is_finite() && *space >= 0.0, "invalid gap height"),
                StyledBlock::Rule { space, thickness, .. } => {
                    ensure!(space.is_finite() && *space >= 0.0 && *thickness > 0, "invalid divider");
                }
                StyledBlock::Paragraph(paragraph) => {
                    paragraph.validate(width as f32)?;
                    ensure!(paragraph.base.line_height() <= height as f32, "page is shorter than one text line");
                }
            }
        }
        let mut pagination = Self {
            blocks,
            paragraphs: Vec::new(),
            pages: Vec::new(),
            width,
            height,
            next_block: 0,
            next_byte: 0,
            gap: 0.0,
            rule: None,
            page: StyledPage::default(),
            complete: false,
            layouts: VecDeque::new(),
            missing_logged: Vec::new(),
        };
        pagination.skip_gaps();
        Ok(pagination)
    }

    /// Finished pages. The list grows until `is_complete()`.
    pub fn pages(&self) -> &[StyledPage] {
        &self.pages
    }

    pub fn paragraphs(&self) -> &[PaginatedParagraph] {
        &self.paragraphs
    }

    /// Whether every block is paginated, so `pages()` is final.
    pub fn is_complete(&self) -> bool {
        self.complete
    }

    /// The bounded paragraph shaped for `paragraphs()[index]`.
    #[cfg(test)]
    pub fn piece(&self, index: usize) -> StyledParagraph {
        let piece = &self.paragraphs[index];
        source(&self.blocks, piece.block).slice(piece.source_bytes.clone())
    }

    /// Merge gaps up to the next paragraph, and finish when none is left.
    fn skip_gaps(&mut self) {
        while let Some(block) = self.blocks.get(self.next_block) {
            match block {
                StyledBlock::Paragraph(_) => break,
                StyledBlock::Gap(space) => self.gap = self.gap.max(*space),
                StyledBlock::Rule { space, thickness, gray } => {
                    self.gap = self.gap.max(*space);
                    self.rule = Some((*thickness, *gray));
                }
            }
            self.next_block += 1;
        }
        if self.next_block == self.blocks.len() && !self.complete {
            // Trailing gaps disappear; an empty input still has one page.
            if !self.page.paragraphs.is_empty() || self.pages.is_empty() {
                self.finish_page();
            }
            self.complete = true;
        }
    }

    fn finish_page(&mut self) {
        self.pages.push(std::mem::take(&mut self.page));
        self.missing_logged.push(false);
    }

    /// Place a piece's lines by height; the pending gap goes before its first
    /// line unless that line starts a page.
    fn place(&mut self, piece: PaginatedParagraph) -> Result<()> {
        let index = self.paragraphs.len();
        let height = self.height as f32;
        for (line, record) in piece.lines.iter().enumerate() {
            ensure!(record.height <= height, "page is shorter than one text line");
            let space = if self.page.paragraphs.is_empty() {
                0.0
            } else if let Some((thickness, _)) = self.rule {
                2.0 * self.gap + thickness as f32
            } else {
                self.gap
            };
            if self.page.height + space + record.height > height {
                self.finish_page();
            } else {
                if !self.page.paragraphs.is_empty()
                    && let Some((thickness, gray)) = self.rule
                {
                    self.page.rules.push((self.page.height + self.gap, thickness, gray));
                }
                self.page.height += space;
            }
            self.gap = 0.0;
            self.rule = None;
            if let Some(last) = self.page.paragraphs.last_mut().filter(|last| last.paragraph == index) {
                last.lines.end = line + 1;
            } else {
                self.page.paragraphs.push(PageParagraph {
                    paragraph: index,
                    lines: line..line + 1,
                    y: self.page.height,
                });
            }
            self.page.height += record.height;
        }
        self.paragraphs.push(piece);
        Ok(())
    }
}

fn source(blocks: &[StyledBlock], block: usize) -> &StyledParagraph {
    match &blocks[block] {
        StyledBlock::Paragraph(paragraph) => paragraph,
        StyledBlock::Gap(_) | StyledBlock::Rule { .. } => unreachable!("pieces always come from paragraphs"),
    }
}

impl StyledParagraph {
    fn validate(&self, width: f32) -> Result<()> {
        ensure!(width.is_finite() && width > 0.0, "paragraph width must be positive");
        ensure!(self.indent.is_finite() && self.indent >= 0.0 && self.indent < width, "indent must fit within width");
        ensure!(
            self.base.size.is_finite()
                && self.base.size > 0.0
                && self.base.line_height().is_finite()
                && self.base.line_height() > 0.0,
            "invalid font size"
        );
        ensure!(self.base.tracking.is_finite(), "invalid tracking");
        let mut end = 0;
        for span in &self.spans {
            ensure!(
                end <= span.bytes.start
                    && span.bytes.start < span.bytes.end
                    && self.text.get(span.bytes.clone()).is_some(),
                "style spans must be sorted, disjoint UTF-8 ranges"
            );
            end = span.bytes.end;
        }
        Ok(())
    }

    fn slice(&self, bytes: Range<usize>) -> Self {
        let spans = self
            .spans
            .iter()
            .filter_map(|span| {
                let start = span.bytes.start.max(bytes.start);
                let end = span.bytes.end.min(bytes.end);
                (start < end).then(|| StyledSpan { bytes: start - bytes.start..end - bytes.start, style: span.style })
            })
            .collect();
        Self { text: self.text[bytes].to_owned(), spans, base: self.base, muted: self.muted, indent: self.indent }
    }
}

impl TextRenderer {
    /// Shape one paragraph of at most 16 KiB. The paginator splits larger
    /// inputs first. No entry is added to the UI layout cache.
    pub fn layout_styled_paragraph(&mut self, paragraph: &StyledParagraph, width: f32) -> Result<ParagraphLayout> {
        paragraph.validate(width)?;
        ensure!(paragraph.text.len() <= MAX_PARAGRAPH_BYTES, "paragraph exceeds the 16 KiB shaping limit");
        Ok(self.shape_styled(paragraph, width))
    }

    fn shape_styled(&mut self, paragraph: &StyledParagraph, width: f32) -> ParagraphLayout {
        let base = paragraph.base;
        let family = match base.family {
            Family::Book(i) => self.book_families.get(i as usize).map(String::as_str).unwrap_or("Literata"),
            other => other.name(),
        };
        let attrs = |style: SpanStyle| {
            // Inter has no italic face. Keep phonetic text in its complete,
            // upright IPA face, including when the surrounding prose is italic.
            let phonetic = style.contains(SpanStyle::PHONETIC);
            let italic = style.contains(SpanStyle::ITALIC) && !phonetic;
            let weight = if style.contains(SpanStyle::BOLD) || base.weight == Weight::SemiBold {
                fontdb::Weight::SEMIBOLD
            } else {
                fontdb::Weight::NORMAL
            };
            let gray = if style.contains(SpanStyle::MUTED) { paragraph.muted } else { base.gray };
            Attrs::new()
                .family(CtFamily::Name(if phonetic { "Inter" } else { family }))
                .weight(weight)
                .style(if italic { fontdb::Style::Italic } else { fontdb::Style::Normal })
                .letter_spacing(base.tracking)
                .metadata(style.0 as usize | ((gray as usize) << 8))
        };
        let default_attrs = attrs(SpanStyle::default());
        let mut spans = Vec::with_capacity(paragraph.spans.len() * 2 + 1);
        let mut end = 0;
        for span in &paragraph.spans {
            if end < span.bytes.start {
                spans.push((&paragraph.text[end..span.bytes.start], default_attrs.clone()));
            }
            spans.push((&paragraph.text[span.bytes.clone()], attrs(span.style)));
            end = span.bytes.end;
        }
        if end < paragraph.text.len() || spans.is_empty() {
            spans.push((&paragraph.text[end..], default_attrs.clone()));
        }
        let mut buffer = Buffer::new(&mut self.fonts, Metrics::new(base.size, base.line_height()));
        buffer.set_size(Some(width - paragraph.indent), None);
        buffer.set_wrap(Wrap::WordOrGlyph);
        buffer.set_rich_text(spans, &default_attrs, Shaping::Advanced, Some(Align::Left));
        buffer.shape_until_scroll(&mut self.fonts, false);

        // Use the same hard-line splitter as set_rich_text, including Unicode
        // paragraph separators. Its slices give exact offsets even for CRLF.
        let offsets: Vec<_> = BidiParagraphs::new(&paragraph.text)
            .map(|line| line.as_ptr() as usize - paragraph.text.as_ptr() as usize)
            .collect();
        let mut previous_line = None;
        let mut lines: Vec<StyledLine> = buffer
            .layout_runs()
            .map(|run| {
                let first = previous_line != Some(run.line_i);
                previous_line = Some(run.line_i);
                let start = offsets.get(run.line_i).copied().unwrap_or(0)
                    + if first { 0 } else { run.glyphs.iter().map(|g| g.start).min().unwrap_or(0) };
                StyledLine { bytes: start..paragraph.text.len(), height: run.line_height }
            })
            .collect();
        for i in 1..lines.len() {
            lines[i - 1].bytes.end = lines[i].bytes.start;
        }
        ParagraphLayout { buffer, lines }
    }

    /// Paginate everything at once. Screens showing long entries should use
    /// `StyledPagination::new` with `paginate_styled_until` instead.
    #[cfg(test)]
    pub fn paginate_styled(&mut self, blocks: Vec<StyledBlock>, width: i32, height: i32) -> Result<StyledPagination> {
        let mut pagination = StyledPagination::new(blocks, width, height)?;
        self.paginate_styled_until(&mut pagination, usize::MAX)?;
        Ok(pagination)
    }

    /// Continue until at least `pages` pages are finished or the input ends.
    /// `paginate_styled_until(p, page + 1)` makes `page` drawable.
    pub fn paginate_styled_until(&mut self, pagination: &mut StyledPagination, pages: usize) -> Result<()> {
        while pagination.pages.len() < pages && self.paginate_styled_step(pagination)? {}
        Ok(())
    }

    /// Shape and place the next piece of at most 16 KiB, holding one
    /// temporary layout. Returns whether input remains.
    pub fn paginate_styled_step(&mut self, pagination: &mut StyledPagination) -> Result<bool> {
        if pagination.complete {
            return Ok(false);
        }
        let (block, start) = (pagination.next_block, pagination.next_byte);
        let source = source(&pagination.blocks, block);
        let len = source.text.len();
        let cut = chunk_end(&source.text, start);
        let ParagraphLayout { buffer, mut lines } =
            self.layout_styled_paragraph(&source.slice(start..cut), pagination.width as f32)?;
        // Release the full layout before placing lines or shaping more.
        drop(buffer);
        // A cut inside a line may end that line early. Wrapping is greedy, so
        // the earlier lines are final; the last one starts the next piece.
        let mut end = cut;
        if cut < len && lines.len() > 1 && lines.last().is_some_and(|line| line.bytes.start > 0) {
            end = start + lines.pop().unwrap().bytes.start;
        }
        pagination.place(PaginatedParagraph { block, source_bytes: start..end, lines })?;
        if end == len {
            pagination.next_block += 1;
            pagination.next_byte = 0;
            pagination.skip_gaps();
        } else {
            pagination.next_byte = end;
        }
        Ok(!pagination.complete)
    }

    /// Draw a finished page laid out at `rect`'s size, the size used for
    /// pagination. Ink is limited to `clip`, so accents and descenders that
    /// reach past the first or last line can stay visible when the caller
    /// allows some margin. Only the page's pieces are shaped, reusing at most
    /// four layouts.
    pub fn draw_styled_page(
        &mut self,
        canvas: &mut Canvas,
        pagination: &mut StyledPagination,
        page: usize,
        rect: Rect,
        clip: Rect,
        hairline: i32,
    ) -> Result<()> {
        ensure!(
            rect.w == pagination.width && rect.h == pagination.height,
            "page rect differs from pagination dimensions"
        );
        ensure!(page < pagination.pages.len(), "page is not paginated yet");
        ensure!(hairline > 0, "underline hairline must be positive");
        let mut missing = BTreeSet::new();
        for &(y, thickness, gray) in &pagination.pages[page].rules {
            let rule = Rect::new(rect.x, rect.y + y.round() as i32, rect.w, thickness);
            if let Some(rule) = rule.intersect(&clip) {
                canvas.fill_rect(rule, gray);
            }
        }
        for part in &pagination.pages[page].paragraphs {
            let piece = &pagination.paragraphs[part.paragraph];
            let paragraph = source(&pagination.blocks, piece.block);
            let buffer = if let Some(at) = pagination.layouts.iter().position(|(index, _)| *index == part.paragraph) {
                pagination.layouts.remove(at).unwrap().1
            } else {
                // Evict before shaping to keep the live-layout bound at four.
                if pagination.layouts.len() == DRAW_CACHE_LIMIT {
                    pagination.layouts.pop_front();
                }
                let text = paragraph.slice(piece.source_bytes.clone());
                self.layout_styled_paragraph(&text, rect.w as f32)?.buffer
            };
            let mut top = None;
            for run in buffer.layout_runs().skip(part.lines.start).take(part.lines.len()) {
                let offset = rect.y as f32 + part.y - *top.get_or_insert(run.line_top);
                let x = rect.x as f32 + paragraph.indent;
                for glyph in run.glyphs {
                    if glyph.glyph_id == 0 {
                        missing.extend(
                            run.text[glyph.start..glyph.end].chars().filter(|c| !c.is_whitespace() && !c.is_control()),
                        );
                    }
                    let phys = glyph.physical((x, offset + run.line_y), 1.0);
                    let Some(image) = self.glyphs.get_image(&mut self.fonts, phys.cache_key) else { continue };
                    let p = image.placement;
                    let (gx, gy) = (phys.x + p.left, phys.y - p.top);
                    let gray = (glyph.metadata >> 8) as u8;
                    match image.content {
                        SwashContent::Mask => canvas.blend_mask(gx, gy, p.width, p.height, &image.data, gray, clip),
                        SwashContent::Color => {
                            let alpha: Vec<_> = image.data.as_chunks::<4>().0.iter().map(|px| px[3]).collect();
                            canvas.blend_mask(gx, gy, p.width, p.height, &alpha, gray, clip);
                        }
                        SwashContent::SubpixelMask => {}
                    }
                }
                let underline_y = (offset + run.line_y + paragraph.base.size * 0.08).round() as i32;
                // Keep even an unusually thick requested hairline in its line.
                let bottom = (offset + run.line_top + run.line_height).floor() as i32;
                let thickness = hairline.min((run.line_height.floor() as i32).max(1));
                for (left, right, gray) in underlines(&run) {
                    let left = (x + left).floor() as i32;
                    let right = (x + right).ceil() as i32;
                    let line = Rect::new(left, underline_y.min(bottom - thickness), right - left, thickness);
                    if let Some(line) = line.intersect(&clip) {
                        canvas.fill_rect(line, gray);
                    }
                }
            }
            pagination.layouts.push_back((part.paragraph, buffer));
        }
        if !pagination.missing_logged[page] {
            if !missing.is_empty() {
                log::debug!("styled page {} missing glyphs: {:?}", page + 1, missing);
            }
            pagination.missing_logged[page] = true;
        }
        Ok(())
    }
}

/// Where the piece starting at `start` is cut before shaping: at most 16 KiB
/// later, on a character boundary and never inside CRLF. The paginator then
/// moves the cut back to the start of the last wrapped line.
fn chunk_end(text: &str, start: usize) -> usize {
    let mut end = (start + MAX_PARAGRAPH_BYTES).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    if text[..end].ends_with('\r') && text[end..].starts_with('\n') && end - 1 > start {
        end -= 1;
    }
    end
}

/// Merge in visual order, so RTL and mixed-direction lines work too. Each
/// contiguous underlined run gets one stroke, including its word spaces.
fn underlines(run: &LayoutRun<'_>) -> Vec<(f32, f32, u8)> {
    let mut glyphs: Vec<_> = run
        .glyphs
        .iter()
        .filter(|g| g.metadata & SpanStyle::UNDERLINE.0 as usize != 0 && g.w > 0.0)
        .map(|g| (g.x, g.x + g.w, (g.metadata >> 8) as u8))
        .collect();
    glyphs.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut runs: Vec<(f32, f32, u8)> = Vec::new();
    for (left, right, gray) in glyphs {
        if let Some(last) = runs.last_mut().filter(|last| last.2 == gray && left <= last.1 + 0.5) {
            last.1 = last.1.max(right);
        } else {
            runs.push((left, right, gray));
        }
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::{INK, INK2, PAPER};

    const BODY: TextStyle = TextStyle::new(24.0, Weight::Regular, Family::Serif);

    fn paragraph(text: &str) -> StyledParagraph {
        StyledParagraph { text: text.into(), spans: Vec::new(), base: BODY, muted: INK2, indent: 0.0 }
    }

    fn styled(text: &str, style: SpanStyle) -> StyledParagraph {
        let mut p = paragraph(text);
        if !text.is_empty() {
            p.spans.push(StyledSpan { bytes: 0..text.len(), style });
        }
        p
    }

    fn draw(renderer: &mut TextRenderer, p: StyledParagraph) -> Canvas {
        let mut pages = renderer.paginate_styled(vec![StyledBlock::Paragraph(p)], 600, 120).unwrap();
        let mut canvas = Canvas::new(620, 140);
        let rect = Rect::new(10, 10, 600, 120);
        renderer.draw_styled_page(&mut canvas, &mut pages, 0, rect, rect, 2).unwrap();
        canvas
    }

    /// Read pages in order and reconstruct each original paragraph's bytes.
    /// Contiguous line ranges are also the only boundaries pages can use.
    fn assert_partition(pagination: &StyledPagination) {
        assert!(pagination.is_complete());
        let blocks = &pagination.blocks;
        let mut recovered = vec![String::new(); blocks.len()];
        let mut next_line = vec![0; pagination.paragraphs.len()];
        for page in pagination.pages() {
            assert!(page.height <= pagination.height as f32);
            let mut bottom = 0.0;
            for part in &page.paragraphs {
                let p = &pagination.paragraphs()[part.paragraph];
                let text = &source(blocks, p.block).text[p.source_bytes.clone()];
                assert_eq!(part.lines.start, next_line[part.paragraph]);
                next_line[part.paragraph] = part.lines.end;
                assert!(part.y >= bottom);
                for line in &p.lines[part.lines.clone()] {
                    assert_eq!(recovered[p.block].len(), p.source_bytes.start + line.bytes.start);
                    recovered[p.block].push_str(&text[line.bytes.clone()]);
                }
                bottom = part.y + p.lines[part.lines.clone()].iter().map(|l| l.height).sum::<f32>();
            }
            assert_eq!(bottom, page.height);
        }
        for (i, block) in blocks.iter().enumerate() {
            if let StyledBlock::Paragraph(p) = block {
                assert_eq!(recovered[i], p.text);
            }
        }
        for (p, end) in pagination.paragraphs().iter().zip(next_line) {
            assert_eq!(end, p.lines.len());
            assert_eq!(p.lines.last().unwrap().bytes.end, p.source_bytes.len());
        }
    }

    /// Every piece, shaped again for drawing, wraps exactly as recorded.
    fn assert_pieces_rewrap(renderer: &mut TextRenderer, pagination: &StyledPagination) {
        for (i, p) in pagination.paragraphs().iter().enumerate() {
            let layout = renderer.layout_styled_paragraph(&pagination.piece(i), pagination.width as f32).unwrap();
            assert_eq!(
                layout.lines.iter().map(|l| l.bytes.clone()).collect::<Vec<_>>(),
                p.lines.iter().map(|l| l.bytes.clone()).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn pages_partition_utf8_whitespace_and_hard_breaks_at_shaped_lines() {
        let mut renderer = TextRenderer::new();
        for text in [
            "  First line has words to wrap.   \n\nSecond line.\n\n".repeat(15),
            "Длинное определение с переносами строк.\r\n\r\n".repeat(15),
            "עברית العربية café e\u{301} שלום\n".repeat(15),
            "paragraph\u{2029}next\u{0085}last".repeat(15),
            "supercalifragilisticexpialidocious".repeat(30),
            " \n\n\n \t\n".into(),
            "".into(),
        ] {
            let blocks = vec![StyledBlock::Paragraph(styled(&text, SpanStyle::ITALIC))];
            let pagination = renderer.paginate_styled(blocks, 210, 150).unwrap();
            assert_partition(&pagination);
            assert_pieces_rewrap(&mut renderer, &pagination);
        }
    }

    #[test]
    fn dividers_fit_with_the_following_line_and_collapse_at_page_edges() {
        let mut renderer = TextRenderer::new();
        let rule = || StyledBlock::Rule { space: 10.0, thickness: 2, gray: 0xbb };
        let blocks = vec![
            rule(),
            StyledBlock::Paragraph(paragraph("first")),
            rule(),
            StyledBlock::Paragraph(paragraph("second")),
            rule(),
        ];
        let mut pages = renderer.paginate_styled(blocks.clone(), 200, 100).unwrap();
        assert_partition(&pages);
        assert_eq!(pages.pages.len(), 1);
        assert_eq!(pages.pages[0].rules, vec![(40.0, 2, 0xbb)]);
        let mut canvas = Canvas::new(200, 100);
        let rect = canvas.bounds();
        renderer.draw_styled_page(&mut canvas, &mut pages, 0, rect, rect, 1).unwrap();
        assert!(canvas.gray_data()[40 * 200..42 * 200].iter().all(|&p| p == 0xbb));
        let pages = renderer.paginate_styled(blocks, 200, 60).unwrap();
        assert_partition(&pages);
        assert_eq!(pages.pages.len(), 2);
        assert!(pages.pages.iter().all(|p| p.rules.is_empty() && p.height == 30.0));
    }

    #[test]
    fn mixed_headword_body_heights_and_gaps_fit() {
        let mut renderer = TextRenderer::new();
        let mut headword = styled("An important headword", SpanStyle::BOLD);
        headword.base = BODY.size(38.0);
        let mut blocks = vec![StyledBlock::Gap(1000.0)];
        for _ in 0..12 {
            blocks.extend([
                StyledBlock::Paragraph(headword.clone()),
                StyledBlock::Paragraph(paragraph(&"Some definition words. ".repeat(20))),
                StyledBlock::Gap(12.0),
                StyledBlock::Gap(8.0),
            ]);
        }
        let pagination = renderer.paginate_styled(blocks.clone(), 360, 230).unwrap();
        assert_partition(&pagination);
        assert!(pagination.pages().len() > 12);
        assert!(pagination.pages().iter().all(|p| p.paragraphs.first().unwrap().y == 0.0));
        assert!(renderer.paginate_styled(blocks, 360, 20).is_err());
        let empty = renderer.paginate_styled(Vec::new(), 360, 230).unwrap();
        assert_eq!(empty.pages().len(), 1);
        assert_eq!(empty.pages()[0].height, 0.0);
        let gaps = renderer.paginate_styled(vec![StyledBlock::Gap(5.0)], 360, 230).unwrap();
        assert_eq!(gaps.pages().len(), 1);
    }

    #[test]
    fn every_wrapped_line_keeps_the_indent_and_page_clipping() {
        let mut renderer = TextRenderer::new();
        let mut p = paragraph(&"Hamburg handword ".repeat(8));
        p.indent = 37.0;
        let layout = renderer.layout_styled_paragraph(&p, 210.0).unwrap();
        assert!(layout.lines.len() > 2);
        for run in layout.buffer.layout_runs() {
            assert_eq!(run.glyphs.first().unwrap().x, 0.0);
            assert!(run.line_w <= 210.0 - p.indent);
        }
        let mut pagination = renderer.paginate_styled(vec![StyledBlock::Paragraph(p)], 210, 150).unwrap();
        let mut canvas = Canvas::new(260, 200);
        let rect = Rect::new(10, 15, 210, 150);
        for page in 0..pagination.pages().len() {
            canvas.fill(PAPER);
            renderer.draw_styled_page(&mut canvas, &mut pagination, page, rect, rect, 2).unwrap();
            let content = &pagination.pages()[page];
            for part in &content.paragraphs {
                for line in 0..part.lines.len() {
                    let y = rect.y as usize + part.y as usize + line * BODY.line_height() as usize;
                    let left = (0..260)
                        .find(|&x| {
                            (y..y + BODY.line_height() as usize).any(|y| canvas.gray_data()[y * 260 + x] < PAPER)
                        })
                        .unwrap();
                    assert!((47..=49).contains(&left), "line {line} starts at {left}");
                }
                assert_eq!(pagination.piece(part.paragraph).indent, 37.0);
            }
            for (i, pixel) in canvas.gray_data().iter().enumerate() {
                if i % 260 < 47 || i % 260 >= 220 || i / 260 < 15 || i / 260 >= 165 {
                    assert_eq!(*pixel, PAPER);
                }
            }
        }
    }

    #[test]
    fn a_wider_clip_keeps_accents_above_the_first_line() {
        let mut renderer = TextRenderer::new();
        let mut p = paragraph("Ǻ Å É");
        p.base = BODY.size(38.0);
        let mut pagination = renderer.paginate_styled(vec![StyledBlock::Paragraph(p)], 300, 100).unwrap();
        let rect = Rect::new(10, 20, 300, 100);
        let ink_above = |clip: Rect, renderer: &mut TextRenderer, pagination: &mut StyledPagination| {
            let mut canvas = Canvas::new(320, 140);
            renderer.draw_styled_page(&mut canvas, pagination, 0, rect, clip, 1).unwrap();
            canvas.gray_data()[..20 * 320].iter().any(|&pixel| pixel < PAPER)
        };
        assert!(!ink_above(rect, &mut renderer, &mut pagination));
        assert!(ink_above(Rect::new(0, 0, 320, 140), &mut renderer, &mut pagination));
    }

    #[test]
    fn bold_italic_and_bold_italic_use_real_faces_and_change_width_and_ink() {
        let mut renderer = TextRenderer::new();
        let text = "Verso reading office";
        let regular = renderer.layout_styled_paragraph(&paragraph(text), 600.0).unwrap();
        let width = regular.buffer.layout_runs().next().unwrap().line_w;
        let ink = draw(&mut renderer, paragraph(text)).gray_data().to_vec();
        for style in [SpanStyle::BOLD, SpanStyle::ITALIC, SpanStyle::BOLD.union(SpanStyle::ITALIC)] {
            let p = styled(text, style);
            let layout = renderer.layout_styled_paragraph(&p, 600.0).unwrap();
            let run = layout.buffer.layout_runs().next().unwrap();
            assert!((width - run.line_w).abs() > 0.1, "{style:?} should change width");
            for glyph in run.glyphs {
                let face = renderer.fonts.db().face(glyph.font_id).unwrap();
                assert_eq!(face.families[0].0, "Literata");
                assert_eq!(face.style == fontdb::Style::Italic, style.contains(SpanStyle::ITALIC));
                assert_eq!(face.weight.0, if style.contains(SpanStyle::BOLD) { 600 } else { 400 });
            }
            assert_ne!(ink, draw(&mut renderer, p).gray_data());
        }
    }

    #[test]
    fn span_metadata_controls_muted_ink_and_phonetic_family() {
        let mut renderer = TextRenderer::new();
        let mut p = paragraph("HHHH HHHH /ɪʊʌɒɔɜːʃʒ/");
        p.spans = vec![
            StyledSpan { bytes: 5..9, style: SpanStyle::MUTED },
            StyledSpan { bytes: 10..p.text.len(), style: SpanStyle::PHONETIC.union(SpanStyle::ITALIC) },
        ];
        let layout = renderer.layout_styled_paragraph(&p, 600.0).unwrap();
        let run = layout.buffer.layout_runs().next().unwrap();
        for glyph in run.glyphs {
            let gray = (glyph.metadata >> 8) as u8;
            assert_eq!(gray, if (5..9).contains(&glyph.start) { INK2 } else { INK });
            assert_ne!(glyph.glyph_id, 0);
            if glyph.start >= 10 {
                assert_eq!(renderer.fonts.db().face(glyph.font_id).unwrap().families[0].0, "Inter");
            }
        }
        let muted_x = run.glyphs.iter().find(|g| g.start == 5).unwrap().x as usize + 10;
        let end_x = run.glyphs.iter().find(|g| g.start == 9).unwrap().x as usize + 10;
        let canvas = draw(&mut renderer, p);
        let pixels = canvas.gray_data();
        let band = |left, right| (10..60).flat_map(|y| (left..right).map(move |x| pixels[y * 620 + x])).min().unwrap();
        assert_eq!(band(10, muted_x - 1), INK);
        assert_eq!(band(muted_x, end_x), INK2);
        let black = draw(&mut renderer, paragraph("Hamburg"));
        let muted = draw(&mut renderer, styled("Hamburg", SpanStyle::MUTED));
        for (&black, &muted) in black.gray_data().iter().zip(muted.gray_data()) {
            // The same glyph mask blends against PAPER, with INK2 as its ink.
            let expected = INK2 as f32 + black as f32 * (255 - INK2) as f32 / 255.0;
            assert!((muted as f32 - expected).abs() <= 1.0);
        }
        // The muted gray comes from the paragraph, not a fixed theme value.
        let mut lighter = styled("Hamburg", SpanStyle::MUTED);
        lighter.muted = 0x80;
        let canvas = draw(&mut renderer, lighter);
        assert_eq!(canvas.gray_data().iter().min(), Some(&0x80));
    }

    #[test]
    fn underlines_form_one_hairline_per_visual_run_and_stop_at_wraps() {
        let mut renderer = TextRenderer::new();
        let mut p = paragraph("one two three four five six seven");
        p.spans = vec![
            StyledSpan { bytes: 0..7, style: SpanStyle::UNDERLINE },
            StyledSpan { bytes: 14..p.text.len(), style: SpanStyle::UNDERLINE.union(SpanStyle::MUTED) },
        ];
        let wide = renderer.layout_styled_paragraph(&p, 600.0).unwrap();
        let strokes = underlines(&wide.buffer.layout_runs().next().unwrap());
        assert_eq!(strokes.len(), 2);
        assert_eq!((strokes[0].2, strokes[1].2), (INK, INK2));
        let layout = renderer.layout_styled_paragraph(&p, 180.0).unwrap();
        let runs: Vec<_> = layout.buffer.layout_runs().collect();
        assert!(runs.len() > 1);
        for run in &runs {
            let strokes = underlines(run);
            for (left, right, gray) in strokes {
                assert!(right > left && right <= 180.0);
                assert!(gray == INK || gray == INK2);
            }
        }
        let linked = draw(&mut renderer, styled("one two three", SpanStyle::UNDERLINE));
        let plain = draw(&mut renderer, paragraph("one two three"));
        let changed_rows: BTreeSet<_> = linked
            .gray_data()
            .iter()
            .zip(plain.gray_data())
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(i, _)| i / 620)
            .collect();
        assert_eq!(changed_rows.len(), 2);
        for y in changed_rows {
            let xs: Vec<_> =
                (0..620).filter(|&x| linked.gray_data()[y * 620 + x] != plain.gray_data()[y * 620 + x]).collect();
            assert!(xs.len() > 100);
            assert_eq!(xs.len(), xs.last().unwrap() - xs[0] + 1, "one continuous stroke including spaces");
        }
    }

    #[test]
    fn oversized_paragraphs_split_at_the_lines_of_the_whole_paragraph() {
        let mut renderer = TextRenderer::new();
        for text in ["café definition слова ".repeat(1900), "Ж".repeat(18000), "word ".repeat(4000)] {
            let mut p = styled(&text, SpanStyle::ITALIC.union(SpanStyle::MUTED));
            p.indent = 28.0;
            assert!(renderer.layout_styled_paragraph(&p, 400.0).is_err());
            // Shaping the whole paragraph is only allowed here, as a reference.
            let whole: Vec<_> = renderer.shape_styled(&p, 400.0).lines.into_iter().map(|l| l.bytes).collect();
            let pagination = renderer.paginate_styled(vec![StyledBlock::Paragraph(p)], 400, 300).unwrap();
            assert!(pagination.paragraphs().len() > 1);
            assert_partition(&pagination);
            assert_pieces_rewrap(&mut renderer, &pagination);
            // No piece boundary adds a short line: the pieces' lines are
            // exactly the lines of the unsplit paragraph.
            let pieces: Vec<_> = pagination
                .paragraphs()
                .iter()
                .flat_map(|p| {
                    p.lines.iter().map(|l| l.bytes.start + p.source_bytes.start..l.bytes.end + p.source_bytes.start)
                })
                .collect();
            assert_eq!(pieces, whole);
            for i in 0..pagination.paragraphs().len() {
                let piece = pagination.piece(i);
                assert!(piece.text.len() <= MAX_PARAGRAPH_BYTES);
                assert_eq!(piece.indent, 28.0);
                assert_eq!(piece.spans.len(), 1);
                assert_eq!(piece.spans[0].bytes, 0..piece.text.len());
                assert_eq!(piece.spans[0].style, SpanStyle::ITALIC.union(SpanStyle::MUTED));
            }
        }
    }

    #[test]
    fn cuts_never_split_crlf_or_utf8() {
        let mut text = "x".repeat(MAX_PARAGRAPH_BYTES - 1);
        text.push_str("\r\nnext");
        assert_eq!(chunk_end(&text, 0), MAX_PARAGRAPH_BYTES - 1);
        let text = "Ж".repeat(MAX_PARAGRAPH_BYTES);
        assert!(text.is_char_boundary(chunk_end(&text, 0)));
        assert_eq!(chunk_end("\r\n", 1), 2);
    }

    #[test]
    fn incremental_pagination_shows_the_first_page_early_and_matches_the_whole() {
        let mut renderer = TextRenderer::new();
        let blocks: Vec<_> = (0..40)
            .flat_map(|i| {
                [
                    StyledBlock::Paragraph(styled(&format!("Sense {i} "), SpanStyle::BOLD)),
                    StyledBlock::Paragraph(paragraph(&"Long definition text. ".repeat(900))),
                    StyledBlock::Gap(10.0),
                ]
            })
            .collect();
        let whole = renderer.paginate_styled(blocks.clone(), 400, 300).unwrap();
        let mut pagination = StyledPagination::new(blocks, 400, 300).unwrap();
        assert!(pagination.pages().is_empty() && !pagination.is_complete());
        renderer.paginate_styled_until(&mut pagination, 1).unwrap();
        assert!(!pagination.pages().is_empty() && !pagination.is_complete());
        assert!(pagination.paragraphs().len() <= 2, "only the first blocks were shaped");
        let mut canvas = Canvas::new(400, 300);
        let rect = Rect::new(0, 0, 400, 300);
        renderer.draw_styled_page(&mut canvas, &mut pagination, 0, rect, rect, 1).unwrap();
        let unfinished = pagination.pages().len();
        assert!(renderer.draw_styled_page(&mut canvas, &mut pagination, unfinished, rect, rect, 1).is_err());
        while renderer.paginate_styled_step(&mut pagination).unwrap() {}
        assert!(pagination.is_complete());
        assert!(!renderer.paginate_styled_step(&mut pagination).unwrap());
        assert_partition(&pagination);
        let lines = |p: &StyledPagination| {
            p.pages()
                .iter()
                .map(|page| page.paragraphs.iter().map(|part| part.lines.clone()).collect())
                .collect::<Vec<Vec<_>>>()
        };
        assert_eq!(lines(&pagination), lines(&whole));
        assert_eq!(pagination.missing_logged.len(), pagination.pages().len());
        assert!(pagination.missing_logged[0]);
    }

    #[test]
    fn one_mib_pagination_retains_no_layouts_and_drawing_cache_is_bounded() {
        let mut renderer = TextRenderer::new();
        let text = "styled definition words ".repeat(1_048_576 / 24 + 1);
        let mut p = paragraph(&text[..1_048_576]);
        p.base = BODY.size(16.0);
        p.spans = (0..p.text.len())
            .step_by(48)
            .map(|start| StyledSpan {
                bytes: start..(start + 24).min(p.text.len()),
                style: SpanStyle::BOLD.union(SpanStyle::ITALIC),
            })
            .collect();
        let blocks = vec![StyledBlock::Paragraph(p)];
        let start = std::time::Instant::now();
        let mut pagination = renderer.paginate_styled(blocks, 600, 500).unwrap();
        eprintln!("1 MiB styled pagination: {:?}, {} pages", start.elapsed(), pagination.pages().len());
        assert_partition(&pagination);
        assert!(pagination.pages().len() > 100);
        assert!(pagination.paragraphs().len() >= 64);
        assert!(pagination.layouts.is_empty());
        assert!(renderer.layouts.is_empty());
        let mut canvas = Canvas::new(600, 500);
        let rect = Rect::new(0, 0, 600, 500);
        for index in (0..pagination.paragraphs().len()).step_by(3) {
            let page = pagination
                .pages()
                .iter()
                .position(|page| page.paragraphs.iter().any(|part| part.paragraph == index))
                .unwrap();
            renderer.draw_styled_page(&mut canvas, &mut pagination, page, rect, rect, 1).unwrap();
            assert!(pagination.layouts.len() <= DRAW_CACHE_LIMIT);
        }
        assert!(renderer.layouts.is_empty());
    }

    #[test]
    fn drawing_only_shapes_visible_paragraphs_and_missing_glyphs_are_page_scoped() {
        let mut renderer = TextRenderer::new();
        let blocks: Vec<_> = (0..12).map(|_| StyledBlock::Paragraph(paragraph("word \u{10ffff} \u{10ffff}"))).collect();
        let mut pagination = renderer.paginate_styled(blocks, 300, 60).unwrap();
        let mut canvas = Canvas::new(300, 60);
        let rect = Rect::new(0, 0, 300, 60);
        renderer.draw_styled_page(&mut canvas, &mut pagination, 2, rect, rect, 1).unwrap();
        let visible: Vec<_> = pagination.pages()[2].paragraphs.iter().map(|part| part.paragraph).collect();
        assert_eq!(pagination.layouts.len(), visible.len());
        assert!(pagination.layouts.iter().all(|(index, _)| visible.contains(index)));
        assert_eq!(pagination.missing_logged.iter().filter(|&&logged| logged).count(), 1);
        let missing: BTreeSet<_> = pagination
            .layouts
            .iter()
            .flat_map(|(_, buffer)| buffer.layout_runs())
            .flat_map(|run| {
                run.glyphs.iter().filter(|g| g.glyph_id == 0).flat_map(move |g| run.text[g.start..g.end].chars())
            })
            .collect();
        assert_eq!(missing, BTreeSet::from(['\u{10ffff}']));
        let first = canvas.gray_data().to_vec();
        canvas.fill(PAPER);
        renderer.draw_styled_page(&mut canvas, &mut pagination, 2, rect, rect, 1).unwrap();
        assert_eq!(first, canvas.gray_data());
        assert_eq!(pagination.missing_logged.iter().filter(|&&logged| logged).count(), 1);
    }

    #[test]
    fn invalid_input_returns_errors() {
        let mut renderer = TextRenderer::new();
        let mut p = paragraph("café");
        p.spans = vec![StyledSpan { bytes: 0..4, style: SpanStyle::ITALIC }];
        assert!(renderer.layout_styled_paragraph(&p, 200.0).is_err());
        p.spans = vec![
            StyledSpan { bytes: 1..5, style: SpanStyle::BOLD },
            StyledSpan { bytes: 0..1, style: SpanStyle::ITALIC },
        ];
        assert!(renderer.layout_styled_paragraph(&p, 200.0).is_err());
        // Bad input anywhere fails before any shaping.
        let blocks = vec![StyledBlock::Paragraph(paragraph("fine")), StyledBlock::Paragraph(p.clone())];
        assert!(StyledPagination::new(blocks, 200, 200).is_err());
        p.spans.clear();
        p.indent = 200.0;
        assert!(renderer.layout_styled_paragraph(&p, 200.0).is_err());
        assert!(renderer.paginate_styled(Vec::new(), 0, 200).is_err());
        assert!(renderer.paginate_styled(vec![StyledBlock::Gap(f32::NAN)], 200, 200).is_err());
    }
}
