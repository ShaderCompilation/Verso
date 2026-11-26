//! Library sort orders. Every order is a [`Key`] built once per book, so a
//! comparison never folds case or parses numbers.
//!
//! Text compares case-insensitively and naturally (`Book 2` before `Book 10`):
//! [`Natural`] lowercases the text and spells each digit run as its length
//! and digits, so plain byte order is natural order.
//!
//! Every sort ends with the same tie-breakers: series order, then title, then
//! path. Series order puts books with a series (from metadata) first, by series
//! name and number; then the rest, numbered ones by the first number in the file
//! name, or failing that in the title ([`guess_number`]); then by file name.

use std::cmp::{Ordering, Reverse};
use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::Book;

/// How the library and search order books. Folders always come first, by name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    #[default]
    Title,
    Author,
    Series,
    /// By the opened order; never-opened books follow, newest file first.
    RecentlyRead,
    /// Newest file (`stamp.modified`) first.
    RecentlyAdded,
}
impl Sort {
    pub const ALL: [Sort; 5] = [Sort::Title, Sort::Author, Sort::Series, Sort::RecentlyRead, Sort::RecentlyAdded];

    /// Builds `book`'s key; `scratch` is reused between keys. `ranks` (book
    /// id → place in the opened order, `Opened::ranks`) matters only to
    /// Recently read.
    fn key(self, book: &Book, ranks: &Ranks, scratch: &mut Vec<u8>) -> Key {
        let m = &book.metadata;
        let name = book.path.file_name().unwrap_or_default().to_string_lossy();
        let (group, position) = if m.series.trim().is_empty() {
            let stem = book.path.file_stem().unwrap_or_default().to_string_lossy();
            (None, guess_number(&stem).or_else(|| guess_number(&m.title)))
        } else {
            (Some(Natural::build(&m.series, scratch)), parse_number(&m.series_index))
        };
        let title = Natural::build(&m.title, scratch);
        let (lead, title) = match self {
            // The title leads, so it needn't break ties again.
            Sort::Title => (title, Natural::default()),
            Sort::Author => {
                let author = if m.author_sort.trim().is_empty() { &m.authors } else { &m.author_sort };
                (Natural::build(author, scratch), title)
            }
            _ => (Natural::default(), title),
        };
        let recent = matches!(self, Sort::RecentlyRead | Sort::RecentlyAdded);
        Key {
            rank: if self == Sort::RecentlyRead { ranks.get(&book.id).copied().unwrap_or(u32::MAX) } else { 0 },
            newest: Reverse(if recent { (book.stamp.modified_secs, book.stamp.modified_nanos) } else { (0, 0) }),
            // Books without an author go after those with one.
            no_lead: self == Sort::Author && lead.0.is_empty(),
            lead,
            ungrouped: group.is_none(),
            group: group.unwrap_or_default(),
            unnumbered: position.is_none(),
            position: position.unwrap_or_default(),
            name: Natural::build(&name, scratch),
            title,
        }
    }
}

/// Book id → place in the opened order, most recent first.
pub type Ranks = HashMap<String, u32>;

/// One book's place in a sort, compared field by field. The path breaks the
/// last ties; it is read from the book rather than copied here.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key {
    rank: u32,
    newest: Reverse<(u64, u32)>,
    no_lead: bool,
    lead: Natural,
    ungrouped: bool,
    group: Natural,
    unnumbered: bool,
    position: Position,
    name: Natural,
    title: Natural,
}

/// A series number in fixed point: the whole part and millionths.
pub type Position = (u64, u32);

/// Text folded for natural, case-insensitive comparison by plain byte order.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Natural(Box<[u8]>);
impl Natural {
    /// A digit run becomes `0`, its length in two bytes and its digits
    /// without leading zeros, so a longer number sorts after a shorter one and
    /// numbers sort before letters, as digits do.
    pub fn new(text: &str) -> Self {
        Self::build(text, &mut Vec::new())
    }
    /// Folds `text` in `out`, so the key takes one allocation of its exact size.
    fn build(text: &str, out: &mut Vec<u8>) -> Self {
        out.clear();
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if b.is_ascii_digit() {
                let start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                let run = &bytes[start..i];
                let digits = &run[run.iter().position(|&d| d != b'0').unwrap_or(run.len())..];
                let len = digits.len().min(u16::MAX as usize) as u16;
                out.push(b'0');
                out.extend_from_slice(&len.to_be_bytes());
                out.extend_from_slice(&digits[..len as usize]);
            } else if b.is_ascii() {
                out.push(b.to_ascii_lowercase());
                i += 1;
            } else {
                let c = text[i..].chars().next().expect("a char starts here");
                let mut buf = [0; 4];
                for lower in c.to_lowercase() {
                    out.extend_from_slice(lower.encode_utf8(&mut buf).as_bytes());
                }
                i += c.len_utf8();
            }
        }
        Self(Box::from(&out[..]))
    }
}

