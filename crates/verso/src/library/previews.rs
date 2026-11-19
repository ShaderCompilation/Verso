//! Which books a folder tile previews. A bounded, derived order of recently
//! opened books (`opened.json`) stands in for reading every book's state.
use super::{Book, Cover, LibraryIndex, collections::Collections};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Older books fall back to title order in previews.
pub const MAX_OPENED: usize = 1000;
/// Covers shows four; Stack takes the first three and Latest book the first.
pub const SLOTS: usize = 4;

/// Book ids by last opened, newest first. A display cache only: never a
/// reading position, rebuilt from the reading states when missing or invalid.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Opened {
    pub version: u32,
    /// A save can seed a partial order before the first full rebuild, like
    /// Home's summary. Only a rebuild from every state completes it.
    #[serde(default)]
    pub complete: bool,
    /// (book id, last opened). The leader's time may lag its newest saves:
    /// saves of the leading book don't rewrite the file.
    pub books: Vec<(String, u64)>,
}
impl Default for Opened {
    fn default() -> Self {
        Self { version: Self::VERSION, complete: false, books: Vec::new() }
    }
}
impl Opened {
    pub const VERSION: u32 = 1;
    pub fn valid(&self) -> bool {
        self.version == Self::VERSION
            && self.books.len() <= MAX_OPENED
            && self.books.iter().all(|(id, at)| !id.is_empty() && *at > 0)
            && self.books.iter().map(|(id, _)| id).collect::<HashSet<_>>().len() == self.books.len()
    }
    /// The order of every book's last-opened time; never-opened books stay out.
    pub fn select(entries: impl IntoIterator<Item = (String, u64)>) -> Self {
        let mut books: Vec<_> = entries.into_iter().filter(|(_, at)| *at > 0).collect();
        books.sort_by(|a, b| (b.1, &b.0).cmp(&(a.1, &a.0)));
        let mut seen = HashSet::new();
        books.retain(|(id, _)| seen.insert(id.clone()));
        books.truncate(MAX_OPENED);
        Self { books, ..Default::default() }
    }
    pub fn leader(&self) -> Option<&str> {
        self.books.first().map(|(id, _)| id.as_str())
    }
    /// Records a saved reading state. Returns whether the order changed.
    /// A save that doesn't open the book (marking it finished) keeps its
    /// place, because its last-opened time is unchanged.
    pub fn record(&mut self, id: &str, at: u64) -> bool {
        if at == 0 || self.leader() == Some(id) {
            return false;
        }
        let old = self.books.iter().position(|(other, _)| other == id);
        let entry = old.map(|i| self.books.remove(i)).filter(|(_, saved)| *saved == at);
        let place = self.books.iter().position(|(_, other)| *other <= at).unwrap_or(self.books.len());
        let changed = old != Some(place) || entry.is_none();
        self.books.insert(place, (id.to_owned(), at));
        self.books.truncate(MAX_OPENED);
        changed
    }
    /// Each book's place in the order, for [`Previews`].
    pub fn ranks(&self) -> HashMap<String, u32> {
        self.books.iter().enumerate().map(|(rank, (id, _))| (id.clone(), rank as u32)).collect()
    }
}

/// Same order as the Title sort: title, then author, then path, ignoring
/// case. Compared without allocating, because most candidates lose early.
pub fn title_order(a: &Book, b: &Book) -> Ordering {
    fn folded(a: &str, b: &str) -> Ordering {
        a.chars().flat_map(char::to_lowercase).cmp(b.chars().flat_map(char::to_lowercase))
    }
    folded(&a.metadata.title, &b.metadata.title)
        .then_with(|| folded(&a.metadata.authors, &b.metadata.authors))
        .then_with(|| a.path.cmp(&b.path))
}

struct Candidate<'a> {
    no_art: bool,
    /// The place in the opened order; `u32::MAX` for books never opened.
    recent: u32,
    nested: bool,
    book: &'a Book,
    index: usize,
}
impl Candidate<'_> {
    fn order(&self, other: &Self) -> Ordering {
        (self.no_art, self.recent, self.nested)
            .cmp(&(other.no_art, other.recent, other.nested))
            .then_with(|| title_order(self.book, other.book))
    }
}

