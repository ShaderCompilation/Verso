//! Reader logic independent of widgets and crengine.
use crate::document::{TocEntry, thread::Page};
use crate::geom::{Point, Rect};
use crate::platform::RefreshMode;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

#[derive(Debug, PartialEq, Eq)]
pub enum Zone {
    Back,
    Forward,
    Menu,
}
pub fn tap_zone(screen: Rect, p: Point) -> Zone {
    let centre =
        Rect::new(screen.x + screen.w * 3 / 10, screen.y + screen.h * 3 / 10, screen.w * 4 / 10, screen.h * 4 / 10);
    if centre.contains(p) {
        Zone::Menu
    } else if p.x < screen.x + screen.w * 3 / 10 {
        Zone::Back
    } else {
        Zone::Forward
    }
}
pub fn chapter(toc: &[TocEntry], page: usize) -> Option<usize> {
    toc.iter().enumerate().rev().find(|(_, c)| c.level <= 2 && c.page <= page).map(|(i, _)| i)
}
pub fn footer(toc: &[TocEntry], page: usize, pages: usize) -> String {
    let title = chapter(toc, page).map(|i| toc[i].title.as_str()).unwrap_or("Book");
    let end = toc.iter().find(|c| c.level <= 2 && c.page > page).map_or(pages, |c| c.page);
    let left = end.saturating_sub(page + 1);
    format!("{title} · {left} {} left", if left == 1 { "page" } else { "pages" })
}
#[derive(Default)]
pub struct PageCache {
    pub generation: u64,
    pages: BTreeMap<usize, Arc<Page>>,
    requested: HashSet<usize>,
}
impl PageCache {
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
        self.pages.retain(|n, _| n.abs_diff(current) <= 1);
    }
    pub fn request(&mut self, page: usize) -> bool {
        !self.pages.contains_key(&page) && self.requested.insert(page)
    }
    pub fn retain_neighbours(&mut self, current: usize) {
        self.pages.retain(|n, _| n.abs_diff(current) <= 1);
    }
}
#[derive(Default)]
pub struct Refresh {
    since_flash: u32,
}
impl Refresh {
    pub fn turn(&mut self, image: bool, chapter_start: bool) -> RefreshMode {
        self.since_flash += 1;
        if image || chapter_start || self.since_flash >= 6 {
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
        assert_eq!(tap_zone(s, Point::new(500, 700)), Zone::Menu);
        assert_eq!(tap_zone(s, Point::new(200, 700)), Zone::Back);
        assert_eq!(tap_zone(s, Point::new(500, 50)), Zone::Forward);
        assert_eq!(tap_zone(s, Point::new(990, 700)), Zone::Forward);
    }
    #[test]
    fn stale_pages_never_enter_cache_and_only_three_are_kept() {
        let mut c = PageCache::default();
        c.reset(2);
        let page = |generation, number| {
            Arc::new(Page {
                generation,
                number,
                pixels: vec![],
                position: Position::default(),
                progress: 0,
                images: false,
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
    fn footer_ignores_deep_subsections_and_flash_counter_resets() {
        let toc = vec![
            TocEntry { title: "One".into(), level: 1, page: 0, position: Position::default() },
            TocEntry { title: "Detail".into(), level: 3, page: 2, position: Position::default() },
            TocEntry { title: "Two".into(), level: 1, page: 10, position: Position::default() },
        ];
        assert_eq!(footer(&toc, 8, 20), "One · 1 page left");
        assert_eq!(footer(&toc, 19, 20), "Two · 0 pages left");
        let mut f = Refresh::default();
        for _ in 0..5 {
            assert_eq!(f.turn(false, false), RefreshMode::Page);
        }
        assert_eq!(f.turn(false, false), RefreshMode::Full);
        assert_eq!(f.turn(false, false), RefreshMode::Page);
        assert_eq!(f.turn(true, false), RefreshMode::Full);
        assert_eq!(f.turn(false, true), RefreshMode::Full);
    }
}
