//! Text layout and drawing with cosmic-text.
//!
//! The UI fonts are compiled into the binary, so the app can always draw text,
//! even if its folder on the device is incomplete: Inter for controls and
//! labels, Literata for titles. Laid-out text is cached, so redrawing a whole
//! screen every frame stays cheap.

use std::collections::HashMap;
use std::sync::Arc;

use cosmic_text::{
    Align, Attrs, Buffer, Ellipsize, EllipsizeHeightLimit, Family as CtFamily, FontSystem, Metrics, Shaping,
    SwashCache, SwashContent, Wrap, fontdb,
};

use super::Canvas;
use crate::geom::Rect;

const INTER_REGULAR: &[u8] = include_bytes!("../../../../assets/fonts/Inter-Regular.ttf");
const INTER_SEMIBOLD: &[u8] = include_bytes!("../../../../assets/fonts/Inter-SemiBold.ttf");
const LITERATA_REGULAR: &[u8] = include_bytes!("../../../../assets/fonts/Literata-Regular.ttf");
const LITERATA_SEMIBOLD: &[u8] = include_bytes!("../../../../assets/fonts/Literata-SemiBold.ttf");

/// Laid-out texts kept at most. The cache is simply cleared when it's full;
/// a screen only shows a few dozen texts.
const CACHE_LIMIT: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Family {
    /// Inter: controls, labels, everything you tap.
    Sans,
    /// Literata: screen titles and book titles.
    Serif,
}

impl Family {
    fn name(self) -> &'static str {
        match self {
            Family::Sans => "Inter",
            Family::Serif => "Literata",
        }
    }

    /// Height of capital letters, as a fraction of the font size. Text is
    /// centred on its capitals, so labels look centred in their boxes.
    fn cap_height(self) -> f32 {
        match self {
            Family::Sans => 0.727,
            Family::Serif => 0.700,
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
    glyphs: SwashCache,
    layouts: HashMap<Key, Buffer>,
}

impl TextRenderer {
    pub fn new() -> Self {
        let mut db = fontdb::Database::new();
        for data in [INTER_REGULAR, INTER_SEMIBOLD, LITERATA_REGULAR, LITERATA_SEMIBOLD] {
            db.load_font_source(fontdb::Source::Binary(Arc::new(data)));
        }
        db.set_sans_serif_family(Family::Sans.name());
        db.set_serif_family(Family::Serif.name());
        Self {
            fonts: FontSystem::new_with_locale_and_db("en-US".into(), db),
            glyphs: SwashCache::new(),
            layouts: HashMap::new(),
        }
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
        let Self { fonts, glyphs, layouts } = self;
        let buffer = &layouts[&key];

        // Centre the block from the top of the first line's capitals to the
        // last baseline.
        let mut runs = buffer.layout_runs().peekable();
        let Some(first) = runs.peek().map(|r| r.line_y) else { return };
        let last = buffer.layout_runs().last().map_or(first, |r| r.line_y);
        let cap = style.family.cap_height() * style.size;
        let top = rect.y as f32 + rect.h as f32 / 2.0 - (first - cap + last) / 2.0;
        let top = top.round();

        for run in runs {
            for glyph in run.glyphs {
                let phys = glyph.physical((rect.x as f32, top + run.line_y), 1.0);
                let Some(image) = glyphs.get_image(fonts, phys.cache_key) else { continue };
                let p = image.placement;
                let (x, y) = (phys.x + p.left, phys.y - p.top);
                match image.content {
                    SwashContent::Mask => canvas.blend_mask(x, y, p.width, p.height, &image.data, style.gray, rect),
                    // Colour glyphs (emoji): use their alpha as coverage.
                    SwashContent::Color => {
                        let alpha: Vec<u8> = image.data.chunks_exact(4).map(|px| px[3]).collect();
                        canvas.blend_mask(x, y, p.width, p.height, &alpha, style.gray, rect)
                    }
                    SwashContent::SubpixelMask => {}
                }
            }
        }
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
        let mut attrs = Attrs::new().family(CtFamily::Name(style.family.name())).weight(weight);
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
    fn both_families_and_weights_are_available() {
        let mut t = TextRenderer::new();
        let faces: Vec<(String, u16)> = t.fonts.db().faces().map(|f| (f.families[0].0.clone(), f.weight.0)).collect();
        for want in [("Inter", 400), ("Inter", 600), ("Literata", 400), ("Literata", 600)] {
            assert!(faces.iter().any(|(n, w)| n == want.0 && *w == want.1), "{want:?} missing: {faces:?}");
        }
        let regular = t.width("Verso", STYLE);
        let bold = t.width("Verso", STYLE.weight(Weight::SemiBold));
        assert!(bold > regular, "semibold should be wider: {bold} vs {regular}");
        let serif = t.width("Verso", STYLE.family(Family::Serif));
        assert!((serif - regular).abs() > 1.0, "Literata should differ from Inter");
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
