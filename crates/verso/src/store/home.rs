//! Derived Home writes follow authoritative book writes and serialize with
//! background rebuilds. No reading position is ever copied out of this cache.
use super::{BookState, Store, atomic, read};
use crate::library::{
    Book, LibraryIndex,
    home::{Entry, Summary},
};
use anyhow::{Result, ensure};
use std::sync::Mutex;

// There is one app process and one ordered position writer. This also covers
// independent Store handles for the same root and prevents .tmp-file collisions.
static SUMMARY_WRITE: Mutex<()> = Mutex::new(());
const MAX_BYTES: u64 = 64 * 1024;

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

    /// Library-worker operation, including migration of older installations.
    /// Reads existing records but never modifies any of them.
    /// Stops between records when `cancelled` returns true (shutdown). A
    /// cancelled rebuild writes nothing.
    pub fn rebuild_home_summary_until(&self, index: &LibraryIndex, cancelled: &dyn Fn() -> bool) -> Result<Summary> {
        let started = std::time::Instant::now();
        let _guard = SUMMARY_WRITE.lock().unwrap();
        let stopped = std::cell::Cell::new(false);
        let mut summary = Summary::select(index.books.iter().map_while(|book| {
            stopped.set(cancelled());
            (!stopped.get()).then(|| Entry::new(book, &self.book(&book.id)))
        }));
        ensure!(!stopped.get(), "Home rebuild cancelled");
        summary.complete = true;
        self.persist_home(&summary)?;
        log::info!(
            "startup stage: Home rebuild books={} elapsed_us={}",
            index.books.len(),
            started.elapsed().as_micros()
        );
        Ok(summary)
    }
    #[cfg(test)]
    pub fn rebuild_home_summary(&self, index: &LibraryIndex) -> Result<Summary> {
        self.rebuild_home_summary_until(index, &|| false)
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

    pub(super) fn update_home_after_save(&self, id: &str, state: &BookState) -> Result<()> {
        let _guard = SUMMARY_WRITE.lock().unwrap();
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
        let mut summary = Summary::select(summary.books);
        summary.complete = complete;
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
}
