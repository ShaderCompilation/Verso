//! Derived Home writes follow authoritative book writes and serialize with
//! background rebuilds. No reading position is ever copied out of this cache.
//! The opened order for folder previews (`opened.json`) follows the same rules.
use super::{BookState, Store, atomic, read};
use crate::library::{
    Book, LibraryIndex,
    home::{Entry, Summary},
    previews::Opened,
};
use anyhow::{Result, ensure};
use std::sync::Mutex;

// There is one app process and one ordered position writer. This also covers
// independent Store handles for the same root and prevents .tmp-file collisions.
static SUMMARY_WRITE: Mutex<()> = Mutex::new(());
const MAX_BYTES: u64 = 64 * 1024;
/// 1,000 entries of the longest generated ids take about 70 KiB.
const MAX_OPENED_BYTES: u64 = 256 * 1024;

impl Store {
    /// Only reads the small summary file. Does not load the index, per-book
    /// records, source EPUBs, covers or collection hierarchy.
    pub fn home_summary(&self) -> Option<Summary> {
        let path = self.root.join("home.json");
        if std::fs::metadata(&path).ok()?.len() > MAX_BYTES {
            return None;
        }
        read::<Summary>(&path).filter(Summary::valid)
    }

    /// Stage display metadata without changing recency. Failed opens therefore
    /// never enter Home. The first successful presented-position save adds it.
    pub fn select_home_book(&self, book: &Book) {
        self.selected_home.lock().unwrap().replace(book.clone());
    }

    /// Only reads the small opened-order file; None when missing or invalid.
    pub fn opened(&self) -> Option<Opened> {
        let path = self.root.join("opened.json");
        if std::fs::metadata(&path).ok()?.len() > MAX_OPENED_BYTES {
            return None;
        }
        read::<Opened>(&path).filter(Opened::valid)
    }

