//! A bounded display cache, never the authoritative reading position.
use super::Book;
use crate::store::BookState;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Five recent candidates allow four recents even when the featured book is one
/// of them. One additional eligible resume candidate handles older unfinished books.
pub const RECENT_CANDIDATES: usize = 5;
pub const MAX_BOOKS: usize = RECENT_CANDIDATES + 1;

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
        book.error = None;
        Self { book, status: state.into() }
    }
    pub fn can_resume(&self) -> bool {
        self.book.available
            && self.status.has_position
            && (!self.status.finished || self.status.rereading)
            && !self.status.hidden_from_continue
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
    pub const VERSION: u32 = 3;
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
    pub fn select(candidates: impl IntoIterator<Item = Entry>) -> Self {
        let mut books: Vec<_> = candidates.into_iter().filter(|entry| entry.book.available).collect();
        books.sort_by(|a, b| (b.status.last_opened, &b.book.id).cmp(&(a.status.last_opened, &a.book.id)));
        let mut seen = HashSet::new();
        books.retain(|entry| seen.insert(entry.book.id.clone()));
        let resume = books.iter().find(|entry| entry.can_resume()).map(|entry| entry.book.id.clone());
        let mut recent = 0;
        books.retain(|entry| {
            let keep_recent = entry.status.last_opened > 0 && recent < RECENT_CANDIDATES;
            recent += usize::from(keep_recent);
            keep_recent || Some(&entry.book.id) == resume.as_ref()
        });
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
        assert_eq!(summary.books.len(), MAX_BOOKS);
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
    fn featured_recent_is_excluded_and_unopened_books_do_not_fill_empty_home() {
        let empty = Summary::select((0..10).map(|id| Entry::new(&book(id), &BookState::default())));
        assert!(empty.books.is_empty());
        let state = BookState { position: Position("anchor".into()), last_opened: 7, ..Default::default() };
        let summary = Summary::select([Entry::new(&book(1), &state), Entry::new(&book(2), &state)]);
        assert_eq!(summary.resume().unwrap().book.id, "2");
        assert_eq!(summary.recents(4)[0].book.id, "1");
    }
}
