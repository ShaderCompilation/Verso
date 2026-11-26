//! The bulk import from Settings → Library.
//! A scan reads every book's KOReader sidecar and decides each book without
//! writing anything; the import then writes the changed states in one pass
//! and rebuilds Home and the opened order once. Both run on a [`Job`]
//! thread; Settings shows the preview in between.
//!
//! The same parse gives each book's new highlights, notes and page bookmarks
//! (`annotations::import`); the import commits them per book. Their
//! positions are checked when the book next opens, as for any saved
//! highlight; a page bookmark takes its opening words then
//! (`Annotation::awaits_opening`). Nothing else writes a book's annotations
//! while no reader has it open, and the open book is left out.
use super::{Found, Sidecar};
use crate::annotations::{self, Annotation, Collection, Kind};
use crate::library::LibraryIndex;
use crate::library::collections::{Collections, normalize};
use crate::platform::Waker;
use crate::store::{BookState, Store};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

/// How many unreadable sidecars the preview names.
pub const UNREADABLE_SHOWN: usize = 3;
/// Scan progress reaches the screen at most this often.
const PROGRESS_EVERY: Duration = Duration::from_millis(400);

/// KOReader's folder: `$VERSO_KOREADER` (for development), else its
/// folder on the Kindle. The simulator has none by default.
pub fn koreader_dir() -> PathBuf {
    std::env::var_os("VERSO_KOREADER").map_or_else(|| PathBuf::from("/mnt/us/koreader"), PathBuf::from)
}

/// KOReader's `history.lua`.
pub fn history_path() -> PathBuf {
    koreader_dir().join("history.lua")
}

/// What the scan found, as the sheet shows it before *Import*.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Preview {
    pub not_started: usize,
    pub newer_in_koreader: usize,
    pub newer_in_verso: usize,
    pub up_to_date: usize,
    /// Books the import marks finished: complete in KOReader, and KOReader's
    /// status newer than Verso's (`koreader::marks_finished`).
    pub finished: usize,
    /// Books with no KOReader position, only a `complete` status, that the
    /// import marks finished (also counted in `finished`).
    pub complete_only: usize,
    /// The book open in the reader under Settings, left out.
    pub open_skipped: usize,
    /// Sidecars that couldn't be read, and the first few of their books.
    pub unreadable: usize,
    pub unreadable_books: Vec<PathBuf>,
    /// Books whose state the import changes.
    pub changes: usize,
    /// Books with new highlights, notes or page bookmarks, and how many of
    /// each: a highlight with a note counts as a note.
    pub highlight_books: usize,
    pub highlights: usize,
    pub notes: usize,
    pub bookmarks: usize,
}
impl Preview {
    /// Whether the import would write anything.
    pub fn importable(&self) -> bool {
        self.changes > 0 || self.highlight_books > 0
    }
    /// Books with KOReader progress: a position, counted in one of the four
    /// rows, or only a `complete` status.
    pub fn found(&self) -> usize {
        self.not_started + self.newer_in_koreader + self.newer_in_verso + self.up_to_date + self.complete_only
    }
}

/// A book with KOReader progress, kept from the scan for the import.
#[derive(Clone, Debug)]
struct Entry {
    id: String,
    sidecar: Sidecar,
    /// KOReader's last open of the file, from `history.lua`.
    opened: Option<u64>,
}

/// The scan's result: the preview and what the import needs.
#[derive(Clone, Debug, Default)]
pub struct Scan {
    pub preview: Preview,
    entries: Vec<Entry>,
    /// New annotation records, by book annotation identity.
    annotations: Vec<(String, Vec<Annotation>)>,
}

