//! An opaque drawing surface, shared by every backend.
//!
//! A canvas is either 8-bit gray (`ColorMode::Gray8`, every monochrome
//! Kindle) or 32-bit RGBA (`ColorMode::Color32`, color panels), chosen once
//! when it is made. Each call branches on the mode once, never per pixel, so
//! the gray canvas keeps its 1 byte per pixel and its loops.

use tiny_skia::{FillRule, Paint, Path, PathBuilder, PixmapMut, Stroke, Transform};

use crate::geom::Rect;

// Straight parts of rounded rectangles use short reusable RGBA bands.
// Curved caps stay intact to preserve the rasterizer's edge coverage.
const SHAPE_ROWS: usize = 64;

/// How a canvas stores its pixels, chosen once at startup from the device.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorMode {
    /// One gray byte per pixel.
    #[default]
    Gray8,
    /// Four bytes per pixel, R, G, B and an alpha that is always 255.
    Color32,
}

/// A color to draw with. UI chrome stays gray; color is for book content,
/// covers and highlights.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const fn gray(v: u8) -> Self {
        Self { r: v, g: v, b: v }
    }
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
    /// What a gray screen shows for this color.
    pub fn to_luma(self) -> u8 {
        luma(self.r, self.g, self.b)
    }
    fn rgba(self) -> [u8; 4] {
        [self.r, self.g, self.b, 255]
    }
}

/// Saved highlights on a color screen: a soft yellow about as light as the
/// gray screen's 210, so text under it stays as legible. Applied as
/// "darken" (the smaller of each channel), like the gray screen's `min`:
/// dark ink stays dark and drawing a highlight twice changes nothing.
pub const HIGHLIGHT: Color = Color::rgb(255, 222, 110);
/// The gray screen's highlight.
const HIGHLIGHT_GRAY: u8 = 210;

enum Pixels {
    Gray(Vec<u8>),
    Rgba(Vec<u8>),
}

pub struct Canvas {
    width: u32,
    height: u32,
    pixels: Pixels,
    /// Gray canvases only: shape-sized RGBA bands for tiny-skia.
    shape_rgba: Vec<u8>,
}

impl Canvas {
    /// A white gray canvas.
    #[cfg(test)]
    pub fn new(width: u32, height: u32) -> Self {
        Self::with_mode(width, height, ColorMode::Gray8)
    }

    /// A white canvas storing its pixels as `mode` says.
    pub fn with_mode(width: u32, height: u32, mode: ColorMode) -> Self {
        assert!(width > 0 && height > 0, "canvas size must be non-zero");
        let len = (width as usize).checked_mul(height as usize).expect("canvas size overflow");
        let pixels = match mode {
            ColorMode::Gray8 => Pixels::Gray(vec![255; len]),
            ColorMode::Color32 => Pixels::Rgba(vec![255; len.checked_mul(4).expect("canvas size overflow")]),
        };
        Self { width, height, pixels, shape_rgba: Vec::new() }
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn mode(&self) -> ColorMode {
        match self.pixels {
            Pixels::Gray(_) => ColorMode::Gray8,
            Pixels::Rgba(_) => ColorMode::Color32,
        }
    }

    pub fn bytes_per_pixel(&self) -> usize {
        match self.pixels {
            Pixels::Gray(_) => 1,
            Pixels::Rgba(_) => 4,
        }
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.width as i32, self.height as i32)
    }

    /// The pixels as stored: gray bytes, or RGBA.
    pub fn raw(&self) -> &[u8] {
        match &self.pixels {
            Pixels::Gray(p) | Pixels::Rgba(p) => p,
        }
    }

    /// The rows of `r` (inside the canvas) as byte slices, and the bytes
    /// per pixel.
    fn rows_mut(&mut self, r: Rect) -> (usize, impl Iterator<Item = &mut [u8]>) {
        let bpp = self.bytes_per_pixel();
        let stride = self.width as usize * bpp;
        let (x0, x1) = (r.x as usize * bpp, r.right() as usize * bpp);
        let buf = match &mut self.pixels {
            Pixels::Gray(p) | Pixels::Rgba(p) => p,
        };
        (bpp, buf.chunks_exact_mut(stride).skip(r.y as usize).take(r.h as usize).map(move |row| &mut row[x0..x1]))
    }

    pub fn fill(&mut self, gray: u8) {
        match &mut self.pixels {
            Pixels::Gray(p) => p.fill(gray),
            Pixels::Rgba(p) => p.as_chunks_mut::<4>().0.fill([gray, gray, gray, 255]),
        }
    }

    /// Replaces the whole canvas with a cached grayscale page.
    pub fn set_gray(&mut self, pixels: &[u8]) {
        match &mut self.pixels {
            Pixels::Gray(p) => p.copy_from_slice(pixels),
            Pixels::Rgba(p) => {
                assert_eq!(pixels.len() * 4, p.len(), "page size");
                for (px, &g) in p.as_chunks_mut::<4>().0.iter_mut().zip(pixels) {
                    *px = [g, g, g, 255];
                }
            }
        }
    }

    /// Replaces the whole canvas with an RGBA image (alpha ignored: pages
    /// are opaque). A gray canvas keeps each pixel's brightness.
    pub fn set_rgba(&mut self, pixels: &[u8]) {
        match &mut self.pixels {
            Pixels::Gray(p) => {
                assert_eq!(p.len() * 4, pixels.len(), "page size");
                for (out, px) in p.iter_mut().zip(pixels.as_chunks::<4>().0) {
                    *out = luma(px[0], px[1], px[2]);
                }
            }
            Pixels::Rgba(p) => {
                p.copy_from_slice(pixels);
                for px in p.as_chunks_mut::<4>().0 {
                    px[3] = 255;
                }
            }
        }
    }

    /// The gray pixels of a `Gray8` canvas.
    #[cfg(test)]
    pub fn gray_data(&self) -> &[u8] {
        match &self.pixels {
            Pixels::Gray(p) => p,
            Pixels::Rgba(_) => panic!("gray_data on a color canvas"),
        }
    }

    /// Blend a carried library card towards paper while preserving its details.
    pub fn fade_rect(&mut self, rect: Rect) {
        let Some(r) = rect.intersect(&self.bounds()) else { return };
        let fade = |v: u8| 153 + (v as u16 * 102 / 255) as u8;
        let (bpp, rows) = self.rows_mut(r);
        for row in rows {
            if bpp == 1 {
                for pixel in row {
                    *pixel = fade(*pixel);
                }
            } else {
                for px in row.as_chunks_mut::<4>().0 {
                    *px = [fade(px[0]), fade(px[1]), fade(px[2]), 255];
                }
            }
        }
    }

