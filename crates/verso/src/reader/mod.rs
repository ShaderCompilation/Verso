//! Reader logic independent of widgets and crengine.
pub mod bars;
pub mod speed;
use crate::document::{TocEntry, thread::Page};
use crate::geom::{Point, Rect};
use crate::platform::RefreshMode;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

/// Where chapters start in the book's text, for time left and the progress
/// line's ticks. Text offsets don't depend on the layout.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextMap {
    /// Characters of text in the book.
    pub length: u64,
    /// Text offset of each chapter start (top two TOC levels), by position.
    pub starts: std::collections::HashMap<String, u64>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Zone {
    Back,
    Forward,
    Menu,
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
pub fn tap_zone(screen: Rect, p: Point, zones: TapZones) -> Zone {
    if TapZones::menu(screen).contains(p) {
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
/// Counts page turns between full flashes. Chapter starts and pages with
/// images always flash; `every` (0 = never) adds a periodic one.
pub struct Refresh {
    since_flash: u32,
    every: u32,
}
impl Refresh {
    pub fn new(every: u32) -> Self {
        Self { since_flash: 0, every }
    }
    /// A new full-refresh interval (0: only images and chapter starts).
    pub fn set_every(&mut self, every: u32) {
        self.every = every;
    }
    pub fn turn(&mut self, image: bool, chapter_start: bool) -> RefreshMode {
        self.since_flash += 1;
        if image || chapter_start || self.every > 0 && self.since_flash >= self.every {
            self.since_flash = 0;
            RefreshMode::Full
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
    fn zones_prioritise_menu_and_leave_forward_accessible_on_both_edges() {
        let s = Rect::new(0, 0, 1000, 1400);
        let z = TapZones::Standard;
        assert_eq!(tap_zone(s, Point::new(500, 700), z), Zone::Menu);
        assert_eq!(tap_zone(s, Point::new(200, 700), z), Zone::Back);
        assert_eq!(tap_zone(s, Point::new(500, 50), z), Zone::Forward);
        assert_eq!(tap_zone(s, Point::new(990, 700), z), Zone::Forward);
    }
    #[test]
    fn mirrored_and_halves_keep_the_menu_in_the_middle() {
        let s = Rect::new(0, 0, 1000, 1400);
        for z in TapZones::ALL {
            assert_eq!(tap_zone(s, Point::new(500, 700), z), Zone::Menu, "{z:?}");
        }
        let m = TapZones::Mirrored;
        assert_eq!(tap_zone(s, Point::new(200, 700), m), Zone::Forward);
        assert_eq!(tap_zone(s, Point::new(800, 700), m), Zone::Back);
        assert_eq!(tap_zone(s, Point::new(500, 50), m), Zone::Forward);
        let h = TapZones::Halves;
        assert_eq!(tap_zone(s, Point::new(450, 50), h), Zone::Back);
        assert_eq!(tap_zone(s, Point::new(550, 50), h), Zone::Forward);
        assert_eq!(tap_zone(s, Point::new(200, 1300), h), Zone::Back);
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
                pixels: vec![],
                position: Position::default(),
                progress: 0,
                offset: None,
                images: false,
                annotation_revision: 0,
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
                pixels: vec![0; len],
                position: Position::default(),
                progress: 0,
                offset: None,
                images: false,
                annotation_revision: 0,
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
        let mut f = Refresh::new(6);
        for _ in 0..5 {
            assert_eq!(f.turn(false, false), RefreshMode::Page);
        }
        assert_eq!(f.turn(false, false), RefreshMode::Full);
        assert_eq!(f.turn(false, false), RefreshMode::Page);
        assert_eq!(f.turn(true, false), RefreshMode::Full);
        assert_eq!(f.turn(false, true), RefreshMode::Full);
        let mut every = Refresh::new(1);
        assert!((0..3).all(|_| every.turn(false, false) == RefreshMode::Full));
        let mut never = Refresh::new(0);
        assert!((0..50).all(|_| never.turn(false, false) == RefreshMode::Page));
        assert_eq!(never.turn(false, true), RefreshMode::Full);
    }
}
