//! What the reader's top and bottom bars show, and how three items share a
//! bar's width. Drawing lives in `screens::reader::bars`.
use crate::document::TocEntry;
use crate::page_layout::BarItem;

/// What the bar items are computed from: the displayed page and the book.
pub struct BarData<'a> {
    pub toc: &'a [TocEntry],
    pub title: &'a str,
    pub author: &'a str,
    /// Zero-based page number.
    pub page: usize,
    pub pages: usize,
    /// Share of the book's text before this page, in hundredths of a percent.
    pub progress: u32,
}

/// An item's text in two parts. `head` may end in "…" when space is short;
/// `tail` always shows in full. Short items have only a tail.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ItemText {
    pub head: String,
    pub tail: String,
}
impl ItemText {
    fn short(text: String) -> Self {
        Self { head: String::new(), tail: text }
    }
    fn title(text: &str) -> Self {
        Self { head: text.to_owned(), tail: String::new() }
    }
}

impl BarData<'_> {
    /// The chapter around the page: (title, first page, end page). Before the
    /// first chapter it is the book itself up to that chapter.
    fn chapter(&self) -> (&str, usize, usize) {
        let current = super::chapter(self.toc, self.page).map(|i| &self.toc[i]);
        let end = self.toc.iter().find(|c| c.level <= 2 && c.page > self.page).map_or(self.pages, |c| c.page);
        match current {
            Some(c) => (c.title.as_str(), c.page, end),
            None => (self.book_title(), 0, end),
        }
    }
    fn book_title(&self) -> &str {
        if self.title.trim().is_empty() { "Book" } else { self.title }
    }
    /// Pages after this one in the chapter.
    fn pages_left(&self) -> String {
        let (_, _, end) = self.chapter();
        let left = end.saturating_sub(self.page + 1);
        format!("{left} {} left", if left == 1 { "page" } else { "pages" })
    }
    /// The book percentage; the last page always reads 100%.
    pub fn percent(&self) -> u32 {
        if self.page + 1 >= self.pages { 100 } else { (self.progress / 100).min(100) }
    }

    /// The book progress as a fraction, for the progress line.
    pub fn fraction(&self) -> f32 {
        if self.page + 1 >= self.pages { 1.0 } else { self.progress.min(10_000) as f32 / 10_000.0 }
    }

    /// The text for `item`, or None for items that show nothing yet.
    pub fn text(&self, item: BarItem) -> Option<ItemText> {
        Some(match item {
            BarItem::ChapterTitle => ItemText::title(self.chapter().0),
            BarItem::ChapterPagesLeft => {
                ItemText { head: self.chapter().0.to_owned(), tail: format!(" · {}", self.pages_left()) }
            }
            BarItem::BookTitle => ItemText::title(self.book_title()),
            BarItem::Author => ItemText::title(self.author.trim()),
            BarItem::PagesLeft => ItemText::short(self.pages_left()),
            BarItem::PageInChapter => {
                let (_, start, end) = self.chapter();
                let count = end.saturating_sub(start).max(1);
                ItemText::short(format!("{} / {count}", (self.page - start.min(self.page) + 1).min(count)))
            }
            BarItem::Percent => ItemText::short(format!("{}%", self.percent())),
            BarItem::PageTotal => ItemText::short(format!("{} / {}", self.page + 1, self.pages.max(1))),
            // Need reading speed, local time and the battery level (Phase 3).
            BarItem::TimeLeftChapter
            | BarItem::TimeLeftBook
            | BarItem::Clock24
            | BarItem::Clock12
            | BarItem::Battery => return None,
        })
        .filter(|t| !t.head.is_empty() || !t.tail.is_empty())
    }

    /// Where chapters start along the book, as fractions 0..1 for the
    /// progress line's ticks. The first page gets no tick.
    pub fn chapter_starts(&self) -> Vec<f32> {
        if self.pages < 2 {
            return Vec::new();
        }
        let mut starts: Vec<f32> = self
            .toc
            .iter()
            .filter(|c| c.level <= 2 && c.page > 0 && c.page < self.pages)
            .map(|c| c.page as f32 / self.pages as f32)
            .collect();
        starts.dedup();
        starts
    }
}

/// One slot's width needs: the least it can show (a short item in full, or
/// a title's tail) and its full width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Need {
    pub min: i32,
    pub full: i32,
}

