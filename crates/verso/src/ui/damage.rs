//! Finding what changed on screen, and how to refresh it.
//!
//! Screens redraw everything into the canvas; this module compares the new
//! frame with the previous one in small tiles, groups the changed tiles into
//! rectangles, and gives each rectangle a refresh mode:
//! - widgets *hint* modes for areas they know about (a pressed button says
//!   "this rect, `Fast`"); a changed tile takes the strongest hint it overlaps,
//!   or the screen's default if nobody hinted it.
//! - screens can also *force* a refresh of an area even if nothing changed
//!   there (a periodic full flash, a redraw after wake).

use crate::geom::Rect;
use crate::gfx::Canvas;
use crate::platform::{RefreshMode, Update};

/// Tile size in pixels. Small enough not to refresh much more than changed,
/// big enough to keep the comparison and the rectangle count cheap.
pub const TILE: i32 = 16;

/// Which tiles changed between two frames.
pub struct Tiles {
    pub cols: i32,
    pub rows: i32,
    pub dirty: Vec<bool>,
}

impl Tiles {
    fn tile_rect(&self, col: i32, row: i32) -> Rect {
        Rect::new(col * TILE, row * TILE, TILE, TILE)
    }

    #[cfg(test)]
    pub fn any(&self) -> bool {
        self.dirty.iter().any(|&d| d)
    }
}

/// Keeps a copy of the last frame sent to the screen.
pub struct FrameDiff {
    width: i32,
    height: i32,
    prev: Vec<u8>,
}

impl FrameDiff {
    pub fn new(canvas: &Canvas) -> Self {
        let mut diff = Self {
            width: canvas.width() as i32,
            height: canvas.height() as i32,
            prev: vec![0; canvas.width() as usize * canvas.height() as usize],
        };
        diff.commit_full(canvas);
        diff
    }

    /// Synchronizes the baseline after a known full-screen update, without
    /// comparing tiles. Store exactly the grayscale pixels sent to the panel.
    pub fn commit_full(&mut self, canvas: &Canvas) {
        canvas.copy_gray(canvas.bounds(), &mut self.prev, self.width as usize);
    }

    /// Compares `canvas` with the last committed frame.
    pub fn changed(&mut self, canvas: &Canvas) -> Tiles {
        let gray = canvas.gray_data();
        let rgba = gray.is_none().then(|| canvas.data());
        let stride = self.width as usize;
        // UI conversion needs only one row, never another screen-sized frame.
        let mut converted = if rgba.is_some() { vec![0; stride] } else { Vec::new() };
        let (cols, rows) = ((self.width + TILE - 1) / TILE, (self.height + TILE - 1) / TILE);
        let mut dirty = vec![false; (cols * rows) as usize];
        for (row, tiles) in dirty.chunks_mut(cols as usize).enumerate() {
            let y0 = row * TILE as usize;
            let y1 = (y0 + TILE as usize).min(self.height as usize);
            let band = y0 * stride..y1 * stride;
            if gray.is_some_and(|cur| cur[band.clone()] == self.prev[band]) {
                continue;
            }
            for y in y0..y1 {
                let line = y * stride..(y + 1) * stride;
                let a = if let Some(gray) = gray {
                    &gray[line.clone()]
                } else {
                    let rgba = rgba.as_ref().unwrap();
                    for (out, px) in
                        converted.iter_mut().zip(rgba[y * stride * 4..(y + 1) * stride * 4].as_chunks::<4>().0)
                    {
                        *out = crate::gfx::luma(px[0], px[1], px[2]);
                    }
                    &converted
                };
                let b = &self.prev[line];
                if a == b {
                    continue;
                }
                for (col, d) in tiles.iter_mut().enumerate().filter(|(_, d)| !**d) {
                    let x0 = col * TILE as usize;
                    let x1 = (x0 + TILE as usize).min(stride);
                    *d = a[x0..x1] != b[x0..x1];
                }
            }
        }
        Tiles { cols, rows, dirty }
    }

    /// Remembers the changed tiles of `canvas` as what's now on screen.
    pub fn commit(&mut self, canvas: &Canvas, tiles: &Tiles) {
        for row in 0..tiles.rows {
            for col in 0..tiles.cols {
                if tiles.dirty[(row * tiles.cols + col) as usize] {
                    canvas.copy_gray(tiles.tile_rect(col, row), &mut self.prev, self.width as usize);
                }
            }
        }
    }
}