/// Reads the KOReader data of every available EPUB in `index`, plus
/// `history.lua` once, and decides each book (§5). Writes nothing. `open`
/// is the book open in the reader under Settings, left out. `progress`
/// gets (books done, books in all). None when cancelled.
pub fn scan(
    store: &Store,
    index: &LibraryIndex,
    history: &Path,
    open: Option<&str>,
    cancelled: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(usize, usize),
) -> Option<Scan> {
    let started = Instant::now();
    let mut seen = HashSet::new();
    let books: Vec<_> = index
        .books
        .iter()
        .filter(|b| b.available && seen.insert(b.id.as_str()))
        .filter_map(|b| Some((b, super::metadata_path(&b.path)?)))
        .collect();
    let times: HashMap<PathBuf, u64> = match super::history(history, cancelled) {
        Ok(times) => times.into_iter().map(|(path, time)| (normalize(&path), time)).collect(),
        Err(_) if cancelled() => return None,
        Err(e) => {
            log::warn!("reading KOReader history {}: {e:#}", history.display());
            HashMap::new()
        }
    };
    let mut scan = Scan::default();
    let total = books.len();
    progress(0, total);
    for (done, (book, path)) in books.into_iter().enumerate() {
        if cancelled() {
            return None;
        }
        let found = match super::read_all(&path, cancelled) {
            Ok(found) => found,
            Err(_) if cancelled() => return None,
            Err(e) => {
                log::warn!("reading KOReader data {}: {e:#}", path.display());
                let preview = &mut scan.preview;
                preview.unreadable += 1;
                if preview.unreadable_books.len() < UNREADABLE_SHOWN {
                    preview.unreadable_books.push(book.path.clone());
                }
                None
            }
        };
        progress(done + 1, total);
        let Some(found) = found else { continue };
        let records = found.annotations.as_ref().and_then(|table| new_records(store, book, &path, table, cancelled));
        if cancelled() {
            return None;
        }
        if open == Some(book.id.as_str()) {
            if found.sidecar.is_some() || records.is_some() {
                scan.preview.open_skipped += 1;
            }
            continue;
        }
        if let Some(records) = records {
            let preview = &mut scan.preview;
            preview.highlight_books += 1;
            for a in &records {
                match (a.kind, a.note.is_some()) {
                    (Kind::Bookmark, _) => preview.bookmarks += 1,
                    (Kind::Highlight, true) => preview.notes += 1,
                    (Kind::Highlight, false) => preview.highlights += 1,
                }
            }
            scan.annotations.push((book.annotation_id.clone(), records));
        }
        let Some(found) = found.sidecar else { continue };
        let opened = times.get(&normalize(&book.path)).copied();
        let state = store.book(&book.id);
        let decision = super::import(&state, &found, opened);
        let preview = &mut scan.preview;
        match decision.found {
            Found::NotStarted => preview.not_started += 1,
            Found::NewerInKoreader => preview.newer_in_koreader += 1,
            Found::UpToDate => preview.up_to_date += 1,
            Found::NewerInVerso => preview.newer_in_verso += 1,
            Found::CompleteOnly => preview.complete_only += 1,
        }
        if decision.state.as_ref().is_some_and(|s| s.done() && !state.done()) {
            preview.finished += 1;
        }
        if decision.state.is_some() {
            preview.changes += 1;
            scan.entries.push(Entry { id: book.id.clone(), sidecar: found, opened });
        }
    }
    log::info!(
        "KOReader scan: books={total} found={} changes={} highlight_books={} unreadable={} elapsed_ms={}",
        scan.preview.found(),
        scan.preview.changes,
        scan.preview.highlight_books,
        scan.preview.unreadable,
        started.elapsed().as_millis()
    );
    Some(scan)
}

/// A book's annotation records new to Verso: None when there are none, or
/// they can't be read (logged; the book's progress still imports).
fn new_records(
    store: &Store,
    book: &crate::library::Book,
    path: &Path,
    table: &crate::annotations::lua::Value,
    cancelled: &dyn Fn() -> bool,
) -> Option<Vec<Annotation>> {
    if book.annotation_id.is_empty() {
        return None;
    }
    let found = annotations::sources(store, &book.annotation_id).and_then(|known| {
        let source = path.display().to_string();
        annotations::import::records(&book.annotation_id, "", &source, table.table()?, &known, cancelled)
    });
    match found {
        Ok(found) => (!found.records.is_empty()).then_some(found.records),
        Err(_) if cancelled() => None,
        Err(e) => {
            log::warn!("reading KOReader highlights {}: {e:#}", path.display());
            None
        }
    }
}

