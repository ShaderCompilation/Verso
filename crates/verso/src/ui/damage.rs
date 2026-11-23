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

/// Keeps a copy of the last frame sent to the screen, stored like the
/// canvas: 1 byte per pixel on a gray canvas, 4 on a color one.
pub struct FrameDiff {
    width: i32,
    height: i32,
    /// Bytes per pixel of `prev` and the canvas.
    bpp: usize,
    prev: Vec<u8>,
    /// True when the last presented frame bypassed tile diffing and `canvas`
    /// itself still holds that baseline, avoiding a full-buffer copy into
    /// `prev` across back-to-back full-screen frames (e.g. page turns).
    canvas_is_baseline: bool,
    #[cfg(test)]
    full_copies: usize,
}

impl FrameDiff {
    pub fn new(canvas: &Canvas) -> Self {
        let mut diff = Self {
            width: canvas.width() as i32,
            height: canvas.height() as i32,
            bpp: canvas.bytes_per_pixel(),
            prev: vec![0; canvas.width() as usize * canvas.height() as usize * canvas.bytes_per_pixel()],
            canvas_is_baseline: false,
            #[cfg(test)]
            full_copies: 0,
        };
        diff.commit_full(canvas);
        diff
    }

    /// Synchronizes the baseline after a known full-screen update, without
    /// comparing tiles. Store exactly the pixels sent to the panel.
    pub fn commit_full(&mut self, canvas: &Canvas) {
        debug_assert_eq!(canvas.bytes_per_pixel(), self.bpp, "the canvas changed mode");
        canvas.copy_raw(canvas.bounds(), &mut self.prev);
        self.canvas_is_baseline = false;
        #[cfg(test)]
        {
            self.full_copies += 1;
        }
    }

    /// Records that `canvas` now holds the last presented full-screen frame,
    /// deferring the copy into `prev` until a partial frame needs to diff
    /// against it.
    pub fn mark_canvas_as_baseline(&mut self) {
        self.canvas_is_baseline = true;
    }

    /// Materializes a deferred baseline into `prev` before a partial frame
    /// mutates `canvas`. No-op when `prev` is already current.
    pub fn sync_baseline(&mut self, canvas: &Canvas) {
        if self.canvas_is_baseline {
            self.commit_full(canvas);
        }
    }

    #[cfg(test)]
    pub fn is_baseline_deferred(&self) -> bool {
        self.canvas_is_baseline
    }

    #[cfg(test)]
    pub fn full_copies(&self) -> usize {
        self.full_copies
    }

    /// Compares `canvas` with the last committed frame.
    #[cfg(test)]
    pub fn changed(&mut self, canvas: &Canvas) -> Tiles {
        self.changed_outside(canvas, &[])
    }

    /// Compares `canvas` with the last committed frame everywhere except
    /// inside `skip`, and commits `skip` unconditionally. For areas that are
    /// sent to the panel anyway (a forced `Full` grid flash), so comparing
    /// them is wasted reading. Tiles straddling a skipped edge are still
    /// marked if their outside part changed.
    pub fn changed_outside(&mut self, canvas: &Canvas, skip: &[Rect]) -> Tiles {
        let bounds = canvas.bounds();
        let skip: Vec<Rect> = skip.iter().filter_map(|r| r.intersect(&bounds)).collect();
        self.compare(canvas, &skip)
    }

