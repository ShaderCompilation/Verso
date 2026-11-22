//! What the reader's top and bottom bars show, and how three items share a
//! bar's width. Drawing lives in `screens::reader::bars`.
use super::TextMap;
use super::speed::format_duration;
use crate::clock::LocalTime;
use crate::document::TocEntry;
use crate::page_layout::BarItem;
use crate::platform::Battery;

/// What the bar items are computed from: the displayed page, the book, and
/// the clock and battery as of the last reader redraw.
#[derive(Default)]
pub struct BarData<'a> {
    pub toc: &'a [TocEntry],
    pub title: &'a str,
    pub author: &'a str,
    /// Zero-based page number.
    pub page: usize,
    pub pages: usize,
    /// Share of the book's text before this page, in hundredths of a percent.
    pub progress: u32,
    /// Characters of text before this page, when known.
    pub offset: Option<u64>,
    /// Chapter starts in the text; None until the book thread sends them.
    pub text: Option<&'a TextMap>,
    /// Reading speed in characters per second, once it is known.
    pub speed: Option<f32>,
    pub clock: Option<LocalTime>,
    pub battery: Option<Battery>,
}

/// An item's text in two parts. `head` may end in "…" when space is short;
/// `tail` always shows in full. Short items have only a tail. The battery
/// item also has an icon before its text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ItemText {
    pub head: String,
    pub tail: String,
    pub battery: Option<Battery>,
}
impl ItemText {
    pub fn short(text: String) -> Self {
        Self { tail: text, ..Self::default() }
    }
    fn title(text: &str) -> Self {
        Self { head: text.to_owned(), ..Self::default() }
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
        book_percent(self.page, self.pages, self.progress)
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
                ItemText {
                    head: self.chapter().0.to_owned(), tail: format!(" · {}", self.pages_left()), battery: None
                }
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
            BarItem::TimeLeftChapter => ItemText::short(format!("{} left in chapter", self.time_left(true)?)),
            BarItem::TimeLeftBook => ItemText::short(format!("{} left in book", self.time_left(false)?)),
            BarItem::Clock24 => ItemText::short(self.clock?.h24()),
            BarItem::Clock12 => ItemText::short(self.clock?.h12()),
            BarItem::Battery => {
                let battery = self.battery?;
                ItemText { battery: Some(battery), ..ItemText::short(format!("{}%", battery.percent)) }
            }
        })
        .filter(|t| !t.head.is_empty() || !t.tail.is_empty())
    }

    /// Characters of text from this page's start to the end of its chapter
    /// (`chapter`) or of the book. The chapter is the one the bar names.
    pub fn chars_left(&self, chapter: bool) -> Option<u64> {
        let text = self.text?;
        let here = self.offset.unwrap_or(text.length * self.progress.min(10_000) as u64 / 10_000);
        let end = if chapter {
            self.toc
                .iter()
                .filter(|c| c.level <= 2 && c.page > self.page)
                .find_map(|c| text.starts.get(&c.position.0).copied())
                .unwrap_or(text.length)
        } else {
            text.length
        };
        Some(end.saturating_sub(here))
    }
    fn time_left(&self, chapter: bool) -> Option<String> {
        let speed = self.speed.filter(|s| *s > 0.0)?;
        Some(format_duration(self.chars_left(chapter)? as f32 / speed))
    }

    /// Where chapters start along the book, as fractions 0..1 for the
    /// progress line's ticks: by text offset once known, else by page. The
    /// first page gets no tick.
    pub fn chapter_starts(&self) -> Vec<f32> {
        if let Some(text) = self.text.filter(|t| t.length > 0) {
            let mut starts: Vec<f32> = self
                .toc
                .iter()
                .filter(|c| c.level <= 2 && c.page > 0)
                .filter_map(|c| text.starts.get(&c.position.0))
                .filter(|&&o| o > 0 && o < text.length)
                .map(|&o| o as f32 / text.length as f32)
                .collect();
            starts.dedup();
            return starts;
        }
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

/// The book percentage of zero-based `page` at `progress` (basis points
/// of the text); the last page always reads 100%. The bars, the Reader Menu
/// and Home show this same number.
pub fn book_percent(page: usize, pages: usize, progress: u32) -> u32 {
    if page + 1 >= pages { 100 } else { (progress / 100).min(100) }
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
        BarData { toc, title: "Dune", author: "Frank Herbert", page, pages: 20, progress: 3712, ..BarData::default() }
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
            ItemText { head: "One".into(), tail: " · 2 pages left".into(), battery: None }
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
    fn live_items_show_nothing_without_a_value() {
        let toc = toc();
        for item in
            [BarItem::TimeLeftChapter, BarItem::TimeLeftBook, BarItem::Clock24, BarItem::Clock12, BarItem::Battery]
        {
            assert_eq!(data(&toc, 5).text(item), None, "{item:?}");
        }
    }

    #[test]
    fn clocks_and_battery() {
        let toc = toc();
        let d = BarData {
            clock: Some(LocalTime { hour: 14, minute: 41 }),
            battery: Some(Battery { percent: 82, charging: true }),
            ..data(&toc, 5)
        };
        assert_eq!(text(&d, BarItem::Clock24), "14:41");
        assert_eq!(text(&d, BarItem::Clock12), "2:41 PM");
        let battery = d.text(BarItem::Battery).unwrap();
        assert_eq!(battery.tail, "82%");
        assert_eq!(battery.battery, Some(Battery { percent: 82, charging: true }), "drawn with its icon");
    }

    /// Chapters One (page 2), Two (10), Three (15) start at these offsets in
    /// a 20 000-character book; "Detail" is a subsection and doesn't count.
    fn text_map() -> TextMap {
        let starts = [("one", 2_000), ("detail", 4_000), ("two", 10_000), ("three", 15_000)];
        TextMap { length: 20_000, starts: starts.into_iter().map(|(p, o)| (p.to_owned(), o)).collect() }
    }
    fn toc_with_positions() -> Vec<TocEntry> {
        let mut toc = toc();
        for (entry, position) in toc.iter_mut().zip(["one", "detail", "two", "three"]) {
            entry.position = Position(position.into());
        }
        toc
    }

    #[test]
    fn chars_left_run_to_the_next_chapter_or_the_book_end() {
        let (toc, map) = (toc_with_positions(), text_map());
        let at = |page, offset| BarData { offset: Some(offset), text: Some(&map), ..data(&toc, page) };
        assert_eq!(at(7, 7_000).chars_left(true), Some(3_000), "to Two, past the subsection");
        assert_eq!(at(7, 7_000).chars_left(false), Some(13_000));
        // Before the first chapter, the chapter is the book up to One.
        assert_eq!(at(0, 0).chars_left(true), Some(2_000));
        // The last chapter and the last page run to the end of the book.
        assert_eq!(at(16, 16_000).chars_left(true), Some(4_000));
        assert_eq!(at(19, 19_400).chars_left(true), Some(600));
        assert_eq!(at(19, 19_400).chars_left(false), Some(600));
        // Never negative, even if offsets disagree with page numbers.
        assert_eq!(at(9, 12_000).chars_left(true), Some(0));
        // Without this page's offset, the percentage stands in.
        let estimated = BarData { offset: None, text: Some(&map), progress: 5_000, ..data(&toc, 9) };
        assert_eq!(estimated.chars_left(false), Some(10_000));
        // An unresolved chapter start is skipped for the next one.
        let mut partial = text_map();
        partial.starts.remove("two");
        let d = BarData { offset: Some(7_000), text: Some(&partial), ..data(&toc, 7) };
        assert_eq!(d.chars_left(true), Some(8_000));
        assert_eq!(data(&toc, 7).chars_left(true), None, "nothing before the map arrives");
    }

    #[test]
    fn time_left_needs_a_speed() {
        let (toc, map) = (toc_with_positions(), text_map());
        let d = BarData { offset: Some(7_000), text: Some(&map), speed: Some(20.0), ..data(&toc, 7) };
        // 3 000 characters at 20 per second: 2.5 minutes, shown rounded.
        assert_eq!(text(&d, BarItem::TimeLeftChapter), "3 min left in chapter");
        assert_eq!(text(&d, BarItem::TimeLeftBook), "11 min left in book");
        let slow = BarData { speed: Some(0.5), ..d };
        assert_eq!(text(&slow, BarItem::TimeLeftBook), "7 h 13 min left in book");
        let end = BarData { offset: Some(20_000), text: Some(&map), speed: Some(20.0), ..data(&toc, 19) };
        assert_eq!(text(&end, BarItem::TimeLeftBook), "< 1 min left in book");
        let unknown = BarData { speed: None, ..BarData { offset: Some(7_000), text: Some(&map), ..data(&toc, 7) } };
        assert_eq!(unknown.text(BarItem::TimeLeftChapter), None);
    }

    #[test]
    fn ticks_mark_chapter_starts() {
        let toc = toc();
        assert_eq!(data(&toc, 0).chapter_starts(), vec![0.1, 0.5, 0.75]);
        let first = vec![TocEntry { title: "Start".into(), level: 1, page: 0, position: Position::default() }];
        assert!(data(&first, 0).chapter_starts().is_empty(), "no tick at the very start");
        assert!(BarData { pages: 1, ..data(&toc, 0) }.chapter_starts().is_empty());
        // Once text offsets are known, ticks sit where the fill crosses them.
        let (toc, map) = (toc_with_positions(), text_map());
        let d = BarData { text: Some(&map), ..data(&toc, 0) };
        assert_eq!(d.chapter_starts(), vec![0.1, 0.5, 0.75]);
        let mut shifted = text_map();
        shifted.starts.insert("two".into(), 9_000);
        assert_eq!(BarData { text: Some(&shifted), ..data(&toc, 0) }.chapter_starts(), vec![0.1, 0.45, 0.75]);
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
