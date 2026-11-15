//! SVG icons, rendered with resvg at whatever size is needed so they stay
//! crisp at 300 dpi. Icons are drawn as a coverage mask, so they can be any gray.

use std::collections::HashMap;

use resvg::{tiny_skia, usvg};

use super::Canvas;
use crate::geom::Rect;

/// The Lucide icons in `assets/icons/`.
#[allow(dead_code)] // some are for screens still to come (library, settings)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    ArrowLeft,
    ChevronLeft,
    ChevronRight,
    ChevronDown,
    List,
    TextSize,
    Settings,
    Sun,
    Close,
    Check,
    BookOpen,
    Info,
    Menu,
    Exit,
    Folder,
    FolderPlus,
    Pencil,
    SquareCheck,
    FolderInput,
    ClipboardPaste,
    Search,
    Trash,
}

impl Icon {
    #[cfg(test)]
    pub const ALL: [Icon; 22] = [
        Icon::ArrowLeft,
        Icon::ChevronLeft,
        Icon::ChevronRight,
        Icon::ChevronDown,
        Icon::List,
        Icon::TextSize,
        Icon::Settings,
        Icon::Sun,
        Icon::Close,
        Icon::Check,
        Icon::BookOpen,
        Icon::Info,
        Icon::Menu,
        Icon::Exit,
        Icon::Folder,
        Icon::FolderPlus,
        Icon::Pencil,
        Icon::SquareCheck,
        Icon::FolderInput,
        Icon::ClipboardPaste,
        Icon::Search,
        Icon::Trash,
    ];

    fn svg(self) -> &'static str {
        match self {
            Icon::ArrowLeft => include_str!("../../../../assets/icons/arrow-left.svg"),
            Icon::ChevronLeft => include_str!("../../../../assets/icons/chevron-left.svg"),
            Icon::ChevronRight => include_str!("../../../../assets/icons/chevron-right.svg"),
            Icon::ChevronDown => include_str!("../../../../assets/icons/chevron-down.svg"),
            Icon::List => include_str!("../../../../assets/icons/list.svg"),
            Icon::TextSize => include_str!("../../../../assets/icons/a-large-small.svg"),
            Icon::Settings => include_str!("../../../../assets/icons/settings.svg"),
            Icon::Sun => include_str!("../../../../assets/icons/sun.svg"),
            Icon::Close => include_str!("../../../../assets/icons/x.svg"),
            Icon::Check => include_str!("../../../../assets/icons/check.svg"),
            Icon::BookOpen => include_str!("../../../../assets/icons/book-open.svg"),
            Icon::Info => include_str!("../../../../assets/icons/info.svg"),
            Icon::Menu => include_str!("../../../../assets/icons/menu.svg"),
            Icon::Exit => include_str!("../../../../assets/icons/log-out.svg"),
            Icon::Search => include_str!("../../../../assets/icons/search.svg"),
            Icon::Folder => include_str!("../../../../assets/icons/folder.svg"),
            Icon::FolderPlus => include_str!("../../../../assets/icons/folder-plus.svg"),
            Icon::SquareCheck => include_str!("../../../../assets/icons/square-check.svg"),
            Icon::FolderInput => include_str!("../../../../assets/icons/folder-input.svg"),
            Icon::ClipboardPaste => include_str!("../../../../assets/icons/clipboard-paste.svg"),
            Icon::Pencil => include_str!("../../../../assets/icons/pencil.svg"),
            Icon::Trash => include_str!("../../../../assets/icons/trash-2.svg"),
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
        for icon in Icon::ALL {
            let mask = render_mask(icon, 48, 48);
            assert!(mask.iter().any(|&a| a > 200), "{icon:?} is blank");
        }
    }
}
