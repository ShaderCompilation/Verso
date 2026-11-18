//! Folder tiles in Collections show covers of the folder's books
//! (docs/folder-previews.md). Each mode draws inside the tile rectangle the
//! folder icon uses, so names, the pressed outline and the selection tick
//! keep their places.
use super::*;
use crate::store::FolderPreview;
use crate::ui::theme::{INK3, PAPER};

/// A chosen book whose art can't be shown: a plain light-gray card. At
/// mosaic size a title would be unreadable noise. An exact panel gray.
const PLACEHOLDER: u8 = 0xdd;

/// Where a tile draws its previews.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Layout {
    /// Back to front: (position in the pick, slot, how it decodes).
    pub covers: Vec<(usize, Rect, Fit)>,
    /// The count badge sits in its bottom-right corner.
    pub front: Rect,
    /// The folder tab above the cover (Latest book).
    pub tab: Option<Rect>,
    /// Everything the preview draws, for the refresh hint.
    pub bounds: Rect,
}

/// The layout of `books` previews (already a prefix of the folder's pick) in
/// tile `r`. None for the Icon mode, and when there is nothing to preview.
pub(super) fn layout(mode: FolderPreview, books: usize, r: Rect, t: &Theme) -> Option<Layout> {
    let n = books.min(mode.slots());
    if n == 0 || r.w <= 0 || r.h <= 0 {
        return None;
    }
    Some(match mode {
        FolderPreview::Icon => return None,
        FolderPreview::Covers => {
            // The size of a book cover in the same cell.
            let w = r.w.min(r.h * 2 / 3).max(1);
            let h = (w * 3 / 2).min(r.h);
            let area = Rect::new(r.x + (r.w - w) / 2, r.y, w, h);
            let gap = t.mm(1.0);
            let (sw, sh) = (((w - gap) / 2).max(1), ((h - gap) / 2).max(1));
            let covers = if n == 1 {
                // One book: a single quarter-size cover in the middle.
                vec![(0, Rect::new(area.x + (w - sw) / 2, area.y + (h - sh) / 2, sw, sh), Fit::Preview)]
            } else {
                // Reading order; free slots stay empty.
                (0..n)
                    .map(|i| {
                        let (col, row) = ((i % 2) as i32, (i / 2) as i32);
                        (i, Rect::new(area.x + col * (sw + gap), area.y + row * (sh + gap), sw, sh), Fit::Preview)
                    })
                    .collect()
            };
            Layout { covers, front: area, tab: None, bounds: area }
        }
        FolderPreview::Stack => {
            // Same-size covers, each shifted up and right behind the one in front.
            let d = t.mm(2.0);
            let w = (r.w - 2 * d).min((r.h - 2 * d) * 2 / 3).max(1);
            let h = w * 3 / 2;
            let x = r.x + (r.w - w - 2 * d) / 2;
            let bounds = Rect::new(x, r.y, w + 2 * d, h + 2 * d);
            // Shorter stacks stay centred in the same box.
            let shift = (3 - n as i32) * d / 2;
            let at = |k: usize| Rect::new(x + shift + k as i32 * d, r.y + 2 * d - shift - k as i32 * d, w, h);
            let covers = (0..n).rev().map(|k| (k, at(k), if k == 0 { Fit::Preview } else { Fit::Behind })).collect();
            Layout { covers, front: at(0), tab: None, bounds }
        }
        FolderPreview::Latest => {
            let tab_h = t.mm(2.0);
            let w = r.w.min((r.h - tab_h) * 2 / 3).max(1);
            let h = w * 3 / 2;
            let cover = Rect::new(r.x + (r.w - w) / 2, r.y + tab_h, w, h);
            Layout {
                covers: vec![(0, cover, Fit::Preview)],
                front: cover,
                tab: Some(Rect::new(cover.x, r.y, w * 2 / 5, tab_h)),
                bounds: Rect::new(cover.x, r.y, w, h + tab_h),
            }
        }
    })
}

