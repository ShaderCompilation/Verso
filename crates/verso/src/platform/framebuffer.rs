//! Writing the app's canvas into the Kindle's framebuffer, whatever its
//! pixel format and viewport (docs/device-support.md, Phases 4 and 6).
//!
//! An 8-bit panel (every monochrome Kindle) gets the exact 8-bit grayscale
//! copy it always had. A 32-bit panel (Colorsoft) writes true color pixels
//! in the panel's channel order (`RGBA`/`RGB32` or `BGRA`/`BGR32`).

use crate::geom::Rect;
use crate::gfx::Canvas;
use crate::platform::Orientation;

/// How a framebuffer pixel is laid out in memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// One byte of gray per pixel.
    Gray8,
    /// Four bytes per pixel: `[R, G, B, A/X]` (`RGBA`, `RGB32`).
    Rgba32,
    /// Four bytes per pixel: `[B, G, R, A/X]` (`BGRA`, `BGR32`).
    Bgra32,
}

impl Format {
    /// The layout for FBInk's `bpp` and pixel format name, if Verso can
    /// draw on it.
    pub fn from_fbink(bpp: u32, pixel_format: &str) -> Option<Self> {
        match (bpp, pixel_format) {
            (8, _) => Some(Self::Gray8),
            (32, "RGBA" | "RGB32") => Some(Self::Rgba32),
            (32, "BGRA" | "BGR32") => Some(Self::Bgra32),
            _ => None,
        }
    }

    pub fn bytes(self) -> usize {
        match self {
            Self::Gray8 => 1,
            Self::Rgba32 | Self::Bgra32 => 4,
        }
    }
}

/// Where the visible panel sits in the framebuffer mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub format: Format,
    /// Bytes per framebuffer row.
    pub stride: usize,
    /// The viewport's top-left pixel in the framebuffer.
    pub origin: (u32, u32),
    /// The viewport (the panel, in portrait).
    pub width: u32,
    pub height: u32,
    /// The panel shows 0 as white.
    pub inverted: bool,
}

impl Layout {
    /// The bytes the mapping must hold for every viewport pixel, or `None`
    /// when the viewport doesn't fit in a row.
    pub fn required_len(&self) -> Option<usize> {
        let bytes = self.format.bytes();
        let (x0, y0) = (self.origin.0 as usize, self.origin.1 as usize);
        let row_end = (x0 + self.width as usize) * bytes;
        (row_end <= self.stride && self.height > 0).then(|| (y0 + self.height as usize - 1) * self.stride + row_end)
    }

    fn offset(&self) -> usize {
        self.origin.1 as usize * self.stride + self.origin.0 as usize * self.format.bytes()
    }
}

pub struct Writer {
    layout: Layout,
    /// Cached-RAM staging buffer for 32-bit panels so rotation and channel
    /// swizzling never read back from mmap'd `/dev/fb0`. Empty for `Gray8`.
    shadow: Vec<u8>,
}

impl Writer {
    pub fn new(layout: Layout) -> Self {
        let shadow = match layout.format {
            Format::Gray8 => Vec::new(),
            Format::Rgba32 | Format::Bgra32 => vec![255; layout.width as usize * layout.height as usize * 4],
        };
        Self { layout, shadow }
    }

    /// Copies `rect` of `canvas` (drawn in `orientation`) into `fb`, the
    /// whole framebuffer mapping, which must be at least
    /// `layout.required_len()` long. Returns the changed panel rectangle in
    /// framebuffer coordinates (viewport origin added), ready to refresh.
    pub fn write(&mut self, canvas: &Canvas, rect: Rect, orientation: Orientation, fb: &mut [u8]) -> Option<Rect> {
        let l = self.layout;
        let panel = Rect::new(0, 0, l.width as i32, l.height as i32);
        let r = match l.format {
            Format::Gray8 => {
                let dst = &mut fb[l.offset()..];
                let r = copy_gray(canvas, rect, orientation, panel, dst, l.stride)?;
                if l.inverted {
                    for y in r.y..r.bottom() {
                        let row = y as usize * l.stride;
                        dst[row + r.x as usize..row + r.right() as usize].iter_mut().for_each(|v| *v = !*v);
                    }
                }
                r
            }
            Format::Rgba32 | Format::Bgra32 => {
                let w = l.width as usize;
                let r = copy_rgba(canvas, rect, orientation, panel, &mut self.shadow, w)?;
                let swap_rb = l.format == Format::Bgra32;
                let invert = if l.inverted { 0xff } else { 0 };
                let dst = &mut fb[l.offset()..];
                for y in r.y as usize..r.bottom() as usize {
                    let src_start = (y * w + r.x as usize) * 4;
                    let len = r.w as usize * 4;
                    let src = &self.shadow[src_start..src_start + len];
                    let dst_start = y * l.stride + r.x as usize * 4;
                    let out = &mut dst[dst_start..dst_start + len];
                    if !swap_rb && invert == 0 {
                        out.copy_from_slice(src);
                    } else if swap_rb {
                        for (o, s) in out.as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<4>().0) {
                            *o = [s[2] ^ invert, s[1] ^ invert, s[0] ^ invert, 0xff];
                        }
                    } else {
                        for (o, s) in out.as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<4>().0) {
                            *o = [s[0] ^ invert, s[1] ^ invert, s[2] ^ invert, 0xff];
                        }
                    }
                }
                r
            }
        };
        Some(Rect::new(r.x + l.origin.0 as i32, r.y + l.origin.1 as i32, r.w, r.h))
    }
}