/// The books `folder`'s tile previews, best first, as positions in `index`:
/// books read before, most recent first; then the rest by title, books
/// directly in the folder before those in subfolders. Books without cover art
/// come after every book with art, so they only fill slots that art can't;
/// a pending cover counts as art. Empty when no book in the folder has art.
/// Walks only the folder's own subtree, once.
pub fn pick(
    index: &LibraryIndex,
    collections: &Collections,
    folder: &Path,
    ranks: &HashMap<String, u32>,
) -> Vec<usize> {
    let mut best: Vec<Candidate> = Vec::with_capacity(SLOTS + 1);
    let mut stack = vec![folder];
    while let Some(path) = stack.pop() {
        let Some(node) = collections.folders.get(path) else { continue };
        let nested = path != folder;
        for &i in &node.indices {
            let Some(book) = index.books.get(i) else { continue };
            let recent = ranks.get(book.id.as_str()).copied().unwrap_or(u32::MAX);
            let candidate = Candidate {
                no_art: matches!(book.cover, Cover::Unavailable(_)),
                recent,
                // The opened order alone ranks books read before.
                nested: nested && recent == u32::MAX,
                book,
                index: i,
            };
            if best.len() == SLOTS && candidate.order(best.last().unwrap()) != Ordering::Less {
                continue;
            }
            let place = best.partition_point(|other| other.order(&candidate) == Ordering::Less);
            best.insert(place, candidate);
            best.truncate(SLOTS);
        }
        stack.extend(node.children.iter().map(PathBuf::as_path));
    }
    if best.first().is_none_or(|c| c.no_art) {
        return Vec::new();
    }
    best.into_iter().map(|c| c.index).collect()
}

/// Picks for the folders shown so far. Positions refer to the current index,
/// so every new index or opened order clears them.
#[derive(Default)]
pub struct Previews {
    /// None until the opened order is first needed.
    ranks: Option<Arc<HashMap<String, u32>>>,
    picks: HashMap<PathBuf, Vec<usize>>,
}
impl Previews {
    pub fn has_order(&self) -> bool {
        self.ranks.is_some()
    }
    pub fn ranks(&self) -> Option<Arc<HashMap<String, u32>>> {
        self.ranks.clone()
    }
    pub fn set_ranks(&mut self, ranks: Arc<HashMap<String, u32>>) {
        self.ranks = Some(ranks);
        self.picks.clear();
    }
    pub fn set_order(&mut self, opened: &Opened) {
        self.set_ranks(Arc::new(opened.ranks()));
    }
    /// The order changed elsewhere; it is read again when next needed.
    pub fn forget_order(&mut self) {
        self.ranks = None;
        self.picks.clear();
    }
    /// The index, its folders or a cover state changed.
    pub fn index_changed(&mut self) {
        self.picks.clear();
    }
    /// True when `id` leads the loaded order.
    pub fn leads(&self, id: &str) -> bool {
        self.ranks.as_ref().is_some_and(|ranks| ranks.get(id) == Some(&0))
    }
    /// Computes the picks of `folders` not yet known. Needs the order loaded.
    pub fn update<'a>(
        &mut self,
        index: &LibraryIndex,
        collections: &Collections,
        folders: impl IntoIterator<Item = &'a PathBuf>,
    ) {
        let Some(ranks) = &self.ranks else { return };
        for folder in folders {
            if !self.picks.contains_key(folder) {
                self.picks.insert(folder.clone(), pick(index, collections, folder, ranks));
            }
        }
    }
    pub fn get(&self, folder: &Path) -> Option<&[usize]> {
        self.picks.get(folder).map(Vec::as_slice)
    }
}