/// Turns changed tiles plus hints and forced refreshes into the list of
/// screen updates to send.
pub fn plan(tiles: &Tiles, bounds: Rect, hints: &[Update], default: RefreshMode, forced: &[Update]) -> Vec<Update> {
    // Each changed tile's mode.
    let mode_of = |col: i32, row: i32| {
        let r = tiles.tile_rect(col, row);
        hints.iter().filter(|h| h.rect.intersect(&r).is_some()).map(|h| h.mode).max().unwrap_or(default)
    };
    let modes: Vec<Option<RefreshMode>> = (0..tiles.rows)
        .flat_map(|row| (0..tiles.cols).map(move |col| (col, row)))
        .map(|(col, row)| tiles.dirty[(row * tiles.cols + col) as usize].then(|| mode_of(col, row)))
        .collect();

    // Runs of same-mode tiles in each row, grown downwards while the next
    // row has a run with the same columns and mode.
    let mut rects: Vec<(Rect, RefreshMode)> = Vec::new();
    let mut open: Vec<usize> = Vec::new(); // indexes into `rects` that ended on the previous row
    for row in 0..tiles.rows {
        let mut next_open = Vec::new();
        let mut col = 0;
        while col < tiles.cols {
            let Some(mode) = modes[(row * tiles.cols + col) as usize] else {
                col += 1;
                continue;
            };
            let start = col;
            while col < tiles.cols && modes[(row * tiles.cols + col) as usize] == Some(mode) {
                col += 1;
            }
            let run = Rect::new(start * TILE, row * TILE, (col - start) * TILE, TILE);
            let grown = open.iter().copied().find(|&i| {
                let (r, m) = rects[i];
                m == mode && r.x == run.x && r.w == run.w && r.bottom() == run.y
            });
            match grown {
                Some(i) => {
                    rects[i].0.h += TILE;
                    next_open.push(i);
                }
                None => {
                    rects.push((run, mode));
                    next_open.push(rects.len() - 1);
                }
            }
        }
        open = next_open;
    }

    merge_touching(&mut rects);

    let mut updates: Vec<Update> = rects
        .into_iter()
        .filter_map(|(r, mode)| r.intersect(&bounds).map(|rect| Update { rect, mode }))
        // Skip what a forced refresh of at least the same strength covers anyway.
        .filter(|u| !forced.iter().any(|f| f.mode >= u.mode && contains(&f.rect, &u.rect)))
        .collect();
    updates.extend(forced.iter().filter_map(|f| f.rect.intersect(&bounds).map(|rect| Update { rect, mode: f.mode })));
    updates
}

/// Merges same-mode rectangles that overlap or touch into their bounding
/// box. Fewer, larger refreshes are cheaper on e-ink than many small ones.
fn merge_touching(rects: &mut Vec<(Rect, RefreshMode)>) {
    loop {
        let mut merged = false;
        'outer: for i in 0..rects.len() {
            for j in i + 1..rects.len() {
                let ((a, ma), (b, mb)) = (rects[i], rects[j]);
                if ma == mb && a.inset(-1).intersect(&b).is_some() {
                    rects[i].0 = a.union(&b);
                    rects.swap_remove(j);
                    merged = true;
                    break 'outer;
                }
            }
        }
        if !merged {
            return;
        }
    }
}

fn contains(outer: &Rect, inner: &Rect) -> bool {
    inner.x >= outer.x && inner.y >= outer.y && inner.right() <= outer.right() && inner.bottom() <= outer.bottom()
}

#[cfg(test)]
mod tests {
    use super::*;
    use RefreshMode::*;

    const W: u32 = 160;
    const H: u32 = 96;

    #[test]
    fn bypassed_pages_synchronize_the_baseline_before_overlay_comparison() {
        let (mut canvas, mut diff) = setup();
        for value in [17, 42] {
            canvas.set_gray(&vec![value; (W * H) as usize]);
            canvas.fill_rect(Rect::new(0, 80, W as i32, 16), 255);
            diff.commit_full(&canvas);
        }
        canvas.materialize();
        assert!(!diff.changed(&canvas).any());
        canvas.fill_rect(Rect::new(20, 20, 3, 3), 0);
        let tiles = diff.changed(&canvas);
        assert_eq!(plan(&tiles, bounds(), &[], Ui, &[]), vec![Update { rect: Rect::new(16, 16, 16, 16), mode: Ui }]);
        diff.commit(&canvas, &tiles);
        canvas.set_gray(&vec![42; (W * H) as usize]);
        canvas.fill_rect(Rect::new(0, 80, W as i32, 16), 255);
        canvas.materialize();
        assert_eq!(
            plan(&diff.changed(&canvas), bounds(), &[], Ui, &[]),
            vec![Update { rect: Rect::new(16, 16, 16, 16), mode: Ui }]
        );
        diff.commit_full(&canvas);
        assert!(!diff.changed(&canvas).any());
    }

    fn setup() -> (Canvas, FrameDiff) {
        let canvas = Canvas::new(W, H);
        let diff = FrameDiff::new(&canvas);
        (canvas, diff)
    }

    fn bounds() -> Rect {
        Rect::new(0, 0, W as i32, H as i32)
    }

    #[test]
    fn nothing_changed_means_no_updates() {
        let (canvas, mut diff) = setup();
        let tiles = diff.changed(&canvas);
        assert!(!tiles.any());
        assert!(plan(&tiles, bounds(), &[], Ui, &[]).is_empty());
    }

    #[test]
    fn a_change_becomes_one_tile_aligned_rect() {
        let (mut canvas, mut diff) = setup();
        canvas.fill_rect(Rect::new(20, 20, 30, 5), 0);
        let tiles = diff.changed(&canvas);
        let updates = plan(&tiles, bounds(), &[], Ui, &[]);
        assert_eq!(updates, vec![Update { rect: Rect::new(16, 16, 48, 16), mode: Ui }]);
    }

