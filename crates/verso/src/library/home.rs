//! A bounded display cache, never the authoritative reading position.
use super::Book;
use crate::store::BookState;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Five recent candidates allow four recents even when the featured book is one
/// of them. One additional eligible resume candidate handles older unfinished books.
pub const RECENT_CANDIDATES: usize = 5;
/// Never-opened books kept while Home has no recents: two rows of four.
pub const SHELF: usize = 8;
/// One resume book plus either the recents or the shelf.
pub const MAX_BOOKS: usize = if RECENT_CANDIDATES > SHELF { RECENT_CANDIDATES + 1 } else { SHELF + 1 };

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub has_position: bool,
    pub progress: u32,
    pub last_opened: u64,
    pub finished: bool,
    pub rereading: bool,
    pub hidden_from_continue: bool,
}
impl From<&BookState> for Status {
    fn from(state: &BookState) -> Self {
        Self {
            has_position: !state.position.0.is_empty(),
            progress: state.progress.min(10000),
            last_opened: state.last_opened,
            finished: state.finished,
            rereading: state.finished && state.rereading,
            hidden_from_continue: state.hidden_from_continue,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Reuses library display/cover/opening APIs. Source parsing fields are
    /// stripped; this Book must never replace an entry in the authoritative index.
    pub book: Book,
    pub status: Status,
}
impl Entry {
    pub fn new(book: &Book, state: &BookState) -> Self {
        let mut book = book.clone();
        book.fingerprint.clear();
        book.metadata.identifier.clear();
        book.metadata.language.clear();
        book.metadata.cover = None;
        book.metadata.title = book.metadata.title.chars().take(512).collect();
        book.metadata.authors = book.metadata.authors.chars().take(512).collect();
        // Only whether there is one.
        book.error = book.error.map(|_| String::new());
        Self { book, status: state.into() }
    }
    pub fn can_resume(&self) -> bool {
        self.book.available
            && self.status.has_position
            && (!self.status.finished || self.status.rereading)
            && !self.status.hidden_from_continue
    }
    /// Never opened, readable and not finished elsewhere.
    fn can_start(&self) -> bool {
        self.book.available
            && self.status.last_opened == 0
            && !self.status.has_position
            && !self.status.finished
            && self.book.error.is_none()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub version: u32,
    /// A successful open can seed a usable partial cache before migration or
    /// after cache failure. Only a full background rebuild establishes history.
    #[serde(default)]
    pub complete: bool,
    pub books: Vec<Entry>,
    /// Cached top-level Library counts so Home renders its Library card without
    /// loading `library.json` or building `Collections` on startup.
    #[serde(default)]
    pub library_root_count: usize,
    #[serde(default)]
    pub library_folders: usize,
    #[serde(default)]
    pub library_books: usize,
}
impl Default for Summary {
    fn default() -> Self {
        Self {
            version: Self::VERSION,
            complete: false,
            books: Vec::new(),
            library_root_count: 0,
            library_folders: 0,
            library_books: 0,
        }
    }
}
impl Summary {
    pub const VERSION: u32 = 4;
    pub fn set_library_counts(&mut self, (roots, folders, books): (usize, usize, usize)) {
        self.library_root_count = roots;
        self.library_folders = folders;
        self.library_books = books;
    }
    pub fn valid(&self) -> bool {
        self.version == Self::VERSION
            && self.books.len() <= MAX_BOOKS
            && self.books.iter().all(|entry| !entry.book.id.is_empty())
            && self.books.iter().map(|entry| &entry.book.id).collect::<HashSet<_>>().len() == self.books.len()
    }
    pub fn resume(&self) -> Option<&Entry> {
        self.books.iter().find(|entry| entry.can_resume())
    }
    pub fn recents(&self, limit: usize) -> Vec<&Entry> {
        let featured = self.resume().map(|entry| &entry.book.id);
        self.books
            .iter()
            .filter(|entry| entry.book.available && entry.status.last_opened > 0 && Some(&entry.book.id) != featured)
            .take(limit.min(4))
            .collect()
    }
    /// Keeps the recents and the resume book. Without recents it also keeps
    /// never-opened books, so an empty selection means an empty library.
    pub fn select(candidates: impl IntoIterator<Item = Entry>) -> Self {
        let mut books: Vec<_> = candidates.into_iter().filter(|entry| entry.book.available).collect();
        books.sort_by(|a, b| (b.status.last_opened, &b.book.id).cmp(&(a.status.last_opened, &a.book.id)));
        let mut seen = HashSet::new();
        books.retain(|entry| seen.insert(entry.book.id.clone()));
        let resume = books.iter().find(|entry| entry.can_resume()).map(|entry| entry.book.id.clone());
        let is_resume = |entry: &Entry| Some(&entry.book.id) == resume.as_ref();
        let (mut books, mut unopened): (Vec<_>, Vec<_>) =
            books.into_iter().partition(|entry| entry.status.last_opened > 0 || is_resume(entry));
        let mut recent = 0;
        books.retain(|entry| {
            let keep_recent = entry.status.last_opened > 0 && recent < RECENT_CANDIDATES;
            recent += usize::from(keep_recent);
            keep_recent || is_resume(entry)
        });
        if books.iter().all(is_resume) {
            // Newest first, the order pre-layout prepares unopened books in.
            let newest = |e: &Entry| (e.book.stamp.modified_secs, e.book.stamp.modified_nanos);
            unopened.sort_by(|a, b| {
                (!a.can_start(), newest(b), &a.book.path).cmp(&(!b.can_start(), newest(a), &b.book.path))
            });
            books.extend(unopened.into_iter().take(SHELF));
        }
        Self { books, ..Default::default() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{document::Position, library::selection_tests::book};

    #[test]
    fn older_resume_is_retained_with_four_finished_or_hidden_recents() {
        let mut entries: Vec<_> = (0..30)
            .map(|id| {
                Entry::new(
                    &book(id),
                    &BookState {
                        position: Position("saved".into()),
                        last_opened: id as u64 + 1,
                        finished: id != 0,
                        ..Default::default()
                    },
                )
            })
            .collect();
        entries[29].status.hidden_from_continue = true;
        entries[29].status.finished = false;
        entries[28].book.available = false;
        entries.push(entries[29].clone());
        let summary = Summary::select(entries);
        assert!(summary.valid());
        assert_eq!(summary.books.len(), RECENT_CANDIDATES + 1);
        assert_eq!(summary.resume().unwrap().book.id, "0");
        assert_eq!(summary.recents(4).iter().map(|e| e.book.id.as_str()).collect::<Vec<_>>(), ["29", "27", "26", "25"]);
        assert!(!serde_json::to_string(&summary).unwrap().contains("saved"));
    }

    #[test]
    fn a_rereading_finished_book_can_resume_and_a_finished_one_cannot() {
        let state = BookState { position: Position("p".into()), last_opened: 3, finished: true, ..Default::default() };
        assert!(!Entry::new(&book(1), &state).can_resume());
        let rereading = BookState { rereading: true, ..state.clone() };
        assert!(Entry::new(&book(1), &rereading).can_resume());
        assert!(!Entry::new(&book(1), &BookState { hidden_from_continue: true, ..rereading }).can_resume());
        // Re-reading means nothing without finished.
        let stray = BookState { finished: false, rereading: true, ..state };
        assert!(!Entry::new(&book(1), &stray).status.rereading);
    }

    #[test]
    fn never_opened_books_stay_until_there_are_recents_newest_first() {
        let opened = |id, at| {
            let state = BookState { position: Position("p".into()), last_opened: at, ..Default::default() };
            Entry::new(&book(id), &state)
        };
        let mut unopened: Vec<_> = (0..10).map(|id| Entry::new(&book(id), &BookState::default())).collect();
        unopened[9].status.finished = true;
        unopened[8].book.error = Some(String::new());
        let ids = |s: &Summary| s.books.iter().map(|e| e.book.id.clone()).collect::<Vec<_>>();
        let summary = Summary::select(unopened.clone());
        assert_eq!(ids(&summary), ["7", "6", "5", "4", "3", "2", "1", "0"]);
        assert_eq!(Summary::select(summary.books.clone()), summary, "a save selects from the summary's own books");
        // Books that can't be started still say the library has books.
        assert_eq!(ids(&Summary::select(unopened[8..].to_vec())), ["9", "8"]);
        // The first book opened leaves them; a second one is a recent.
        let mut books = summary.books.clone();
        books.push(opened(7, 5));
        books.rotate_right(1);
        let one = Summary::select(books);
        assert!(one.valid() && one.resume().is_some_and(|e| e.book.id == "7"));
        assert_eq!(ids(&one), ["7", "6", "5", "4", "3", "2", "1", "0"]);
        let mut books = one.books.clone();
        books.insert(0, opened(6, 9));
        assert_eq!(ids(&Summary::select(books)), ["6", "7"]);
        // Full: the resume book and a whole shelf.
        let full = Summary::select(unopened.iter().cloned().chain([opened(20, 5)]));
        assert!(full.valid() && full.books.len() == MAX_BOOKS && full.recents(4).is_empty());
        unopened.iter_mut().for_each(|e| e.book.available = false);
        assert!(Summary::select(unopened).books.is_empty());
    }

    #[test]
    fn featured_recent_is_excluded() {
        let state = BookState { position: Position("anchor".into()), last_opened: 7, ..Default::default() };
        let summary = Summary::select([Entry::new(&book(1), &state), Entry::new(&book(2), &state)]);
        assert_eq!(summary.resume().unwrap().book.id, "2");
        assert_eq!(summary.recents(4)[0].book.id, "1");
    }
}