/// What the import wrote.
#[derive(Debug)]
pub struct Imported {
    /// Books whose progress was written, of the books to write.
    pub written: usize,
    pub total: usize,
    /// Books whose highlights were written, of the books to write.
    pub highlights_written: usize,
    pub highlights_total: usize,
    /// What stopped the rest. Running the import again finishes them.
    pub error: Option<anyhow::Error>,
}

/// Writes the states the scan decided on, in one pass, then rebuilds Home
/// and the opened order from every state. Each book is decided again from
/// its current state, so a state changed since the scan isn't overwritten
/// with a stale decision.
pub fn import(store: &Store, scan: &Scan, index: &LibraryIndex) -> Imported {
    let started = Instant::now();
    let states: Vec<(String, BookState)> = scan
        .entries
        .iter()
        .filter_map(|e| Some((e.id.clone(), super::import(&store.book(&e.id), &e.sidecar, e.opened).state?)))
        .collect();
    let (written, error) = store.save_books(&states);
    let (highlights_written, highlights_error) = save_annotations(store, &scan.annotations);
    let error = error.or(highlights_error);
    if written > 0 {
        let counts = store.home_summary().map_or_else(
            || {
                let roots = store.library_folders();
                Collections::from_index(index, &roots).top_level_counts(&roots)
            },
            |s| (s.library_root_count, s.library_folders, s.library_books),
        );
        if let Err(e) = store.rebuild_home_summary_until(index, counts, &|| false) {
            log::warn!("rebuilding Home after the KOReader import: {e:#}");
        }
    }
    log::info!(
        "KOReader import: written={written} of {} highlight_books={highlights_written} of {} error={} elapsed_ms={}",
        states.len(),
        scan.annotations.len(),
        error.is_some(),
        started.elapsed().as_millis()
    );
    Imported { written, total: states.len(), highlights_written, highlights_total: scan.annotations.len(), error }
}

/// Adds each book's new records to its collection and commits it (one
/// manifest swap per book), stopping at the first failure. Records imported
/// since the scan, by either import, are skipped by their identity.
fn save_annotations(store: &Store, books: &[(String, Vec<Annotation>)]) -> (usize, Option<anyhow::Error>) {
    let mut written = 0;
    for (book, records) in books {
        let result = Collection::load(store, book).and_then(|mut c| {
            let before = c.records.len();
            for a in records {
                if !a.provenance.as_ref().is_some_and(|p| c.manifest.sources.contains(&p.entry)) {
                    c.add(a.clone());
                }
            }
            if c.records.len() > before { c.commit_from(store, book, before) } else { Ok(()) }
        });
        if let Err(e) = result {
            return (written, Some(e.context(format!("saving highlights for book {book}"))));
        }
        written += 1;
    }
    (written, None)
}

/// What a [`Job`] reports.
#[derive(Debug)]
pub enum Event {
    /// Books read so far, of the books to read.
    Scanning(usize, usize),
    Scanned(Box<Scan>),
    Imported(Imported),
}

