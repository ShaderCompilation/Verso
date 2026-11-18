//! An opaque 8-bit grayscale drawing surface, shared by both backends.

use tiny_skia::{FillRule, Paint, Path, PathBuilder, PixmapMut, Stroke, Transform};

use crate::geom::Rect;

// Straight parts of rounded rectangles use short reusable RGBA bands.
// Curved caps stay intact to preserve the rasterizer's edge coverage.
const SHAPE_ROWS: usize = 64;

pub struct Canvas {
    width: u32,
    height: u32,
    gray: Vec<u8>,
    shape_rgba: Vec<u8>,
}

impl Canvas {
    /// A white canvas.
    pub fn new(width: u32, height: u32) -> Self {
        assert!(width > 0 && height > 0, "canvas size must be non-zero");
        let len = (width as usize).checked_mul(height as usize).expect("canvas size overflow");
        Self { width, height, gray: vec![255; len], shape_rgba: Vec::new() }
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.width as i32, self.height as i32)
    }

    pub fn fill(&mut self, gray: u8) {
        self.gray.fill(gray);
    }

    /// Replaces the whole canvas with a cached grayscale page.
    pub fn set_gray(&mut self, pixels: &[u8]) {
        self.gray.copy_from_slice(pixels);
    }

    pub fn gray_data(&self) -> &[u8] {
        &self.gray
    }

    /// Blend a carried library card towards paper while preserving its details.
    pub fn fade_rect(&mut self, rect: Rect) {
        let Some(r) = rect.intersect(&self.bounds()) else { return };
        for y in r.y..r.bottom() {
            let row = y as usize * self.width as usize;
            for pixel in &mut self.gray[row + r.x as usize..row + r.right() as usize] {
                *pixel = 153 + (*pixel as u16 * 102 / 255) as u8;
            }
        }
    }

    pub fn highlight_rect(&mut self, rect: Rect) {
        let Some(r) = rect.intersect(&self.bounds()) else { return };
        for y in r.y..r.bottom() {
            let row = y as usize * self.width as usize;
            for pixel in &mut self.gray[row + r.x as usize..row + r.right() as usize] {
                *pixel = (*pixel).min(210);
            }
        }
    }

    pub fn fill_rect(&mut self, rect: Rect, gray: u8) {
        let Some(r) = rect.intersect(&self.bounds()) else { return };
        for y in r.y..r.bottom() {
            let row = y as usize * self.width as usize;
            self.gray[row + r.x as usize..row + r.right() as usize].fill(gray);
        }
    }

    /// Selection is composed on the canvas after copying cached page pixels.
    pub fn invert_rect(&mut self, rect: Rect) {
        let Some(r) = rect.intersect(&self.bounds()) else { return };
        for y in r.y..r.bottom() {
            let row = y as usize * self.width as usize;
            for pixel in &mut self.gray[row + r.x as usize..row + r.right() as usize] {
                *pixel = 255 - *pixel;
            }
        }
    }

    pub fn fill_rounded_rect(&mut self, rect: Rect, radius: f32, gray: u8) {
        if let Some(path) = rounded_rect_path(rect, radius) {
            self.draw_path(&path, None, gray, Some(radius));
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
            self.draw_path(&path, Some(&Stroke { width, ..Stroke::default() }), gray, None);
        }
    }

    pub fn fill_circle(&mut self, cx: f32, cy: f32, r: f32, gray: u8) {
        if let Some(path) = PathBuilder::from_circle(cx, cy, r) {
            self.draw_path(&path, None, gray, None);
        }
    }

    /// Strokes a circle *inside* radius `r`.
    pub fn stroke_circle(&mut self, cx: f32, cy: f32, r: f32, width: f32, gray: u8) {
        if let Some(path) = PathBuilder::from_circle(cx, cy, r - width / 2.0) {
            self.draw_path(&path, Some(&Stroke { width, ..Stroke::default() }), gray, None);
        }
    }

    /// Fills the closed polygon through `points`.
    pub fn fill_polygon(&mut self, points: &[(f32, f32)], gray: u8) {
        let mut pb = PathBuilder::new();
        for (i, &(x, y)) in points.iter().enumerate() {
            if i == 0 { pb.move_to(x, y) } else { pb.line_to(x, y) }
        }
        pb.close();
        if let Some(path) = pb.finish() {
            self.draw_path(&path, None, gray, None);
        }
    }

    /// Seed shape-sized RGBA surfaces with the opaque destination before using
    /// the existing rasterizer, preserving antialiasing and blend rounding.
    fn draw_path(&mut self, path: &Path, stroke: Option<&Stroke>, gray: u8, corner_radius: Option<f32>) {
        let b = path.bounds();
        let outset = stroke.map_or(1.0, |s| s.width / 2.0 + 1.0);
        let x = (b.left() - outset).floor() as i32;
        let y = (b.top() - outset).floor() as i32;
        let bounds = Rect::new(x, y, (b.right() + outset).ceil() as i32 - x, (b.bottom() + outset).ceil() as i32 - y);
        let Some(area) = bounds.intersect(&self.bounds()) else { return };
        let band_width = area.w as usize;
        // Never split a curve across surfaces: clipping the middle of a curve
        // changes tiny-skia's subdivision and can change edge coverage. Rounded
        // fills have two curved caps and a straight middle. Circles and strokes
        // stay in a single shape-sized surface: shifting a stroked cap to a
        // separate surface can also change edge coverage through rounding.
        let (mut top_end, bottom_start) = if let Some(radius) = corner_radius {
            let radius = radius.max(0.0).min(b.width() / 2.0).min(b.height() / 2.0);
            ((b.top() + radius + outset + 2.0).ceil() as i32, (b.bottom() - radius - outset - 2.0).floor() as i32)
        } else {
            (area.bottom(), area.bottom())
        };
        if top_end >= bottom_start {
            top_end = area.bottom();
        }
        let paint = paint(gray);
        let mut top = area.y;
        while top < area.bottom() {
            let end = if top < top_end {
                top_end
            } else if top < bottom_start {
                (top + SHAPE_ROWS as i32).min(bottom_start)
            } else {
                area.bottom()
            }
            .min(area.bottom());
            let height = (end - top) as usize;
            let size = band_width * height * 4;
            if size > self.shape_rgba.capacity() {
                self.shape_rgba.reserve_exact(size - self.shape_rgba.len());
            }
            self.shape_rgba.resize(size, 0);
            let pixels = &mut self.shape_rgba[..band_width * height * 4];
            for (row, dst) in pixels.chunks_exact_mut(band_width * 4).enumerate() {
                let offset = (top as usize + row) * self.width as usize + area.x as usize;
                for (px, &g) in dst.as_chunks_mut::<4>().0.iter_mut().zip(&self.gray[offset..offset + band_width]) {
                    *px = [g, g, g, 255];
                }
            }
            let mut surface = PixmapMut::from_bytes(pixels, band_width as u32, height as u32).unwrap();
            let transform = Transform::from_translate(-area.x as f32, -top as f32);
            if let Some(stroke) = stroke {
                surface.stroke_path(path, &paint, stroke, transform, None);
            } else {
                surface.fill_path(path, &paint, FillRule::Winding, transform, None);
            }
            for (row, src) in pixels.chunks_exact(band_width * 4).enumerate() {
                let offset = (top as usize + row) * self.width as usize + area.x as usize;
                for (out, px) in self.gray[offset..offset + band_width].iter_mut().zip(src.as_chunks::<4>().0) {
                    *out = px[0];
                }
            }
            top = end;
        }
        // Large outline surfaces are temporary. Retain only a small scratch
        // allocation between shapes/frames, including during cached page turns.
        if self.shape_rgba.capacity() > self.width as usize * SHAPE_ROWS * 4 {
            self.shape_rgba = Vec::new();
        }
    }

    /// Blends a single-channel coverage mask (0 = none, 255 = full) onto the
    /// canvas in the given gray. Used for glyphs and icons.
    #[allow(clippy::too_many_arguments)]
    pub fn blend_mask(&mut self, x: i32, y: i32, w: u32, h: u32, mask: &[u8], gray: u8, clip: Rect) {
        let Some(clip) = clip.intersect(&self.bounds()) else { return };
        let Some(area) = Rect::new(x, y, w as i32, h as i32).intersect(&clip) else { return };
        for py in area.y..area.bottom() {
            let mrow = (py - y) as usize * w as usize;
            for px in area.x..area.right() {
                let a = mask[mrow + (px - x) as usize] as u32;
                if a == 0 {
                    continue;
                }
                let out = &mut self.gray[py as usize * self.width as usize + px as usize];
                *out = ((*out as u32 * (255 - a) + gray as u32 * a + 127) / 255) as u8;
            }
        }
    }

    /// Copies an 8-bit grayscale image (`stride` bytes per row) onto the
    /// canvas, with its top-left corner at the top-left of `rect`.
    pub fn blit_gray(&mut self, rect: Rect, src: &[u8], stride: usize) {
        let Some(r) = rect.intersect(&self.bounds()) else { return };
        for y in r.y..r.bottom() {
            let s = (y - rect.y) as usize * stride + (r.x - rect.x) as usize;
            let d = y as usize * self.width as usize + r.x as usize;
            self.gray[d..d + r.w as usize].copy_from_slice(&src[s..s + r.w as usize]);
        }
    }

    /// Copies `rect` into a screen-layout grayscale buffer, preserving padding.
    pub fn copy_gray(&self, rect: Rect, dst: &mut [u8], stride: usize) {
        let Some(r) = rect.intersect(&self.bounds()) else { return };
        for y in r.y..r.bottom() {
            let s = y as usize * self.width as usize + r.x as usize;
            let d = y as usize * stride + r.x as usize;
            dst[d..d + r.w as usize].copy_from_slice(&self.gray[s..s + r.w as usize]);
        }
    }

    /// Copies `rect` into a portrait panel that is this canvas turned a
    /// quarter turn (see `Orientation::Landscape`): the panel is `height`
    /// wide and `width` tall, and canvas pixel (x, y) lands on panel pixel
    /// (y, width - 1 - x). Returns the panel rectangle that changed.
    #[cfg_attr(not(feature = "kindle"), allow(dead_code))] // the simulator turns its window instead
    pub fn copy_gray_rotated(&self, rect: Rect, dst: &mut [u8], stride: usize) -> Option<Rect> {
        let r = rect.intersect(&self.bounds())?;
        let cw = self.width as usize;
        let (x0, x1, y0, y1) = (r.x as usize, r.right() as usize, r.y as usize, r.bottom() as usize);
        // Panel rows for canvas columns x1 - 1 down to x0, each reaching y1.
        assert!(stride >= self.height as usize && dst.len() >= (cw - 1 - x0) * stride + y1, "panel too small");
        // Turn a band of 64 columns (one cache line of each canvas row) at a
        // time in an ordinary buffer, in 4×4 blocks of 32-bit words, then copy
        // each finished panel row in one go. On the Kindle a full page takes
        // ~15–19 ms this way; byte-by-byte turning took ~30 ms.
        const BAND: usize = 64;
        let h = y1 - y0;
        let mut band = vec![0u8; BAND * h];
        for bx in (x0..x1).step_by(BAND) {
            let bw = (bx + BAND).min(x1) - bx;
            let whole = bw / 4 * 4;
            let mut y = y0;
            // The canvas rows y..y + 4 and band rows 0..whole are in bounds
            // for every block below: y + 4 <= y1 <= height, bx + whole <= x1
            // <= width, and band is BAND * h long.
            assert!(y1 <= self.height as usize && bx + whole <= cw && whole * h <= band.len());
            let (src, out) = (self.gray.as_ptr(), band.as_mut_ptr());
            while y + 4 <= y1 {
                let at = y * cw + bx;
                for i in (0..whole).step_by(4) {
                    // SAFETY: in bounds as asserted above; unaligned 32-bit
                    // access is allowed on every target we build for.
                    unsafe {
                        let word = |k: usize| u32::from_le(src.add(at + k * cw + i).cast::<u32>().read_unaligned());
                        let t = transpose4([word(0), word(1), word(2), word(3)]);
                        for (j, column) in t.iter().enumerate() {
                            out.add((i + j) * h + (y - y0)).cast::<u32>().write_unaligned(column.to_le());
                        }
                    }
                }
                y += 4;
            }
            // Columns past the last whole block, and rows past the last whole
            // block, one pixel at a time.
            for yy in y0..y1 {
                let first = if yy < y { whole } else { 0 };
                for i in first..bw {
                    band[i * h + (yy - y0)] = self.gray[yy * cw + bx + i];
                }
            }
            for i in 0..bw {
                let row = (cw - 1 - (bx + i)) * stride;
                dst[row + y0..row + y1].copy_from_slice(&band[i * h..(i + 1) * h]);
            }
        }
        Some(Rect::new(r.y, self.width as i32 - r.right(), r.h, r.w))
    }
}

