//! Text layout and drawing with cosmic-text, using the bundled UI font.

use std::sync::Arc;

use cosmic_text::{Align, Attrs, Buffer, Family, FontSystem, Metrics, Shaping, SwashCache, SwashContent, fontdb};

use super::Canvas;
use crate::geom::Rect;

/// The UI font is compiled into the binary, so the app can always draw text,
/// even if its folder on the device is incomplete.
const UI_FAMILY: &str = "Inter";
const UI_REGULAR: &[u8] = include_bytes!("../../../../assets/fonts/Inter-Regular.ttf");
const UI_SEMIBOLD: &[u8] = include_bytes!("../../../../assets/fonts/Inter-SemiBold.ttf");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weight {
    Regular,
    SemiBold,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HAlign {
    Left,
    Center,
    #[allow(dead_code)] // not needed by the demo yet
    Right,
}

#[derive(Clone, Copy, Debug)]
pub struct TextStyle {
    /// Font size in pixels.
    pub size: f32,
    pub weight: Weight,
    /// 0 = black, 255 = white.
    pub gray: u8,
}

pub struct TextRenderer {
    fonts: FontSystem,
    cache: SwashCache,
}

impl TextRenderer {
    pub fn new() -> Self {
        let mut db = fontdb::Database::new();
        for data in [UI_REGULAR, UI_SEMIBOLD] {
            db.load_font_source(fontdb::Source::Binary(Arc::new(data)));
        }
        db.set_sans_serif_family(UI_FAMILY);
        Self { fonts: FontSystem::new_with_locale_and_db("en-US".into(), db), cache: SwashCache::new() }
    }

    /// Size of the laid-out text. Lines wrap at `max_width`, if given.
    pub fn measure(&mut self, text: &str, style: TextStyle, max_width: Option<f32>) -> (f32, f32) {
        let buffer = self.layout(text, style, max_width, HAlign::Left);
        extent(&buffer)
    }

    /// Draws `text` inside `rect`: wrapped to its width, aligned horizontally,
    /// centred vertically, and clipped to it.
    pub fn draw(&mut self, canvas: &mut Canvas, text: &str, style: TextStyle, rect: Rect, align: HAlign) {
        let buffer = self.layout(text, style, Some(rect.w as f32), align);
        let (_, height) = extent(&buffer);
        let top = rect.y as f32 + ((rect.h as f32 - height) / 2.0).round();
        for run in buffer.layout_runs() {
            for glyph in run.glyphs {
                let phys = glyph.physical((rect.x as f32, top + run.line_y), 1.0);
                let Some(image) = self.cache.get_image(&mut self.fonts, phys.cache_key) else { continue };
                let p = image.placement;
                let x = phys.x + p.left;
                let y = phys.y - p.top;
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

    fn layout(&mut self, text: &str, style: TextStyle, width: Option<f32>, align: HAlign) -> Buffer {
        let metrics = Metrics::new(style.size, (style.size * 1.25).round());
        let mut buffer = Buffer::new(&mut self.fonts, metrics);
        buffer.set_size(width, None);
        let weight = match style.weight {
            Weight::Regular => fontdb::Weight::NORMAL,
            Weight::SemiBold => fontdb::Weight::SEMIBOLD,
        };
        let attrs = Attrs::new().family(Family::Name(UI_FAMILY)).weight(weight);
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

fn extent(buffer: &Buffer) -> (f32, f32) {
    buffer.layout_runs().fold((0.0, 0.0), |(w, h), run| (w.max(run.line_w), h.max(run.line_top + run.line_height)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const STYLE: TextStyle = TextStyle { size: 40.0, weight: Weight::Regular, gray: 0 };

    #[test]
    fn both_weights_are_available() {
        let mut t = TextRenderer::new();
        let weights: Vec<u16> = t.fonts.db().faces().map(|f| f.weight.0).collect();
        assert!(weights.contains(&400) && weights.contains(&600), "{weights:?}");
        let (regular, _) = t.measure("Verso", STYLE, None);
        let (bold, _) = t.measure("Verso", TextStyle { weight: Weight::SemiBold, ..STYLE }, None);
        assert!(bold > regular, "semibold should be wider: {bold} vs {regular}");
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
        let mut gray = vec![0u8; 300 * 100];
        c.copy_gray(c.bounds(), &mut gray, 300);
        assert!(gray.iter().any(|&v| v < 64), "some ink should be drawn");
    }
}
