//! Reader logic independent of widgets and crengine.
pub mod bars;
pub mod speed;
use crate::document::{TocEntry, thread::Page};
use crate::geom::{Point, Rect};
use crate::platform::RefreshMode;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Where chapters start in the book's text, for time left and the progress
/// line's ticks. Text offsets don't depend on the layout.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextMap {
    /// Characters of text in the book.
    pub length: u64,
    /// Text offset of each chapter start (top two TOC levels), by position.
    pub starts: std::collections::HashMap<String, u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zone {
    Back,
    Forward,
    Menu,
    /// The top-right corner, when tapping it bookmarks the page.
    Bookmark,
}
/// The side of the top-right square that toggles the page bookmark.
pub const BOOKMARK_CORNER_MM: f32 = 12.0;
/// The square in the top-right corner of `screen`, `size` pixels a side,
/// that toggles the page bookmark. It comes out of the forward (or, for the
/// left hand, the back) zone.
pub fn bookmark_corner(screen: Rect, size: i32) -> Rect {
    Rect::new(screen.right() - size, screen.y, size, size)
}
/// Where the bookmark mark hangs from the top edge: centred in the right
/// margin `side` pixels wide when that is at least 5 mm, otherwise 1.5 mm
/// from the edge. `mm` converts millimetres to pixels.
pub fn bookmark_mark(screen: Rect, side: i32, mm: impl Fn(f32) -> i32) -> Rect {
    let (w, h) = (mm(3.4), mm(5.5));
    let x = if side >= mm(5.0) { screen.right() - side / 2 - w / 2 } else { screen.right() - mm(1.5) - w };
    Rect::new(x, screen.y, w, h)
}
/// Where taps turn pages. Every layout keeps the centre box for the menu.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TapZones {
    /// The left 30% goes back; the rest goes forward.
    #[default]
    Standard,
    /// The right 30% goes back; the rest goes forward (for the left hand).
    Mirrored,
    /// The left half goes back, the right half forward.
    Halves,
}
impl TapZones {
    pub const ALL: [TapZones; 3] = [TapZones::Standard, TapZones::Mirrored, TapZones::Halves];
    /// The menu box in the middle of `screen`.
    pub fn menu(screen: Rect) -> Rect {
        Rect::new(screen.x + screen.w * 3 / 10, screen.y + screen.h * 3 / 10, screen.w * 4 / 10, screen.h * 4 / 10)
    }
    /// The area that goes back (forward is everything else outside the menu).
    pub fn back(self, screen: Rect) -> Rect {
        match self {
            TapZones::Standard => screen.split_left(screen.w * 3 / 10).0,
            TapZones::Mirrored => screen.split_right(screen.w * 3 / 10).0,
            TapZones::Halves => screen.split_left(screen.w / 2).0,
        }
    }
}
/// The zone a tap at `p` falls in. `corner` is the bookmark square's side in
/// pixels, when the corner tap is on.
pub fn tap_zone(screen: Rect, p: Point, zones: TapZones, corner: Option<i32>) -> Zone {
    if corner.is_some_and(|size| bookmark_corner(screen, size).contains(p)) {
        Zone::Bookmark
    } else if TapZones::menu(screen).contains(p) {
        Zone::Menu
    } else if zones.back(screen).contains(p) {
        Zone::Back
    } else {
        Zone::Forward
    }
}
pub fn chapter(toc: &[TocEntry], page: usize) -> Option<usize> {
    toc.iter().enumerate().rev().find(|(_, c)| c.level <= 2 && c.page <= page).map(|(i, _)| i)
}
pub struct PageCache {
    pub generation: u64,
    pages: BTreeMap<usize, Arc<Page>>,
    requested: HashSet<usize>,
    /// Maximum pixel bytes kept across cached pages; `current` is always kept.
    max_bytes: usize,
}
impl Default for PageCache {
    fn default() -> Self {
        Self::with_max_bytes(Self::DEFAULT_MAX_BYTES)
    }
}
impl PageCache {
    /// Holds 3 full 32-bit Colorsoft pages (~24 MiB) or many 8-bit pages while
    /// capping memory on larger screens.
    pub const DEFAULT_MAX_BYTES: usize = 32 * 1024 * 1024;