/// A series number from metadata (`3`, `1.5`, `3.0`); `None` when it isn't one.
pub fn parse_number(text: &str) -> Option<Position> {
    let text = text.trim();
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if (whole.is_empty() && fraction.is_empty()) || !digits(whole) || !digits(fraction) || whole.len() > 18 {
        return None;
    }
    let whole = if whole.is_empty() { 0 } else { whole.parse().ok()? };
    let millionths = fraction.bytes().chain(std::iter::repeat(b'0')).take(6).fold(0, |n, b| n * 10 + (b - b'0') as u32);
    Some((whole, millionths))
}

/// The first number in a file name or title: a run of 1–3 digits, with a
/// fraction when `.` or `_` and one more digit follow (`Sun_Eater_1_5_` is
/// 1.5). Longer runs are years and IDs and are skipped.
pub fn guess_number(text: &str) -> Option<Position> {
    let b = text.as_bytes();
    let digit = |i: usize| b.get(i).is_some_and(u8::is_ascii_digit);
    let mut i = 0;
    while i < b.len() {
        if !digit(i) {
            i += 1;
            continue;
        }
        let start = i;
        while digit(i) {
            i += 1;
        }
        if i - start > 3 {
            continue;
        }
        let whole = text[start..i].parse().expect("1-3 digits");
        let fraction = if matches!(b.get(i), Some(b'.' | b'_')) && digit(i + 1) && !digit(i + 2) {
            (b[i + 1] - b'0') as u32 * 100_000
        } else {
            0
        };
        return Some((whole, fraction));
    }
    None
}