/// The scan or the import on its own thread. Dropping it cancels a scan
/// and waits for the thread, so nothing runs on after the sheet closes (an
/// import, which can't be cancelled, finishes first).
pub struct Job {
    events: Receiver<Event>,
    cancel: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Job {
    /// Scans the library as `library.json` has it.
    pub fn scan(store: Store, open: Option<String>, waker: Waker) -> Self {
        Self::spawn(waker, move |tx, cancel| {
            let index = store.library();
            let mut last = None::<Instant>;
            let mut report = |done: usize, total: usize| {
                if done == 0 || done == total || last.is_none_or(|at| at.elapsed() >= PROGRESS_EVERY) {
                    last = Some(Instant::now());
                    tx.send(Event::Scanning(done, total));
                }
            };
            let cancelled = || cancel.load(Ordering::Relaxed);
            if let Some(scan) = scan(&store, &index, &history_path(), open.as_deref(), &cancelled, &mut report) {
                tx.send(Event::Scanned(Box::new(scan)));
            }
        })
    }
    pub fn import(store: Store, scan: Scan, waker: Waker) -> Self {
        Self::spawn(waker, move |tx, _| {
            let index = store.library();
            tx.send(Event::Imported(import(&store, &scan, &index)));
        })
    }
    fn spawn(waker: Waker, work: impl FnOnce(&Reporter, &AtomicBool) + Send + 'static) -> Self {
        let (tx, events) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let thread = std::thread::Builder::new()
            .name("verso-koreader".into())
            .spawn(move || {
                let reporter = Reporter { tx, waker };
                work(&reporter, &flag);
                // The end of the thread: the screen notices it finished.
                reporter.waker.wake();
            })
            .map_err(|e| log::error!("starting the KOReader import: {e}"))
            .ok();
        Self { events, cancel, thread }
    }
    /// Events so far, oldest first.
    pub fn events(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }
    /// True once the thread has ended, or couldn't start.
    pub fn finished(&self) -> bool {
        self.thread.as_ref().is_none_or(std::thread::JoinHandle::is_finished)
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The thread's side: sends an event and wakes the screen.
struct Reporter {
    tx: Sender<Event>,
    waker: Waker,
}
impl Reporter {
    fn send(&self, event: Event) {
        let _ = self.tx.send(event);
        self.waker.wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Position;
    use crate::library::Book;
    use crate::library::selection_tests::book;
    use std::collections::BTreeMap;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    /// A library of `n` books in `dir/lib`, Verso's data in `dir/data`.
    fn library(dir: &Path, n: usize) -> (Store, LibraryIndex) {
        let books = (0..n)
            .map(|i| Book { path: dir.join(format!("lib/{i}.epub")), annotation_id: format!("a{i}"), ..book(i) })
            .collect();
        (Store::at(dir.join("data")), LibraryIndex { books, ..Default::default() })
    }
    /// KOReader's metadata file for `book`, saved at `modified` (seconds).
    fn sidecar(book: &Path, text: &str, modified: u64) -> PathBuf {
        let path = crate::koreader::metadata_path(book)
            .unwrap_or_else(|| crate::library::files::sidecar(book).join("metadata.pdf.lua"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(UNIX_EPOCH + Duration::from_secs(modified)).unwrap();
        path
    }
    fn koreader(position: &str, status: &str) -> String {
        format!(
            "return {{ last_xpointer = '{position}', percent_finished = 0.5, summary = {{ status = '{status}' }} }}"
        )
    }
    fn verso(position: &str, last_opened: u64) -> BookState {
        BookState { position: Position(position.into()), page: 4, progress: 300, last_opened, ..Default::default() }
    }
    /// Every file under `dir` with its contents and modified time.
    fn snapshot(dir: &Path) -> BTreeMap<PathBuf, (Vec<u8>, SystemTime)> {
        let mut files = BTreeMap::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    let modified = entry.metadata().unwrap().modified().unwrap();
                    files.insert(path.clone(), (std::fs::read(&path).unwrap(), modified));
                }
            }
        }
        files
    }
    fn run_scan(store: &Store, index: &LibraryIndex, history: &Path, open: Option<&str>) -> Scan {
        scan(store, index, history, open, &|| false, &mut |_, _| {}).expect("not cancelled")
    }

    #[test]
    fn every_row_then_import_and_run_again() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let (store, mut index) = library(dir.path(), 12);
        // 8: not an EPUB. 9: not available.
        index.books[8].path = dir.path().join("lib/8.pdf");
        index.books[9].available = false;
        let path = |i: usize| index.books[i].path.clone();
        // 0: no sidecar. 1: a sidecar without a position.
        sidecar(&path(1), "return { percent_finished = 0.4 }", 1000);
        // 2: not started in Verso; KOReader last opened it at 5000.
        sidecar(&path(2), &koreader("/k2", "reading"), 6000);
        // 3: newer in KOReader, hidden from Continue reading in Verso.
        sidecar(&path(3), &koreader("/k3", "reading"), 2000);
        store.save_book("3", &BookState { hidden_from_continue: true, ..verso("/v3", 1000) })?;
        // 4: newer in Verso, finished in KOReader.
        sidecar(&path(4), &koreader("/k4", "complete"), 2000);
        store.save_book("4", &verso("/v4", 3000))?;
        // 5: the same position.
        sidecar(&path(5), &koreader("/k5", "reading"), 2000);
        store.save_book("5", &verso("/k5", 100))?;
        // 6: unreadable. 7: open in the reader.
        sidecar(&path(6), "return { last_xpointer = os.exit() }", 2000);
        sidecar(&path(7), &koreader("/k7", "reading"), 9000);
        store.save_book("7", &verso("/v7", 4000))?;
        sidecar(&path(8), &koreader("/k8", "reading"), 2000);
        sidecar(&path(9), &koreader("/k9", "reading"), 2000);
        // 10: marked complete in KOReader without a position. 11: the same,
        // already finished in Verso.
        sidecar(&path(10), "return { summary = { status = 'complete' } }", 2000);
        sidecar(&path(11), "return { summary = { status = 'complete' } }", 2000);
        store.save_book("11", &BookState { finished: true, ..Default::default() })?;
        let history = dir.path().join("history.lua");
        let file = |i: usize| path(i).display().to_string();
        std::fs::write(
            &history,
            format!("return {{ {{ file = '{}', time = 5000 }}, {{ file = '{}', time = 1 }} }}", file(2), file(8)),
        )?;

        let before = snapshot(dir.path());
        let mut reports = Vec::new();
        let found = scan(&store, &index, &history, Some("7"), &|| false, &mut |d, t| reports.push((d, t))).unwrap();
        assert_eq!(snapshot(dir.path()), before, "the scan writes nothing");
        assert_eq!((reports.first(), reports.last()), (Some(&(0, 10)), Some(&(10, 10))), "EPUBs available");
        let p = &found.preview;
        assert_eq!(
            (p.not_started, p.newer_in_koreader, p.newer_in_verso, p.up_to_date, p.finished, p.open_skipped),
            (1, 1, 1, 2, 2, 1)
        );
        assert_eq!(p.complete_only, 1);
        assert_eq!((p.unreadable, p.unreadable_books.clone(), p.changes, p.found()), (1, vec![path(6)], 4, 6));

        let imported = import(&store, &found, &index);
        assert_eq!((imported.written, imported.total, imported.error.is_none()), (4, 4, true));
        let s = store.book("2");
        assert_eq!((s.position.0.as_str(), s.progress, s.page, s.last_opened), ("/k2", 5000, 0, 5000));
        assert_eq!(s.koreader_seen, Some(crate::koreader::Seen { modified: 6000, position: Position("/k2".into()) }));
        let s = store.book("3");
        assert_eq!((s.position.0.as_str(), s.last_opened, s.hidden_from_continue), ("/k3", 2000, false));
        let s = store.book("4");
        assert_eq!((s.position.0.as_str(), s.finished, s.koreader_seen), ("/v4", true, None));
        assert_eq!(store.book("5").last_opened, 100, "up to date: untouched");
        let s = store.book("10");
        assert_eq!((s.finished, s.position.0.as_str(), s.last_opened, s.koreader_seen), (true, "", 0, None));
        assert_eq!(store.book("7").position.0, "/v7", "the open book is left out");
        for i in [0, 1, 6, 8, 9] {
            assert!(store.book(&i.to_string()).position.0.is_empty(), "{i}");
        }
        // Continue reading is KOReader's latest book, and Recent follows (excluding finished book 4).
        let home = store.home_summary().expect("rebuilt");
        assert!(home.complete);
        assert_eq!(home.resume().unwrap().book.id, "2");
        let recents: Vec<_> = home.recents(4).iter().map(|e| e.book.id.clone()).collect();
        assert_eq!(recents, ["7", "3", "5"]);
        let opened: Vec<_> = store.opened().unwrap().books.into_iter().map(|(id, _)| id).collect();
        assert_eq!(opened, ["2", "7", "4", "3", "5"]);
        // KOReader's files are never written.
        let after = snapshot(dir.path());
        for (path, file) in before.iter().filter(|(p, _)| !p.starts_with(dir.path().join("data"))) {
            assert_eq!(after.get(path), Some(file), "{}", path.display());
        }

        // Again: the positions just imported are up to date. The book that
        // was open is closed now, and its newer KOReader position imported.
        let again = run_scan(&store, &index, &history, None);
        let p = &again.preview;
        assert_eq!((p.not_started, p.newer_in_koreader, p.newer_in_verso, p.up_to_date), (0, 1, 1, 5));
        assert_eq!((p.finished, p.complete_only, p.changes, p.open_skipped), (0, 0, 1, 0));
        assert_eq!(import(&store, &again, &index).written, 1);
        assert_eq!(store.book("7").position.0, "/k7");
        let p = run_scan(&store, &index, &history, None).preview;
        assert_eq!((p.up_to_date, p.changes), (6, 0));
        Ok(())
    }

    #[test]
    fn highlights_import_once_whichever_import_runs_first_and_removed_ones_stay_removed() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let (store, index) = library(dir.path(), 3);
        let entries = "annotations = { \
            { drawer='lighten', pos0='/p/text().0', pos1='/p/text().9', text='Quote', note='Mine' }, \
            { drawer='lighten', pos0='/q/text().0', pos1='/q/text().5', text='Other' }, \
            { page='/body/DocFragment[2]', text='in Two' } }";
        let paths: Vec<_> =
            index.books.iter().map(|b| sidecar(&b.path, &format!("return {{ {entries} }}"), 2000)).collect();
        let records = |i: usize| Collection::load(&store, &index.books[i].annotation_id).unwrap().records;
        // Book 0 was imported in the reader first, keeping only the bookmark.
        let per_book = |i: usize| {
            let b = &index.books[i];
            let known = annotations::sources(&store, &b.annotation_id).unwrap();
            annotations::import::read(&b.annotation_id, "content", &paths[i], &known, &|| false).unwrap()
        };
        let mut c = Collection::load(&store, "a0")?;
        for a in per_book(0).records.into_iter().filter(|a| a.kind == Kind::Bookmark) {
            c.add(a);
        }
        c.commit(&store, "a0")?;

        // Book 2 is open in the reader: left out.
        let found = run_scan(&store, &index, Path::new("/nonexistent"), Some("2"));
        let p = &found.preview;
        assert_eq!((p.highlight_books, p.highlights, p.notes, p.bookmarks), (2, 2, 2, 1));
        assert_eq!((p.open_skipped, p.importable()), (1, true));
        let imported = import(&store, &found, &index);
        assert_eq!((imported.highlights_written, imported.highlights_total, imported.error.is_none()), (2, 2, true));
        assert_eq!(records(0).len(), 3, "the highlights joined the bookmark imported before");
        let lone = &records(1);
        assert_eq!(lone.len(), 3);
        let bookmark = lone.iter().find(|a| a.kind == Kind::Bookmark).unwrap();
        assert!(bookmark.awaits_opening() && bookmark.quotation.is_empty());
        assert!(records(2).is_empty());

        // The per-book import afterwards finds nothing new.
        let again = per_book(1);
        assert_eq!((again.records.len(), again.duplicates), (0, 3));
        // A removed import stays removed, by either import.
        let mut c = Collection::load(&store, "a1")?;
        let id = c.records[0].id.clone();
        c.remove(&id);
        c.commit(&store, "a1")?;
        assert_eq!(per_book(1).records.len(), 0);
        let p = run_scan(&store, &index, Path::new("/nonexistent"), Some("2")).preview;
        assert_eq!((p.highlight_books, p.importable()), (0, false));
        assert_eq!(records(1).len(), 2);
        Ok(())
    }

    #[test]
    fn a_book_marked_unfinished_stays_so_until_koreader_saves_it_again() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let (store, index) = library(dir.path(), 3);
        let path = |i: usize| index.books[i].path.clone();
        // 0: finished in KOReader with a position kept in Verso. 1: complete
        // only, with KOReader's date. 2: still reading in KOReader.
        sidecar(&path(0), &koreader("/k0", "complete"), 2000);
        store.save_book("0", &verso("/v0", 3000))?;
        let complete = "return { summary = { modified = '2026-11-20', status = 'complete' } }";
        sidecar(&path(1), complete, 2000);
        sidecar(&path(2), &koreader("/k2", "reading"), 2000);
        let history = Path::new("/nonexistent");
        let found = run_scan(&store, &index, history, None);
        let p = &found.preview;
        assert_eq!((p.finished, p.complete_only, p.newer_in_verso, p.not_started, p.changes), (2, 1, 1, 1, 3));
        assert_eq!(import(&store, &found, &index).written, 3);
        let s = store.book("0");
        assert_eq!((s.finished, s.status_at, s.position.0.as_str()), (true, 2000, "/v0"));
        let day = crate::clock::LocalDateTime::midnight("2026-11-20").unwrap();
        assert_eq!((store.book("1").finished, store.book("1").status_at), (true, day));

        // Marked unfinished in Verso: a re-run leaves both alone.
        let (changed, error) = store.set_finished(&["0".into(), "1".into()], false);
        assert_eq!((changed.len(), error.is_none()), (2, true));
        let again = run_scan(&store, &index, history, None);
        let p = &again.preview;
        assert_eq!((p.finished, p.complete_only, p.changes, p.up_to_date, p.newer_in_verso), (0, 0, 0, 2, 1));
        assert!(!store.book("0").finished && !store.book("1").finished);

        // KOReader saves book 0 again later (read there): marked again.
        let later = crate::update::unix_now() + 100;
        sidecar(&path(0), &koreader("/k0", "complete"), later);
        let again = run_scan(&store, &index, history, None);
        let p = &again.preview;
        assert_eq!((p.finished, p.newer_in_koreader, p.changes), (1, 1, 1));
        assert_eq!(import(&store, &again, &index).written, 1);
        let s = store.book("0");
        assert_eq!((s.finished, s.status_at, s.position.0.as_str()), (true, later, "/k0"));
        assert!(!store.book("1").finished, "its sidecar wasn't saved again");
        Ok(())
    }

