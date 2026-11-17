//! Text layout and drawing with cosmic-text.
//!
//! The UI fonts are compiled into the binary, so the app can always draw text,
//! even if its folder on the device is incomplete: Inter for controls and
//! labels, Literata for titles. Laid-out text is cached, so redrawing a whole
//! screen every frame stays cheap.

use std::collections::HashMap;
use std::sync::Arc;

use cosmic_text::{
    Align, Attrs, Buffer, Ellipsize, EllipsizeHeightLimit, Family as CtFamily, FontSystem, LineIter, Metrics, Shaping,
    SwashCache, SwashContent, Wrap, fontdb,
};

use super::Canvas;
use crate::geom::Rect;

pub mod styled;

const INTER_REGULAR: &[u8] = include_bytes!("../../../../assets/fonts/Inter-Regular.ttf");
const INTER_SEMIBOLD: &[u8] = include_bytes!("../../../../assets/fonts/Inter-SemiBold.ttf");
const LITERATA_REGULAR: &[u8] = include_bytes!("../../../../assets/fonts/Literata-Regular.ttf");
const LITERATA_SEMIBOLD: &[u8] = include_bytes!("../../../../assets/fonts/Literata-SemiBold.ttf");
const LITERATA_ITALIC: &[u8] = include_bytes!("../../../../assets/fonts/Literata-Italic.ttf");
const LITERATA_SEMIBOLD_ITALIC: &[u8] = include_bytes!("../../../../assets/fonts/Literata-SemiBoldItalic.ttf");

/// Laid-out texts kept at most. The cache is simply cleared when it's full;
/// a screen only shows a few dozen texts.
const CACHE_LIMIT: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Family {
    /// Inter: controls, labels, everything you tap.
    Sans,
    /// Literata: screen titles and book titles.
    Serif,
    Book(u16),
}