/// Draws a folder's preview of `books` in tile `r`, with a badge counting
/// the folder's books. Returns the area drawn, or None when the tile should
/// show the folder icon instead.
pub(super) fn draw(
    cx: &mut DrawCx,
    mode: FolderPreview,
    books: &[&Book],
    count: usize,
    r: Rect,
    store: &Store,
    cache: &mut Thumbnails,
) -> Option<Rect> {
    let t = cx.theme;
    let layout = layout(mode, books.len(), r, t)?;
    for &(i, rect, fit) in &layout.covers {
        if let Some(image) = load_cover(cache, store, books[i], rect.w, rect.h, fit) {
            cx.canvas.blit_gray(rect, image.as_raw(), image.width() as usize);
            // A stack's covers need an edge where they overlap.
            if mode == FolderPreview::Stack {
                cx.canvas.stroke_rounded_rect(rect, 0.0, t.hairline as f32, INK);
            }
        } else {
            cx.fill(rect, PLACEHOLDER);
            cx.canvas.stroke_rounded_rect(rect, 0.0, t.hairline as f32, INK3);
        }
    }
    if let Some(tab) = layout.tab {
        // The folder icon's tab: flat on top, sloping down on the right, on
        // a frame around the cover.
        cx.canvas.stroke_rounded_rect(layout.front, 0.0, t.border as f32, INK);
        let (x, y, bottom) = (tab.x as f32, tab.y as f32, (tab.bottom() + t.border) as f32);
        let slope = tab.h as f32 * 2.0 / 3.0;
        let right = tab.right() as f32;
        cx.canvas.fill_polygon(&[(x, bottom), (x, y), (right, y), (right + slope, bottom)], INK);
    }
    draw_badge(cx, count, layout.front);
    Some(layout.bounds)
}

