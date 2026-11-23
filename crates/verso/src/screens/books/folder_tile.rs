//! Folder tiles in Collections show covers of the folder's books.
//! Each mode draws inside the tile rectangle the
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
            // The whole tile, no wider than 5:6 so a quarter keeps about four
            // fifths of a cover's height, and no larger than four small
            // thumbnails so a full grid decodes only small files.
            let w = r.w.min(r.h * 5 / 6).min(2 * covers::SMALL_WIDTH as i32).max(1);
            let h = r.h.min(2 * covers::SMALL_HEIGHT as i32);
            let area = Rect::new(r.x + (r.w - w) / 2, r.y, w, h);
            // No gaps; covers are cropped to fill their slots.
            let (left, top) = (w / 2, h / 2);
            let (right, bottom) = (w - left, h - top);
            let (x2, y2) = (area.x + left, area.y + top);
            let slots = match n {
                1 => vec![area],
                2 => vec![Rect::new(area.x, area.y, left, h), Rect::new(x2, area.y, right, h)],
                // The first book takes the tall left half.
                3 => vec![
                    Rect::new(area.x, area.y, left, h),
                    Rect::new(x2, area.y, right, top),
                    Rect::new(x2, y2, right, bottom),
                ],
                _ => vec![
                    Rect::new(area.x, area.y, left, top),
                    Rect::new(x2, area.y, right, top),
                    Rect::new(area.x, y2, left, bottom),
                    Rect::new(x2, y2, right, bottom),
                ],
            };
            let covers = slots.into_iter().enumerate().map(|(i, s)| (i, s, Fit::Cover)).collect();
            Layout { covers, front: area, tab: None, bounds: area }
        }
        FolderPreview::Stack => {
            // Same-size covers, each shifted up and right behind the one in front.
            // 1- and 2-book stacks pin their front cover to the bottom-left baseline
            // so every front cover and badge aligns across the row.
            let d = t.mm(2.0);
            let w = (r.w - 2 * d).min((r.h - 2 * d) * 2 / 3).max(1);
            let h = w * 3 / 2;
            let x = r.x;
            let front_y = r.bottom() - h;
            let bounds = Rect::new(x, front_y - 2 * d, w + 2 * d, h + 2 * d);
            let at = |k: usize| Rect::new(x + k as i32 * d, front_y - k as i32 * d, w, h);
            let covers = (0..n).rev().map(|k| (k, at(k), if k == 0 { Fit::Cover } else { Fit::Behind(d) })).collect();
            Layout { covers, front: at(0), tab: None, bounds }
        }
        FolderPreview::Latest => {
            let tab_h = t.mm(2.0);
            let w = r.w.min((r.h - tab_h) * 2 / 3).max(1);
            let h = w * 3 / 2;
            let cover = Rect::new(r.x, r.bottom() - h, w, h);
            Layout {
                covers: vec![(0, cover, Fit::Cover)],
                front: cover,
                tab: Some(Rect::new(cover.x, cover.y - tab_h, w * 2 / 5, tab_h)),
                bounds: Rect::new(cover.x, cover.y - tab_h, w, h + tab_h),
            }
        }
    })
}

/// Drawn preview geometry: `bounds` for the refresh hint, `front` for the
/// top-left selection badge (matching the cover that holds the count badge).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Drawn {
    pub bounds: Rect,
    pub front: Rect,
}

