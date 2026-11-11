//! The in-memory drawing surface. Everything is drawn here first; the platform
//! then copies the changed parts to the screen.

use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};

use crate::geom::Rect;

/// An RGBA canvas the size of the screen. We only draw in shades of gray, but
/// tiny-skia, resvg and cosmic-text all work in RGBA, so that's what we keep.
pub struct Canvas {
    pixmap: Pixmap,
}

impl Canvas {
    /// A white canvas.
    pub fn new(width: u32, height: u32) -> Self {
        let mut pixmap = Pixmap::new(width, height).expect("canvas size must be non-zero");
        pixmap.fill(tiny_skia::Color::WHITE);
        Self { pixmap }
    }

    pub fn width(&self) -> u32 {
        self.pixmap.width()
    }

    pub fn height(&self) -> u32 {
        self.pixmap.height()
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.width() as i32, self.height() as i32)
    }

    pub fn fill_rect(&mut self, rect: Rect, gray: u8) {
        let Some(r) = rect.intersect(&self.bounds()) else { return };
        let px = gray_pixel(gray);
        let width = self.width() as usize;
        let data = self.pixmap.pixels_mut();
        for y in r.y..r.bottom() {
            let row = y as usize * width;
            data[row + r.x as usize..row + r.right() as usize].fill(px);
        }
    }

    pub fn fill_rounded_rect(&mut self, rect: Rect, radius: f32, gray: u8) {
        if let Some(path) = rounded_rect_path(rect, radius) {
            self.pixmap.fill_path(&path, &paint(gray), FillRule::Winding, Transform::identity(), None);
        }
    }

    /// Strokes the outline *inside* `rect`, so it never spills outside it.
    pub fn stroke_rounded_rect(&mut self, rect: Rect, radius: f32, width: f32, gray: u8) {
        let half = width / 2.0;
        let inner = tiny_skia::Rect::from_xywh(
            rect.x as f32 + half,
            rect.y as f32 + half,
            rect.w as f32 - width,
            rect.h as f32 - width,
        );
        if let Some(path) = inner.and_then(|r| rounded_path(r, (radius - half).max(0.0))) {
            let stroke = Stroke { width, ..Stroke::default() };
            self.pixmap.stroke_path(&path, &paint(gray), &stroke, Transform::identity(), None);
        }
    }

    /// Blends a single-channel coverage mask (0 = none, 255 = full) onto the
    /// canvas in the given gray. Used for glyphs and icons. `x`/`y` is where the
    /// mask's top-left corner goes; anything outside `clip` is skipped.
    pub fn blend_mask(&mut self, x: i32, y: i32, w: u32, h: u32, mask: &[u8], gray: u8, clip: Rect) {
        let Some(clip) = clip.intersect(&self.bounds()) else { return };
        let Some(area) = Rect::new(x, y, w as i32, h as i32).intersect(&clip) else { return };
        let width = self.width() as usize;
        let data = self.pixmap.data_mut();
        for py in area.y..area.bottom() {
            let mrow = (py - y) as usize * w as usize;
            for px in area.x..area.right() {
                let a = mask[mrow + (px - x) as usize] as u32;
                if a == 0 {
                    continue;
                }
                let i = (py as usize * width + px as usize) * 4;
                // The canvas is always opaque, so a plain lerp per channel works.
                for c in &mut data[i..i + 3] {
                    *c = ((*c as u32 * (255 - a) + gray as u32 * a + 127) / 255) as u8;
                }
            }
        }
    }

    /// Copies an 8-bit grayscale image (`stride` bytes per row) onto the
    /// canvas, with its top-left corner at the top-left of `rect`.
    pub fn blit_gray(&mut self, rect: Rect, src: &[u8], stride: usize) {
        let Some(r) = rect.intersect(&self.bounds()) else { return };
        let width = self.width() as usize;
        let data = self.pixmap.data_mut();
        for y in r.y..r.bottom() {
            let s = (y - rect.y) as usize * stride + (r.x - rect.x) as usize;
            let d = (y as usize * width + r.x as usize) * 4;
            for (px, &g) in data[d..d + r.w as usize * 4].chunks_exact_mut(4).zip(&src[s..s + r.w as usize]) {
                px.copy_from_slice(&[g, g, g, 255]);
            }
        }
    }

    /// Copies `rect` into an 8-bit grayscale buffer laid out like the screen
    /// (`stride` bytes per row). Both backends present through this.
    pub fn copy_gray(&self, rect: Rect, dst: &mut [u8], stride: usize) {
        let Some(r) = rect.intersect(&self.bounds()) else { return };
        let width = self.width() as usize;
        let src = self.pixmap.data();
        for y in r.y..r.bottom() {
            let s = (y as usize * width + r.x as usize) * 4;
            let d = y as usize * stride + r.x as usize;
            let src_row = &src[s..s + r.w as usize * 4];
            for (out, px) in dst[d..d + r.w as usize].iter_mut().zip(src_row.chunks_exact(4)) {
                *out = luma(px[0], px[1], px[2]);
            }
        }
    }
}