/// Every book's key for one sort, kept until the index, the sort or the
/// opened order changes. `clear` it whenever book positions change.
#[derive(Default)]
pub struct Keys {
    built: Option<(Sort, Arc<Ranks>)>,
    keys: Vec<Key>,
}
impl Keys {
    /// The keys of `books`, by position, built when missing or stale.
    pub fn get(&mut self, books: &[Book], sort: Sort, ranks: &Arc<Ranks>) -> &[Key] {
        let fresh = self.keys.len() == books.len()
            && self
                .built
                .as_ref()
                .is_some_and(|(s, r)| *s == sort && (sort != Sort::RecentlyRead || Arc::ptr_eq(r, ranks)));
        if !fresh {
            let mut scratch = Vec::new();
            self.keys = books.iter().map(|b| sort.key(b, ranks, &mut scratch)).collect();
            self.built = Some((sort, ranks.clone()));
        }
        &self.keys
    }
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// Orders `indices` (positions in `books`) by their `keys`.
pub fn order_by_keys(books: &[Book], keys: &[Key], indices: &mut [usize]) {
    indices.sort_unstable_by(|&a, &b| keys[a].cmp(&keys[b]).then_with(|| books[a].path.cmp(&books[b].path)));
}

fn keyed(books: &[Book], indices: &[usize], sort: Sort, ranks: &Ranks) -> Vec<(Key, usize)> {
    let mut scratch = Vec::new();
    indices.iter().map(|&i| (sort.key(&books[i], ranks, &mut scratch), i)).collect()
}
fn compare(books: &[Book]) -> impl Fn(&(Key, usize), &(Key, usize)) -> Ordering {
    |(a, i), (b, j)| a.cmp(b).then_with(|| books[*i].path.cmp(&books[*j].path))
}

/// Orders `indices` (positions in `books`) by `sort`, building keys for
/// them alone.
pub fn order(books: &[Book], indices: &mut [usize], sort: Sort, ranks: &Ranks) {
    let mut keyed = keyed(books, indices, sort, ranks);
    keyed.sort_unstable_by(compare(books));
    for (slot, (_, i)) in indices.iter_mut().zip(keyed) {
        *slot = i;
    }
}

/// The slice `start..end` of `indices` once ordered by `sort`, without
/// cloning books or ordering items outside the page.
pub fn select_page(
    books: &[Book],
    indices: Vec<usize>,
    start: usize,
    end: usize,
    sort: Sort,
    ranks: &Ranks,
) -> Vec<usize> {
    if start >= indices.len() || start >= end {
        return Vec::new();
    }
    let end = end.min(indices.len());
    let cmp = compare(books);
    let mut keyed = keyed(books, &indices, sort, ranks);
    if end < keyed.len() {
        keyed.select_nth_unstable_by(end - 1, &cmp);
        keyed.truncate(end);
    }
    if start > 0 {
        keyed.select_nth_unstable_by(start, &cmp);
    }
    keyed[start..end].sort_unstable_by(&cmp);
    keyed[start..end].iter().map(|(_, i)| *i).collect()
}

/// Sorts whole books; browsing orders positions instead.
pub fn sort_books(books: &mut Vec<Book>, sort: Sort, ranks: &Ranks) {
    let mut indices: Vec<usize> = (0..books.len()).collect();
    order(books, &mut indices, sort, ranks);
    let mut taken: Vec<Option<Book>> = std::mem::take(books).into_iter().map(Some).collect();
    books.extend(indices.into_iter().map(|i| taken[i].take().expect("each book once")));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::selection_tests::book;

    struct Spec {
        path: &'static str,
        title: &'static str,
        authors: &'static str,
        author_sort: &'static str,
        series: (&'static str, &'static str),
        modified: u64,
    }
    const NONE: Spec = Spec { path: "", title: "", authors: "", author_sort: "", series: ("", ""), modified: 0 };

    fn books(specs: &[Spec]) -> Vec<Book> {
        specs
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let mut b = book(i);
                b.path = s.path.into();
                b.stamp.modified_secs = s.modified;
                let m = &mut b.metadata;
                (m.title, m.authors, m.author_sort) = (s.title.into(), s.authors.into(), s.author_sort.into());
                (m.series, m.series_index) = (s.series.0.into(), s.series.1.into());
                b
            })
            .collect()
    }
    fn ids(books: &[Book], indices: &[usize]) -> Vec<String> {
        indices.iter().map(|&i| books[i].id.clone()).collect()
    }
    fn ordered(books: &[Book], sort: Sort, ranks: &Ranks) -> Vec<String> {
        let mut indices: Vec<usize> = (0..books.len()).collect();
        order(books, &mut indices, sort, ranks);
        ids(books, &indices)
    }

    #[test]
    fn each_sort_orders_books_and_breaks_ties_the_same_way() {
        let books = books(&[
            // 0: numbered by its title only.
            Spec {
                path: "/b/a.epub",
                title: "Book 10",
                authors: "Ann Zed",
                author_sort: "Zed, Ann",
                modified: 30,
                ..NONE
            },
            // 1: no file-as, so the author as written.
            Spec { path: "/b/b.epub", title: "book 2", authors: "Bo Abe", modified: 10, ..NONE },
            Spec {
                path: "/b/m2.epub",
                title: "The Well",
                author_sort: "Sanderson, B",
                series: ("Mistborn", "2"),
                modified: 20,
                ..NONE
            },
            Spec {
                path: "/b/m1.epub",
                title: "The Final Empire",
                author_sort: "Sanderson, B",
                series: ("Mistborn", "1.0"),
                modified: 40,
                ..NONE
            },
            // 4 and 5 tie on title (case aside), author and time: the file name decides.
            Spec { path: "/b/e.epub", title: "Alpha", modified: 50, ..NONE },
            Spec { path: "/b/f.epub", title: "alpha", modified: 50, ..NONE },
        ]);
        let ranks = Ranks::from([("2".into(), 0), ("0".into(), 1), ("gone".into(), 2)]);
        for (sort, expected) in [
            (Sort::Title, ["4", "5", "1", "0", "3", "2"]),
            // Books without an author go last.
            (Sort::Author, ["1", "3", "2", "0", "4", "5"]),
            (Sort::Series, ["3", "2", "1", "0", "4", "5"]),
            // Never-opened books follow, newest first.
            (Sort::RecentlyRead, ["2", "0", "4", "5", "3", "1"]),
            (Sort::RecentlyAdded, ["4", "5", "3", "0", "2", "1"]),
        ] {
            assert_eq!(ordered(&books, sort, &ranks), expected, "{sort:?}");
        }
    }

    #[test]
    fn series_order_groups_first_then_guessed_numbers_then_the_rest() {
        let books = books(&[
            Spec { path: "/s/Cat_and_Mouse.epub", title: "Cat and Mouse", ..NONE },
            // A guessed number never joins a group, however low.
            Spec { path: "/s/00 - Prologue.epub", title: "Prologue", ..NONE },
            Spec { path: "/s/Mistborn 3.epub", title: "Hero", series: ("Mistborn", "3"), ..NONE },
            Spec { path: "/s/x.epub", title: "Secret", series: ("Mistborn", ""), ..NONE },
            Spec { path: "/s/y.epub", title: "Elantris", series: ("elantris", "1"), ..NONE },
            Spec { path: "/s/Saga_2_5_-_Interlude.epub", title: "Interlude", ..NONE },
            // The file name wins over the title; the title stands in when the name has no number.
            Spec { path: "/s/saga-3.epub", title: "Saga Book 9", ..NONE },
            Spec { path: "/s/saga.epub", title: "Saga Book 7", ..NONE },
            Spec { path: "/s/Aardvark.epub", title: "Aardvark", ..NONE },
        ]);
        // Groups by name, numbered books before the group's unnumbered ones.
        assert_eq!(ordered(&books, Sort::Series, &Ranks::new()), ["4", "2", "3", "1", "5", "6", "7", "8", "0"]);
    }

    #[test]
    fn numbers_guessed_from_names() {
        for (name, expected) in [
            ("01 - A Song of Dragons", Some((1, 0))),
            ("1.  Jane Roe - Traitor's Oath", Some((1, 0))),
            ("Roe_Jane_-_Ashsworn_2_-_The_Hollow", Some((2, 0))),
            ("Sun_Walker_1_5_-_Demon_in_Grey", Some((1, 500_000))),
            ("Moths 2.5 Interlude", Some((2, 500_000))),
            ("Empire of the Moths 12 - Night", Some((12, 0))),
            // Four or more digits are years and IDs.
            ("RuLit_Me_739338", None),
            ("Atlas 2019 Edition 4", Some((4, 0))),
            // A fraction is one digit.
            ("Saga_1_50_Notes", Some((1, 0))),
            ("No number here", None),
        ] {
            assert_eq!(guess_number(name), expected, "{name}");
        }
        for (index, expected) in
            [("3", Some((3, 0))), ("3.0", Some((3, 0))), (" 1.5 ", Some((1, 500_000))), ("III", None), ("", None)]
        {
            assert_eq!(parse_number(index), expected, "{index:?}");
        }
    }

    #[test]
    fn natural_text_ignores_case_and_compares_numbers_by_value() {
        let mut names = ["book 10", "Book 2", "book 02b", "Book", "book 1a", "Book!", "Äpfel", "äb", "Zed"];
        names.sort_by_key(|n| Natural::new(n));
        assert_eq!(names, ["Book", "book 1a", "Book 2", "book 02b", "book 10", "Book!", "Zed", "äb", "Äpfel"]);
    }

    #[test]
    fn select_page_and_kept_keys_match_full_order_for_every_sort() {
        let mut books: Vec<_> = (0..25).map(book).collect();
        books[0].metadata.title = "beta".into();
        books[1].metadata.title = "Alpha".into();
        books[2].metadata.title = "alpha".into();
        books[3].metadata.title = "Война и мир".into();
        books[4].metadata.title = "война и мир".into();
        books[1].metadata.authors = "Zed".into();
        books[2].metadata.author_sort = "ann".into();
        books[3].metadata.authors = "Толстой".into();
        books[5].stamp = books[6].stamp.clone();
        books[5].metadata.title = books[6].metadata.title.clone();
        books[7].metadata.series = "Saga".into();
        books[7].metadata.series_index = "2".into();
        books[8].metadata.series = "Saga".into();
        books[9].path = "/books/x 3.epub".into();
        let ranks = Ranks::from([("12".into(), 0), ("3".into(), 1)]);
        let shared = Arc::new(ranks.clone());
        let mut keys = Keys::default();
        for sort in Sort::ALL {
            let full = ordered(&books, sort, &ranks);
            let mut cached: Vec<usize> = (0..books.len()).collect();
            order_by_keys(&books, keys.get(&books, sort, &shared), &mut cached);
            assert_eq!(ids(&books, &cached), full, "{sort:?} with kept keys");
            for (start, end) in [(0, 8), (8, 16), (16, 24), (24, 32), (3, 11), (0, 25), (30, 38)] {
                let picked = select_page(&books, (0..books.len()).collect(), start, end, sort, &ranks);
                let expected = &full[start.min(full.len())..end.min(full.len())];
                assert_eq!(ids(&books, &picked), expected, "{sort:?} {start}..{end}");
            }
        }
    }
}