    /// A saved highlight: light gray on a gray canvas, `HIGHLIGHT` yellow on
    /// a color one.
    pub fn highlight_rect(&mut self, rect: Rect) {
        let Some(r) = rect.intersect(&self.bounds()) else { return };
        let (bpp, rows) = self.rows_mut(r);
        for row in rows {
            if bpp == 1 {
                for pixel in row {
                    *pixel = (*pixel).min(HIGHLIGHT_GRAY);
                }
            } else {
                let [hr, hg, hb, _] = HIGHLIGHT.rgba();
                for px in row.as_chunks_mut::<4>().0 {
                    *px = [px[0].min(hr), px[1].min(hg), px[2].min(hb), 255];
                }
            }
        }
    }

    pub fn fill_rect(&mut self, rect: Rect, gray: u8) {
        self.fill_rect_color(rect, Color::gray(gray));
    }

    /// Fills `rect` with `color`; a gray canvas gets its brightness.
    pub fn fill_rect_color(&mut self, rect: Rect, color: Color) {
        let Some(r) = rect.intersect(&self.bounds()) else { return };
        let (bpp, rows) = self.rows_mut(r);
        for row in rows {
            if bpp == 1 {
                row.fill(color.to_luma());
            } else {
                row.as_chunks_mut::<4>().0.fill(color.rgba());
            }
        }
    }

    /// Selection is composed on the canvas after copying cached page pixels.
    pub fn invert_rect(&mut self, rect: Rect) {
        let Some(r) = rect.intersect(&self.bounds()) else { return };
        let (bpp, rows) = self.rows_mut(r);
        for row in rows {
            if bpp == 1 {
                for pixel in row {
                    *pixel = 255 - *pixel;
                }
            } else {
                for px in row.as_chunks_mut::<4>().0 {
                    *px = [255 - px[0], 255 - px[1], 255 - px[2], 255];
                }
            }
        }
    }

    pub fn fill_rounded_rect(&mut self, rect: Rect, radius: f32, gray: u8) {
        if rect.w <= 0 || rect.h <= 0 {
            return;
        }
        let rad = radius.min(rect.w as f32 / 2.0).min(rect.h as f32 / 2.0).max(0.0);
        if rad == 0.0 {
            self.fill_rect(rect, gray);
            return;
        }
        let Some(path) = rounded_rect_path(rect, radius) else { return };
        if !matches!(self.pixels, Pixels::Gray(_)) {
            self.draw_path(&path, None, gray, Some(radius));
            return;
        }
        // Integer-coordinate rounded fills have exact pixel-aligned straight
        // sides between the two corner bands. Fill the straight middle directly
        // with `fill_rect` and only rasterize the top and bottom corner bands
        // through tiny-skia (with a 1 px outset + 2 px margin matching draw_path).
        let bounds = Rect::new(rect.x - 1, rect.y - 1, rect.w + 2, rect.h + 2);
        let Some(area) = bounds.intersect(&self.bounds()) else { return };
        let top_end = ((rect.y as f32 + rad + 3.0).ceil() as i32).clamp(area.y, area.bottom());
        let bottom_start = ((rect.bottom() as f32 - rad - 3.0).floor() as i32).clamp(area.y, area.bottom());
        if top_end >= bottom_start {
            self.draw_path(&path, None, gray, Some(radius));
            return;
        }
        self.fill_rect(Rect::new(rect.x, top_end, rect.w, bottom_start - top_end), gray);
        let paint = paint(gray);
        self.rasterize_path(Rect::new(area.x, area.y, area.w, top_end - area.y), &path, None, &paint);
        self.rasterize_path(Rect::new(area.x, bottom_start, area.w, area.bottom() - bottom_start), &path, None, &paint);
        self.trim_shape_rgba();
    }

    /// Strokes the outline *inside* `rect`, so it never spills outside it.
    pub fn stroke_rounded_rect(&mut self, rect: Rect, radius: f32, width: f32, gray: u8) {
        let half = width / 2.0;
        let Some(inner) = tiny_skia::Rect::from_xywh(
            rect.x as f32 + half,
            rect.y as f32 + half,
            rect.w as f32 - width,
            rect.h as f32 - width,
        ) else {
            return;
        };
        let rad = (radius - half).max(0.0).min(inner.width() / 2.0).min(inner.height() / 2.0);
        let integer_width = width >= 1.0 && width.fract() == 0.0 && width <= i32::MAX as f32;
        // A 1 px stroke goes through tiny-skia's hairline rasterizer, whose
        // corner pixels are only partly covered, so only thicker square
        // outlines are plain strips.
        if integer_width && width > 1.0 && rad == 0.0 {
            let w = width as i32;
            if rect.w > w && rect.h > w {
                let mid_h = (rect.h - 2 * w).max(0);
                self.fill_rect(Rect::new(rect.x, rect.y, rect.w, w), gray);
                self.fill_rect(Rect::new(rect.x, rect.bottom() - w, rect.w, w), gray);
                if mid_h > 0 {
                    self.fill_rect(Rect::new(rect.x, rect.y + w, w, mid_h), gray);
                    self.fill_rect(Rect::new(rect.right() - w, rect.y + w, w, mid_h), gray);
                }
                return;
            }
        }
        let Some(path) = rounded_path(inner, rad) else { return };
        let stroke = Stroke { width, ..Stroke::default() };
        if !integer_width || rad == 0.0 {
            self.draw_path(&path, Some(&stroke), gray, None);
            return;
        }
        // Integer-width rounded outlines have exact pixel-aligned straight
        // sides between the two corner bands. Stroke the path once, fill the
        // straight vertical sides directly, and only rasterize the top and
        // bottom corner bands through tiny-skia (with a 1 px margin matching
        // draw_path so conservative bounds never clip an arc).
        let bounds = Rect::new(rect.x - 1, rect.y - 1, rect.w + 2, rect.h + 2);
        let Some(area) = bounds.intersect(&self.bounds()) else { return };
        let w = width as i32;
        let top_end = ((inner.top() + rad + half + 2.0).ceil() as i32).clamp(area.y, area.bottom());
        let bottom_start = ((inner.bottom() - rad - half - 2.0).floor() as i32).clamp(area.y, area.bottom());
        if top_end >= bottom_start {
            self.draw_path(&path, Some(&stroke), gray, None);
            return;
        }
        let paint = paint(gray);
        let mid_h = bottom_start - top_end;
        self.fill_rect(Rect::new(rect.x, top_end, w, mid_h), gray);
        self.fill_rect(Rect::new(rect.right() - w, top_end, w, mid_h), gray);
        // tiny-skia routes width <= 1.0 through its hairline rasterizer, so
        // only pre-stroke thick paths (width > 1.0) to match pixel-for-pixel.
        let stroked = (width > 1.0).then(|| path.stroke(&stroke, 1.0)).flatten();
        let (band_path, band_stroke) = match &stroked {
            Some(stroked) => (stroked, None),
            None => (&path, Some(&stroke)),
        };
        self.rasterize_path(Rect::new(area.x, area.y, area.w, top_end - area.y), band_path, band_stroke, &paint);
        self.rasterize_path(
            Rect::new(area.x, bottom_start, area.w, area.bottom() - bottom_start),
            band_path,
            band_stroke,
            &paint,
        );
        self.trim_shape_rgba();
    }