    #[test]
    fn cancelling_the_scan_writes_nothing() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let (store, index) = library(dir.path(), 6);
        for book in &index.books {
            sidecar(&book.path, &koreader("/k", "complete"), 2000);
        }
        let before = snapshot(dir.path());
        let done = std::cell::Cell::new(0);
        let cancelled = || done.get() >= 3;
        let scanned = scan(&store, &index, Path::new("/nonexistent"), None, &cancelled, &mut |d, _| done.set(d));
        assert!(scanned.is_none());
        assert_eq!(done.get(), 3, "stopped between books");
        assert_eq!(snapshot(dir.path()), before);
        Ok(())
    }

    #[test]
    fn a_failed_write_stops_and_a_second_run_finishes() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let (store, index) = library(dir.path(), 3);
        for book in &index.books {
            sidecar(&book.path, &koreader(&format!("/k{}", book.id), "reading"), 2000);
        }
        // Book 1's record can't be replaced.
        let blocked = dir.path().join("data/books/1.json");
        std::fs::create_dir_all(blocked.join("in-the-way"))?;
        let found = run_scan(&store, &index, Path::new("/nonexistent"), None);
        assert_eq!(found.preview.changes, 3);
        let imported = import(&store, &found, &index);
        assert_eq!((imported.written, imported.total), (1, 3));
        assert!(imported.error.is_some());
        assert_eq!(store.book("0").position.0, "/k0");
        assert!(store.book("2").position.0.is_empty(), "the pass stopped");
        assert!(
            std::fs::read_dir(dir.path().join("data/books"))?
                .flatten()
                .all(|e| !e.file_name().to_string_lossy().ends_with(".tmp")),
            "unrenamed .tmp files cleaned up"
        );
        assert_eq!(store.home_summary().unwrap().resume().unwrap().book.id, "0", "Home has what was written");

        std::fs::remove_dir_all(&blocked)?;
        let again = run_scan(&store, &index, Path::new("/nonexistent"), None);
        assert_eq!((again.preview.up_to_date, again.preview.not_started, again.preview.changes), (1, 2, 2));
        let imported = import(&store, &again, &index);
        assert_eq!((imported.written, imported.error.is_none()), (2, true));
        assert_eq!(store.book("2").position.0, "/k2");
        Ok(())
    }

    #[test]
    fn a_state_changed_after_the_scan_is_decided_again() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let (store, index) = library(dir.path(), 1);
        sidecar(&index.books[0].path, &koreader("/k", "reading"), 2000);
        let found = run_scan(&store, &index, Path::new("/nonexistent"), None);
        assert_eq!(found.preview.not_started, 1);
        // Read on in Verso since (not possible under the sheet today).
        store.save_book("0", &verso("/v", 3000))?;
        let imported = import(&store, &found, &index);
        assert_eq!((imported.written, imported.total), (0, 0));
        assert_eq!(store.book("0").position.0, "/v");
        Ok(())
    }

    #[test]
    fn the_job_reports_progress_then_the_preview_then_the_import() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let (store, index) = library(dir.path(), 2);
        store.save_library(&index)?;
        sidecar(&index.books[1].path, &koreader("/k", "reading"), 2000);
        let wait = |job: &Job| {
            let mut events = Vec::new();
            while !job.finished() {
                std::thread::sleep(Duration::from_millis(5));
            }
            events.extend(job.events());
            events
        };
        let job = Job::scan(store.clone(), None, Waker::new(|| {}));
        let events = wait(&job);
        assert!(matches!(events.first(), Some(Event::Scanning(0, 2))), "{events:?}");
        let Some(Event::Scanned(scan)) = events.into_iter().last() else { panic!("no preview") };
        assert_eq!(scan.preview.not_started, 1);
        let job = Job::import(store.clone(), *scan, Waker::new(|| {}));
        let Some(Event::Imported(imported)) = wait(&job).into_iter().last() else { panic!("no result") };
        assert_eq!((imported.written, imported.total), (1, 1));
        assert_eq!(store.book("1").position.0, "/k");
        Ok(())
    }

    /// §6: under 30 s for 10,000 books in the simulator. Run with
    /// `cargo test --release -- --ignored ten_thousand`.
    #[test]
    #[ignore = "a timing check: 10,000 sidecars"]
    fn ten_thousand_books() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let (store, index) = library(dir.path(), 10_000);
        let text = crate::annotations::lua::tests::sidecar(20);
        for (i, book) in index.books.iter().enumerate() {
            sidecar(&book.path, &text, 2000 + i as u64);
            if i % 3 == 0 {
                store.save_book(&book.id, &verso("/v", 5000))?;
            }
        }
        let started = Instant::now();
        let found = run_scan(&store, &index, Path::new("/nonexistent"), None);
        let scanned = started.elapsed();
        let imported = import(&store, &found, &index);
        let total = started.elapsed();
        eprintln!(
            "10,000 books: scan {scanned:?}, scan and import {total:?}, {} written, highlights for {}",
            imported.written, imported.highlights_written
        );
        assert_eq!(found.preview.found(), 10_000);
        assert!(imported.error.is_none());
        assert!(total < Duration::from_secs(30), "{total:?}");
        Ok(())
    }
}