/// Widths for the left, centre and right items of a bar `width` wide. Short
/// items always get their full width; titles take what's left. A centre item
/// is centred, so each side gets at most half of the rest, minus `gap`.
pub fn fit(slots: [Option<Need>; 3], width: i32, gap: i32) -> [i32; 3] {
    let [left, centre, right] = slots;
    let take = |s: Option<Need>, cap: i32| s.map_or(0, |n| n.full.min(cap.max(n.min)));
    match centre {
        Some(c) => {
            // A centre title lets side titles have a quarter each to begin with.
            let first_cap = if c.min < c.full { (width - 2 * gap) / 4 } else { (width - c.full) / 2 - gap };
            let side = take(left, first_cap).max(take(right, first_cap));
            let centre_w = c.full.min(c.min.max(width - 2 * (side + gap)));
            let cap = (width - centre_w) / 2 - gap;
            [take(left, cap), centre_w, take(right, cap)]
        }
        None => {
            let both = left.is_some() && right.is_some();
            let avail = width - if both { gap } else { 0 };
            // What the other side keeps for itself: a title at most half.
            let keeps = |s: Option<Need>| take(s, avail / 2);
            [take(left, avail - keeps(right)), 0, take(right, avail - keeps(left))]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Position;

    fn toc() -> Vec<TocEntry> {
        let entry =
            |title: &str, level, page| TocEntry { title: title.into(), level, page, position: Position::default() };
        vec![entry("One", 1, 2), entry("Detail", 3, 4), entry("Two", 2, 10), entry("Three", 1, 15)]
    }
    fn data(toc: &[TocEntry], page: usize) -> BarData<'_> {
        BarData { toc, title: "Dune", author: "Frank Herbert", page, pages: 20, progress: 3712 }
    }
    fn text(d: &BarData, item: BarItem) -> String {
        d.text(item).map(|t| t.head + &t.tail).unwrap_or_default()
    }

    #[test]
    fn items_describe_the_page() {
        let toc = toc();
        let d = data(&toc, 7);
        assert_eq!(text(&d, BarItem::ChapterTitle), "One");
        assert_eq!(
            d.text(BarItem::ChapterPagesLeft).unwrap(),
            ItemText { head: "One".into(), tail: " · 2 pages left".into() }
        );
        assert_eq!(text(&d, BarItem::BookTitle), "Dune");
        assert_eq!(text(&d, BarItem::Author), "Frank Herbert");
        assert_eq!(text(&d, BarItem::PagesLeft), "2 pages left");
        // Deep subsections don't count as chapters: page 8 of 2..10 is 6 / 8.
        assert_eq!(text(&d, BarItem::PageInChapter), "6 / 8");
        assert_eq!(text(&d, BarItem::Percent), "37%");
        assert_eq!(text(&d, BarItem::PageTotal), "8 / 20");
        let d = data(&toc, 9);
        assert_eq!(text(&d, BarItem::PagesLeft), "0 pages left");
        assert_eq!(text(&d, BarItem::PageInChapter), "8 / 8");
        let d = data(&toc, 8);
        assert_eq!(text(&d, BarItem::PagesLeft), "1 page left");
        // The last chapter runs to the end; the last page reads 100%.
        let d = data(&toc, 19);
        assert_eq!(text(&d, BarItem::ChapterPagesLeft), "Three · 0 pages left");
        assert_eq!(text(&d, BarItem::Percent), "100%");
        assert_eq!(text(&d, BarItem::PageInChapter), "5 / 5");
    }

    #[test]
    fn before_the_first_chapter_the_book_stands_in() {
        let toc = toc();
        let d = data(&toc, 0);
        assert_eq!(text(&d, BarItem::ChapterTitle), "Dune");
        assert_eq!(text(&d, BarItem::ChapterPagesLeft), "Dune · 1 page left");
        assert_eq!(text(&d, BarItem::PageInChapter), "1 / 2");
        let untitled = BarData { title: " ", author: "", ..data(&[], 3) };
        assert_eq!(text(&untitled, BarItem::ChapterTitle), "Book");
        assert_eq!(text(&untitled, BarItem::PagesLeft), "16 pages left");
        assert_eq!(untitled.text(BarItem::Author), None, "an empty item shows nothing");
    }

    #[test]
    fn later_phase_items_show_nothing_yet() {
        let toc = toc();
        for item in
            [BarItem::TimeLeftChapter, BarItem::TimeLeftBook, BarItem::Clock24, BarItem::Clock12, BarItem::Battery]
        {
            assert_eq!(data(&toc, 5).text(item), None, "{item:?}");
        }
    }

    #[test]
    fn ticks_mark_chapter_starts() {
        let toc = toc();
        assert_eq!(data(&toc, 0).chapter_starts(), vec![0.1, 0.5, 0.75]);
        let first = vec![TocEntry { title: "Start".into(), level: 1, page: 0, position: Position::default() }];
        assert!(data(&first, 0).chapter_starts().is_empty(), "no tick at the very start");
        assert!(BarData { pages: 1, ..data(&toc, 0) }.chapter_starts().is_empty());
    }

    #[test]
    fn short_items_show_in_full_and_titles_take_the_rest() {
        let short = |w| Some(Need { min: w, full: w });
        let title = |min, full| Some(Need { min, full });
        // Everything fits.
        assert_eq!(fit([title(0, 300), None, short(50)], 1000, 30), [300, 0, 50]);
        // A long title yields to a short item.
        assert_eq!(fit([title(200, 2000), None, short(50)], 1000, 30), [920, 0, 50]);
        // Two long titles split the width.
        assert_eq!(fit([title(0, 2000), None, title(0, 2000)], 1000, 30), [485, 0, 485]);
        // A short title keeps its width; the other title gets the rest.
        assert_eq!(fit([title(0, 200), None, title(0, 2000)], 1000, 30), [200, 0, 770]);
        // A title never shrinks below its tail.
        assert_eq!(fit([title(400, 2000), None, short(700)], 1000, 30), [400, 0, 700]);
        // A lone item may use the whole bar.
        assert_eq!(fit([None, None, title(0, 2000)], 1000, 30), [0, 0, 1000]);
    }

    #[test]
    fn a_centre_item_leaves_each_side_half_of_the_rest() {
        let short = |w| Some(Need { min: w, full: w });
        let title = |min, full| Some(Need { min, full });
        // Short centre: sides get (1000 - 100) / 2 - 30 = 420 at most.
        assert_eq!(fit([title(0, 2000), short(100), short(50)], 1000, 30), [420, 100, 50]);
        assert_eq!(fit([title(0, 300), short(100), None], 1000, 30), [300, 100, 0]);
        // Centre title with short sides: it gets what the wider side leaves.
        assert_eq!(fit([short(80), title(0, 2000), short(50)], 1000, 30), [80, 780, 50]);
        // Titles everywhere: the centre gets about half, the sides a quarter.
        assert_eq!(fit([title(0, 2000), title(0, 2000), title(0, 2000)], 1000, 30), [235, 470, 235]);
        // A short centre title leaves room for the sides.
        assert_eq!(fit([title(0, 2000), title(0, 200), None], 1000, 30), [370, 200, 0]);
    }
}