    /// Strokes a dashed outline *inside* `rect` (`dash` px on, `dash` px off),
    /// for controls waiting on something (a Wi-Fi tile turning on).
    pub fn stroke_dashed_rounded_rect(&mut self, rect: Rect, radius: f32, width: f32, dash: f32, gray: u8) {
        let half = width / 2.0;
        let Some(inner) = tiny_skia::Rect::from_xywh(
            rect.x as f32 + half,
            rect.y as f32 + half,
            rect.w as f32 - width,
            rect.h as f32 - width,
        ) else {
            return;
        };
        let rad = (radius - half).max(0.0).min(inner.width() / 2.0).min(inner.height() / 2.0);
        let Some(path) = rounded_path(inner, rad) else { return };
        let Some(dash) = tiny_skia::StrokeDash::new(vec![dash.max(1.0), dash.max(1.0)], 0.0) else { return };
        let stroke = Stroke { width, dash: Some(dash), ..Stroke::default() };
        self.draw_path(&path, Some(&stroke), gray, None);
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
        if let Pixels::Rgba(_) = self.pixels {
            // Nothing to copy: tiny-skia draws straight into the rows.
            self.rasterize_path(area, path, stroke, &paint);
            return;
        }
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
            self.rasterize_path(Rect::new(area.x, top, area.w, end - top), path, stroke, &paint);
            top = end;
        }
        self.trim_shape_rgba();
    }

    fn rasterize_path(&mut self, area: Rect, path: &Path, stroke: Option<&Stroke>, paint: &Paint<'_>) {
        if area.w <= 0 || area.h <= 0 {
            return;
        }
        let Pixels::Gray(gray) = &mut self.pixels else {
            // A color canvas is tiny-skia's own (opaque, so premultiplied)
            // format: draw into its full-width rows of `area` directly.
            let Pixels::Rgba(rgba) = &mut self.pixels else { unreachable!() };
            let stride = self.width as usize * 4;
            let rows = &mut rgba[area.y as usize * stride..area.bottom() as usize * stride];
            let mut surface = PixmapMut::from_bytes(rows, self.width, area.h as u32).unwrap();
            let transform = Transform::from_translate(0.0, -area.y as f32);
            if let Some(stroke) = stroke {
                surface.stroke_path(path, paint, stroke, transform, None);
            } else {
                surface.fill_path(path, paint, FillRule::Winding, transform, None);
            }
            return;
        };
        let band_width = area.w as usize;
        let height = area.h as usize;
        let size = band_width * height * 4;
        if size > self.shape_rgba.capacity() {
            self.shape_rgba.reserve_exact(size - self.shape_rgba.len());
        }
        self.shape_rgba.resize(size, 0);
        let pixels = &mut self.shape_rgba[..size];
        for (row, dst) in pixels.chunks_exact_mut(band_width * 4).enumerate() {
            let offset = (area.y as usize + row) * self.width as usize + area.x as usize;
            for (px, &g) in dst.as_chunks_mut::<4>().0.iter_mut().zip(&gray[offset..offset + band_width]) {
                *px = [g, g, g, 255];
            }
        }
        let mut surface = PixmapMut::from_bytes(pixels, band_width as u32, height as u32).unwrap();
        let transform = Transform::from_translate(-area.x as f32, -area.y as f32);
        if let Some(stroke) = stroke {
            surface.stroke_path(path, paint, stroke, transform, None);
        } else {
            surface.fill_path(path, paint, FillRule::Winding, transform, None);
        }
        for (row, src) in pixels.chunks_exact(band_width * 4).enumerate() {
            let offset = (area.y as usize + row) * self.width as usize + area.x as usize;
            for (out, px) in gray[offset..offset + band_width].iter_mut().zip(src.as_chunks::<4>().0) {
                *out = px[0];
            }
        }
    }

    /// Large outline surfaces are temporary. Retain only a small scratch
    /// allocation between shapes/frames, including during cached page turns.
    fn trim_shape_rgba(&mut self) {
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
        let blend = |v: &mut u8, a: u32| *v = ((*v as u32 * (255 - a) + gray as u32 * a + 127) / 255) as u8;
        let width = self.width as usize;
        let mask_rows = (area.y..area.bottom()).map(|py| {
            let mrow = (py - y) as usize * w as usize + (area.x - x) as usize;
            (py as usize * width + area.x as usize, &mask[mrow..mrow + area.w as usize])
        });
        match &mut self.pixels {
            Pixels::Gray(p) => {
                for (at, mask) in mask_rows {
                    for (out, &a) in p[at..at + mask.len()].iter_mut().zip(mask) {
                        if a != 0 {
                            blend(out, a as u32);
                        }
                    }
                }
            }
            Pixels::Rgba(p) => {
                for (at, mask) in mask_rows {
                    for (px, &a) in p[at * 4..(at + mask.len()) * 4].as_chunks_mut::<4>().0.iter_mut().zip(mask) {
                        if a != 0 {
                            px[..3].iter_mut().for_each(|v| blend(v, a as u32));
                        }
                    }
                }
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
            let src = &src[s..s + r.w as usize];
            match &mut self.pixels {
                Pixels::Gray(p) => p[d..d + r.w as usize].copy_from_slice(src),
                Pixels::Rgba(p) => {
                    for (px, &g) in p[d * 4..(d + r.w as usize) * 4].as_chunks_mut::<4>().0.iter_mut().zip(src) {
                        *px = [g, g, g, 255];
                    }
                }
            }
        }
    }

    /// Copies a 24-bit RGB image (`stride` pixels per row) onto the canvas,
    /// with its top-left corner at the top-left of `rect`. A gray canvas
    /// keeps each pixel's brightness.
    pub fn blit_rgb(&mut self, rect: Rect, src: &[u8], stride: usize) {
        let Some(r) = rect.intersect(&self.bounds()) else { return };
        for y in r.y..r.bottom() {
            let s = (y - rect.y) as usize * stride + (r.x - rect.x) as usize;
            let src = &src[s * 3..(s + r.w as usize) * 3];
            let d = y as usize * self.width as usize + r.x as usize;
            match &mut self.pixels {
                Pixels::Gray(p) => {
                    for (out, px) in p[d..d + r.w as usize].iter_mut().zip(src.as_chunks::<3>().0) {
                        *out = luma(px[0], px[1], px[2]);
                    }
                }
                Pixels::Rgba(p) => {
                    for (out, px) in
                        p[d * 4..(d + r.w as usize) * 4].as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<3>().0)
                    {
                        *out = [px[0], px[1], px[2], 255];
                    }
                }
            }
        }
    }

    /// Copies an RGBA image (`stride` pixels per row, alpha ignored) onto
    /// the canvas, with its top-left corner at the top-left of `rect`. A
    /// gray canvas keeps each pixel's brightness.
    #[cfg_attr(not(test), allow(dead_code))] // color covers and illustrations come with the color content
    pub fn blit_rgba(&mut self, rect: Rect, src: &[u8], stride: usize) {
        let Some(r) = rect.intersect(&self.bounds()) else { return };
        for y in r.y..r.bottom() {
            let s = (y - rect.y) as usize * stride + (r.x - rect.x) as usize;
            let src = &src[s * 4..(s + r.w as usize) * 4];
            let d = y as usize * self.width as usize + r.x as usize;
            match &mut self.pixels {
                Pixels::Gray(p) => {
                    for (out, px) in p[d..d + r.w as usize].iter_mut().zip(src.as_chunks::<4>().0) {
                        *out = luma(px[0], px[1], px[2]);
                    }
                }
                Pixels::Rgba(p) => {
                    let dst = &mut p[d * 4..(d + r.w as usize) * 4];
                    dst.copy_from_slice(src);
                    for px in dst.as_chunks_mut::<4>().0 {
                        px[3] = 255;
                    }
                }
            }
        }
    }

    /// Copies `rect` into a screen-layout grayscale buffer, preserving padding.
    pub fn copy_gray(&self, rect: Rect, dst: &mut [u8], stride: usize) {
        let Some(r) = rect.intersect(&self.bounds()) else { return };
        for y in r.y..r.bottom() {
            let s = y as usize * self.width as usize + r.x as usize;
            let d = y as usize * stride + r.x as usize;
            let dst = &mut dst[d..d + r.w as usize];
            match &self.pixels {
                Pixels::Gray(p) => dst.copy_from_slice(&p[s..s + r.w as usize]),
                Pixels::Rgba(p) => {
                    for (out, px) in dst.iter_mut().zip(p[s * 4..(s + r.w as usize) * 4].as_chunks::<4>().0) {
                        *out = luma(px[0], px[1], px[2]);
                    }
                }
            }
        }
    }

    /// Copies `rect` into a screen-layout RGBA buffer (`stride` pixels per
    /// row), preserving padding. Gray pixels become `[g, g, g, 255]`.
    pub fn copy_rgba(&self, rect: Rect, dst: &mut [u8], stride: usize) {
        let Some(r) = rect.intersect(&self.bounds()) else { return };
        for y in r.y..r.bottom() {
            let s = y as usize * self.width as usize + r.x as usize;
            let d = y as usize * stride + r.x as usize;
            let dst = &mut dst[d * 4..(d + r.w as usize) * 4];
            match &self.pixels {
                Pixels::Gray(p) => {
                    for (px, &g) in dst.as_chunks_mut::<4>().0.iter_mut().zip(&p[s..s + r.w as usize]) {
                        *px = [g, g, g, 255];
                    }
                }
                Pixels::Rgba(p) => dst.copy_from_slice(&p[s * 4..(s + r.w as usize) * 4]),
            }
        }
    }

    /// Copies `rect` as stored (gray or RGBA) into a buffer laid out like
    /// this canvas, for keeping a copy of a frame.
    pub fn copy_raw(&self, rect: Rect, dst: &mut [u8]) {
        let Some(r) = rect.intersect(&self.bounds()) else { return };
        let bpp = self.bytes_per_pixel();
        let stride = self.width as usize * bpp;
        let src = self.raw();
        for y in r.y as usize..r.bottom() as usize {
            let span = y * stride + r.x as usize * bpp..y * stride + r.right() as usize * bpp;
            dst[span.clone()].copy_from_slice(&src[span]);
        }
    }

    /// Copies `rect` into a portrait panel that is this canvas turned a
    /// quarter turn (see `Orientation::Landscape`): the panel is `height`
    /// wide and `width` tall, and canvas pixel (x, y) lands on panel pixel
    /// (y, width - 1 - x). Returns the panel rectangle that changed.
    #[cfg_attr(not(feature = "kindle"), allow(dead_code))] // the simulator turns its window instead
    pub fn copy_gray_rotated(&self, rect: Rect, dst: &mut [u8], stride: usize) -> Option<Rect> {
        let r = rect.intersect(&self.bounds())?;
        let gray = match &self.pixels {
            Pixels::Gray(gray) => gray,
            Pixels::Rgba(rgba) => {
                let cw = self.width as usize;
                for y in r.y as usize..r.bottom() as usize {
                    for x in r.x as usize..r.right() as usize {
                        let p = &rgba[(y * cw + x) * 4..];
                        dst[(cw - 1 - x) * stride + y] = luma(p[0], p[1], p[2]);
                    }
                }
                return Some(Rect::new(r.y, self.width as i32 - r.right(), r.h, r.w));
            }
        };
        let cw = self.width as usize;
        let (x0, x1, y0, y1) = (r.x as usize, r.right() as usize, r.y as usize, r.bottom() as usize);
        // Panel rows for canvas columns x1 - 1 down to x0, each reaching y1.
        assert!(stride >= self.height as usize && dst.len() >= (cw - 1 - x0) * stride + y1, "panel too small");
        // Turn a band of 64 columns (one cache line of each canvas row) at a
        // time in an ordinary buffer, in 4×4 blocks of 32-bit words, then copy
        // each finished panel row in one go. Walking down a band crosses a
        // 4 KiB page every two or three rows, which the hardware prefetcher
        // doesn't follow, so the rows a few blocks ahead are requested
        // explicitly: on the Kindle a full page takes ~5 ms this way and
        // ~13 ms without (the arithmetic is not the cost: NEON 8×16 blocks
        // measured no faster). Byte-by-byte turning took ~30 ms.
        const BAND: usize = 64;
        const AHEAD: usize = 8;
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
            let (src, out) = (gray.as_ptr(), band.as_mut_ptr());
            while y + 4 <= y1 {
                for k in y + AHEAD..y + AHEAD + 4 {
                    // A row's 64 bytes may straddle two cache lines.
                    let row = src.wrapping_add(k * cw + bx);
                    prefetch(row);
                    prefetch(row.wrapping_add(BAND - 1));
                }
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
                    band[i * h + (yy - y0)] = gray[yy * cw + bx + i];
                }
            }
            for i in 0..bw {
                let row = (cw - 1 - (bx + i)) * stride;
                dst[row + y0..row + y1].copy_from_slice(&band[i * h..(i + 1) * h]);
            }
        }
        Some(Rect::new(r.y, self.width as i32 - r.right(), r.h, r.w))
    }

    /// `copy_gray_rotated` for an RGBA panel (`stride` pixels per row).
    #[cfg_attr(not(test), allow(dead_code))] // the Kindle's true-color path
    pub fn copy_rgba_rotated(&self, rect: Rect, dst: &mut [u8], stride: usize) -> Option<Rect> {
        let r = rect.intersect(&self.bounds())?;
        let cw = self.width as usize;
        for x in r.x as usize..r.right() as usize {
            let row = (cw - 1 - x) * stride;
            for y in r.y as usize..r.bottom() as usize {
                let px = match &self.pixels {
                    Pixels::Gray(p) => {
                        let g = p[y * cw + x];
                        [g, g, g, 255]
                    }
                    Pixels::Rgba(p) => p[(y * cw + x) * 4..(y * cw + x) * 4 + 4].try_into().unwrap(),
                };
                dst[(row + y) * 4..(row + y) * 4 + 4].copy_from_slice(&px);
            }
        }
        Some(Rect::new(r.y, self.width as i32 - r.right(), r.h, r.w))
    }
}