impl Family {
    fn name(self) -> &'static str {
        match self {
            Family::Sans => "Inter",
            Family::Serif | Family::Book(_) => "Literata",
        }
    }

    /// Height of capital letters, as a fraction of the font size. Text is
    /// centred on its capitals, so labels look centred in their boxes.
    fn cap_height(self) -> f32 {
        match self {
            Family::Sans => 0.727,
            Family::Serif | Family::Book(_) => 0.700,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Weight {
    Regular,
    SemiBold,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HAlign {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextStyle {
    /// Font size in pixels.
    pub size: f32,
    pub weight: Weight,
    pub family: Family,
    /// 0 = black, 255 = white.
    pub gray: u8,
    /// Extra space between letters, in em (0.08 = 8% of the font size).
    pub tracking: f32,
}

impl TextStyle {
    pub const fn new(size: f32, weight: Weight, family: Family) -> Self {
        Self { size, weight, family, gray: 0, tracking: 0.0 }
    }

    pub const fn gray(self, gray: u8) -> Self {
        Self { gray, ..self }
    }

    pub const fn weight(self, weight: Weight) -> Self {
        Self { weight, ..self }
    }

    #[allow(dead_code)] // for screens still to come
    pub const fn size(self, size: f32) -> Self {
        Self { size, ..self }
    }

    #[allow(dead_code)] // for screens still to come
    pub const fn family(self, family: Family) -> Self {
        Self { family, ..self }
    }

    pub const fn tracking(self, tracking: f32) -> Self {
        Self { tracking, ..self }
    }

    /// Distance between baselines of wrapped lines.
    pub fn line_height(&self) -> f32 {
        (self.size * 1.25).round()
    }
}

/// Everything that changes how text is laid out (colour doesn't).
#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    text: String,
    size: u32,
    weight: Weight,
    family: Family,
    tracking: u32,
    width: Option<u32>,
    /// 0 = no limit.
    max_lines: usize,
    align: HAlign,
}

pub struct TextRenderer {
    fonts: FontSystem,
    book_families: Vec<String>,
    glyphs: SwashCache,
    layouts: HashMap<Key, Buffer>,
}

impl TextRenderer {
    pub fn new() -> Self {
        let mut db = fontdb::Database::new();
        for data in [
            INTER_REGULAR,
            INTER_SEMIBOLD,
            LITERATA_REGULAR,
            LITERATA_SEMIBOLD,
            LITERATA_ITALIC,
            LITERATA_SEMIBOLD_ITALIC,
        ] {
            db.load_font_source(fontdb::Source::Binary(Arc::new(data)));
        }
        db.set_sans_serif_family(Family::Sans.name());
        db.set_serif_family(Family::Serif.name());
        Self {
            fonts: FontSystem::new_with_locale_and_db("en-US".into(), db),
            book_families: Vec::new(),
            glyphs: SwashCache::new(),
            layouts: HashMap::new(),
        }
    }

    /// Load a selected font locally, without copying any Kindle font off the device.
    pub fn book_family(&mut self, name: &str, path: Option<&std::path::Path>) -> Family {
        if let Some(i) = self.book_families.iter().position(|f| f == name) {
            return Family::Book(i as u16);
        }
        if let Some(path) = path
            && let Err(e) = self.fonts.db_mut().load_font_file(path)
        {
            log::warn!("loading font preview {}: {e}", path.display());
            return Family::Serif;
        }
        let id = self.book_families.len();
        self.book_families.push(name.into());
        Family::Book(id as u16)
    }

    /// Size of the laid-out text: the widest line, and lines × line height.
    /// Lines wrap at `max_width`, if given.
    pub fn measure(&mut self, text: &str, style: TextStyle, max_width: Option<f32>) -> (f32, f32) {
        let buffer = self.layout(text, style, max_width, 0, HAlign::Left);
        buffer.layout_runs().fold((0.0, 0.0), |(w, h), run| (w.max(run.line_w), h.max(run.line_top + run.line_height)))
    }

    /// Width of one line of text.
    pub fn width(&mut self, text: &str, style: TextStyle) -> f32 {
        self.measure(text, style, None).0
    }

    /// Split at shaped line boundaries, including empty paragraph lines.
    /// Shape at most 16 KiB at once, keeping long dictionary entries bounded.
    pub fn pages(&mut self, source: &str, style: TextStyle, width: i32, height: i32) -> Vec<String> {
        let lines = (height as f32 / style.line_height()).floor().max(1.0) as usize;
        let mut pages = Vec::new();
        let mut remaining = source;
        while !remaining.is_empty() {
            let mut end = remaining.len().min(16384);
            while !remaining.is_char_boundary(end) {
                end -= 1;
            }
            if end < remaining.len()
                && let Some(space) = remaining[..end].rfind(char::is_whitespace)
                && space > end / 2
            {
                end = space + remaining[space..].chars().next().unwrap().len_utf8();
            }
            let text = &remaining[..end];
            let buffer = self.shape(text, style, Some(width.max(1) as f32), 0, HAlign::Left);
            let offsets = paragraph_offsets(text);
            let mut start = 0;
            for (i, run) in buffer.layout_runs().enumerate() {
                if i > 0 && i % lines == 0 {
                    let boundary = offsets[run.line_i] + run.glyphs.iter().map(|g| g.start).min().unwrap_or(0);
                    if boundary > start {
                        pages.push(text[start..boundary].to_owned());
                        start = boundary;
                    }
                }
            }
            if start < text.len() {
                pages.push(text[start..].to_owned());
            }
            remaining = &remaining[end..];
        }
        if pages.is_empty() {
            pages.push(String::new());
        }
        pages
    }

    /// Byte offsets where each wrapped line of `text` starts (at least `[0]`).
    /// Hard paragraph breaks start a new line too.
    pub fn line_starts(&mut self, text: &str, style: TextStyle, width: i32) -> Vec<usize> {
        let offsets = paragraph_offsets(text);
        let buffer = self.layout(text, style, Some(width.max(1) as f32), 0, HAlign::Left);
        let mut starts: Vec<usize> = buffer
            .layout_runs()
            .map(|run| offsets[run.line_i] + run.glyphs.iter().map(|g| g.start).min().unwrap_or(0))
            .collect();
        starts.dedup();
        if starts.first() != Some(&0) {
            starts.insert(0, 0);
        }
        starts
    }

    /// Draws one line of text in `rect`, cut short with "…" if it's too wide,
    /// and centred vertically on its capital letters.
    pub fn line(&mut self, canvas: &mut Canvas, text: &str, style: TextStyle, rect: Rect, align: HAlign) {
        self.draw_lines(canvas, text, style, rect, align, 1);
    }

    /// Draws text wrapped to the width of `rect`, as many lines as fit (the
    /// last one ends in "…" if there's more), centred vertically.
    pub fn draw(&mut self, canvas: &mut Canvas, text: &str, style: TextStyle, rect: Rect, align: HAlign) {
        let fit = ((rect.h as f32 / style.line_height()).floor() as usize).max(1);
        self.draw_lines(canvas, text, style, rect, align, fit);
    }

    /// Draws at most `max_lines` lines (0 = no limit), centred vertically.
    pub fn draw_lines(
        &mut self,
        canvas: &mut Canvas,
        text: &str,
        style: TextStyle,
        rect: Rect,
        align: HAlign,
        max_lines: usize,
    ) {
        if rect.is_empty() || text.is_empty() {
            return;
        }
        let key = self.ensure_layout(text, style, Some(rect.w as f32), max_lines, align);
        self.paint(canvas, &key, style, rect, align);
    }

    /// Draws one line laid out without a width limit, from the left of
    /// `rect` and clipped to it. It shares its layout with `width`, so
    /// measuring a text and then drawing it shapes it only once.
    pub fn line_measured(&mut self, canvas: &mut Canvas, text: &str, style: TextStyle, rect: Rect) {
        if rect.is_empty() || text.is_empty() {
            return;
        }
        let key = self.ensure_layout(text, style, None, 0, HAlign::Left);
        self.paint(canvas, &key, style, rect, HAlign::Left);
    }

    fn paint(&mut self, canvas: &mut Canvas, key: &Key, style: TextStyle, rect: Rect, align: HAlign) {
        let Self { fonts, glyphs, layouts, .. } = self;
        let buffer = &layouts[key];

        // Centre the block from the top of the first line's capitals to the
        // last baseline.
        let mut runs = buffer.layout_runs().peekable();
        let Some(first) = runs.peek().map(|r| r.line_y) else { return };
        let last = buffer.layout_runs().last().map_or(first, |r| r.line_y);
        let cap = style.family.cap_height() * style.size;
        let top = rect.y as f32 + rect.h as f32 / 2.0 - (first - cap + last) / 2.0;
        let top = top.round();

        for run in runs {
            let x = rect.x as f32 - wrapped_space(&run, align);
            for glyph in run.glyphs {
                let phys = glyph.physical((x, top + run.line_y), 1.0);
                let Some(image) = glyphs.get_image(fonts, phys.cache_key) else { continue };
                let p = image.placement;
                let (x, y) = (phys.x + p.left, phys.y - p.top);
                match image.content {
                    SwashContent::Mask => canvas.blend_mask(x, y, p.width, p.height, &image.data, style.gray, rect),
                    // Colour glyphs (emoji): use their alpha as coverage.
                    SwashContent::Color => {
                        let alpha: Vec<u8> = image.data.as_chunks::<4>().0.iter().map(|px| px[3]).collect();
                        canvas.blend_mask(x, y, p.width, p.height, &alpha, style.gray, rect)
                    }
                    SwashContent::SubpixelMask => {}
                }
            }
        }
    }

    #[cfg(test)]
    fn runs(&mut self, text: &str, style: TextStyle, width: f32, max_lines: usize) -> Vec<(String, f32)> {
        let buffer = self.shape(text, style, Some(width), max_lines, HAlign::Left);
        buffer
            .layout_runs()
            .map(|r| {
                let first = r.glyphs.first();
                (first.map_or(String::new(), |g| r.text[g.start..g.end].to_owned()), wrapped_space(&r, HAlign::Left))
            })
            .collect()
    }

    fn layout(&mut self, text: &str, style: TextStyle, width: Option<f32>, max_lines: usize, align: HAlign) -> &Buffer {
        let key = self.ensure_layout(text, style, width, max_lines, align);
        &self.layouts[&key]
    }

    fn ensure_layout(
        &mut self,
        text: &str,
        style: TextStyle,
        width: Option<f32>,
        max_lines: usize,
        align: HAlign,
    ) -> Key {
        let key = Key {
            text: text.to_owned(),
            size: style.size.to_bits(),
            weight: style.weight,
            family: style.family,
            tracking: style.tracking.to_bits(),
            width: width.map(|w| w.to_bits()),
            max_lines,
            align,
        };
        if !self.layouts.contains_key(&key) {
            if self.layouts.len() >= CACHE_LIMIT {
                self.layouts.clear();
            }
            let buffer = self.shape(text, style, width, max_lines, align);
            self.layouts.insert(key.clone(), buffer);
        }
        key
    }

    fn shape(&mut self, text: &str, style: TextStyle, width: Option<f32>, max_lines: usize, align: HAlign) -> Buffer {
        let metrics = Metrics::new(style.size, style.line_height());
        let mut buffer = Buffer::new(&mut self.fonts, metrics);
        buffer.set_size(width, None);
        if max_lines == 1 {
            buffer.set_wrap(Wrap::None);
        }
        if max_lines > 0 && width.is_some() {
            buffer.set_ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(max_lines)));
        }
        let weight = match style.weight {
            Weight::Regular => fontdb::Weight::NORMAL,
            Weight::SemiBold => fontdb::Weight::SEMIBOLD,
        };
        let family = match style.family {
            Family::Book(i) => self.book_families.get(i as usize).map(String::as_str).unwrap_or("Literata"),
            other => other.name(),
        };
        let mut attrs = Attrs::new().family(CtFamily::Name(family)).weight(weight);
        if style.tracking != 0.0 {
            attrs = attrs.letter_spacing(style.tracking);
        }
        let align = match align {
            HAlign::Left => Align::Left,
            HAlign::Center => Align::Center,
            HAlign::Right => Align::Right,
        };
        buffer.set_text(text, &attrs, Shaping::Advanced, Some(align));
        buffer.shape_until_scroll(&mut self.fonts, false);
        buffer
    }
}

/// Use the same hard-line splitter as `Buffer::set_text`, so run indices and
/// byte offsets agree for CR, LF, CRLF, and LFCR.
fn paragraph_offsets(text: &str) -> Vec<usize> {
    let mut offsets: Vec<_> = LineIter::new(text).map(|(range, _)| range.start).collect();
    // Buffer::set_text adds a final empty line after a trailing hard break,
    // and one empty line when the input itself is empty.
    if offsets.is_empty() || text.ends_with('\r') || text.ends_with('\n') {
        offsets.push(text.len());
    }
    offsets
}

/// How far to shift a wrapped line left so it doesn't start with a space.
/// With a line limit, cosmic-text lays out the last line again for the "…"
/// and keeps the space it wrapped at.
fn wrapped_space(run: &cosmic_text::LayoutRun, align: HAlign) -> f32 {
    let continues = run.glyphs.first().is_some_and(|g| g.start > 0);
    if !continues {
        return 0.0;
    }
    let space: f32 =
        run.glyphs.iter().take_while(|g| run.text[g.start..g.end].chars().all(char::is_whitespace)).map(|g| g.w).sum();
    match align {
        HAlign::Left => space,
        HAlign::Center => space / 2.0,
        HAlign::Right => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STYLE: TextStyle = TextStyle::new(40.0, Weight::Regular, Family::Sans);

    fn ink(c: &Canvas) -> Vec<u8> {
        let mut gray = vec![0u8; (c.width() * c.height()) as usize];
        c.copy_gray(c.bounds(), &mut gray, c.width() as usize);
        gray
    }

    #[test]
    fn all_six_embedded_faces_are_available() {
        let mut t = TextRenderer::new();
        let faces: Vec<_> = t.fonts.db().faces().map(|f| (f.families[0].0.clone(), f.weight.0, f.style)).collect();
        for (name, weight, style) in [
            ("Inter", 400, fontdb::Style::Normal),
            ("Inter", 600, fontdb::Style::Normal),
            ("Literata", 400, fontdb::Style::Normal),
            ("Literata", 600, fontdb::Style::Normal),
            ("Literata", 400, fontdb::Style::Italic),
            ("Literata", 600, fontdb::Style::Italic),
        ] {
            assert!(
                faces.iter().any(|(n, w, s)| n == name && *w == weight && *s == style),
                "{name} {weight} {style:?} missing: {faces:?}"
            );
        }
        let regular = t.width("Verso", STYLE);
        let bold = t.width("Verso", STYLE.weight(Weight::SemiBold));
        assert!(bold > regular, "semibold should be wider: {bold} vs {regular}");
        let serif = t.width("Verso", STYLE.family(Family::Serif));
        assert!((serif - regular).abs() > 1.0, "Literata should differ from Inter");
        for weight in [fontdb::Weight::NORMAL, fontdb::Weight::SEMIBOLD] {
            let mut shape = |style| {
                let mut buffer = Buffer::new(&mut t.fonts, Metrics::new(40.0, 50.0));
                let attrs = Attrs::new().family(CtFamily::Name("Literata")).weight(weight).style(style);
                buffer.set_text("Verso office", &attrs, Shaping::Advanced, None);
                buffer.shape_until_scroll(&mut t.fonts, false);
                let run = buffer.layout_runs().next().unwrap();
                (run.line_w, run.glyphs.iter().map(|g| (g.font_id, g.glyph_id)).collect::<Vec<_>>())
            };
            let (upright_w, upright) = shape(fontdb::Style::Normal);
            let (italic_w, italic) = shape(fontdb::Style::Italic);
            assert!((upright_w - italic_w).abs() > 1.0, "italic {weight:?} should have its own advances");
            assert!(upright.iter().zip(&italic).all(|(a, b)| a.0 != b.0), "italic {weight:?} should use its own face");
        }
    }

    #[test]
    fn wrapped_lines_never_start_with_a_space() {
        let mut t = TextRenderer::new();
        let style = crate::ui::theme::Theme::for_dpi(300).body;
        let text = "No dictionaries installed. Add a StarDict dictionary to the app data dictionaries/ folder, then open Dictionaries and tap Rescan.";
        // Without a line limit, wrapped lines start at their first word.
        assert!(t.runs(text, style, 812.0, 0).iter().all(|(first, shift)| first != " " && *shift == 0.0));
        // With a limit, cosmic-text lays out the last line again and keeps
        // the space it wrapped at; drawing skips it.
        let runs = t.runs(text, style, 812.0, 3);
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[2].0, " ");
        assert!(runs[2].1 > 9.0, "the leading space is skipped");
        assert_eq!(runs[0].1, 0.0, "a paragraph's own first character is kept");
    }

    #[test]
    fn tracking_widens_text() {
        let mut t = TextRenderer::new();
        assert!(t.width("BUTTONS", STYLE.tracking(0.08)) > t.width("BUTTONS", STYLE) + 10.0);
    }

    #[test]
    fn text_wraps_and_draws() {
        let mut t = TextRenderer::new();
        let (w1, h1) = t.measure("hello e-ink", STYLE, None);
        let (w2, h2) = t.measure("hello e-ink", STYLE, Some(w1 / 2.0 + 10.0));
        assert!(h2 > h1 && w2 < w1);

        let mut c = Canvas::new(300, 100);
        let bounds = c.bounds();
        t.draw(&mut c, "Hi", STYLE, bounds, HAlign::Center);
        assert!(ink(&c).iter().any(|&v| v < 64), "some ink should be drawn");
    }

    #[test]
    fn paged_text_preserves_every_byte_and_fits_without_ellipses() {
        let mut renderer = TextRenderer::new();
        for text in [
            "Paragraph one has some words.\n\nParagraph two.\n\n".repeat(1200),
            "Длинное определение с переносами строк.\n\n".repeat(300),
            "word ".repeat(20000),
            "verylongunbrokenword".repeat(100),
            "עברית العربية café e\u{301} שלום\n".repeat(20),
        ] {
            let pages = renderer.pages(&text, STYLE, 300, 250);
            assert_eq!(pages.concat(), text);
            assert!(pages.len() > 1);
            for page in pages {
                let height = renderer.measure(page.trim_end(), STYLE, Some(300.0)).1;
                assert!(height <= 250.0, "page exceeds its area: {height}: {page:?}");
            }
        }
    }

    #[test]
    fn hard_break_offsets_match_shaped_paragraphs() {
        let mut renderer = TextRenderer::new();
        for (source, starts) in [("a\rb", vec![0, 2]), ("a\n\rb", vec![0, 3]), ("a\r\nb", vec![0, 3])] {
            assert_eq!(renderer.line_starts(source, STYLE, 300), starts, "{source:?}");
            let pages = renderer.pages(source, STYLE, 300, 50);
            assert_eq!(pages.concat(), source, "{source:?}");
            assert!(pages.len() > 1, "hard breaks should paginate: {source:?}");
        }
    }

    #[test]
    fn long_lines_are_cut_short_inside_the_rect() {
        let mut t = TextRenderer::new();
        let mut c = Canvas::new(400, 100);
        let rect = Rect::new(0, 0, 200, 100);
        t.line(&mut c, "A very long title that does not fit at all", STYLE, rect, HAlign::Left);
        let gray = ink(&c);
        let inked_cols: Vec<usize> = (0..400).filter(|&x| (0..100).any(|y| gray[y * 400 + x] < 128)).collect();
        assert!(!inked_cols.is_empty());
        assert!(*inked_cols.last().unwrap() < 200, "text spilled out of its rect");
        assert!(*inked_cols.last().unwrap() > 150, "text should fill most of the width");
    }

    #[test]
    fn capitals_are_centred_vertically() {
        let mut t = TextRenderer::new();
        let mut c = Canvas::new(200, 120);
        t.line(&mut c, "HI", STYLE, Rect::new(0, 0, 200, 120), HAlign::Center);
        let gray = ink(&c);
        let rows: Vec<usize> = (0..120).filter(|&y| (0..200).any(|x| gray[y * 200 + x] < 128)).collect();
        let (top, bottom) = (rows[0] as i32, *rows.last().unwrap() as i32);
        let centre = (top + bottom) / 2;
        assert!((centre - 60).abs() <= 2, "capitals span {top}..{bottom}");
    }
}