    /// Tile comparison outside `skip` (clipped to the canvas), copying the
    /// skipped pixels into the baseline in the same pass while their rows
    /// are in cache.
    fn compare(&mut self, canvas: &Canvas, skip: &[Rect]) -> Tiles {
        debug_assert_eq!(canvas.bytes_per_pixel(), self.bpp, "the canvas changed mode");
        debug_assert!(!self.canvas_is_baseline, "sync_baseline must be called before mutating canvas for a diff");
        let cur = canvas.raw();
        // Spans below are in pixels; `bpp` turns them into bytes (1 on a
        // gray canvas, so its loop is the same as ever).
        let bpp = self.bpp;
        let width = self.width as usize;
        let stride = width * bpp;
        let (cols, rows) = ((self.width + TILE - 1) / TILE, (self.height + TILE - 1) / TILE);
        let mut dirty = vec![false; (cols * rows) as usize];
        for (row, tiles) in dirty.chunks_mut(cols as usize).enumerate() {
            let y0 = row * TILE as usize;
            let y1 = (y0 + TILE as usize).min(self.height as usize);
            let band_skipped = skip.iter().any(|r| (r.y as usize) < y1 && r.bottom() as usize > y0);
            if !band_skipped {
                let band = y0 * stride..y1 * stride;
                if same(&cur[band.clone()], &self.prev[band]) {
                    continue;
                }
            }
            // The parts of a line outside every skipped rect, and the skipped
            // ones (merged). Recomputed per line only in bands where a skipped
            // rect starts or ends.
            let spans_at = |y: usize| {
                let mut taken: Vec<(usize, usize)> = skip
                    .iter()
                    .filter(|r| (r.y as usize) <= y && y < r.bottom() as usize)
                    .map(|r| (r.x as usize, r.right() as usize))
                    .collect();
                taken.sort_unstable();
                let (mut outside, mut merged, mut x) = (Vec::new(), Vec::<(usize, usize)>::new(), 0);
                for (s, e) in taken {
                    if s > x {
                        outside.push((x, s));
                    }
                    match merged.last_mut() {
                        Some(last) if s <= last.1 => last.1 = last.1.max(e),
                        _ => merged.push((s, e)),
                    }
                    x = x.max(e);
                }
                if x < width {
                    outside.push((x, width));
                }
                (outside, merged)
            };
            let uniform = skip.iter().all(|r| {
                let (top, bottom) = (r.y as usize, r.bottom() as usize);
                bottom <= y0 || top >= y1 || (top <= y0 && bottom >= y1)
            });
            let band_spans = if uniform { spans_at(y0) } else { Default::default() };
            for y in y0..y1 {
                let line = y * stride..(y + 1) * stride;
                let a = &cur[line.clone()];
                let b = &mut self.prev[line];
                let line_spans;
                let (outside, taken) = if uniform {
                    &band_spans
                } else {
                    line_spans = spans_at(y);
                    &line_spans
                };
                for &(s, e) in taken {
                    b[s * bpp..e * bpp].copy_from_slice(&a[s * bpp..e * bpp]);
                }
                for &(s, e) in outside {
                    if same(&a[s * bpp..e * bpp], &b[s * bpp..e * bpp]) {
                        continue;
                    }
                    let first = s / TILE as usize;
                    for (col, d) in tiles[first..=(e - 1) / TILE as usize].iter_mut().enumerate() {
                        if !*d {
                            let x0 = ((first + col) * TILE as usize).max(s);
                            let x1 = ((first + col + 1) * TILE as usize).min(e);
                            *d = a[x0 * bpp..x1 * bpp] != b[x0 * bpp..x1 * bpp];
                        }
                    }
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
                    canvas.copy_raw(tiles.tile_rect(col, row), &mut self.prev);
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
        .filter(|u| !forced.iter().any(|f| f.mode >= u.mode && f.rect.contains_rect(&u.rect)))
        .collect();
    // Clip forced refreshes and drop any covered by a changed update or another
    // forced refresh of at least the same strength (e.g. a Fast→Ui cleanup
    // inside a changed area or inside a forced Full rect).
    let clipped_forced: Vec<Update> =
        forced.iter().filter_map(|f| f.rect.intersect(&bounds).map(|rect| Update { rect, mode: f.mode })).collect();
    for (i, &u) in clipped_forced.iter().enumerate() {
        let covered_by_changed = updates.iter().any(|other| other.mode >= u.mode && other.rect.contains_rect(&u.rect));
        let covered_by_forced = clipped_forced.iter().enumerate().any(|(j, other)| {
            j != i
                && other.mode >= u.mode
                && other.rect.contains_rect(&u.rect)
                && (other.mode > u.mode || other.rect != u.rect || j < i)
        });
        if !covered_by_changed && !covered_by_forced {
            updates.push(u);
        }
    }
    // Send non-flashing updates before flashing ones so Display::present never
    // blocks on a flash it just started in the same frame.
    updates.sort_by_key(|u| u.mode == RefreshMode::Full);
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

/// `a == b` for equal-length rows of pixels, in 64-byte blocks the
/// compiler turns into NEON on the Kindle. glibc's ARM `memcmp` is
/// several times slower on the long, mostly equal rows tile diffing reads.
fn same(a: &[u8], b: &[u8]) -> bool {
    debug_assert_eq!(a.len(), b.len());
    let ((ca, ra), (cb, rb)) = (a.as_chunks::<64>(), b.as_chunks::<64>());
    for (x, y) in ca.iter().zip(cb) {
        let mut diff = 0u8;
        for i in 0..64 {
            diff |= x[i] ^ y[i];
        }
        if diff != 0 {
            return false;
        }
    }
    ra == rb
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::{Color, ColorMode};
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
        assert!(!diff.changed(&canvas).any());
        canvas.fill_rect(Rect::new(20, 20, 3, 3), 0);
        let tiles = diff.changed(&canvas);
        assert_eq!(plan(&tiles, bounds(), &[], Ui, &[]), vec![Update { rect: Rect::new(16, 16, 16, 16), mode: Ui }]);
        diff.commit(&canvas, &tiles);
        canvas.set_gray(&vec![42; (W * H) as usize]);
        canvas.fill_rect(Rect::new(0, 80, W as i32, 16), 255);
        assert_eq!(
            plan(&diff.changed(&canvas), bounds(), &[], Ui, &[]),
            vec![Update { rect: Rect::new(16, 16, 16, 16), mode: Ui }]
        );
        diff.commit_full(&canvas);
        assert!(!diff.changed(&canvas).any());
    }

    #[test]
    fn deferred_baseline_skips_copies_across_full_frames_and_diffs_accurately_after() {
        for mode in [ColorMode::Gray8, ColorMode::Color32] {
            let mut canvas = Canvas::with_mode(W, H, mode);
            let mut diff = FrameDiff::new(&canvas);
            assert_eq!(diff.full_copies(), 1);
            // Back-to-back full-screen frames copy `prev` 0 times.
            for value in [17u8, 42, 88] {
                canvas.set_gray(&vec![value; (W * H) as usize]);
                diff.mark_canvas_as_baseline();
            }
            assert!(diff.is_baseline_deferred());
            assert_eq!(diff.full_copies(), 1, "no copies during full-screen bypasses");
            // Before a partial frame mutates `canvas`, sync copies once.
            diff.sync_baseline(&canvas);
            assert!(!diff.is_baseline_deferred());
            assert_eq!(diff.full_copies(), 2);
            // A second sync is a no-op.
            diff.sync_baseline(&canvas);
            assert_eq!(diff.full_copies(), 2);
            // Mutate one small area and verify the diff finds only that tile against value 88.
            canvas.fill_rect(Rect::new(20, 20, 3, 3), 0);
            let tiles = diff.changed(&canvas);
            assert_eq!(
                plan(&tiles, bounds(), &[], Ui, &[]),
                vec![Update { rect: Rect::new(16, 16, 16, 16), mode: Ui }]
            );
            diff.commit(&canvas, &tiles);
            assert!(!diff.changed(&canvas).any());
        }
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
    fn a_color_canvas_marks_the_same_tiles_and_sees_single_channels() {
        let mut gray = Canvas::new(W, H);
        let mut color = Canvas::with_mode(W, H, ColorMode::Color32);
        let (mut gray_diff, mut color_diff) = (FrameDiff::new(&gray), FrameDiff::new(&color));
        assert_eq!(color_diff.prev.len(), (W * H * 4) as usize);
        for canvas in [&mut gray, &mut color] {
            canvas.fill_rect(Rect::new(20, 20, 30, 5), 0);
            canvas.fill_rect(Rect::new(100, 60, 4, 4), 90);
        }
        let (g, c) = (gray_diff.changed(&gray), color_diff.changed(&color));
        assert_eq!(g.dirty, c.dirty);
        assert_eq!(plan(&c, bounds(), &[], Ui, &[]).len(), 2);
        color_diff.commit(&color, &c);
        gray_diff.commit(&gray, &g);
        assert!(!color_diff.changed(&color).any());
        // Blue only: the same brightness on a gray screen would be nearly
        // the same, but the color screen shows it.
        color.fill_rect_color(Rect::new(70, 40, 1, 1), Color::rgb(255, 255, 0));
        let tiles = color_diff.changed(&color);
        assert_eq!(plan(&tiles, bounds(), &[], Ui, &[]), vec![Update { rect: Rect::new(64, 32, 16, 16), mode: Ui }]);
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

    #[test]
    fn forced_refreshes_drop_covered_weaker_forced_and_order_full_last() {
        let (canvas, mut diff) = setup();
        let grid = Rect::new(10, 20, 120, 60);
        let inside_debt = Update { rect: Rect::new(20, 30, 20, 20), mode: Ui };
        let outside_debt = Update { rect: Rect::new(10, 82, 30, 10), mode: Ui };
        let forced = [Update { rect: grid, mode: Full }, inside_debt, outside_debt];
        let updates = plan(&diff.changed(&canvas), bounds(), &[], Ui, &forced);
        assert_eq!(updates, vec![outside_debt, Update { rect: grid, mode: Full }]);
    }

    #[test]
    fn forced_ui_covered_by_changed_ui_is_not_duplicated() {
        let (mut canvas, mut diff) = setup();
        canvas.fill_rect(Rect::new(0, 0, 64, 32), 0);
        let forced = [Update { rect: Rect::new(0, 0, 32, 32), mode: Ui }];
        let updates = plan(&diff.changed(&canvas), bounds(), &[], Ui, &forced);
        assert_eq!(updates, vec![Update { rect: Rect::new(0, 0, 64, 32), mode: Ui }]);
    }

    #[test]
    fn skipped_rects_compare_as_if_committed_first() {
        // Random frames and skip rects (overlapping, unaligned, off-screen):
        // skipping must mark exactly what committing the rects first and
        // comparing everything marked, and leave the same baseline.
        let mut seed = 0x2545_f491_u32;
        let mut next = |n: u32| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed % n
        };
        for i in 0..400 {
            // Every other frame on a color canvas, where a change can be in one channel.
            let mode = if i % 2 == 0 { ColorMode::Gray8 } else { ColorMode::Color32 };
            let mut canvas = Canvas::with_mode(70, 53, mode);
            let mut skipping = FrameDiff::new(&canvas);
            let mut committing = FrameDiff::new(&canvas);
            for _ in 0..next(6) {
                let r = Rect::new(next(80) as i32 - 5, next(60) as i32 - 5, next(20) as i32 + 1, next(20) as i32 + 1);
                let mut channel = || next(2) as u8 * 255;
                let color = Color::rgb(channel(), 255, channel());
                canvas.fill_rect_color(r, color);
            }
            let skip: Vec<Rect> = (0..next(4))
                .map(|_| Rect::new(next(80) as i32 - 5, next(60) as i32 - 5, next(50) as i32 + 1, next(40) as i32 + 1))
                .collect();
            for &r in &skip {
                if let Some(r) = r.intersect(&canvas.bounds()) {
                    canvas.copy_raw(r, &mut committing.prev);
                }
            }
            let expected = committing.changed(&canvas);
            let tiles = skipping.changed_outside(&canvas, &skip);
            assert_eq!(tiles.dirty, expected.dirty, "skip {skip:?}");
            skipping.commit(&canvas, &tiles);
            committing.commit(&canvas, &expected);
            assert!(skipping.prev == committing.prev);
            assert!(!skipping.changed(&canvas).any());
        }
    }

    /// `cargo test --release -- --ignored --nocapture diff_timing`: a
    /// grid-only library frame on the PW6 canvas.
    #[test]
    #[ignore]
    fn diff_timing() {
        let (w, h) = (1272, 1696);
        let mut canvas = Canvas::new(w, h);
        let grid = Rect::new(59, 609, 1154, 886);
        let mut diff = FrameDiff::new(&canvas);
        let runs = 50;
        let mut time = |skip: bool| {
            let mut spent = std::time::Duration::ZERO;
            for i in 0..runs {
                canvas.fill_rect(grid, (i * 7) as u8);
                canvas.fill_rect(Rect::new(500, 1560, 200, 30), (i * 11) as u8);
                let started = std::time::Instant::now();
                if skip {
                    let tiles = diff.changed_outside(&canvas, &[grid]);
                    diff.commit(&canvas, &tiles);
                } else {
                    canvas.copy_raw(grid, &mut diff.prev);
                    let tiles = diff.changed(&canvas);
                    diff.commit(&canvas, &tiles);
                }
                spent += started.elapsed();
            }
            spent / runs
        };
        let before = time(false);
        let after = time(true);
        let skip = [grid];
        let started = std::time::Instant::now();
        for _ in 0..runs {
            std::hint::black_box(diff.compare(&canvas, &skip));
        }
        let compare = started.elapsed() / runs;
        let started = std::time::Instant::now();
        for _ in 0..runs {
            std::hint::black_box(diff.compare(&canvas, &[]));
        }
        let compare_all = started.elapsed() / runs;
        let tiles = diff.compare(&canvas, &[]);
        let started = std::time::Instant::now();
        for _ in 0..runs {
            diff.commit(&canvas, &tiles);
        }
        let commit = started.elapsed() / runs;
        let started = std::time::Instant::now();
        for _ in 0..runs {
            canvas.copy_raw(grid, &mut diff.prev);
        }
        let copy_grid = started.elapsed() / runs;
        println!(
            "MiB parts: compare outside {compare:?}, compare all {compare_all:?}, clean commit {commit:?}, grid copy {copy_grid:?}"
        );
        println!("commit + compare all: {before:?}, compare outside: {after:?}");
        let n = 1 << 20;
        let (a, mut b) = (vec![1u8; n], vec![1u8; n]);
        let started = std::time::Instant::now();
        for _ in 0..runs {
            b.copy_from_slice(std::hint::black_box(&a));
        }
        let copy = started.elapsed() / runs;
        let started = std::time::Instant::now();
        for _ in 0..runs {
            assert!(std::hint::black_box(&a[..]) == std::hint::black_box(&b[..]));
        }
        let eq = started.elapsed() / runs;
        let started = std::time::Instant::now();
        for _ in 0..runs {
            assert!(super::same(std::hint::black_box(&a[..]), std::hint::black_box(&b[..])));
        }
        let fast = started.elapsed() / runs;
        println!("1 MiB: copy {copy:?}, slice == {eq:?}, same() {fast:?}");
    }

    #[test]
    fn skipped_rects_drop_unaligned_forced_full_edges() {
        let (mut canvas, mut diff) = setup();
        let grid = Rect::new(10, 20, 120, 50);
        canvas.fill_rect(grid, 0);
        // A separate change outside the forced rect (e.g. pager).
        canvas.fill_rect(Rect::new(10, 84, 20, 6), 0);
        let tiles = diff.changed_outside(&canvas, &[grid]);
        let forced = [Update { rect: grid, mode: Full }];
        let updates = plan(&tiles, bounds(), &[], Ui, &forced);
        diff.commit(&canvas, &tiles);
        assert_eq!(
            updates,
            vec![Update { rect: Rect::new(0, 80, 32, 16), mode: Ui }, Update { rect: grid, mode: Full }]
        );
        assert!(!diff.changed(&canvas).any());
    }
}