    pub fn with_max_bytes(max_bytes: usize) -> Self {
        Self { generation: 0, pages: BTreeMap::new(), requested: HashSet::new(), max_bytes }
    }
    pub fn reset(&mut self, generation: u64) {
        self.generation = generation;
        self.pages.clear();
        self.requested.clear();
    }
    /// Drops the cached pages `stale` picks; their requests can go out again.
    pub fn evict(&mut self, stale: impl Fn(&Page) -> bool) {
        self.pages.retain(|_, p| !stale(p));
    }
    pub fn get(&self, number: usize) -> Option<Arc<Page>> {
        self.pages.get(&number).cloned()
    }
    pub fn insert(&mut self, p: Arc<Page>, current: usize) {
        if p.generation != self.generation {
            return;
        }
        self.requested.remove(&p.number);
        if p.number.abs_diff(current) <= 1 {
            self.pages.insert(p.number, p);
        }
        self.retain_neighbours(current);
    }
    pub fn request(&mut self, page: usize) -> bool {
        !self.pages.contains_key(&page) && self.requested.insert(page)
    }
    /// Release only the failed request, leaving other in-flight renders alone.
    pub fn failed(&mut self, page: usize) {
        self.requested.remove(&page);
    }
    pub fn retain_neighbours(&mut self, current: usize) {
        self.pages.retain(|n, _| n.abs_diff(current) <= 1);
        while self.bytes() > self.max_bytes && self.pages.len() > 1 {
            // Drop a neighbour before `current` (preferring the page behind over
            // the page ahead when both are 1 away).
            let Some(&drop) = self
                .pages
                .keys()
                .filter(|&&n| n != current)
                .max_by_key(|&&n| (n.abs_diff(current), usize::from(n < current)))
            else {
                break;
            };
            self.pages.remove(&drop);
        }
    }
    fn bytes(&self) -> usize {
        self.pages.values().map(|p| p.pixels.len()).sum()
    }
}
/// Turns closer together than this are rapid flipping: a periodic flash
/// due then waits (see [`Refresh::turn`]).
pub const RAPID_TURN: Duration = Duration::from_millis(400);