/// Perceptual brightness of an (opaque) RGB pixel. The weights add up to 256,
/// so pure grays map to themselves.
pub fn luma(r: u8, g: u8, b: u8) -> u8 {
    ((r as u32 * 77 + g as u32 * 150 + b as u32 * 29) >> 8) as u8
}

fn gray_pixel(gray: u8) -> tiny_skia::PremultipliedColorU8 {
    tiny_skia::PremultipliedColorU8::from_rgba(gray, gray, gray, 255).unwrap()
}

fn paint(gray: u8) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color_rgba8(gray, gray, gray, 255);
    paint.anti_alias = true;
    paint
}

fn rounded_rect_path(rect: Rect, radius: f32) -> Option<tiny_skia::Path> {
    let r = tiny_skia::Rect::from_xywh(rect.x as f32, rect.y as f32, rect.w as f32, rect.h as f32)?;
    rounded_path(r, radius)
}

fn rounded_path(r: tiny_skia::Rect, radius: f32) -> Option<tiny_skia::Path> {
    let rad = radius.min(r.width() / 2.0).min(r.height() / 2.0).max(0.0);
    if rad == 0.0 {
        return Some(PathBuilder::from_rect(r));
    }
    // Circle approximation constant for cubic Béziers.
    let k = rad * 0.552_284_8;
    let (l, t, rt, b) = (r.left(), r.top(), r.right(), r.bottom());
    let mut pb = PathBuilder::new();
    pb.move_to(l + rad, t);
    pb.line_to(rt - rad, t);
    pb.cubic_to(rt - rad + k, t, rt, t + rad - k, rt, t + rad);
    pb.line_to(rt, b - rad);
    pb.cubic_to(rt, b - rad + k, rt - rad + k, b, rt - rad, b);
    pb.line_to(l + rad, b);
    pb.cubic_to(l + rad - k, b, l, b - rad + k, l, b - rad);
    pb.line_to(l, t + rad);
    pb.cubic_to(l, t + rad - k, l + rad - k, t, l + rad, t);
    pb.close();
    pb.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grays_survive_conversion() {
        for g in [0u8, 1, 77, 128, 254, 255] {
            assert_eq!(luma(g, g, g), g);
        }
    }

    #[test]
    fn copy_gray_only_touches_rect() {
        let mut c = Canvas::new(8, 4);
        c.fill_rect(Rect::new(2, 1, 3, 2), 0);
        let mut out = vec![200u8; 8 * 4];
        c.copy_gray(Rect::new(1, 1, 5, 1), &mut out, 8);
        assert_eq!(&out[8..16], &[200, 255, 0, 0, 0, 255, 200, 200]);
        assert!(out[..8].iter().all(|&v| v == 200));
    }

    #[test]
    fn blit_gray_round_trips() {
        let mut c = Canvas::new(4, 3);
        let src = [10u8, 20, 30, 40, 50, 60];
        c.blit_gray(Rect::new(1, 1, 3, 2), &src, 3);
        let mut out = vec![0u8; 12];
        c.copy_gray(c.bounds(), &mut out, 4);
        assert_eq!(out, [255, 255, 255, 255, 255, 10, 20, 30, 255, 40, 50, 60]);
    }

    #[test]
    fn blend_mask_is_clipped() {
        let mut c = Canvas::new(4, 4);
        let mask = [255u8; 9];
        c.blend_mask(-1, -1, 3, 3, &mask, 0, Rect::new(0, 0, 1, 4));
        let mut out = vec![0u8; 16];
        c.copy_gray(Rect::new(0, 0, 4, 4), &mut out, 4);
        assert_eq!(&out[0..4], &[0, 255, 255, 255]);
        assert_eq!(&out[4..8], &[0, 255, 255, 255]);
        assert_eq!(&out[8..12], &[255, 255, 255, 255]);
    }
}