/// A small white pill with the folder's book count, in the bottom-right
/// corner of `front`: a single cover would otherwise read as a book.
fn draw_badge(cx: &mut DrawCx, count: usize, front: Rect) {
    let t = cx.theme;
    let text = count.to_string();
    let h = t.mm(4.5);
    let w = (cx.text.width(&text, t.caption).ceil() as i32 + t.mm(2.5)).max(h).min(front.w);
    let inset = t.mm(1.0);
    let badge = Rect::new(front.right() - inset - w, front.bottom() - inset - h, w, h);
    let radius = h as f32 / 2.0;
    cx.canvas.fill_rounded_rect(badge, radius, PAPER);
    cx.canvas.stroke_rounded_rect(badge, radius, t.border as f32, INK);
    cx.line(&text, t.caption.gray(INK), badge, HAlign::Center);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tile() -> (Theme, Rect) {
        // A portrait Collections tile on the Kindle (1272 × 1696).
        (Theme::for_dpi(300), Rect::new(59, 609, 257, 306))
    }
    fn inside(outer: Rect, inner: Rect) -> bool {
        inner.x >= outer.x && inner.y >= outer.y && inner.right() <= outer.right() && inner.bottom() <= outer.bottom()
    }

    #[test]
    fn icon_mode_and_folders_without_previews_draw_the_icon() {
        let (t, r) = tile();
        assert_eq!(layout(FolderPreview::Icon, 4, r, &t), None);
        for mode in FolderPreview::ALL {
            assert_eq!(layout(mode, 0, r, &t), None);
        }
    }

    #[test]
    fn covers_fill_a_two_by_two_grid_in_reading_order() {
        let (t, r) = tile();
        let l = layout(FolderPreview::Covers, 4, r, &t).unwrap();
        assert!(inside(r, l.bounds));
        assert_eq!(l.covers.iter().map(|c| c.0).collect::<Vec<_>>(), [0, 1, 2, 3]);
        let slots: Vec<_> = l.covers.iter().map(|c| c.1).collect();
        assert!(slots.iter().all(|s| inside(l.bounds, *s) && s.w == slots[0].w && s.h == slots[0].h));
        assert_eq!((slots[0].y, slots[1].y), (slots[0].y, slots[0].y));
        assert!(slots[1].x > slots[0].right() && slots[2].y > slots[0].bottom());
        assert!(slots[0].w as u32 <= covers::SMALL_WIDTH && slots[0].h as u32 <= covers::SMALL_HEIGHT);
        // No overlaps: every slot is its own cover.
        for (i, a) in slots.iter().enumerate() {
            assert!(slots[i + 1..].iter().all(|b| a.intersect(b).is_none()));
        }
        assert!(l.covers.iter().all(|c| c.2 == Fit::Preview));
        // More books than slots draw only four.
        assert_eq!(layout(FolderPreview::Covers, 9, r, &t).unwrap(), l);
    }

    #[test]
    fn short_folders_leave_slots_empty_or_centre_one_cover() {
        let (t, r) = tile();
        let full = layout(FolderPreview::Covers, 4, r, &t).unwrap();
        for n in 2..4 {
            let l = layout(FolderPreview::Covers, n, r, &t).unwrap();
            assert_eq!(l.covers[..], full.covers[..n], "the first {n} slots, the rest stay empty");
            assert_eq!(l.front, full.front, "the badge keeps its corner");
        }
        let one = layout(FolderPreview::Covers, 1, r, &t).unwrap();
        let (slot, area) = (one.covers[0].1, one.bounds);
        assert_eq!((slot.w, slot.h), (full.covers[0].1.w, full.covers[0].1.h));
        assert!((slot.center().x - area.center().x).abs() <= 1 && (slot.center().y - area.center().y).abs() <= 1);
    }

    #[test]
    fn stack_draws_same_size_covers_back_to_front() {
        let (t, r) = tile();
        let l = layout(FolderPreview::Stack, 4, r, &t).unwrap();
        assert!(inside(r, l.bounds));
        assert_eq!(l.covers.iter().map(|c| c.0).collect::<Vec<_>>(), [2, 1, 0], "the first book is drawn last");
        let [back, middle, front] = [l.covers[0].1, l.covers[1].1, l.covers[2].1];
        assert!([back, middle].iter().all(|c| (c.w, c.h) == (front.w, front.h)));
        let d = t.mm(2.0);
        assert_eq!((middle.x - front.x, front.y - middle.y), (d, d));
        assert_eq!((back.x - middle.x, middle.y - back.y), (d, d));
        assert_eq!(l.front, front);
        assert!(l.covers.iter().all(|c| inside(l.bounds, c.1)));
        // The front cover is larger than a small thumbnail and decodes the
        // full one; the hidden ones decode small files.
        assert!(front.w as u32 > covers::SMALL_WIDTH);
        assert_eq!(l.covers.iter().map(|c| c.2).collect::<Vec<_>>(), [Fit::Behind, Fit::Behind, Fit::Preview]);
        // One or two books show only those, centred in the same box.
        let two = layout(FolderPreview::Stack, 2, r, &t).unwrap();
        assert_eq!(two.covers.len(), 2);
        assert_eq!(two.bounds, l.bounds);
        let one = layout(FolderPreview::Stack, 1, r, &t).unwrap();
        assert_eq!(one.covers.len(), 1);
        assert!((one.front.center().x - l.bounds.center().x).abs() <= 1);
        assert!((one.front.center().y - l.bounds.center().y).abs() <= 1);
    }

    #[test]
    fn latest_book_draws_one_cover_under_a_folder_tab() {
        let (t, r) = tile();
        let l = layout(FolderPreview::Latest, 3, r, &t).unwrap();
        assert_eq!(l.covers.len(), 1);
        let cover = l.covers[0].1;
        assert_eq!(l.front, cover);
        let tab = l.tab.unwrap();
        assert_eq!((tab.x, tab.bottom()), (cover.x, cover.y));
        assert!(tab.w < cover.w / 2);
        assert!(inside(r, l.bounds) && inside(l.bounds, cover) && inside(l.bounds, tab));
        assert_eq!((cover.w * 3 / 2), cover.h);
    }

    #[test]
    fn layouts_fit_landscape_and_small_screens() {
        for (t, r) in
            [(Theme::for_dpi(300), Rect::new(59, 609, 363, 343)), (Theme::for_dpi(167), Rect::new(33, 300, 120, 170))]
        {
            for mode in [FolderPreview::Covers, FolderPreview::Stack, FolderPreview::Latest] {
                let l = layout(mode, 4, r, &t).unwrap();
                assert!(inside(r, l.bounds), "{mode:?} inside {r:?}");
                assert!(l.covers.iter().all(|c| c.1.w > 0 && c.1.h > 0 && inside(l.bounds, c.1)));
            }
        }
    }
}
