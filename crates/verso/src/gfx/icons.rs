//! SVG icons, rendered with resvg at whatever size is needed so they stay
//! crisp at 300 dpi. Icons are drawn as a coverage mask, so they can be any gray.

use std::collections::HashMap;

use resvg::{tiny_skia, usvg};

use super::Canvas;
use crate::geom::Rect;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    Exit,
}

impl Icon {
    fn svg(self) -> &'static str {
        match self {
            Icon::Exit => include_str!("../../../../assets/icons/log-out.svg"),
        }
    }
}

#[derive(Default)]
pub struct IconRenderer {
    /// Rendered masks by icon and size, so redraws are just a blit.
    masks: HashMap<(Icon, u32, u32), Vec<u8>>,
}

impl IconRenderer {
    /// Draws `icon` scaled to fit `rect`, centred.
    pub fn draw(&mut self, canvas: &mut Canvas, icon: Icon, rect: Rect, gray: u8) {
        if rect.is_empty() {
            return;
        }
        let (w, h) = (rect.w as u32, rect.h as u32);
        let mask = self.masks.entry((icon, w, h)).or_insert_with(|| render_mask(icon, w, h));
        canvas.blend_mask(rect.x, rect.y, w, h, mask, gray, rect);
    }
}

fn render_mask(icon: Icon, w: u32, h: u32) -> Vec<u8> {
    let tree = usvg::Tree::from_str(icon.svg(), &usvg::Options::default()).expect("bundled icon is valid SVG");
    let size = tree.size();
    let scale = (w as f32 / size.width()).min(h as f32 / size.height());
    let dx = (w as f32 - size.width() * scale) / 2.0;
    let dy = (h as f32 - size.height() * scale) / 2.0;
    let mut pixmap = tiny_skia::Pixmap::new(w, h).expect("icon size is non-zero");
    let transform = tiny_skia::Transform::from_translate(dx, dy).pre_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    pixmap.pixels().iter().map(|p| p.alpha()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_render() {
        for icon in [Icon::Exit] {
            let mask = render_mask(icon, 48, 48);
            assert!(mask.iter().any(|&a| a > 200), "{icon:?} is blank");
        }
    }
}