/// The 8-bit copy monochrome panels use: the canvas as is in portrait,
/// turned a quarter in landscape.
fn copy_gray(
    canvas: &Canvas,
    rect: Rect,
    orientation: Orientation,
    panel: Rect,
    dst: &mut [u8],
    stride: usize,
) -> Option<Rect> {
    match orientation {
        Orientation::Portrait => {
            let r = rect.intersect(&panel)?;
            canvas.copy_gray(r, dst, stride);
            Some(r)
        }
        Orientation::Landscape => canvas.copy_gray_rotated(rect, dst, stride),
    }
}

/// The 32-bit RGBA copy color panels use: portrait as is, landscape rotated.
fn copy_rgba(
    canvas: &Canvas,
    rect: Rect,
    orientation: Orientation,
    panel: Rect,
    dst: &mut [u8],
    stride_px: usize,
) -> Option<Rect> {
    match orientation {
        Orientation::Portrait => {
            let r = rect.intersect(&panel)?;
            canvas.copy_rgba(r, dst, stride_px);
            Some(r)
        }
        Orientation::Landscape => canvas.copy_rgba_rotated(rect, dst, stride_px),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 6;
    const H: u32 = 4;

    /// A portrait canvas whose pixel (x, y) is `10 * y + x`.
    fn canvas(orientation: Orientation) -> Canvas {
        let (w, h) = orientation.screen_size(W, H);
        let mut c = Canvas::new(w, h);
        let pixels: Vec<u8> = (0..h).flat_map(|y| (0..w).map(move |x| (10 * y + x) as u8)).collect();
        c.blit_gray(Rect::new(0, 0, w as i32, h as i32), &pixels, w as usize);
        c
    }

    fn layout(format: Format, origin: (u32, u32), inverted: bool) -> Layout {
        // Two spare bytes at the end of every row, like a padded stride.
        let stride = (origin.0 + W) as usize * format.bytes() + 2;
        Layout { format, stride, origin, width: W, height: H, inverted }
    }

    /// The gray value the framebuffer holds for panel pixel (x, y).
    fn gray_at(fb: &[u8], l: &Layout, x: u32, y: u32) -> u8 {
        let i = (l.origin.1 + y) as usize * l.stride + (l.origin.0 + x) as usize * l.format.bytes();
        if matches!(l.format, Format::Rgba32 | Format::Bgra32) {
            assert_eq!(fb[i + 1], fb[i], "gray: equal channels");
            assert_eq!(fb[i + 2], fb[i]);
            assert_eq!(fb[i + 3], 0xff, "opaque");
        }
        fb[i]
    }

    #[test]
    fn pixel_formats_map_to_layouts() {
        for name in ["RGBA", "RGB32"] {
            assert_eq!(Format::from_fbink(32, name), Some(Format::Rgba32), "{name}");
        }
        for name in ["BGRA", "BGR32"] {
            assert_eq!(Format::from_fbink(32, name), Some(Format::Bgra32), "{name}");
        }
        assert_eq!(Format::from_fbink(8, "Y8"), Some(Format::Gray8));
        assert_eq!(Format::from_fbink(8, "UNKNOWN"), Some(Format::Gray8));
        for (bpp, name) in [(16, "RGB565"), (24, "RGB24"), (32, "UNKNOWN"), (4, "Y4")] {
            assert_eq!(Format::from_fbink(bpp, name), None, "{bpp} {name}");
        }
    }

    #[test]
    fn required_len_covers_the_viewport_and_refuses_one_wider_than_a_row() {
        let l = layout(Format::Bgra32, (2, 3), false);
        assert_eq!(l.required_len(), Some((3 + 3) * l.stride + (2 + 6) * 4));
        assert_eq!(Layout { stride: 8 * 4 - 1, ..l }.required_len(), None);
        assert_eq!(Layout { height: 0, ..l }.required_len(), None);
    }

    #[test]
    fn every_format_origin_and_orientation_lands_each_pixel_in_its_place() {
        for format in [Format::Gray8, Format::Rgba32, Format::Bgra32] {
            for origin in [(0, 0), (3, 2)] {
                for inverted in [false, true] {
                    for orientation in [Orientation::Portrait, Orientation::Landscape] {
                        let l = layout(format, origin, inverted);
                        let mut fb = vec![0x55u8; l.required_len().unwrap() + 7];
                        let c = canvas(orientation);
                        let mut writer = Writer::new(l);
                        let changed = writer.write(&c, c.bounds(), orientation, &mut fb);
                        let what = format!("{format:?} {origin:?} inverted={inverted} {orientation:?}");
                        assert_eq!(
                            changed,
                            Some(Rect::new(origin.0 as i32, origin.1 as i32, W as i32, H as i32)),
                            "{what}"
                        );
                        for y in 0..H {
                            for x in 0..W {
                                // Landscape: canvas (cx, cy) lands on panel
                                // (cy, H - 1 - cx), the canvas being H wide.
                                let (cx, cy) = match orientation {
                                    Orientation::Portrait => (x, y),
                                    Orientation::Landscape => (H - 1 - y, x),
                                };
                                let want = (10 * cy + cx) as u8;
                                let want = if inverted { !want } else { want };
                                assert_eq!(gray_at(&fb, &l, x, y), want, "{what} at ({x}, {y})");
                            }
                        }
                        // Nothing outside the viewport is touched.
                        let bytes = format.bytes();
                        for (i, &v) in fb.iter().enumerate() {
                            let (row, col) = (i / l.stride, i % l.stride);
                            let inside = (origin.1 as usize..(origin.1 + H) as usize).contains(&row)
                                && (origin.0 as usize * bytes..(origin.0 + W) as usize * bytes).contains(&col);
                            if !inside {
                                assert_eq!(v, 0x55, "{what}: byte {i} outside the viewport");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_partial_update_writes_only_its_rectangle() {
        for format in [Format::Gray8, Format::Rgba32, Format::Bgra32] {
            let l = layout(format, (1, 1), false);
            let mut fb = vec![0u8; l.required_len().unwrap()];
            let c = canvas(Orientation::Portrait);
            let mut writer = Writer::new(l);
            let changed = writer.write(&c, Rect::new(2, 1, 3, 2), Orientation::Portrait, &mut fb);
            assert_eq!(changed, Some(Rect::new(3, 2, 3, 2)), "{format:?}");
            for y in 0..H {
                for x in 0..W {
                    let inside = (2..5).contains(&x) && (1..3).contains(&y);
                    let i = (1 + y) as usize * l.stride + (1 + x) as usize * format.bytes();
                    if inside {
                        assert_eq!(gray_at(&fb, &l, x, y), (10 * y + x) as u8, "{format:?} ({x}, {y})");
                    } else {
                        assert_eq!(fb[i], 0, "{format:?} ({x}, {y}) untouched");
                    }
                }
            }
            // Off the panel: nothing to refresh.
            assert_eq!(writer.write(&c, Rect::new(50, 50, 4, 4), Orientation::Portrait, &mut fb), None);
        }
    }

    #[test]
    fn color_canvas_swizzles_channels_for_rgba32_and_bgra32() {
        use crate::gfx::ColorMode;
        for format in [Format::Rgba32, Format::Bgra32] {
            for inverted in [false, true] {
                for orientation in [Orientation::Portrait, Orientation::Landscape] {
                    let (cw, ch) = orientation.screen_size(W, H);
                    let mut c = Canvas::with_mode(cw, ch, ColorMode::Color32);
                    let mut rgba = Vec::with_capacity((cw * ch * 4) as usize);
                    for y in 0..ch {
                        for x in 0..cw {
                            rgba.extend_from_slice(&[(10 + x * 20) as u8, (100 + y * 15) as u8, 230, 255]);
                        }
                    }
                    c.set_rgba(&rgba);
                    let l = layout(format, (2, 1), inverted);
                    let mut fb = vec![0x44u8; l.required_len().unwrap()];
                    let mut writer = Writer::new(l);
                    let changed = writer.write(&c, c.bounds(), orientation, &mut fb);
                    assert_eq!(changed, Some(Rect::new(2, 1, W as i32, H as i32)));
                    let inv = if inverted { 0xff } else { 0 };
                    for y in 0..H {
                        for x in 0..W {
                            let (cx, cy) = match orientation {
                                Orientation::Portrait => (x, y),
                                Orientation::Landscape => (H - 1 - y, x),
                            };
                            let (r, g, b) = (((10 + cx * 20) as u8) ^ inv, ((100 + cy * 15) as u8) ^ inv, 230u8 ^ inv);
                            let want = match format {
                                Format::Rgba32 => [r, g, b, 0xff],
                                Format::Bgra32 => [b, g, r, 0xff],
                                Format::Gray8 => unreachable!(),
                            };
                            let i = (1 + y) as usize * l.stride + (2 + x) as usize * 4;
                            assert_eq!(
                                &fb[i..i + 4],
                                &want,
                                "{format:?} inverted={inverted} {orientation:?} at ({x}, {y})"
                            );
                        }
                    }
                }
            }
        }
    }
}