/// Draws a folder's preview of `books` in tile `r`, with a badge counting
/// the folder's books. Returns the drawn bounds and front cover, or None when
/// the tile should show the folder icon instead.
pub(super) fn draw(
    cx: &mut DrawCx,
    mode: FolderPreview,
    books: &[&Book],
    count: usize,
    r: Rect,
    store: &Store,
    cache: &mut Thumbnails,
) -> Option<Drawn> {
    let t = cx.theme;
    let layout = layout(mode, books.len(), r, t)?;
    let outline = cover_outline(t) as f32;
    for &(i, rect, fit) in &layout.covers {
        if let Some(cover) = cache.load(store, books[i], rect.w, rect.h, fit) {
            cover.blit(cx.canvas, rect);
            // Stack and Covers covers get a crisp 2 px outline; Latest draws a
            // folder frame below.
            if matches!(mode, FolderPreview::Stack | FolderPreview::Covers) {
                cx.canvas.stroke_rounded_rect(rect, 0.0, outline, INK);
            }
        } else {
            cx.fill(rect, PLACEHOLDER);
            cx.canvas.stroke_rounded_rect(rect, 0.0, outline, INK3);
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
    Some(Drawn { bounds: layout.bounds, front: layout.front })
}

/// Bottom-right count pill bounds inside `front`, given the rendered count width.
pub(super) fn count_badge_rect(front: Rect, text_w: i32, t: &Theme) -> Rect {
    let h = t.mm(4.5);
    let w = (text_w + t.mm(2.5)).max(h).min(front.w);
    let inset = t.mm(1.0);
    Rect::new(front.right() - inset - w, front.bottom() - inset - h, w, h)
}

/// A small white pill with the folder's book count, in the bottom-right
/// corner of `front`: a single cover would otherwise read as a book.
fn draw_badge(cx: &mut DrawCx, count: usize, front: Rect) {
    let t = cx.theme;
    let text = count.to_string();
    let style = t.caption.weight(Weight::SemiBold).gray(INK);
    let badge = count_badge_rect(front, cx.text.width(&text, style).ceil() as i32, t);
    let radius = badge.h as f32 / 2.0;
    cx.canvas.fill_rounded_rect(badge, radius, PAPER);
    cx.canvas.stroke_rounded_rect(badge, radius, t.border as f32, INK);
    cx.line(&text, style, badge, HAlign::Center);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tile() -> (Theme, Rect) {
        // A portrait 4 × 3 Library folder tile on the Kindle (1272 × 1696).
        (Theme::for_dpi(300), Rect::new(59, 344, 258, 355))
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
    fn covers_fill_the_tile_in_a_two_by_two_grid_without_gaps() {
        let (t, r) = tile();
        let l = layout(FolderPreview::Covers, 4, r, &t).unwrap();
        assert!(r.contains_rect(&l.bounds));
        assert_eq!((l.bounds.w, l.bounds.h), (r.w, r.h), "the whole portrait tile");
        assert_eq!(l.front, l.bounds);
        assert_eq!(l.covers.iter().map(|c| c.0).collect::<Vec<_>>(), [0, 1, 2, 3]);
        let slots: Vec<_> = l.covers.iter().map(|c| c.1).collect();
        // Reading order, left to right then down; the quarters meet exactly.
        assert_eq!((slots[0].x, slots[0].y), (l.bounds.x, l.bounds.y));
        assert_eq!((slots[1].x, slots[1].y), (slots[0].right(), slots[0].y));
        assert_eq!((slots[2].x, slots[2].y), (slots[0].x, slots[0].bottom()));
        assert_eq!((slots[3].x, slots[3].y), (slots[1].x, slots[2].y));
        assert_eq!((slots[3].right(), slots[3].bottom()), (l.bounds.right(), l.bounds.bottom()));
        assert_eq!(slots.iter().map(|s| s.w as i64 * s.h as i64).sum::<i64>(), l.bounds.w as i64 * l.bounds.h as i64);
        // Small thumbnails cover every quarter.
        assert!(slots.iter().all(|s| s.w as u32 <= covers::SMALL_WIDTH && s.h as u32 <= covers::SMALL_HEIGHT));
        assert!(l.covers.iter().all(|c| c.2 == Fit::Cover));
        // More books than slots draw only four.
        assert_eq!(layout(FolderPreview::Covers, 9, r, &t).unwrap(), l);
    }

    #[test]
    fn covers_grid_keeps_most_of_each_cover_and_small_files_in_landscape() {
        // Landscape tiles are wider than tall: the grid narrows instead of
        // cropping covers to squares, and stays within four small thumbnails.
        let (t, r) = (Theme::for_dpi(300), Rect::new(59, 609, 363, 402));
        let l = layout(FolderPreview::Covers, 4, r, &t).unwrap();
        assert!(r.contains_rect(&l.bounds) && l.bounds.w < r.w);
        assert!((l.bounds.center().x - r.center().x).abs() <= 1);
        for (_, s, _) in &l.covers {
            assert!(s.w * 6 <= s.h * 5, "{s:?} no wider than 5:6");
            assert!(s.w as u32 <= covers::SMALL_WIDTH && s.h as u32 <= covers::SMALL_HEIGHT);
        }
    }

    #[test]
    fn short_folders_fill_the_same_grid() {
        let (t, r) = tile();
        let full = layout(FolderPreview::Covers, 4, r, &t).unwrap();
        let area = full.bounds;
        let q = |i: usize| full.covers[i].1;
        // One book: the whole grid.
        let one = layout(FolderPreview::Covers, 1, r, &t).unwrap();
        assert_eq!(one.covers, [(0, area, Fit::Cover)]);
        // Two: tall halves side by side.
        let two = layout(FolderPreview::Covers, 2, r, &t).unwrap();
        let halves: Vec<_> = two.covers.iter().map(|c| (c.0, c.1)).collect();
        assert_eq!(
            halves,
            [(0, Rect::new(area.x, area.y, q(0).w, area.h)), (1, Rect::new(q(1).x, area.y, q(1).w, area.h))]
        );
        // Three: the first book takes the tall left half, the next two the
        // right quarters.
        let three = layout(FolderPreview::Covers, 3, r, &t).unwrap();
        let slots: Vec<_> = three.covers.iter().map(|c| (c.0, c.1)).collect();
        assert_eq!(slots, [(0, halves[0].1), (1, q(1)), (2, q(3))]);
        for l in [&one, &two, &three] {
            assert_eq!((l.front, l.bounds), (area, area), "the badge keeps its corner");
        }
    }

    #[test]
    fn stack_draws_same_size_covers_back_to_front_pinned_bottom_left() {
        let (t, r) = tile();
        let l = layout(FolderPreview::Stack, 4, r, &t).unwrap();
        assert!(r.contains_rect(&l.bounds));
        assert_eq!(l.covers.iter().map(|c| c.0).collect::<Vec<_>>(), [2, 1, 0], "the first book is drawn last");
        let [back, middle, front] = [l.covers[0].1, l.covers[1].1, l.covers[2].1];
        assert!([back, middle].iter().all(|c| (c.w, c.h) == (front.w, front.h)));
        let d = t.mm(2.0);
        assert_eq!((middle.x - front.x, front.y - middle.y), (d, d));
        assert_eq!((back.x - middle.x, middle.y - back.y), (d, d));
        assert_eq!(l.front, front);
        assert_eq!((front.x, front.bottom()), (r.x, r.bottom()), "front cover pins to bottom-left baseline");
        assert!(l.covers.iter().all(|c| l.bounds.contains_rect(&c.1)));
        // The front cover is larger than a small thumbnail and decodes the
        // full one; the hidden ones decode small files.
        assert!(front.w as u32 > covers::SMALL_WIDTH);
        assert_eq!(l.covers.iter().map(|c| c.2).collect::<Vec<_>>(), [Fit::Behind(d), Fit::Behind(d), Fit::Cover]);
        // 1- and 2-book stacks keep their front cover pinned to the exact same bottom-left baseline.
        let two = layout(FolderPreview::Stack, 2, r, &t).unwrap();
        assert_eq!(two.covers.len(), 2);
        assert_eq!(two.bounds, l.bounds);
        assert_eq!(two.front, front);
        assert_eq!(two.covers[0].1, middle);
        let one = layout(FolderPreview::Stack, 1, r, &t).unwrap();
        assert_eq!(one.covers.len(), 1);
        assert_eq!(one.bounds, l.bounds);
        assert_eq!(one.front, front);
        assert_eq!(one.covers[0].1, front);
    }

    #[test]
    fn stack_draws_two_pixel_ink_outline_around_loaded_covers() -> anyhow::Result<()> {
        let root = std::env::temp_dir().join(format!("verso-folder-outline-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = Store::at(root.clone());
        std::fs::create_dir_all(store.cover_path("x").parent().unwrap())?;
        let mut b = crate::library::selection_tests::book(0);
        b.cover = Cover::Ready("c0".into());
        image::GrayImage::from_pixel(covers::WIDTH, covers::HEIGHT, image::Luma([200])).save(store.cover_path("c0"))?;
        let (t, r) = tile();
        let l = layout(FolderPreview::Stack, 1, r, &t).unwrap();
        let mut canvas = crate::gfx::Canvas::new(1272, 1696);
        canvas.fill(PAPER);
        let mut text = crate::gfx::TextRenderer::new();
        let mut icons = crate::gfx::IconRenderer::default();
        let mut cache = Thumbnails::default();
        draw(
            &mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut Vec::new()),
            FolderPreview::Stack,
            &[&b],
            1,
            r,
            &store,
            &mut cache,
        );
        let px = |x: i32, y: i32| canvas.gray_data()[y as usize * 1272 + x as usize];
        assert_eq!(px(l.front.x, l.front.y), INK);
        assert_eq!(px(l.front.x + 1, l.front.y + 1), INK);
        assert_eq!(px(l.front.x + 5, l.front.y + 5), 200);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn latest_book_draws_one_cover_under_a_folder_tab() {
        let (t, r) = tile();
        let l = layout(FolderPreview::Latest, 3, r, &t).unwrap();
        assert_eq!(l.covers.len(), 1);
        let cover = l.covers[0].1;
        assert_eq!(l.front, cover);
        assert_eq!((cover.x, cover.bottom()), (r.x, r.bottom()), "left- and bottom-aligned with book covers");
        let tab = l.tab.unwrap();
        assert_eq!((tab.x, tab.bottom()), (cover.x, cover.y));
        assert!(tab.w < cover.w / 2);
        assert!(r.contains_rect(&l.bounds) && l.bounds.contains_rect(&cover) && l.bounds.contains_rect(&tab));
        assert_eq!((cover.w * 3 / 2), cover.h);
    }
}