/// Asks the CPU to start loading the cache line holding `p`. Only a hint:
/// any address is fine, including one past the end of a buffer.
#[inline(always)]
fn prefetch(p: *const u8) {
    // SAFETY: PLD reads nothing architecturally and never faults, whatever
    // the address.
    #[cfg(target_arch = "arm")]
    unsafe {
        std::arch::asm!("pld [{p}]", p = in(reg) p, options(nostack, preserves_flags, readonly))
    };
    #[cfg(not(target_arch = "arm"))]
    let _ = p;
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

/// Saves an RGBA image (alpha ignored) as an RGB PNG.
#[cfg(feature = "desktop")]
pub fn save_rgba_png(path: &std::path::Path, width: u32, height: u32, rgba: &[u8]) -> anyhow::Result<()> {
    let rgb: Vec<u8> = rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
    image::save_buffer(path, &rgb, width, height, image::ColorType::Rgb8)?;
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
    fn dashed_outline_alternates_along_edges_and_stays_inside() {
        let mut canvas = Canvas::new(120, 60);
        canvas.fill(255);
        canvas.stroke_dashed_rounded_rect(Rect::new(10, 10, 100, 40), 6.0, 4.0, 8.0, 0x55);
        let px = |x: u32, y: u32| canvas.gray_data()[(y * 120 + x) as usize];
        // Along the straight top edge (row 11, inside the 4 px stroke) there
        // are both inked dashes and paper gaps.
        let top: Vec<u8> = (20..100).map(|x| px(x, 11)).collect();
        assert!(top.contains(&0x55) && top.contains(&255), "{top:?}");
        // Nothing outside the rect, and the inside stays clear.
        for y in 0..60 {
            for x in 0..120 {
                if !Rect::new(10, 10, 100, 40).contains(crate::geom::Point::new(x as i32, y as i32)) {
                    assert_eq!(px(x, y), 255, "({x}, {y})");
                }
            }
        }
        assert_eq!(px(60, 30), 255);
    }

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
                    canvas.gray_data().iter().zip(&expected).enumerate().filter(|(_, (a, b))| a != b).take(5).collect();
                assert!(mismatches.is_empty(), "path {i}, stroke {width:?}: {mismatches:?}");
            }
        }
        assert_eq!(canvas.gray_data().len(), (w * h) as usize);
        let mut screen = Canvas::new(1272, 1696);
        screen.fill_rounded_rect(screen.bounds(), 12.0, 51);
        screen.stroke_rounded_rect(screen.bounds(), 12.0, 3.0, 0);
        assert!(screen.shape_rgba.capacity() <= 1272 * SHAPE_ROWS * 4);
        canvas.set_gray(&page);
        assert_eq!(canvas.gray_data(), page, "restoring a page removes all shapes");
    }

    #[test]
    fn stroke_rounded_rect_matches_full_tiny_skia_pixel_for_pixel() {
        let cases: &[(u32, u32, Rect, f32, f32, u8)] = &[
            // Square corners (Stack covers, Latest frame)
            (400, 500, Rect::new(20, 30, 210, 315), 0.0, 1.0, 0),
            (400, 500, Rect::new(20, 30, 210, 315), 0.0, 3.0, 0),
            (400, 500, Rect::new(20, 30, 210, 315), 0.0, 4.0, 51),
            (400, 500, Rect::new(20, 30, 210, 315), 1.5, 4.0, 0), // radius <= width/2 -> square inner path
            (400, 500, Rect::new(-15, -10, 120, 140), 0.0, 3.0, 17), // clipped top-left
            (400, 500, Rect::new(340, 440, 120, 140), 0.0, 4.0, 85), // clipped bottom-right
            (400, 500, Rect::new(10, 10, 5, 20), 0.0, 3.0, 0),    // 2*width > rect.w (solid fill)
            (400, 500, Rect::new(10, 10, 20, 5), 0.0, 3.0, 0),    // 2*width > rect.h (solid fill)
            (400, 500, Rect::new(10, 10, 3, 20), 0.0, 3.0, 0),    // rect.w == width (empty inner)
            // Rounded corners (All books covers, buttons, sheets, menus)
            (400, 500, Rect::new(25, 40, 257, 365), 18.0, 4.0, 0),
            (400, 500, Rect::new(25, 40, 257, 365), 18.0, 8.0, 34),
            (400, 500, Rect::new(13, 17, 350, 106), 18.0, 4.0, 0), // wide button
            (400, 500, Rect::new(13, 17, 350, 53), 26.5, 4.0, 0),  // wide pill (badge/toggle)
            (400, 500, Rect::new(13, 17, 53, 350), 26.5, 4.0, 0),  // tall pill
            (400, 500, Rect::new(13, 17, 40, 40), 20.0, 4.0, 0),   // circle-sized rounded square
            (400, 500, Rect::new(5, 3, 360, 280), 17.3, 3.0, 119), // fractional radius
            (400, 500, Rect::new(5, 3, 360, 280), 35.0, 1.0, 0),   // sheet radius, 1px stroke
            (400, 500, Rect::new(5, 3, 360, 280), 35.0, 9.0, 68),  // thick stroke
            (400, 500, Rect::new(-11, -9, 150, 180), 18.0, 4.0, 0), // clipped top-left
            (400, 500, Rect::new(325, 410, 150, 180), 18.0, 4.0, 0), // clipped bottom-right
            (400, 500, Rect::new(10, 10, 200, 30), 18.0, 12.0, 0), // thick stroke on thin rect
            (400, 500, Rect::new(10, 10, 120, 120), 18.0, 0.75, 0), // fractional hairline width
            (400, 500, Rect::new(10, 10, 120, 120), 18.0, 3.5, 0), // fractional width
            (400, 500, Rect::new(500, 500, 100, 100), 18.0, 4.0, 0), // completely off-screen
            // Full Kindle screen coordinates (tests large y/x translations)
            (1272, 1696, Rect::new(59, 609, 257, 365), 18.0, 4.0, 0),
            (1272, 1696, Rect::new(59, 1010, 257, 365), 18.0, 8.0, 0),
            (1272, 1696, Rect::new(59, 200, 1154, 1300), 35.0, 4.0, 0),
        ];
        for (i, &(w, h, rect, radius, width, gray)) in cases.iter().enumerate() {
            let page: Vec<u8> = (0..w as usize * h as usize).map(|k| ((k * 37 + i * 13) % 256) as u8).collect();
            let mut canvas = Canvas::new(w, h);
            canvas.set_gray(&page);
            canvas.stroke_rounded_rect(rect, radius, width, gray);

            // Reference: today's full-pixmap tiny-skia stroke_path.
            let mut rgba = tiny_skia::Pixmap::new(w, h).unwrap();
            for (px, &g) in rgba.data_mut().as_chunks_mut::<4>().0.iter_mut().zip(&page) {
                *px = [g, g, g, 255];
            }
            let half = width / 2.0;
            let inner = tiny_skia::Rect::from_xywh(
                rect.x as f32 + half,
                rect.y as f32 + half,
                rect.w as f32 - width,
                rect.h as f32 - width,
            );
            if let Some(path) = inner.and_then(|r| rounded_path(r, (radius - half).max(0.0))) {
                rgba.stroke_path(
                    &path,
                    &paint(gray),
                    &Stroke { width, ..Stroke::default() },
                    Transform::identity(),
                    None,
                );
            }
            let expected: Vec<u8> = rgba.pixels().iter().map(|p| p.red()).collect();
            let mismatches: Vec<_> = canvas
                .gray_data()
                .iter()
                .zip(&expected)
                .enumerate()
                .filter(|(_, (a, b))| a != b)
                .take(5)
                .map(|(idx, (a, b))| (idx % w as usize, idx / w as usize, *a, *b))
                .collect();
            assert!(mismatches.is_empty(), "case {i} ({rect:?}, r={radius}, w={width}): {mismatches:?}");
        }
    }

    #[test]
    fn fill_rounded_rect_matches_full_tiny_skia_pixel_for_pixel() {
        let cases: &[(u32, u32, Rect, f32, u8)] = &[
            // Square corners
            (400, 500, Rect::new(20, 30, 210, 315), 0.0, 0),
            (400, 500, Rect::new(-15, -10, 120, 140), 0.0, 51),
            (400, 500, Rect::new(340, 440, 120, 140), 0.0, 255),
            // Odd widths/heights and fractional radii
            (371, 289, Rect::new(5, 3, 359, 279), 17.3, 119),
            (371, 289, Rect::new(7, 11, 101, 213), 12.5, 0),
            (371, 289, Rect::new(13, 17, 351, 53), 26.5, 34), // wide pill
            (371, 289, Rect::new(13, 17, 53, 251), 26.5, 204), // tall pill
            (371, 289, Rect::new(13, 17, 41, 41), 25.0, 68),  // clamped radius
            (371, 289, Rect::new(10, 10, 1, 1), 5.0, 0),      // 1x1
            (371, 289, Rect::new(10, 10, 0, 50), 10.0, 0),    // empty
            // Clipped sheets / TopDrawer / ReaderMenu top bar (corners off-screen)
            (400, 500, Rect::new(-4, -70, 408, 260), 35.0, 255), // top drawer / reader top panel
            (400, 500, Rect::new(-70, 220, 540, 350), 35.0, 255), // bottom sheet
            (400, 500, Rect::new(-11, -9, 150, 180), 18.0, 17),  // clipped top-left
            (400, 500, Rect::new(325, 410, 150, 180), 18.0, 85), // clipped bottom-right
            (400, 500, Rect::new(500, 500, 100, 100), 18.0, 0),  // completely off-screen
            // Full Kindle screen coordinates (ReaderMenu top bar, sheets, lookup card)
            (1272, 1696, Rect::new(-4, -70, 1280, 245), 35.0, 255),
            (1272, 1696, Rect::new(-70, 896, 1412, 870), 35.0, 255),
            (1272, 1696, Rect::new(59, 200, 1154, 1300), 35.0, 240),
        ];
        for (i, &(w, h, rect, radius, gray)) in cases.iter().enumerate() {
            let page: Vec<u8> = (0..w as usize * h as usize).map(|k| ((k * 37 + i * 13) % 256) as u8).collect();
            let mut canvas = Canvas::new(w, h);
            canvas.set_gray(&page);
            canvas.fill_rounded_rect(rect, radius, gray);

            let mut rgba = tiny_skia::Pixmap::new(w, h).unwrap();
            for (px, &g) in rgba.data_mut().as_chunks_mut::<4>().0.iter_mut().zip(&page) {
                *px = [g, g, g, 255];
            }
            if let Some(path) = rounded_rect_path(rect, radius) {
                rgba.fill_path(&path, &paint(gray), FillRule::Winding, Transform::identity(), None);
            }
            let expected: Vec<u8> = rgba.pixels().iter().map(|p| p.red()).collect();
            let mismatches: Vec<_> = canvas
                .gray_data()
                .iter()
                .zip(&expected)
                .enumerate()
                .filter(|(_, (a, b))| a != b)
                .take(5)
                .map(|(idx, (a, b))| (idx % w as usize, idx / w as usize, *a, *b))
                .collect();
            assert!(mismatches.is_empty(), "case {i} ({rect:?}, r={radius}): {mismatches:?}");
        }
    }

    #[test]
    fn grayscale_masks_preserve_coverage_rounding_and_clipping() {
        let mut canvas = Canvas::new(260, 2);
        canvas.fill(119);
        let mask: Vec<u8> = (0..=255).collect();
        canvas.blend_mask(2, 0, 256, 1, &mask, 51, canvas.bounds());
        assert_eq!(&canvas.gray_data()[..4], &[119, 119, 119, 119]);
        assert_eq!(canvas.gray_data()[2 + 127], 85);
        assert_eq!(&canvas.gray_data()[257..260], &[51, 119, 119]);
        assert!(canvas.gray_data()[260..].iter().all(|&g| g == 119));
        let mut out = vec![77; 264 * 2];
        canvas.copy_gray(Rect::new(-2, 0, 265, 2), &mut out, 264);
        assert_eq!(&out[..260], &canvas.gray_data()[..260]);
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
        // The whole canvas, then rectangles with every block and band
        // remainder, down to a single pixel in the corner.
        let rects = [
            c.bounds(),
            Rect::new(5, 3, 160, 60),
            Rect::new(0, 0, 64, 4),
            Rect::new(1, 2, 65, 9),
            Rect::new(17, 7, 129, 23),
            Rect::new(3, 0, 15, 67),
            Rect::new(100, 40, 7, 3),
            Rect::new(170, 66, 1, 1),
        ];
        for rect in rects {
            let mut out = vec![9u8; h * w];
            c.copy_gray_rotated(rect, &mut out, h);
            for y in 0..h {
                for x in 0..w {
                    let inside = rect.contains(crate::geom::Point::new(x as i32, y as i32));
                    let want = if inside { pixels[y * w + x] } else { 9 };
                    assert_eq!(out[(w - 1 - x) * h + y], want, "{rect:?}: canvas ({x}, {y})");
                }
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

    #[test]
    fn color_values_and_luma() {
        assert_eq!(Color::gray(77), Color::rgb(77, 77, 77));
        assert_eq!(Color::gray(200).to_luma(), 200);
        assert_eq!(Color::rgb(255, 0, 0).to_luma(), 76);
        // The color highlight is about as light as the gray one, so text
        // under it reads the same on either screen.
        assert!((HIGHLIGHT.to_luma() as i32 - HIGHLIGHT_GRAY as i32).abs() <= 12, "{}", HIGHLIGHT.to_luma());
    }

    fn rgba_at(c: &Canvas, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * c.width() + x) * 4) as usize;
        c.raw()[i..i + 4].try_into().unwrap()
    }

    /// Draws the same scene on both canvases: gray drawing on a color
    /// canvas must be the gray canvas's pixels, `[g, g, g, 255]`.
    #[test]
    fn gray_drawing_on_a_color_canvas_matches_the_gray_canvas() {
        let (w, h) = (371u32, 289u32);
        let page: Vec<u8> = (0..w * h).map(|i| (i * 7 % 253) as u8).collect();
        let mut gray = Canvas::new(w, h);
        let mut color = Canvas::with_mode(w, h, ColorMode::Color32);
        assert_eq!(
            (color.mode(), color.bytes_per_pixel(), color.raw().len()),
            (ColorMode::Color32, 4, (w * h * 4) as usize)
        );
        let mask: Vec<u8> = (0..40 * 30).map(|i| (i * 13 % 256) as u8).collect();
        let cover: Vec<u8> = (0..50 * 60).map(|i| (i % 256) as u8).collect();
        for c in [&mut gray, &mut color] {
            c.set_gray(&page);
            c.fill_rect(Rect::new(-5, 10, 100, 20), 51);
            c.fade_rect(Rect::new(30, 0, 60, 60));
            c.invert_rect(Rect::new(200, 200, 80, 80));
            c.highlight_rect(Rect::new(0, 100, 371, 10));
            c.fill_rounded_rect(Rect::new(5, 3, 360, 280), 17.3, 119);
            c.stroke_rounded_rect(Rect::new(25, 40, 257, 365), 18.0, 4.0, 0);
            c.stroke_rounded_rect(Rect::new(20, 30, 210, 315), 0.0, 3.0, 17);
            c.stroke_rounded_rect(Rect::new(5, 3, 360, 280), 35.0, 1.0, 0);
            c.stroke_dashed_rounded_rect(Rect::new(10, 10, 100, 40), 6.0, 4.0, 8.0, 0x55);
            c.fill_circle(187.25, 137.75, 103.6, 34);
            c.stroke_circle(-1.5, 280.5, 28.75, 3.5, 200);
            c.fill_polygon(&[(10.0, 250.0), (60.5, 200.25), (90.0, 280.0)], 85);
            c.blend_mask(300, 250, 40, 30, &mask, 0, Rect::new(0, 0, 360, 270));
            c.blit_gray(Rect::new(320, 10, 50, 60), &cover, 50);
        }
        // The highlight is the one place colors differ; compare it apart.
        let mut expected = Vec::new();
        for (i, &g) in gray.gray_data().iter().enumerate() {
            let in_highlight = (100..110).contains(&(i as u32 / w));
            expected.extend(if in_highlight { [0, 0, 0, 0] } else { [g, g, g, 255] });
        }
        let got: Vec<u8> = color
            .raw()
            .chunks(4)
            .enumerate()
            .flat_map(|(i, p)| if (100..110).contains(&(i as u32 / w)) { [0, 0, 0, 0] } else { p.try_into().unwrap() })
            .collect();
        let mismatches: Vec<_> =
            got.chunks(4).zip(expected.chunks(4)).enumerate().filter(|(_, (a, b))| a != b).take(5).collect();
        assert!(mismatches.is_empty(), "{mismatches:?}");
        // copy_gray of a color canvas is what a gray screen shows.
        let mut out = vec![0u8; (w * h) as usize];
        color.copy_gray(color.bounds(), &mut out, w as usize);
        assert_eq!(&out[..100 * w as usize], &gray.gray_data()[..100 * w as usize]);
    }

    #[test]
    fn color_shapes_use_the_full_canvas_without_a_scratch_band() {
        let mut c = Canvas::with_mode(1272, 1696, ColorMode::Color32);
        c.fill_rounded_rect(c.bounds(), 12.0, 51);
        c.stroke_rounded_rect(c.bounds(), 12.0, 3.0, 0);
        assert_eq!(c.shape_rgba.capacity(), 0);
        assert_eq!(rgba_at(&c, 600, 800), [51, 51, 51, 255]);
        assert_eq!(rgba_at(&c, 1, 800), [0, 0, 0, 255]);
    }

    #[test]
    fn color_highlights_tint_paper_keep_ink_and_are_idempotent() {
        let mut c = Canvas::with_mode(20, 20, ColorMode::Color32);
        c.fill_rect(Rect::new(4, 4, 5, 5), 0);
        c.fill_rect_color(Rect::new(10, 0, 2, 2), Color::rgb(40, 90, 230));
        c.highlight_rect(Rect::new(0, 0, 12, 12));
        let first = c.raw().to_vec();
        c.highlight_rect(Rect::new(0, 0, 12, 12));
        assert_eq!(c.raw(), first);
        assert_eq!(rgba_at(&c, 0, 0), [HIGHLIGHT.r, HIGHLIGHT.g, HIGHLIGHT.b, 255], "paper turns yellow");
        assert_eq!(rgba_at(&c, 4, 4), [0, 0, 0, 255], "ink stays black");
        assert_eq!(rgba_at(&c, 10, 0), [40, 90, HIGHLIGHT.b, 255], "colors darken toward yellow");
        assert_eq!(rgba_at(&c, 19, 19), [255, 255, 255, 255], "outside is untouched");
    }

    #[test]
    fn rgba_images_go_in_and_out_of_both_canvases() {
        // 3×2 image: red, green, blue / white, gray, black, alpha 0 to show it's ignored.
        let img = [255, 0, 0, 0, 0, 255, 0, 9, 0, 0, 255, 255, 255, 255, 255, 255, 128, 128, 128, 0, 0, 0, 0, 255];
        let mut color = Canvas::with_mode(4, 3, ColorMode::Color32);
        color.blit_rgba(Rect::new(1, 1, 3, 2), &img, 3);
        assert_eq!(rgba_at(&color, 1, 1), [255, 0, 0, 255]);
        assert_eq!(rgba_at(&color, 3, 1), [0, 0, 255, 255]);
        assert_eq!(rgba_at(&color, 2, 2), [128, 128, 128, 255]);
        assert_eq!(rgba_at(&color, 0, 0), [255; 4]);
        let mut gray = Canvas::new(4, 3);
        gray.blit_rgba(Rect::new(1, 1, 3, 2), &img, 3);
        assert_eq!(&gray.gray_data()[5..8], &[luma(255, 0, 0), luma(0, 255, 0), luma(0, 0, 255)]);
        assert_eq!(&gray.gray_data()[9..12], &[255, 128, 0]);

        // copy_rgba keeps padding, and a gray canvas comes out as gray RGBA.
        let mut out = vec![7u8; 5 * 3 * 4];
        color.copy_rgba(Rect::new(1, 1, 3, 1), &mut out, 5);
        assert_eq!(
            &out[(5 + 1) * 4..(5 + 4) * 4],
            &img[..12].iter().enumerate().map(|(i, &v)| if i % 4 == 3 { 255 } else { v }).collect::<Vec<_>>()[..]
        );
        assert!(out[..(5 + 1) * 4].iter().all(|&v| v == 7) && out[(5 + 4) * 4..].iter().all(|&v| v == 7));
        let mut out = vec![0u8; 4 * 3 * 4];
        gray.copy_rgba(gray.bounds(), &mut out, 4);
        assert_eq!(&out[5 * 4..6 * 4], &[luma(255, 0, 0), luma(255, 0, 0), luma(255, 0, 0), 255]);

        // A whole page, both ways.
        let page: Vec<u8> = (0..4 * 3).flat_map(|i| [i as u8 * 20, 255 - i as u8, 3, 0]).collect();
        color.set_rgba(&page);
        assert_eq!(rgba_at(&color, 1, 2), [9 * 20, 255 - 9, 3, 255]);
        gray.set_rgba(&page);
        assert_eq!(gray.gray_data()[9], luma(180, 246, 3));
        color.set_gray(&[9; 12]);
        assert_eq!(rgba_at(&color, 3, 2), [9, 9, 9, 255]);
    }

    #[test]
    fn color_rotation_matches_the_gray_rule() {
        let (w, h) = (37usize, 23usize);
        let mut c = Canvas::with_mode(w as u32, h as u32, ColorMode::Color32);
        let page: Vec<u8> = (0..w * h).flat_map(|i| [(i * 7) as u8, (i * 3) as u8, (i * 11) as u8, 255]).collect();
        c.set_rgba(&page);
        for rect in [c.bounds(), Rect::new(3, 2, 20, 9), Rect::new(36, 22, 4, 4)] {
            let mut out = vec![9u8; w * h * 4];
            let changed = c.copy_rgba_rotated(rect, &mut out, h);
            let mut gray = vec![9u8; w * h];
            assert_eq!(c.copy_gray_rotated(rect, &mut gray, h), changed);
            for y in 0..h {
                for x in 0..w {
                    let inside = rect.contains(crate::geom::Point::new(x as i32, y as i32));
                    let at = (w - 1 - x) * h + y;
                    let src = &page[(y * w + x) * 4..(y * w + x) * 4 + 4];
                    let want: [u8; 4] = if inside { src.try_into().unwrap() } else { [9; 4] };
                    assert_eq!(out[at * 4..at * 4 + 4], want, "{rect:?}: ({x}, {y})");
                    assert_eq!(gray[at], if inside { luma(src[0], src[1], src[2]) } else { 9 });
                }
            }
        }
        // A gray canvas rotates into an RGBA panel as gray pixels.
        let mut g = Canvas::new(3, 2);
        g.blit_gray(g.bounds(), &[1, 2, 3, 4, 5, 6], 3);
        let mut out = vec![0u8; 6 * 4];
        g.copy_rgba_rotated(g.bounds(), &mut out, 2);
        assert_eq!(out.chunks(4).map(|p| p[0]).collect::<Vec<_>>(), [3, 6, 2, 5, 1, 4]);
        assert!(out.chunks(4).all(|p| p[0] == p[1] && p[1] == p[2] && p[3] == 255));
    }

    #[test]
    fn copy_raw_copies_the_stored_bytes() {
        let mut c = Canvas::with_mode(4, 2, ColorMode::Color32);
        c.fill_rect_color(Rect::new(1, 0, 2, 1), Color::rgb(1, 2, 3));
        let mut out = vec![0u8; 4 * 2 * 4];
        c.copy_raw(Rect::new(2, 0, 5, 5), &mut out);
        assert_eq!(&out[8..16], &[1, 2, 3, 255, 255, 255, 255, 255]);
        assert!(out[..8].iter().all(|&v| v == 0));
    }
}