/// Explicit opt-in diagnostic (`VERSO_PREVIEW_BENCH=1`): the cost of picking
/// previews for every page of folder tiles, from a fresh cache, over the
/// selected index. Reads `opened.json`, or builds the order from the reading
/// states without writing anything. Emits aggregate numbers only.
pub fn benchmark(store: &crate::store::Store, index: &LibraryIndex, roots: &[PathBuf]) -> serde_json::Value {
    use std::time::Instant;
    const PAGE: usize = 8;
    fn ms(started: Instant) -> f64 {
        started.elapsed().as_secs_f64() * 1000.0
    }
    fn percentile(values: &[f64], p: usize) -> f64 {
        let mut values = values.to_vec();
        values.sort_by(f64::total_cmp);
        values.get((values.len() * p).div_ceil(100).saturating_sub(1)).copied().unwrap_or(0.0)
    }
    let started = Instant::now();
    let saved = store.opened();
    let from_file = saved.is_some();
    let opened = saved
        .unwrap_or_else(|| Opened::select(index.books.iter().map(|b| (b.id.clone(), store.book(&b.id).last_opened))));
    let order_ms = ms(started);
    let bytes = serde_json::to_vec(&opened).unwrap_or_default();
    let started = Instant::now();
    let parsed = serde_json::from_slice::<Opened>(&bytes).is_ok_and(|o| o.valid());
    let parse_ms = ms(started);
    let started = Instant::now();
    let collections = Collections::from_index(index, roots);
    let collections_ms = ms(started);
    // The first build in a process also pays for heap growth.
    let started = Instant::now();
    let ranks = opened.ranks();
    let ranks_ms = ms(started);
    let warm: Vec<_> = (0..5)
        .map(|_| {
            let started = Instant::now();
            std::hint::black_box(opened.ranks());
            ms(started)
        })
        .collect();
    let views = std::iter::once(None).chain(collections.folders.keys().map(|p| Some(p.as_path())));
    let (mut pages, mut empty) = (Vec::new(), 0);
    for view in views {
        for page in collections.children(view).chunks(PAGE) {
            let started = Instant::now();
            let picks: Vec<_> = page.iter().map(|folder| pick(index, &collections, folder, &ranks)).collect();
            pages.push(ms(started));
            empty += picks.iter().filter(|p| p.is_empty()).count();
        }
    }
    serde_json::json!({
        "books": index.books.len(),
        "folders": collections.folders.len(),
        "opened": opened.books.len(),
        "opened_from_file": from_file,
        "opened_bytes": bytes.len(),
        "order_ms": order_ms,
        "opened_parse_ms": parse_ms,
        "opened_parse_ok": parsed,
        "ranks_first_ms": ranks_ms,
        "ranks_warm_p50_ms": percentile(&warm, 50),
        "collections_ms": collections_ms,
        "pages": pages.len(),
        "folders_without_art": empty,
        "page_pick_p50_ms": percentile(&pages, 50),
        "page_pick_p90_ms": percentile(&pages, 90),
        "page_pick_max_ms": pages.iter().copied().fold(0.0, f64::max),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::{selection_tests::book, sort_books};

    fn ids(index: &LibraryIndex, picks: &[usize]) -> Vec<String> {
        picks.iter().map(|&i| index.books[i].id.clone()).collect()
    }
    fn library(paths: &[(&str, &str)]) -> LibraryIndex {
        let books = paths
            .iter()
            .enumerate()
            .map(|(i, (path, title))| {
                let mut b = book(i);
                b.path = path.into();
                b.metadata.title = (*title).into();
                b.cover = Cover::Ready(b.id.clone());
                b
            })
            .collect();
        LibraryIndex { books, ..Default::default() }
    }

    #[test]
    fn recent_first_then_direct_books_by_title_then_subfolders() {
        let mut index = library(&[
            ("/b/F/Sub/a.epub", "Alpha"),     // 0
            ("/b/F/z.epub", "zulu"),          // 1
            ("/b/F/m.epub", "Mike"),          // 2
            ("/b/F/Sub/Deep/x.epub", "Xray"), // 3
            ("/b/F/c.epub", "charlie"),       // 4
            ("/b/Other/o.epub", "Oscar"),     // 5
        ]);
        let tree = Collections::from_index(&index, &["/b".into()]);
        let none = HashMap::new();
        let folder = Path::new("/b/F");
        assert_eq!(ids(&index, &pick(&index, &tree, folder, &none)), ["4", "2", "1", "0"]);
        // Read before: most recent first, wherever it lies in the folder.
        let opened = Opened::select([("3".into(), 20), ("5".into(), 30), ("1".into(), 10)]);
        let ranks = opened.ranks();
        assert_eq!(ids(&index, &pick(&index, &tree, folder, &ranks)), ["3", "1", "4", "2"]);
        // Title order is the Title sort's, case-insensitive.
        let mut direct: Vec<_> = index.books[1..=4].iter().filter(|b| b.id != "3").cloned().collect();
        sort_books(&mut direct, &HashMap::new(), crate::library::Sort::Title);
        assert_eq!(direct.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(), ["4", "2", "1"]);
        // Only books inside the folder count.
        let sub = pick(&index, &tree, Path::new("/b/F/Sub"), &ranks);
        assert_eq!(ids(&index, &sub), ["3", "0"]);
        // A move keeps ids, so the order follows the book to its new folder.
        index.move_path(Path::new("/b/F/Sub/Deep"), Path::new("/b/Other/Deep"));
        let tree = Collections::from_index(&index, &["/b".into()]);
        assert_eq!(ids(&index, &pick(&index, &tree, Path::new("/b/Other"), &ranks)), ["5", "3"]);
        assert_eq!(ids(&index, &pick(&index, &tree, folder, &ranks)), ["1", "4", "2", "0"]);
        // A removed book is never picked, even while the order still lists it.
        index.remove_path(Path::new("/b/F/z.epub"));
        let tree = Collections::from_index(&index, &["/b".into()]);
        assert_eq!(ids(&index, &pick(&index, &tree, folder, &ranks)), ["4", "2", "0"]);
    }

    #[test]
    fn books_without_art_fill_only_slots_art_cannot_and_pending_counts_as_art() {
        let mut index = library(&[
            ("/b/F/a.epub", "a"),
            ("/b/F/b.epub", "b"),
            ("/b/F/c.epub", "c"),
            ("/b/F/d.epub", "d"),
            ("/b/F/e.epub", "e"),
            ("/b/F/f.epub", "f"),
        ]);
        for i in [0, 2] {
            index.books[i].cover = Cover::Unavailable("k".into());
        }
        index.books[3].cover = Cover::Pending;
        let tree = Collections::from_index(&index, &["/b".into()]);
        let ranks = Opened::select([("0".into(), 9)]).ranks();
        let folder = Path::new("/b/F");
        assert_eq!(ids(&index, &pick(&index, &tree, folder, &ranks)), ["1", "3", "4", "5"]);
        index.books[5].cover = Cover::Unavailable("k".into());
        index.books[4].cover = Cover::Unavailable("k".into());
        // Too few with art: the rest follow in the same order, recent first.
        assert_eq!(ids(&index, &pick(&index, &tree, folder, &ranks)), ["1", "3", "0", "2"]);
        // The modes take prefixes of one pick, so Latest book is the mosaic's first.
        index.books[1].cover = Cover::Unavailable("k".into());
        index.books[3].cover = Cover::Unavailable("k".into());
        assert!(pick(&index, &tree, folder, &ranks).is_empty(), "no art: the Icon tile");
    }

    #[test]
    fn empty_folders_and_several_roots() {
        let mut index = library(&[("/a/X/one.epub", "One"), ("/b/X/two.epub", "Two"), ("/a/loose.epub", "Loose")]);
        index.empty_folders = vec!["/a/Empty".into(), "/a/Parent/Child".into()];
        index.books.push({
            let mut b = book(9);
            b.path = "/a/X/gone.epub".into();
            b.available = false;
            b
        });
        let tree = Collections::from_index(&index, &["/a".into(), "/b".into()]);
        let ranks = Opened::select([("9".into(), 50), ("1".into(), 40)]).ranks();
        assert!(pick(&index, &tree, Path::new("/a/Empty"), &ranks).is_empty());
        assert!(pick(&index, &tree, Path::new("/a/Parent"), &ranks).is_empty());
        assert!(pick(&index, &tree, Path::new("/missing"), &ranks).is_empty());
        // Each root picks from its own books; unavailable books never appear.
        assert_eq!(ids(&index, &pick(&index, &tree, Path::new("/a"), &ranks)), ["2", "0"]);
        assert_eq!(ids(&index, &pick(&index, &tree, Path::new("/b"), &ranks)), ["1"]);
    }

    #[test]
    fn opened_order_records_saves_and_stays_bounded() {
        let mut opened = Opened::select([("a".into(), 5), ("b".into(), 9), ("c".into(), 0), ("a".into(), 1)]);
        assert_eq!(opened.books, [("b".into(), 9), ("a".into(), 5)]);
        assert!(opened.valid());
        // Saves of the leading book (page turns) change nothing on disk.
        assert!(!opened.record("b", 12));
        assert!(opened.record("c", 20));
        assert!(opened.record("a", 21));
        assert_eq!(opened.leader(), Some("a"));
        // Marking a book finished saves its unchanged time: it keeps its place.
        assert!(!opened.record("c", 20));
        assert!(!opened.record("never", 0));
        assert_eq!(opened.books.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["a", "c", "b"]);
        let mut full = Opened::select((0..MAX_OPENED as u64 + 5).map(|i| (i.to_string(), i + 1)));
        assert_eq!(full.books.len(), MAX_OPENED);
        assert_eq!(full.leader(), Some("1004"));
        assert!(full.record("new", 5000));
        assert_eq!((full.books.len(), full.leader()), (MAX_OPENED, Some("new")));
        let mut invalid = full.clone();
        invalid.books.push(("new".into(), 1));
        assert!(!invalid.valid());
    }

    #[test]
    fn previews_cache_until_index_or_order_changes() {
        let index = library(&[("/b/F/a.epub", "a"), ("/b/F/b.epub", "b")]);
        let tree = Collections::from_index(&index, &["/b".into()]);
        let folder = PathBuf::from("/b/F");
        let mut previews = Previews::default();
        previews.update(&index, &tree, [&folder]);
        assert!(previews.get(&folder).is_none(), "nothing is picked before the order loads");
        previews.set_order(&Opened::default());
        previews.update(&index, &tree, [&folder]);
        assert_eq!(previews.get(&folder), Some(&[0, 1][..]));
        previews.set_order(&Opened::select([("1".into(), 3)]));
        assert!(previews.get(&folder).is_none());
        assert!(previews.leads("1") && !previews.leads("0"));
        previews.update(&index, &tree, [&folder]);
        assert_eq!(previews.get(&folder), Some(&[1, 0][..]));
        previews.index_changed();
        assert!(previews.get(&folder).is_none() && previews.has_order());
        previews.forget_order();
        assert!(!previews.has_order());
    }
}
