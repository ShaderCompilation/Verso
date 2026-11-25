//! Writing the app's canvas into the Kindle's framebuffer, whatever its
//! pixel format and viewport.
//!
//! An 8-bit panel (every monochrome Kindle) gets the exact 8-bit grayscale
//! copy it always had. A 32-bit panel (Colorsoft) writes true color pixels
//! in the panel's channel order (`RGBA`/`RGB32` or `BGRA`/`BGR32`).

use crate::geom::Rect;
use crate::gfx::{Canvas, ColorMode, RotateScratch};
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
    /// Physical framebuffer origin from FBInk's state.
    ///
    /// FBInk adds `view_vert_offset` (vertical padding to centre its bitmap
    /// text grid when `view_height` isn't a multiple of its cell height) onto
    /// `view_vert_origin`. Subtracting it recovers the real panel viewport
    /// origin, matching FBInk's own `viewVertOrigin - viewVertOffset`.
    pub fn fbink_origin(view_hori_origin: u8, view_vert_origin: u8, view_vert_offset: u8) -> (u32, u32) {
        (view_hori_origin as u32, view_vert_origin.saturating_sub(view_vert_offset) as u32)
    }

    /// Whether the viewport lies inside the visible screen (`screen_width`×`screen_height`),
    /// so every refresh rectangle stays within the EPDC's bounds.
    pub fn fits_screen(&self, screen_width: u32, screen_height: u32) -> bool {
        self.width > 0
            && self.height > 0
            && self.origin.0.checked_add(self.width).is_some_and(|r| r <= screen_width)
            && self.origin.1.checked_add(self.height).is_some_and(|b| b <= screen_height)
    }

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
    /// 32-bit landscape only: the turned frame in cached RAM, so channel
    /// swizzling never reads back from mmap'd `/dev/fb0`. Allocated with
    /// the first landscape frame; portrait writes straight from the canvas.
    shadow: Vec<u8>,
    /// The landscape transpose's bands, kept between frames.
    scratch: RotateScratch,
}

impl Writer {
    pub fn new(layout: Layout) -> Self {
        Self { layout, shadow: Vec::new(), scratch: RotateScratch::default() }
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
                let r = match orientation {
                    Orientation::Portrait => {
                        let r = rect.intersect(&panel)?;
                        canvas.copy_gray(r, dst, l.stride);
                        r
                    }
                    Orientation::Landscape => canvas.copy_gray_rotated(rect, dst, l.stride, &mut self.scratch)?,
                };
                if l.inverted {
                    for y in r.y..r.bottom() {
                        let row = y as usize * l.stride;
                        dst[row + r.x as usize..row + r.right() as usize].iter_mut().for_each(|v| *v = !*v);
                    }
                }
                r
            }
            Format::Rgba32 | Format::Bgra32 => {
                let pixel = Pixel { swap_rb: l.format == Format::Bgra32, invert: if l.inverted { 0xff } else { 0 } };
                let dst = &mut fb[l.offset()..];
                match orientation {
                    Orientation::Portrait => {
                        let r = rect.intersect(&panel)?;
                        let (cw, src) = (canvas.width() as usize, canvas.raw());
                        let (x0, w) = (r.x as usize, r.w as usize);
                        let gray = canvas.mode() == ColorMode::Gray8;
                        for y in r.y as usize..r.bottom() as usize {
                            let out = &mut dst[y * l.stride + x0 * 4..][..w * 4];
                            if gray {
                                pixel.gray(out, &src[y * cw + x0..][..w]);
                            } else {
                                pixel.rgba(out, &src[(y * cw + x0) * 4..][..w * 4]);
                            }
                        }
                        r
                    }
                    Orientation::Landscape => {
                        let w = l.width as usize;
                        if self.shadow.is_empty() {
                            self.shadow = vec![255; w * l.height as usize * 4];
                        }
                        let r = canvas.copy_rgba_rotated(rect, &mut self.shadow, w, &mut self.scratch)?;
                        let (x0, len) = (r.x as usize * 4, r.w as usize * 4);
                        for y in r.y as usize..r.bottom() as usize {
                            pixel.rgba(&mut dst[y * l.stride + x0..][..len], &self.shadow[y * w * 4 + x0..][..len]);
                        }
                        r
                    }
                }
            }
        };
        Some(Rect::new(r.x + l.origin.0 as i32, r.y + l.origin.1 as i32, r.w, r.h))
    }
}

/// How a 32-bit panel wants its pixels: channel order and inversion.
#[derive(Clone, Copy)]
struct Pixel {
    swap_rb: bool,
    invert: u8,
}

impl Pixel {
    /// One row of RGBA pixels into the panel row `out`, which is only
    /// written, never read.
    fn rgba(self, out: &mut [u8], src: &[u8]) {
        let i = self.invert;
        if !self.swap_rb && i == 0 {
            out.copy_from_slice(src);
        } else if self.swap_rb {
            for (o, s) in out.as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<4>().0) {
                *o = [s[2] ^ i, s[1] ^ i, s[0] ^ i, 0xff];
            }
        } else {
            for (o, s) in out.as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<4>().0) {
                *o = [s[0] ^ i, s[1] ^ i, s[2] ^ i, 0xff];
            }
        }
    }

    /// One row of gray pixels into the panel row `out`.
    fn gray(self, out: &mut [u8], src: &[u8]) {
        for (o, &g) in out.as_chunks_mut::<4>().0.iter_mut().zip(src) {
            let g = g ^ self.invert;
            *o = [g, g, g, 0xff];
        }
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
    fn fbink_origin_strips_text_row_offset_and_fits_screen_catches_overflow() {
        // Basic 5 (1072x1448): FBInk adds +4 px row centering onto view_vert_origin;
        // PW5/Oasis/Scribe add +8 px. Physical viewport stays at (0, 0).
        assert_eq!(Layout::fbink_origin(0, 4, 4), (0, 0));
        assert_eq!(Layout::fbink_origin(0, 8, 8), (0, 0));
        // A real hardware bezel viewport keeps its physical origin after stripping the text offset.
        assert_eq!(Layout::fbink_origin(0, 14, 4), (0, 10));

        let basic5 = Layout {
            format: Format::Gray8,
            stride: 1072,
            origin: Layout::fbink_origin(0, 4, 4),
            width: 1072,
            height: 1448,
            inverted: false,
        };
        assert!(basic5.fits_screen(1072, 1448));

        // Shifting by the unstripped +4 px offset overflows the visible 1448 px screen
        // even though it still fits inside a double-buffered 1072x2896 mapping.
        let shifted = Layout { origin: (0, 4), ..basic5 };
        assert!(shifted.required_len().unwrap() < 1072 * 2896);
        assert!(!shifted.fits_screen(1072, 1448));
        assert!(!Layout { origin: (1, 0), ..basic5 }.fits_screen(1072, 1448));
        assert!(!Layout { width: 0, ..basic5 }.fits_screen(1072, 1448));
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