/// Counts page turns between full flashes. Pages with images always get a
/// `Clean` (in dark mode it flashes only the changed pixels), and chapter
/// starts flash in light mode (text on black ghosts little, so a dark chapter
/// start isn't worth the white flash); `every` (0 = never) adds a periodic one.
pub struct Refresh {
    since_flash: u32,
    every: u32,
    last_turn: Option<Instant>,
}
impl Refresh {
    pub fn new(every: u32) -> Self {
        Self { since_flash: 0, every, last_turn: None }
    }
    /// A new full-refresh interval (0: only images and chapter starts).
    pub fn set_every(&mut self, every: u32) {
        self.every = every;
    }
    /// The refresh for a page turn shown at `now`. A periodic flash that falls
    /// due while turns come less than [`RAPID_TURN`] apart waits for the first
    /// slower turn, up to `2 × every` turns since the last flash: a ~300 ms
    /// flash would hold up the taps queued behind it.
    pub fn turn(&mut self, image: bool, chapter_start: bool, dark: bool, now: Instant) -> RefreshMode {
        self.since_flash += 1;
        let rapid = self.last_turn.is_some_and(|t| now.saturating_duration_since(t) < RAPID_TURN);
        self.last_turn = Some(now);
        let due = self.every > 0 && self.since_flash >= self.every;
        let deferred = rapid && self.since_flash < self.every.saturating_mul(2);
        if chapter_start && !dark || due && !deferred {
            self.since_flash = 0;
            RefreshMode::Full
        } else if image {
            self.since_flash = 0;
            RefreshMode::Clean
        } else {
            RefreshMode::Page
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Position;
    #[test]
    fn periodic_flash_waits_during_rapid_turns_but_images_and_chapters_do_not() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let (full, clean, page) = (RefreshMode::Full, RefreshMode::Clean, RefreshMode::Page);
        // Slow turns: every 3rd flashes.
        let mut r = Refresh::new(3);
        let modes: Vec<_> = (0..6).map(|i| r.turn(false, false, false, at(i * 1000))).collect();
        assert_eq!(modes, [page, page, full, page, page, full]);
        // Rapid turns (200 ms apart): the due flash waits until 2 × every.
        let mut r = Refresh::new(3);
        let modes: Vec<_> = (0..8).map(|i| r.turn(false, false, false, at(i * 200))).collect();
        assert_eq!(modes, [page, page, page, page, page, full, page, page]);
        // A pause after rapid turns flashes on the next turn.
        let mut r = Refresh::new(3);
        let mut modes: Vec<_> = (0..4).map(|i| r.turn(false, false, false, at(i * 200))).collect();
        modes.push(r.turn(false, false, false, at(600 + RAPID_TURN.as_millis() as u64)));
        assert_eq!(modes, [page, page, page, page, full]);
        // Images and chapter starts flash even mid-flip, and restart the count.
        let mut r = Refresh::new(3);
        let modes = [
            r.turn(false, false, false, at(0)),
            r.turn(true, false, false, at(200)),
            r.turn(false, true, false, at(400)),
            r.turn(false, false, false, at(600)),
        ];
        assert_eq!(modes, [page, clean, full, page]);
        // Dark: a chapter start neither flashes nor restarts the count; images
        // and the periodic flash still do.
        let mut r = Refresh::new(3);
        let turns = [(false, true), (true, false), (false, true), (false, false), (false, false)];
        let modes: Vec<_> = (0..5).map(|i| r.turn(turns[i].0, turns[i].1, true, at(i as u64 * 1000))).collect();
        assert_eq!(modes, [page, clean, page, page, full]);
        // A chapter start or a due flash on an image page beats its Clean.
        let mut r = Refresh::new(2);
        assert_eq!(r.turn(true, true, false, at(0)), full);
        assert_eq!([r.turn(false, false, true, at(1000)), r.turn(true, false, true, at(2000))], [page, full]);
        // No periodic flash: nothing to defer.
        let mut r = Refresh::new(0);
        assert!((0..20).all(|i| r.turn(false, false, false, at(i * 200)) == page));
    }
    #[test]
    fn zones_prioritise_menu_and_leave_forward_accessible_on_both_edges() {
        let s = Rect::new(0, 0, 1000, 1400);
        let z = TapZones::Standard;
        assert_eq!(tap_zone(s, Point::new(500, 700), z, None), Zone::Menu);
        assert_eq!(tap_zone(s, Point::new(200, 700), z, None), Zone::Back);
        assert_eq!(tap_zone(s, Point::new(500, 50), z, None), Zone::Forward);
        assert_eq!(tap_zone(s, Point::new(990, 700), z, None), Zone::Forward);
    }
    #[test]
    fn mirrored_and_halves_keep_the_menu_in_the_middle() {
        let s = Rect::new(0, 0, 1000, 1400);
        for z in TapZones::ALL {
            assert_eq!(tap_zone(s, Point::new(500, 700), z, None), Zone::Menu, "{z:?}");
        }
        let m = TapZones::Mirrored;
        assert_eq!(tap_zone(s, Point::new(200, 700), m, None), Zone::Forward);
        assert_eq!(tap_zone(s, Point::new(800, 700), m, None), Zone::Back);
        assert_eq!(tap_zone(s, Point::new(500, 50), m, None), Zone::Forward);
        let h = TapZones::Halves;
        assert_eq!(tap_zone(s, Point::new(450, 50), h, None), Zone::Back);
        assert_eq!(tap_zone(s, Point::new(550, 50), h, None), Zone::Forward);
        assert_eq!(tap_zone(s, Point::new(200, 1300), h, None), Zone::Back);
    }
    #[test]
    fn the_bookmark_corner_takes_the_top_right_square_of_either_page_turn_zone() {
        let s = Rect::new(0, 0, 1000, 1400);
        for z in TapZones::ALL {
            assert_eq!(tap_zone(s, Point::new(950, 50), z, Some(140)), Zone::Bookmark, "{z:?}");
            assert_eq!(tap_zone(s, Point::new(950, 50), z, None), tap_zone(s, Point::new(950, 700), z, None));
            // Just outside the square, the page-turn zone is as before.
            assert_ne!(tap_zone(s, Point::new(850, 50), z, Some(140)), Zone::Bookmark);
            assert_ne!(tap_zone(s, Point::new(950, 150), z, Some(140)), Zone::Bookmark);
            assert_eq!(tap_zone(s, Point::new(500, 700), z, Some(140)), Zone::Menu);
        }
    }
    #[test]
    fn the_bookmark_mark_sits_in_the_right_margin_or_near_the_edge() {
        // 300 dpi: 1 mm is about 11.8 px.
        let mm = |v: f32| (v * 300.0 / 25.4).round() as i32;
        for screen in [Rect::new(0, 0, 1272, 1696), Rect::new(0, 0, 1696, 1272), Rect::new(0, 0, 600, 800)] {
            for sides in [0, 3, 5, 7, 15, 40] {
                let side = mm(sides as f32);
                let mark = bookmark_mark(screen, side, mm);
                assert_eq!((mark.y, mark.w, mark.h), (0, mm(3.4), mm(5.5)), "hangs from the top edge");
                assert!(mark.right() <= screen.right() && mark.x >= screen.right() - side.max(mm(5.0)));
                if sides >= 5 {
                    assert!((mark.center().x - (screen.right() - side / 2)).abs() <= 1, "centred in the margin");
                    assert!(mark.x >= screen.right() - side, "inside the margin, clear of the text");
                } else {
                    assert_eq!(mark.right(), screen.right() - mm(1.5));
                }
            }
        }
    }
    #[test]
    fn stale_pages_never_enter_cache_and_only_three_are_kept() {
        let mut c = PageCache::default();
        c.reset(2);
        let page = |generation, number| {
            Arc::new(Page {
                generation,
                number,
                color: crate::gfx::ColorMode::Gray8,
                pixels: vec![].into(),
                position: Position::default(),
                progress: 0,
                offset: None,
                images: false,
                annotation_revision: 0,
                contrast: 0,
                dark: false,
                annotations: vec![],
                painted: vec![],
                links: vec![],
            })
        };
        c.insert(page(1, 10), 10);
        assert!(c.get(10).is_none());
        for n in 0..20 {
            c.insert(page(2, n), 10);
        }
        assert_eq!(c.pages.keys().copied().collect::<Vec<_>>(), vec![9, 10, 11]);
        assert!(!c.request(11));
        assert!(c.request(12));
        assert!(!c.request(12));
    }
    #[test]
    fn page_cache_byte_budget_keeps_current_and_prefers_next_page() {
        let mut c = PageCache::with_max_bytes(200);
        c.reset(1);
        let sized = |number, len| {
            Arc::new(Page {
                generation: 1,
                number,
                color: crate::gfx::ColorMode::Color32,
                pixels: vec![0; len].into(),
                position: Position::default(),
                progress: 0,
                offset: None,
                images: false,
                annotation_revision: 0,
                contrast: 0,
                dark: false,
                annotations: vec![],
                painted: vec![],
                links: vec![],
            })
        };
        c.insert(sized(5, 100), 5);
        c.insert(sized(4, 100), 5);
        assert_eq!(c.pages.keys().copied().collect::<Vec<_>>(), vec![4, 5]);
        // Adding page 6 pushes total to 300 > 200; page 4 (behind) is evicted,
        // keeping current (5) and next (6).
        c.insert(sized(6, 100), 5);
        assert_eq!(c.pages.keys().copied().collect::<Vec<_>>(), vec![5, 6]);
        // Even a single page larger than the budget stays as current.
        c.insert(sized(5, 250), 5);
        assert_eq!(c.pages.keys().copied().collect::<Vec<_>>(), vec![5]);
    }
    #[test]
    fn flash_counter_resets() {
        // Slow turns, a second apart: never rapid.
        let start = Instant::now();
        let mut n = 0u32;
        let mut slow = || {
            n += 1;
            start + n * Duration::from_secs(1)
        };
        let mut f = Refresh::new(6);
        for _ in 0..5 {
            assert_eq!(f.turn(false, false, false, slow()), RefreshMode::Page);
        }
        assert_eq!(f.turn(false, false, false, slow()), RefreshMode::Full);
        assert_eq!(f.turn(false, false, false, slow()), RefreshMode::Page);
        assert_eq!(f.turn(true, false, false, slow()), RefreshMode::Clean);
        assert_eq!(f.turn(false, true, false, slow()), RefreshMode::Full);
        let mut every = Refresh::new(1);
        assert!((0..3).all(|_| every.turn(false, false, false, slow()) == RefreshMode::Full));
        let mut never = Refresh::new(0);
        assert!((0..50).all(|_| never.turn(false, false, false, slow()) == RefreshMode::Page));
        assert_eq!(never.turn(false, true, false, slow()), RefreshMode::Full);
    }
}
