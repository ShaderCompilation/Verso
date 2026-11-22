//! A preview of what a Kaleido 3 color e-ink panel (the Kindle Colorsoft)
//! does to an RGB frame, for the simulator: C in the window, or
//! `VERSO_SIM_KALEIDO=1` (sim_device.rs) for the window and screenshots.
//!
//! - Its color filter has half the resolution of the 300-dpi black and
//!   white layer (150 dpi), so each 2×2 block of pixels shares one color
//!   while brightness stays per pixel: text stays sharp, color edges blur.
//! - Its colors are muted, about half the saturation of a phone screen.
//! - Its paper is darker and its black lighter than a monochrome panel's.
//!
//! An impression for judging layouts and highlight contrast, not a
//! calibrated model of the panel (to verify against a Colorsoft).

use crate::gfx::luma;

/// What the panel shows for white and black (the frontlight still dims it).
const PAPER: i32 = 228;
const INK: i32 = 22;
/// Color saturation kept, out of 256.
const SATURATION: i32 = 128;

/// Filters an RGBA frame (`width`×`height`) in place.
pub fn preview(rgba: &mut [u8], width: usize, height: usize) {
    assert_eq!(rgba.len(), width * height * 4, "frame size");
    for by in (0..height).step_by(2) {
        for bx in (0..width).step_by(2) {
            let block: Vec<usize> = (by..(by + 2).min(height))
                .flat_map(|y| (bx..(bx + 2).min(width)).map(move |x| (y * width + x) * 4))
                .collect();
            // The block's average color difference from each pixel's own
            // brightness; brightness itself stays per pixel.
            let mut chroma = [0i32; 3];
            for &i in &block {
                let y = luma(rgba[i], rgba[i + 1], rgba[i + 2]) as i32;
                for (c, sum) in chroma.iter_mut().enumerate() {
                    *sum += rgba[i + c] as i32 - y;
                }
            }
            let n = block.len() as i32;
            let chroma = chroma.map(|sum| sum * SATURATION / (256 * n));
            for &i in &block {
                let y = luma(rgba[i], rgba[i + 1], rgba[i + 2]) as i32;
                let y = INK + y * (PAPER - INK) / 255;
                for c in 0..3 {
                    rgba[i + c] = (y + chroma[c]).clamp(0, 255) as u8;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(pixels: &[[u8; 3]]) -> Vec<u8> {
        pixels.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect()
    }

    #[test]
    fn grays_keep_their_detail_on_darker_paper() {
        // A one-pixel checkerboard of black and white survives: brightness
        // isn't shared by the 2×2 color filter.
        let mut f = frame(&[[0; 3], [255; 3], [255; 3], [0; 3]]);
        preview(&mut f, 2, 2);
        assert_eq!(f, frame(&[[INK as u8; 3], [PAPER as u8; 3], [PAPER as u8; 3], [INK as u8; 3]]));
    }

    #[test]
    fn color_is_muted_and_shared_by_each_two_by_two_block() {
        // A red pixel next to three white ones: the block shares a faint
        // pink, and the red pixel stays as dark as red is.
        let red = [220, 30, 30];
        let mut f = frame(&[red, [255; 3], [255; 3], [255; 3]]);
        preview(&mut f, 2, 2);
        let px = |i: usize| [f[i * 4], f[i * 4 + 1], f[i * 4 + 2]];
        let (r, w) = (px(0), px(1));
        assert!(w[0] > w[1] && w[1] == w[2], "white turns pink: {w:?}");
        assert!(r[0] > r[1] && (r[0] - r[1]) < (red[0] - red[1]) / 4, "red is muted: {r:?}");
        let y = |p: [u8; 3]| luma(p[0], p[1], p[2]) as i32;
        assert!((y(r) - (INK + y(red) * (PAPER - INK) / 255)).abs() <= 3, "{r:?}");
        assert_eq!(px(1), px(2));
    }

    #[test]
    fn odd_sizes_filter_their_edge_blocks() {
        let mut f = frame(&[[255, 0, 0]; 3 * 3]);
        preview(&mut f, 3, 3);
        let corner = &f[8 * 4..8 * 4 + 3];
        assert!(corner[0] > corner[1], "the lone corner pixel keeps its own color: {corner:?}");
        assert!(f.chunks(4).all(|p| p == &f[..4]), "a flat color stays flat");
    }
}