    #[test]
    fn commit_makes_the_next_frame_clean() {
        let (mut canvas, mut diff) = setup();
        canvas.fill_rect(Rect::new(0, 0, 10, 10), 0);
        let tiles = diff.changed(&canvas);
        diff.commit(&canvas, &tiles);
        assert!(!diff.changed(&canvas).any());
        canvas.fill_rect(Rect::new(0, 0, 10, 10), 255);
        assert!(diff.changed(&canvas).any(), "changing it back is a change too");
    }

    #[test]
    fn single_pixels_mark_their_tile_even_at_ragged_edges() {
        // 70 × 37 doesn't divide into 16 px tiles: the last column and row are partial.
        let mut canvas = Canvas::new(70, 37);
        let mut diff = FrameDiff::new(&canvas);
        let dots = [(0, 0), (15, 15), (16, 0), (69, 36), (33, 20), (64, 33)];
        for &(x, y) in &dots {
            canvas.fill_rect(Rect::new(x, y, 1, 1), 0);
        }
        let tiles = diff.changed(&canvas);
        assert_eq!((tiles.cols, tiles.rows), (5, 3));
        for row in 0..tiles.rows {
            for col in 0..tiles.cols {
                let expected = dots.iter().any(|&(x, y)| x / TILE == col && y / TILE == row);
                assert_eq!(tiles.dirty[(row * tiles.cols + col) as usize], expected, "tile {col},{row}");
            }
        }
    }

    #[test]
    fn separate_changes_stay_separate() {
        let (mut canvas, mut diff) = setup();
        canvas.fill_rect(Rect::new(0, 0, 4, 4), 0);
        canvas.fill_rect(Rect::new(100, 60, 4, 4), 0);
        let updates = plan(&diff.changed(&canvas), bounds(), &[], Ui, &[]);
        assert_eq!(updates.len(), 2);
    }

    #[test]
    fn hints_set_the_mode_and_split_the_area() {
        let (mut canvas, mut diff) = setup();
        // One changed band across the screen; a button on its left half hints Fast.
        canvas.fill_rect(Rect::new(0, 0, W as i32, 10), 0);
        let hints = [Update { rect: Rect::new(0, 0, 64, 16), mode: Fast }];
        let mut updates = plan(&diff.changed(&canvas), bounds(), &hints, Ui, &[]);
        updates.sort_by_key(|u| u.rect.x);
        assert_eq!(
            updates,
            vec![
                Update { rect: Rect::new(0, 0, 64, 16), mode: Fast },
                Update { rect: Rect::new(64, 0, 96, 16), mode: Ui },
            ]
        );
    }

    #[test]
    fn the_strongest_hint_wins() {
        let (mut canvas, mut diff) = setup();
        canvas.fill_rect(Rect::new(0, 0, 8, 8), 0);
        let hints = [Update { rect: Rect::new(0, 0, 8, 8), mode: Fast }, Update { rect: bounds(), mode: Page }];
        assert_eq!(plan(&diff.changed(&canvas), bounds(), &hints, Ui, &[])[0].mode, Page);
    }

    #[test]
    fn forced_refreshes_are_sent_and_cover_weaker_changes() {
        let (mut canvas, mut diff) = setup();
        canvas.fill_rect(Rect::new(0, 0, 8, 8), 0);
        let forced = [Update { rect: bounds(), mode: Full }];
        assert_eq!(plan(&diff.changed(&canvas), bounds(), &[], Ui, &forced), forced.to_vec());
        // A forced refresh with nothing changed still goes out.
        let clean = Canvas::new(W, H);
        let forced = [Update { rect: Rect::new(0, 0, 10, 10), mode: Ui }];
        assert_eq!(plan(&diff.changed(&clean), bounds(), &[], Ui, &forced), forced.to_vec());
    }

    #[test]
    fn rects_are_clipped_to_the_screen() {
        // 100 px tall: the last tile row is only partly on screen.
        let mut canvas = Canvas::new(40, 100);
        let mut diff = FrameDiff::new(&canvas);
        canvas.fill_rect(Rect::new(0, 98, 2, 2), 0);
        let updates = plan(&diff.changed(&canvas), Rect::new(0, 0, 40, 100), &[], Ui, &[]);
        assert_eq!(updates, vec![Update { rect: Rect::new(0, 96, 16, 4), mode: Ui }]);
    }

    #[test]
    fn an_l_shape_merges_into_its_bounding_box() {
        let (mut canvas, mut diff) = setup();
        canvas.fill_rect(Rect::new(0, 0, 64, 16), 0);
        canvas.fill_rect(Rect::new(0, 16, 16, 32), 0);
        let updates = plan(&diff.changed(&canvas), bounds(), &[], Ui, &[]);
        assert_eq!(updates, vec![Update { rect: Rect::new(0, 0, 64, 48), mode: Ui }]);
    }
}