/// Transposes a 4×4 block of bytes, one row per word (byte `j` of row `i`
/// is column `j`): swap the 2×2 quarters, then single bytes inside them.
fn transpose4([a, b, c, d]: [u32; 4]) -> [u32; 4] {
    const LOW: u32 = 0x0000_ffff;
    let (a, c) = ((a & LOW) | (c << 16), (a >> 16) | (c & !LOW));
    let (b, d) = ((b & LOW) | (d << 16), (b >> 16) | (d & !LOW));
    const EVEN: u32 = 0x00ff_00ff;
    let (a, b) = ((a & EVEN) | ((b & EVEN) << 8), ((a >> 8) & EVEN) | (b & !EVEN));
    let (c, d) = ((c & EVEN) | ((d & EVEN) << 8), ((c >> 8) & EVEN) | (d & !EVEN));
    [a, b, c, d]
}

/// Saves an 8-bit grayscale image as a PNG.
#[cfg(feature = "desktop")]
pub fn save_gray_png(path: &std::path::Path, width: u32, height: u32, gray: &[u8]) -> anyhow::Result<()> {
    image::save_buffer(path, gray, width, height, image::ColorType::L8)?;
    Ok(())
}

/// Perceptual brightness of an (opaque) RGB pixel. Grays map to themselves.
pub fn luma(r: u8, g: u8, b: u8) -> u8 {
    ((r as u32 * 77 + g as u32 * 150 + b as u32 * 29) >> 8) as u8
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
    fn saved_highlights_preserve_black_text_and_overlaps_are_idempotent() {
        let mut canvas = Canvas::new(20, 20);
        canvas.fill_rect(Rect::new(4, 4, 5, 5), 0);
        canvas.highlight_rect(Rect::new(0, 0, 12, 12));
        let first = canvas.gray_data().to_vec();
        canvas.highlight_rect(Rect::new(0, 0, 12, 12));
        assert_eq!(canvas.gray_data(), first);
        assert_eq!(canvas.gray_data()[4 * 20 + 4], 0);
        assert_eq!(canvas.gray_data()[0], 210);
        assert_eq!(canvas.gray_data()[19 * 20 + 19], 255);
    }
    #[test]
    fn bounded_shapes_match_the_full_rgba_rasterizer() {
        let (w, h) = (371, 289);
        let mut canvas = Canvas::new(w, h);
        let page: Vec<_> = (0..w * h).map(|i| (i % 256) as u8).collect();
        canvas.set_gray(&page);
        let mut rgba = tiny_skia::Pixmap::new(w, h).unwrap();
        for (px, &g) in rgba.data_mut().as_chunks_mut::<4>().0.iter_mut().zip(&page) {
            *px = [g, g, g, 255];
        }
        let paths = [
            (rounded_rect_path(Rect::new(5, 3, 360, 280), 17.3).unwrap(), Some(17.3)),
            (rounded_rect_path(Rect::new(-11, -9, 150, 180), 9.8).unwrap(), Some(9.8)),
            (rounded_rect_path(Rect::new(325, 229, 92, 103), 16.0).unwrap(), Some(16.0)),
            (rounded_rect_path(Rect::new(29, 51, 300, 56), 28.0).unwrap(), Some(28.0)),
            (rounded_rect_path(Rect::new(7, 113, 23, 150), 11.5).unwrap(), Some(11.5)),
            (PathBuilder::from_circle(187.25, 137.75, 103.6).unwrap(), None),
            (PathBuilder::from_circle(-1.5, 280.5, 28.75).unwrap(), None),
        ];
        for (i, (path, radius)) in paths.iter().enumerate() {
            for width in [None, Some(0.75), Some(1.0), Some(3.5), Some(9.0)] {
                let stroke = width.map(|width| Stroke { width, ..Stroke::default() });
                let gray = (i * 47) as u8;
                canvas.draw_path(path, stroke.as_ref(), gray, radius.filter(|_| stroke.is_none()));
                if let Some(stroke) = &stroke {
                    rgba.stroke_path(path, &paint(gray), stroke, Transform::identity(), None);
                } else {
                    rgba.fill_path(path, &paint(gray), FillRule::Winding, Transform::identity(), None);
                }
                let expected: Vec<_> = rgba.pixels().iter().map(|p| p.red()).collect();
                let mismatches: Vec<_> =
                    canvas.gray.iter().zip(&expected).enumerate().filter(|(_, (a, b))| a != b).take(5).collect();
                assert!(mismatches.is_empty(), "path {i}, stroke {width:?}: {mismatches:?}");
            }
        }
        assert_eq!(canvas.gray.len(), (w * h) as usize);
        let mut screen = Canvas::new(1272, 1696);
        screen.fill_rounded_rect(screen.bounds(), 12.0, 51);
        screen.stroke_rounded_rect(screen.bounds(), 12.0, 3.0, 0);
        assert!(screen.shape_rgba.capacity() <= 1272 * SHAPE_ROWS * 4);
        canvas.set_gray(&page);
        assert_eq!(canvas.gray_data(), page, "restoring a page removes all shapes");
    }

    #[test]
    fn grayscale_masks_preserve_coverage_rounding_and_clipping() {
        let mut canvas = Canvas::new(260, 2);
        canvas.fill(119);
        let mask: Vec<u8> = (0..=255).collect();
        canvas.blend_mask(2, 0, 256, 1, &mask, 51, canvas.bounds());
        assert_eq!(&canvas.gray[..4], &[119, 119, 119, 119]);
        assert_eq!(canvas.gray[2 + 127], 85);
        assert_eq!(&canvas.gray[257..260], &[51, 119, 119]);
        assert!(canvas.gray[260..].iter().all(|&g| g == 119));
        let mut out = vec![77; 264 * 2];
        canvas.copy_gray(Rect::new(-2, 0, 265, 2), &mut out, 264);
        assert_eq!(&out[..260], &canvas.gray[..260]);
        assert_eq!(&out[260..264], &[77; 4]);
    }

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
    fn rotated_copy_puts_the_canvas_top_on_the_panel_left() {
        // A 3x2 landscape canvas becomes a 2x3 portrait panel.
        let mut c = Canvas::new(3, 2);
        c.blit_gray(c.bounds(), &[1, 2, 3, 4, 5, 6], 3);
        let mut out = vec![0u8; 6];
        assert_eq!(c.copy_gray_rotated(c.bounds(), &mut out, 2), Some(Rect::new(0, 0, 2, 3)));
        // Panel rows, top to bottom, are the canvas columns from right to left.
        assert_eq!(out, [3, 6, 2, 5, 1, 4]);
        let mut out = vec![0u8; 6];
        assert_eq!(c.copy_gray_rotated(Rect::new(2, 1, 5, 5), &mut out, 2), Some(Rect::new(1, 0, 1, 1)));
        assert_eq!(out, [0, 6, 0, 0, 0, 0]);
    }

    #[test]
    fn rotated_copy_matches_the_pixel_rule_across_bands() {
        let (w, h) = (171usize, 67usize);
        let mut c = Canvas::new(w as u32, h as u32);
        let pixels: Vec<u8> = (0..w * h).map(|i| (i * 7 % 251) as u8).collect();
        c.blit_gray(c.bounds(), &pixels, w);
        let rect = Rect::new(5, 3, 160, 60);
        let mut out = vec![9u8; h * w];
        c.copy_gray_rotated(rect, &mut out, h);
        for y in 0..h {
            for x in 0..w {
                let inside = rect.contains(crate::geom::Point::new(x as i32, y as i32));
                let want = if inside { pixels[y * w + x] } else { 9 };
                assert_eq!(out[(w - 1 - x) * h + y], want, "canvas ({x}, {y})");
            }
        }
    }

    #[test]
    fn transpose4_swaps_rows_and_columns() {
        let rows: [u32; 4] = std::array::from_fn(|i| u32::from_le_bytes(std::array::from_fn(|j| (i * 4 + j) as u8)));
        for (j, column) in transpose4(rows).iter().enumerate() {
            assert_eq!(column.to_le_bytes(), std::array::from_fn::<u8, 4, _>(|i| (i * 4 + j) as u8));
        }
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
    fn circles_are_drawn_where_asked() {
        let mut c = Canvas::new(40, 40);
        c.fill_circle(20.0, 20.0, 10.0, 0);
        let mut out = vec![0u8; 1600];
        c.copy_gray(c.bounds(), &mut out, 40);
        assert_eq!(out[20 * 40 + 20], 0);
        assert_eq!(out[20 * 40 + 35], 255);
        c.fill(255);
        c.stroke_circle(20.0, 20.0, 10.0, 2.0, 0);
        c.copy_gray(c.bounds(), &mut out, 40);
        assert_eq!(out[20 * 40 + 20], 255, "a stroked circle is hollow");
        assert!(out[20 * 40 + 29] < 128, "the ring is just inside the radius");
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