    /// Library-worker operation, including migration of older installations.
    /// Reads existing records but never modifies any of them. The same reads
    /// rebuild the opened order; returns whether that file changed.
    /// Stops between records when `cancelled` returns true (shutdown). A
    /// cancelled rebuild removes the derived caches so the next launch
    /// rebuilds them from authoritative state.
    pub fn rebuild_home_summary_until(
        &self,
        index: &LibraryIndex,
        counts: (usize, usize, usize),
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(Summary, bool)> {
        let started = std::time::Instant::now();
        let _guard = SUMMARY_WRITE.lock().unwrap();
        let stopped = std::cell::Cell::new(false);
        let mut opened = Vec::new();
        let mut summary = Summary::select(index.books.iter().map_while(|book| {
            stopped.set(cancelled());
            (!stopped.get()).then(|| {
                let state = self.book(&book.id);
                opened.push((book.id.clone(), state.last_opened));
                Entry::new(book, &state)
            })
        }));
        if stopped.get() || cancelled() {
            self.invalidate_home_locked();
            anyhow::bail!("Home rebuild cancelled");
        }
        summary.complete = true;
        summary.set_library_counts(counts);
        if let Err(error) = self.persist_home(&summary) {
            self.invalidate_home_locked();
            return Err(error);
        }
        let mut opened = Opened::select(opened);
        opened.complete = true;
        let opened_changed = self.persist_opened(&opened);
        log::info!(
            "startup stage: Home rebuild books={} opened={} elapsed_us={}",
            index.books.len(),
            opened.books.len(),
            started.elapsed().as_micros()
        );
        Ok((summary, opened_changed))
    }

    fn invalidate_home_locked(&self) {
        let _ = std::fs::remove_file(self.root.join("home.json"));
        let _ = std::fs::remove_file(self.root.join("opened.json"));
        *self.opened_leader.lock().unwrap() = None;
    }
    #[cfg(test)]
    pub fn rebuild_home_summary(&self, index: &LibraryIndex) -> Result<Summary> {
        let roots = self.library_folders();
        let counts = crate::library::collections::Collections::from_index(index, &roots).top_level_counts(&roots);
        self.rebuild_home_summary_until(index, counts, &|| false).map(|(summary, _)| summary)
    }

    /// Confirmed cover/metadata changes need only update the bounded selection.
    /// Availability/identity changes use a full background rebuild instead.
    pub fn refresh_home_metadata(&self, index: &LibraryIndex) -> Result<Option<Summary>> {
        let _guard = SUMMARY_WRITE.lock().unwrap();
        let Some(mut summary) = self.home_summary() else { return Ok(None) };
        if !summary.complete {
            return Ok(None);
        }
        for entry in &mut summary.books {
            if let Some(book) = index.books.iter().find(|book| book.id == entry.book.id) {
                let status = entry.status.clone();
                *entry = Entry::new(book, &BookState::default());
                entry.status = status;
            }
        }
        self.persist_home(&summary)?;
        Ok(Some(summary))
    }

    fn persist_home(&self, summary: &Summary) -> Result<()> {
        if self.home_summary().as_ref() == Some(summary) {
            return Ok(());
        }
        ensure!(serde_json::to_vec(summary)?.len() as u64 <= MAX_BYTES, "Home summary exceeds display-cache limit");
        atomic(&self.root.join("home.json"), summary)
    }

    /// Returns whether the file changed.
    fn persist_opened(&self, opened: &Opened) -> bool {
        if self.opened().as_ref() == Some(opened) {
            *self.opened_leader.lock().unwrap() = opened.leader().map(str::to_owned);
            return false;
        }
        self.write_opened(opened);
        true
    }
    /// A failed write removes the file, so the next launch rebuilds it;
    /// previews fall back to title order meanwhile.
    fn write_opened(&self, opened: &Opened) {
        let path = self.root.join("opened.json");
        let result = serde_json::to_vec(opened).map_err(anyhow::Error::from).and_then(|bytes| {
            ensure!(bytes.len() as u64 <= MAX_OPENED_BYTES, "opened order exceeds display-cache limit");
            super::atomic_bytes(&path, &bytes)
        });
        let mut leader = self.opened_leader.lock().unwrap();
        *leader = match result {
            Ok(()) => opened.leader().map(str::to_owned),
            Err(error) => {
                log::warn!("writing opened order: {error:#}");
                let _ = std::fs::remove_file(&path);
                None
            }
        };
    }

    /// Moves a book that was just read to the front of the opened order.
    /// Saves of the book already in front, page turns, read no file.
    fn update_opened_after_save(&self, id: &str, state: &BookState) {
        if state.last_opened == 0 || self.opened_leader.lock().unwrap().as_deref() == Some(id) {
            return;
        }
        let mut opened = self.opened().unwrap_or_default();
        if opened.record(id, state.last_opened) {
            self.write_opened(&opened);
        } else {
            *self.opened_leader.lock().unwrap() = opened.leader().map(str::to_owned);
        }
    }

    pub(super) fn update_home_after_save(&self, id: &str, state: &BookState) -> Result<()> {
        let _guard = SUMMARY_WRITE.lock().unwrap();
        self.update_opened_after_save(id, state);
        let mut summary = self.home_summary().unwrap_or_default();
        let selected = self.selected_home.lock().unwrap();
        let source = selected
            .as_ref()
            .filter(|book| book.id == id)
            .or_else(|| summary.books.iter().find(|entry| entry.book.id == id).map(|entry| &entry.book));
        let Some(book) = source else { return Ok(()) };
        let entry = Entry::new(book, state);
        summary.books.retain(|entry| entry.book.id != id);
        summary.books.push(entry);
        let complete = summary.complete;
        let counts = (summary.library_root_count, summary.library_folders, summary.library_books);
        let mut summary = Summary::select(summary.books);
        summary.complete = complete;
        summary.set_library_counts(counts);
        let result = self.persist_home(&summary);
        if result.is_err() {
            let _ = std::fs::remove_file(self.root.join("home.json"));
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{document::Position, library::selection_tests::book};

    #[test]
    fn cheap_cache_survives_missing_index_and_uses_authoritative_state_on_rebuild() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-home-cache-{}", std::process::id()));
        let store = Store::at(root.clone());
        let book = book(7);
        let state = BookState {
            position: Position("exact anchor".into()),
            progress: 4321,
            last_opened: 9,
            ..Default::default()
        };
        store.save_book(&book.id, &state)?;
        assert!(store.home_summary().is_none());
        let index = LibraryIndex { books: vec![book.clone()], ..Default::default() };
        store.rebuild_home_summary(&index)?;
        std::fs::write(root.join("library.json"), b"broken index")?;
        let summary = store.home_summary().unwrap();
        assert_eq!(summary.resume().unwrap().status.progress, 4321);
        assert_eq!(store.book(&book.id).position, state.position);
        std::fs::write(root.join("home.json"), b"{broken")?;
        assert!(store.home_summary().is_none());
        store.rebuild_home_summary(&index)?;
        let mut old = store.home_summary().unwrap();
        old.version += 1;
        atomic(&root.join("home.json"), &old)?;
        assert!(store.home_summary().is_none());
        store.rebuild_home_summary(&index)?;
        assert_eq!(store.book(&book.id).position, state.position);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn opened_order_follows_saves_skips_page_turns_and_recovers_from_bad_files() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-opened-{}", std::process::id()));
        let store = Store::at(root.clone());
        let index = LibraryIndex { books: (0..3).map(book).collect(), ..Default::default() };
        let opened_at = |at| BookState { position: Position("anchor".into()), last_opened: at, ..Default::default() };
        let order = |store: &Store| store.opened().map(|o| o.books.into_iter().map(|(id, _)| id).collect::<Vec<_>>());
        assert!(store.opened().is_none());
        store.save_book("1", &opened_at(5))?;
        store.save_book("2", &opened_at(9))?;
        assert_eq!(order(&store), Some(vec!["2".into(), "1".into()]));
        assert!(!store.opened().unwrap().complete, "saves seed a partial order");
        // Saves of the leading book (page turns) neither read nor write it.
        std::fs::remove_file(root.join("opened.json"))?;
        store.save_book("2", &opened_at(10))?;
        assert!(store.opened().is_none());
        // A broken file reads as missing; the rebuild uses every state.
        std::fs::write(root.join("opened.json"), b"{broken")?;
        assert!(store.opened().is_none());
        store.rebuild_home_summary(&index)?;
        assert_eq!(order(&store), Some(vec!["2".into(), "1".into()]));
        assert!(store.opened().unwrap().complete);
        assert_eq!(store.opened().unwrap().books[0].1, 10);
        // Marking a book finished keeps its place.
        store.save_book("1", &BookState { finished: true, ..opened_at(5) })?;
        assert_eq!(order(&store), Some(vec!["2".into(), "1".into()]));
        // A failed write drops only the derived file, never the reading state.
        std::fs::create_dir(root.join("opened.json.tmp"))?;
        store.save_book("0", &opened_at(20))?;
        assert_eq!(store.book("0").last_opened, 20);
        assert!(store.opened().is_none());
        std::fs::remove_dir(root.join("opened.json.tmp"))?;
        store.rebuild_home_summary(&index)?;
        assert_eq!(order(&store), Some(vec!["0".into(), "2".into(), "1".into()]));
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn failed_open_does_not_enter_history_and_cache_failure_never_loses_saved_position() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-home-save-{}", std::process::id()));
        let store = Store::at(root.clone());
        let book = book(3);
        store.select_home_book(&book);
        assert!(store.home_summary().is_none(), "staging an open must not change recency");
        let state = BookState {
            position: Position("saved first page".into()),
            progress: 1200,
            last_opened: 44,
            ..Default::default()
        };
        store.save_book(&book.id, &state)?;
        assert_eq!(store.home_summary().unwrap().resume().unwrap().book.id, book.id);
        assert!(!store.home_summary().unwrap().complete);
        std::fs::create_dir(root.join("home.json.tmp"))?;
        let mut next = state.clone();
        next.position = Position("newer page".into());
        next.progress = 1300;
        store.save_book(&book.id, &next)?;
        assert_eq!(store.book(&book.id).position, next.position);
        assert!(store.home_summary().is_none(), "failed cache write invalidates only the derived summary");
        std::fs::remove_dir(root.join("home.json.tmp"))?;
        store.rebuild_home_summary(&LibraryIndex { books: vec![book], ..Default::default() })?;
        assert_eq!(store.home_summary().unwrap().resume().unwrap().status.progress, 1300);
        assert!(store.home_summary().unwrap().complete);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn status_rebuild_promotes_older_resume_and_position_barrier_includes_summary() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-home-order-{}", std::process::id()));
        let store = Store::at(root.clone());
        let index = LibraryIndex { books: (0..12).map(book).collect(), ..Default::default() };
        for book in &index.books {
            store.save_book(
                &book.id,
                &BookState {
                    position: Position("anchor".into()),
                    last_opened: book.id.parse::<u64>()? + 1,
                    finished: book.id != "0" && book.id != "1",
                    ..Default::default()
                },
            )?;
        }
        store.rebuild_home_summary(&index)?;
        assert_eq!(store.home_summary().unwrap().resume().unwrap().book.id, "1");
        let mut state = store.book("1");
        state.hidden_from_continue = true;
        store.save_book("1", &state)?;
        store.rebuild_home_summary(&index)?;
        assert_eq!(store.home_summary().unwrap().resume().unwrap().book.id, "0");
        store.select_home_book(&index.books[11]);
        let writer =
            crate::store::position::Writer::new(store.clone(), "11".into(), crate::platform::Waker::new(|| {}));
        writer.save(crate::store::position::Presented {
            position: Position("latest exact anchor".into()),
            page: 7,
            progress: 3456,
            speed: None,
            at: 99,
        })?;
        writer.flush()?;
        let summary = store.home_summary().unwrap();
        assert_eq!(summary.resume().unwrap().book.id, "11");
        assert_eq!(summary.resume().unwrap().status.progress, 3456);
        assert_eq!(store.book("11").position.0, "latest exact anchor");
        drop(writer);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn cancelled_rebuild_invalidates_stale_home_caches() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-home-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = Store::at(root.clone());
        let index = LibraryIndex { books: vec![book(1)], ..Default::default() };
        store.save_book("1", &BookState { position: Position("p".into()), last_opened: 10, ..Default::default() })?;
        store.rebuild_home_summary(&index)?;
        assert!(store.home_summary().is_some_and(|s| s.complete));
        assert!(store.opened().is_some_and(|o| o.complete));
        assert!(store.rebuild_home_summary_until(&index, (0, 0, 0), &|| true).is_err());
        assert!(store.home_summary().is_none(), "cancelled rebuild removes stale home.json");
        assert!(store.opened().is_none(), "cancelled rebuild removes stale opened.json");
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn library_counts_are_cached_on_rebuild_and_preserved_across_book_saves() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-home-counts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = Store::at(root.clone());
        store.save_library_folders(vec!["/books".into()])?;
        let mut b0 = book(0);
        b0.path = "/books/loose.epub".into();
        let mut b1 = book(1);
        b1.path = "/books/Tolkien/hobbit.epub".into();
        let index = LibraryIndex { books: vec![b0.clone(), b1], ..Default::default() };
        store.rebuild_home_summary(&index)?;
        let summary = store.home_summary().unwrap();
        assert_eq!((summary.library_root_count, summary.library_folders, summary.library_books), (1, 1, 1));
        store.select_home_book(&b0);
        store.save_book(
            &b0.id,
            &BookState { position: Position("p".into()), progress: 2000, last_opened: 12, ..Default::default() },
        )?;
        let after = store.home_summary().unwrap();
        assert_eq!((after.library_root_count, after.library_folders, after.library_books), (1, 1, 1));
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
