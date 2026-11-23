//! Background pre-layout. While the library is idle, a
//! separate `verso --prelayout` process opens and closes the books the reader
//! is likely to open next, so crengine's cache holds their layout and their
//! first open is as fast as a reopen. The process can be killed at any moment
//! (a tapped book never waits behind it); crengine's cache survives that.

use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::JoinHandle;
use std::time::Instant;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{CreEngine, Document, Layout, Paths, Typography};
use crate::library::home::Summary;
use crate::library::{Book, LibraryIndex, Stamp};
use crate::page_layout::PageLayout;

/// The argument that starts Verso as the pre-layout process.
pub const ARG: &str = "--prelayout";
/// Books laid out at most per run.
const MAX_BOOKS: usize = 8;
/// The newest unopened books laid out (All books → Recent order).
const NEWEST_UNOPENED: usize = 3;
/// Read this far (ten-thousandths), the next book in the folder is prepared.
const NEXT_AFTER_PROGRESS: u32 = 5000;
/// Books the record keeps; older ones are likely out of crengine's cache.
const RECORD_BOOKS: usize = 64;
/// crengine never caches a file smaller than this (`LVDocView::swapToCache`
/// hard-codes it); such books open in well under 0.1 s anyway.
const MIN_CACHED_SIZE: u64 = 30_000;

/// Everything the child needs to lay books out exactly as the reader opens
/// them (`Books::open_settings`), so the cached layout is reused.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub layout: Layout,
    pub typography: Typography,
    pub page: PageLayout,
    pub dpi: u32,
}

impl Job {
    pub fn key(&self) -> String {
        key(&self.layout, &self.typography, &self.page, self.dpi)
    }
}

/// Identifies the layout settings crengine caches a book for. A book laid out
/// at another key is laid out again when opened.
pub fn key(layout: &Layout, typography: &Typography, page: &PageLayout, dpi: u32) -> String {
    let mut t = typography.clone();
    t.normalize();
    let mut p = page.clone();
    p.normalize();
    let px = t.font_px(dpi as f32);
    let g = p.geometry(layout.width as i32, layout.height as i32, dpi as f32, px);
    let text = format!(
        "{}|{}x{}@{dpi}|{:?}|{:?}|{}|{}|{px}|{}|{}|{},{},{}",
        env!("CARGO_PKG_VERSION"),
        layout.width,
        layout.height,
        layout.shaping,
        layout.margins,
        layout.font_size,
        t.font,
        t.spacing,
        t.justify,
        g.side,
        g.top,
        g.bottom
    );
    let mut hash = 0xcbf29ce484222325u64;
    for byte in text.bytes() {
        hash = (hash ^ byte as u64).wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

/// One book laid out (or failed) at the record's key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub path: PathBuf,
    pub stamp: Stamp,
    /// The engine couldn't lay it out (or the process died on it): not retried
    /// at this key.
    #[serde(default)]
    pub failed: bool,
}

/// The books known to be laid out in crengine's cache at one key, newest
/// first. Only the book thread writes it; the child reads it. A cache
/// eviction it doesn't see only means a book isn't prepared again.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub key: String,
    pub books: Vec<Entry>,
}

impl Record {
    pub fn path(cache: &Path) -> PathBuf {
        cache.join("prepared.json")
    }
    pub fn load(path: &Path) -> Self {
        std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
    }
    /// A rebuildable hint: no fsync, so a synced write never stalls the book thread.
    pub fn save(&self, path: &Path) -> Result<()> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
    /// Whether `path` at `stamp` was laid out (or failed) at `key`.
    pub fn has(&self, key: &str, path: &Path, stamp: &Stamp) -> bool {
        self.key == key && self.books.iter().any(|e| e.path == path && e.stamp == *stamp)
    }
    /// `path` is now laid out at `key` in the cache. Each cache file holds one
    /// layout, so another key starts the record over. Returns whether it changed.
    pub fn note(&mut self, key: &str, path: &Path, stamp: Stamp, failed: bool) -> bool {
        let entry = Entry { path: path.to_owned(), stamp, failed };
        if self.key == key && self.books.first() == Some(&entry) {
            return false;
        }
        if self.key != key {
            *self = Self { key: key.to_owned(), books: Vec::new() };
        }
        self.books.retain(|e| e.path != path);
        self.books.insert(0, entry);
        self.books.truncate(RECORD_BOOKS);
        true
    }
}

/// The books worth laying out now, most likely to be opened first: the book
/// to resume, the next book in the folder of the book read last (once it is
/// half read or finished), the other Home books, then the newest unopened
/// books. Skips books the record has at `key`, books with errors and books
/// too small for crengine to cache.
/// `opened` holds the ids with reading state (None: unknown, so no unopened books).
pub fn candidates(
    home: &Summary,
    index: &LibraryIndex,
    opened: Option<&HashSet<String>>,
    record: &Record,
    key: &str,
) -> Vec<(PathBuf, Stamp)> {
    let usable = |b: &&Book| b.available && b.error.is_none();
    let by_id = |id: &str| index.books.iter().filter(usable).find(|b| b.id == id);
    let mut recent: Vec<_> = home.books.iter().filter(|e| e.status.last_opened > 0).collect();
    recent.sort_by_key(|e| std::cmp::Reverse(e.status.last_opened));

    let mut picks: Vec<&Book> = Vec::new();
    if let Some(book) = home.resume().and_then(|e| by_id(&e.book.id)) {
        picks.push(book);
    }
    if let Some(last) = recent.first()
        && (last.status.finished || last.status.progress >= NEXT_AFTER_PROGRESS)
        && let Some(book) = by_id(&last.book.id)
        && let Some(next) = next_in_folder(index, &book.path)
    {
        picks.push(next);
    }
    picks.extend(recent.iter().filter_map(|e| by_id(&e.book.id)));
    if let Some(opened) = opened {
        let mut unopened: Vec<&Book> = index.books.iter().filter(usable).filter(|b| !opened.contains(&b.id)).collect();
        unopened.sort_by(|a, b| {
            (b.stamp.modified_secs, b.stamp.modified_nanos, &a.path).cmp(&(
                a.stamp.modified_secs,
                a.stamp.modified_nanos,
                &b.path,
            ))
        });
        picks.extend(unopened.into_iter().take(NEWEST_UNOPENED));
    }

    let mut seen = HashSet::new();
    picks
        .into_iter()
        .filter(|b| seen.insert(&b.path))
        .filter(|b| b.stamp.size >= MIN_CACHED_SIZE && !record.has(key, &b.path, &b.stamp))
        .take(MAX_BOOKS)
        .map(|b| (b.path.clone(), b.stamp.clone()))
        .collect()
}

/// The book after the one at `path` in its folder, by file name with numbers
/// in order ("Book 2" before "Book 10"). Also the end-of-book sheet's *Next*.
pub(crate) fn next_in_folder<'a>(index: &'a LibraryIndex, path: &Path) -> Option<&'a Book> {
    let folder = path.parent()?;
    let mut siblings: Vec<&Book> =
        index.books.iter().filter(|b| b.available && b.error.is_none() && b.path.parent() == Some(folder)).collect();
    siblings.sort_by_cached_key(|b| natural_key(&b.path));
    let at = siblings.iter().position(|b| b.path == path)?;
    siblings.get(at + 1).copied()
}

/// The file's size and modification time, as the library index records them.
fn stamp_of(path: &Path) -> Option<Stamp> {
    std::fs::metadata(path).ok().and_then(|m| crate::library::worker::stamp(&m).ok())
}

/// File name chunks for ordering: digit runs compare as numbers.
fn natural_key(path: &Path) -> Vec<(u8, String)> {
    let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let mut chunks: Vec<(u8, String)> = Vec::new();
    for c in name.chars() {
        let digit = c.is_ascii_digit();
        match chunks.last_mut() {
            Some((1, run)) if digit => run.push(c),
            Some((0, run)) if !digit => run.push(c),
            _ => chunks.push((u8::from(digit), c.to_string())),
        }
    }
    for (kind, run) in &mut chunks {
        if *kind == 1 {
            // Zero-padded to compare numerically as text.
            let trimmed = run.trim_start_matches('0');
            *run = format!("{:0>20}", trimmed);
        }
    }
    chunks
}

/// What the child tells the book thread, one JSON line each.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Report {
    /// About to lay out `path`: if the process dies now, this book is why.
    Started {
        path: PathBuf,
    },
    Prepared {
        key: String,
        path: PathBuf,
        stamp: Stamp,
        parse_ms: u32,
        layout_ms: u32,
        close_ms: u32,
        pages: usize,
    },
    Failed {
        key: String,
        path: PathBuf,
        stamp: Stamp,
        error: String,
    },
}

/// The child process: lays out the chosen books and exits. Reads its `Job`
/// from stdin; never writes Verso's records (only crengine's cache).
pub fn run_child() -> Result<()> {
    let started = Instant::now();
    // crengine prints notices to stdout: reports get stdout's pipe to
    // themselves, and stdout goes to the log.
    // SAFETY: plain descriptor calls; `reports` is a new descriptor we own.
    let mut out = unsafe {
        use std::os::fd::FromRawFd;
        let reports = libc::dup(1);
        anyhow::ensure!(reports >= 0 && libc::dup2(2, 1) >= 0, "couldn't set up the report pipe");
        std::fs::File::from_raw_fd(reports)
    };
    let job: Job = serde_json::from_reader(std::io::stdin().lock()).context("reading the job")?;
    let paths = Paths::for_device();
    let cache = paths.cache.clone().context("no book cache")?;
    let key = job.key();
    let record = Record::load(&Record::path(&cache));
    let store = crate::store::Store::new();
    let home = store.home_summary().unwrap_or_default();
    let index = store.library();
    let opened = store.book_state_ids();
    let books = candidates(&home, &index, opened.as_ref(), &record, &key);
    log::info!(
        "prelayout: {} book(s) to lay out at {key}, chosen from {} in {:?}",
        books.len(),
        index.books.len(),
        started.elapsed()
    );
    if books.is_empty() {
        return Ok(());
    }
    let engine = CreEngine::new(paths, job.dpi)?;
    let mut send = |report: &Report| -> Result<()> {
        serde_json::to_writer(&mut out, report)?;
        out.write_all(b"\n")?;
        out.flush()?;
        Ok(())
    };
    for (path, stamp) in books {
        // The stamp of the file laid out, in case it changed since the scan.
        let stamp = stamp_of(&path).unwrap_or(stamp);
        send(&Report::Started { path: path.clone() })?;
        match engine.open(&path, &job.layout, Some((&job.typography, &job.page))) {
            Ok(mut doc) => {
                let (stats, pages) = (doc.stats, doc.page_count());
                let closing = Instant::now();
                // Closing writes the layout to the cache.
                drop(doc);
                let close_ms = closing.elapsed().as_millis() as u32;
                log::info!(
                    "prelayout: {} laid out: parse {} ms, layout {} ms, close {close_ms} ms, {pages} pages",
                    path.display(),
                    stats.load_ms,
                    stats.layout_ms
                );
                send(&Report::Prepared {
                    key: key.clone(),
                    path,
                    stamp,
                    parse_ms: stats.load_ms,
                    layout_ms: stats.layout_ms,
                    close_ms,
                    pages,
                })?;
            }
            Err(e) => {
                log::warn!("prelayout: {}: {e:#}", path.display());
                send(&Report::Failed { key: key.clone(), path, stamp, error: format!("{e:#}") })?;
            }
        }
    }
    log::info!("prelayout: done in {:?}", started.elapsed());
    Ok(())
}

/// Messages from the thread reading a run's reports.
pub enum Event {
    Report { run: u64, report: Report },
    Ended { run: u64 },
}

struct Running {
    run: u64,
    key: String,
    child: Child,
    reader: Option<JoinHandle<()>>,
    /// Started and not yet reported done.
    current: Option<PathBuf>,
    started: Instant,
}

/// The book thread's side: starts, stops and hears from the child, and keeps
/// the record. Owned by the book thread, the only crengine user in the app.
pub struct Runner {
    record_path: Option<PathBuf>,
    record: Option<Record>,
    record_unsaved: bool,
    running: Option<Running>,
    runs: u64,
    /// A child may have changed the cache index since the engine read it.
    cache_stale: bool,
}

impl Runner {
    /// `VERSO_PRELAYOUT=0` turns background layout off (measurements).
    pub fn new(cache: Option<&Path>) -> Self {
        let enabled = std::env::var("VERSO_PRELAYOUT").as_deref() != Ok("0");
        if !enabled {
            log::info!("prelayout: off (VERSO_PRELAYOUT=0)");
        }
        Self {
            record_path: cache.filter(|_| enabled).map(Record::path),
            record: None,
            record_unsaved: false,
            running: None,
            runs: 0,
            cache_stale: false,
        }
    }

    fn record(&mut self) -> &mut Record {
        let path = self.record_path.clone();
        self.record.get_or_insert_with(|| path.map(|p| Record::load(&p)).unwrap_or_default())
    }

    fn save_record(&mut self) {
        if !self.record_unsaved {
            return;
        }
        self.record_unsaved = false;
        if let (Some(path), Some(record)) = (&self.record_path, &self.record)
            && let Err(e) = record.save(path)
        {
            log::warn!("prelayout: saving {}: {e:#}", path.display());
        }
    }

    fn note(&mut self, key: &str, path: &Path, stamp: Stamp, failed: bool) {
        if self.record_path.is_some() && self.record().note(key, path, stamp, failed) {
            self.record_unsaved = true;
        }
    }

    /// The app laid `path` out at `key` (an open or a settings change).
    pub fn laid_out(&mut self, key: &str, path: &Path) {
        if let Some(stamp) = stamp_of(path) {
            self.note(key, path, stamp, false);
        }
    }

    /// Starts a run unless one at the same settings is going; a run at other
    /// settings is stopped first. `events` is called from the reader thread.
    pub fn start(&mut self, job: &Job, events: impl Fn(Event) + Send + 'static) {
        if self.record_path.is_none() {
            return;
        }
        let key = job.key();
        if self.running.as_ref().is_some_and(|r| r.key == key) {
            return;
        }
        self.stop("settings changed");
        self.save_record();
        self.runs += 1;
        let run = self.runs;
        match spawn(job) {
            Ok(mut child) => {
                let stdout = child.stdout.take().expect("piped");
                let spawned = std::thread::Builder::new().name("verso-prelayout".into()).spawn(move || {
                    for line in std::io::BufReader::new(stdout).lines() {
                        let Ok(line) = line else { break };
                        match serde_json::from_str(&line) {
                            Ok(report) => events(Event::Report { run, report }),
                            Err(e) => log::warn!("prelayout: unreadable report {line:?}: {e}"),
                        }
                    }
                    events(Event::Ended { run });
                });
                // Without its reader the run could never end or be read, and
                // would block a restart at the same settings.
                let reader = match spawned {
                    Ok(reader) => Some(reader),
                    Err(e) => {
                        log::warn!("prelayout: couldn't start the report reader: {e}");
                        let _ = child.kill();
                        let _ = child.wait();
                        return;
                    }
                };
                log::info!("prelayout: run {run} started (pid {})", child.id());
                self.cache_stale = true;
                self.running = Some(Running { run, key, child, reader, current: None, started: Instant::now() });
            }
            Err(e) => log::warn!("prelayout: couldn't start: {e:#}"),
        }
    }

    /// Kills a running child at once and waits for it, so it writes nothing more.
    pub fn stop(&mut self, why: &str) {
        let Some(mut r) = self.running.take() else { return };
        let killing = Instant::now();
        let _ = r.child.kill();
        let _ = r.child.wait();
        if let Some(reader) = r.reader.take() {
            let _ = reader.join();
        }
        log::info!(
            "prelayout: run {} stopped ({why}) after {:?}, in {:?}{}",
            r.run,
            r.started.elapsed(),
            killing.elapsed(),
            r.current.map(|p| format!(", during {}", p.display())).unwrap_or_default()
        );
    }

    /// Before the app opens a book: stop the child and read the cache index
    /// it may have changed, so the app neither loses nor overwrites its work.
    pub fn before_open(&mut self, engine: &CreEngine) {
        self.stop("a book is opening");
        if self.cache_stale {
            self.cache_stale = false;
            let started = Instant::now();
            match engine.reload_cache() {
                Ok(()) => log::info!("prelayout: cache index reloaded in {:?}", started.elapsed()),
                Err(e) => log::warn!("prelayout: {e:#}"),
            }
        }
    }

    pub fn on_event(&mut self, event: Event) {
        match event {
            Event::Report { run, report } => {
                let current = self.running.as_mut().filter(|r| r.run == run);
                match report {
                    Report::Started { path } => {
                        if let Some(r) = current {
                            r.current = Some(path);
                        }
                    }
                    // Valid even from a run since stopped: the book is in the cache.
                    Report::Prepared { key, path, stamp, .. } => {
                        if let Some(r) = current {
                            r.current = None;
                        }
                        self.note(&key, &path, stamp, false);
                    }
                    Report::Failed { key, path, stamp, .. } => {
                        if let Some(r) = current {
                            r.current = None;
                        }
                        self.note(&key, &path, stamp, true);
                    }
                }
            }
            Event::Ended { run } => {
                if self.running.as_ref().is_none_or(|r| r.run != run) {
                    return;
                }
                let mut r = self.running.take().expect("checked");
                let status = r.child.wait();
                if let Some(reader) = r.reader.take() {
                    let _ = reader.join();
                }
                let ok = status.as_ref().is_ok_and(|s| s.success());
                log::info!("prelayout: run {run} ended after {:?}: {status:?}", r.started.elapsed());
                // It died on this book (a crash, say): don't try it again.
                if !ok && let Some(path) = r.current.take() {
                    log::warn!("prelayout: giving up on {}", path.display());
                    if let Some(stamp) = stamp_of(&path) {
                        self.note(&r.key, &path, stamp, true);
                    }
                }
                self.save_record();
            }
        }
    }

    /// Stops the child and saves the record (exit).
    pub fn shutdown(&mut self) {
        self.stop("exit");
        self.save_record();
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        self.stop("exit");
    }
}

/// Starts this same executable as the child, at the lowest CPU and I/O
/// priority, killed by the kernel if the app dies without stopping it.
fn spawn(job: &Job) -> Result<Child> {
    let parent = std::process::id();
    let mut command = Command::new("/proc/self/exe");
    command.arg(ARG).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
    // SAFETY: prctl, getppid, setpriority, syscall and _exit are
    // async-signal-safe, as required between fork and exec.
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(move || {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            // The app died before prctl took effect.
            if libc::getppid() as u32 != parent {
                libc::_exit(0);
            }
            libc::setpriority(libc::PRIO_PROCESS, 0, 19);
            // IOPRIO_WHO_PROCESS, this process, IOPRIO_CLASS_IDLE.
            libc::syscall(libc::SYS_ioprio_set, 1, 0, 3 << 13);
            Ok(())
        });
    }
    let mut child = command.spawn().context("starting the pre-layout process")?;
    let job = serde_json::to_vec(job)?;
    let mut stdin = child.stdin.take().expect("piped");
    if let Err(e) = stdin.write_all(&job) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(e).context("sending the job");
    }
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::TextShaping;
    use crate::library::home::{Entry as HomeEntry, Status};
    use crate::library::selection_tests::book;

    fn index(books: Vec<Book>) -> LibraryIndex {
        LibraryIndex { books, ..Default::default() }
    }
    fn home(entries: &[(&Book, u64, u32, bool)]) -> Summary {
        Summary {
            books: entries
                .iter()
                .map(|(b, opened, progress, finished)| HomeEntry {
                    book: (*b).clone(),
                    status: Status {
                        has_position: true,
                        progress: *progress,
                        last_opened: *opened,
                        finished: *finished,
                        rereading: false,
                        hidden_from_continue: false,
                    },
                })
                .collect(),
            ..Default::default()
        }
    }
    fn at(id: usize, path: &str) -> Book {
        let mut b = Book { path: path.into(), ..book(id) };
        b.stamp.size = MIN_CACHED_SIZE;
        b
    }
    fn paths(picks: &[(PathBuf, Stamp)]) -> Vec<&str> {
        picks.iter().map(|(p, _)| p.to_str().unwrap()).collect()
    }

    #[test]
    fn picks_resume_next_in_folder_home_then_newest_unopened() {
        let books = vec![
            at(1, "/s/Book 2.epub"),
            at(2, "/s/Book 10.epub"),
            at(3, "/s/Book 1.epub"),
            at(4, "/other/finished.epub"),
            at(5, "/new/a.epub"),
            at(6, "/new/b.epub"),
            at(7, "/new/c.epub"),
            at(8, "/new/d.epub"),
        ];
        let summary = home(&[(&books[0], 30, 6000, false), (&books[3], 20, 10000, true)]);
        let opened: HashSet<String> = ["1", "4"].map(String::from).into();
        let picks = candidates(&summary, &index(books), Some(&opened), &Record::default(), "k");
        // Book 2 is resumed and over half read, so Book 10 (not Book 1) is
        // next; then the other Home book and the three newest unopened.
        assert_eq!(
            paths(&picks),
            ["/s/Book 2.epub", "/s/Book 10.epub", "/other/finished.epub", "/new/d.epub", "/new/c.epub", "/new/b.epub"]
        );
    }

    #[test]
    fn skips_books_too_small_for_the_cache() {
        let mut books = vec![at(1, "/a.epub"), at(2, "/b.epub")];
        books[0].stamp.size = MIN_CACHED_SIZE - 1;
        let summary = home(&[(&books[0], 30, 0, false), (&books[1], 20, 0, false)]);
        let picks = candidates(&summary, &index(books), None, &Record::default(), "k");
        assert_eq!(paths(&picks), ["/b.epub"]);
    }

    #[test]
    fn next_in_folder_waits_for_half_and_skips_recorded_and_broken_books() {
        let mut books = vec![at(1, "/s/01.epub"), at(2, "/s/02.epub"), at(3, "/s/03.epub")];
        let summary = home(&[(&books[0], 30, 4999, false)]);
        let opened: HashSet<String> = ["1", "2", "3"].map(String::from).into();
        let picks = candidates(&summary, &index(books.clone()), Some(&opened), &Record::default(), "k");
        assert_eq!(paths(&picks), ["/s/01.epub"], "not half read yet");

        let summary = home(&[(&books[0], 30, 5000, false)]);
        let mut record = Record::default();
        record.note("k", Path::new("/s/01.epub"), books[0].stamp.clone(), false);
        let picks = candidates(&summary, &index(books.clone()), Some(&opened), &record, "k");
        assert_eq!(paths(&picks), ["/s/02.epub"], "the resumed book is laid out already");
        let picks = candidates(&summary, &index(books.clone()), Some(&opened), &record, "other");
        assert_eq!(paths(&picks), ["/s/01.epub", "/s/02.epub"], "but not at these settings");

        books[1].error = Some("broken".into());
        let picks = candidates(&summary, &index(books.clone()), Some(&opened), &Record::default(), "k");
        assert_eq!(paths(&picks), ["/s/01.epub", "/s/03.epub"], "a broken book is passed over");
        books[1].error = None;
        books[0].stamp.size += 1;
        let picks = candidates(&summary, &index(books), Some(&opened), &record, "k");
        assert_eq!(paths(&picks), ["/s/01.epub", "/s/02.epub"], "a changed file is laid out again");
    }

    #[test]
    fn unknown_reading_state_leaves_out_unopened_books_and_runs_are_capped() {
        let books: Vec<Book> = (0..20).map(|i| at(i, &format!("/b/{i:02}.epub"))).collect();
        let picks = candidates(&Summary::default(), &index(books.clone()), None, &Record::default(), "k");
        assert!(picks.is_empty());
        let entries: Vec<_> = books.iter().take(6).enumerate().map(|(i, b)| (b, 100 - i as u64, 0, false)).collect();
        let summary = home(&entries);
        let picks = candidates(&summary, &index(books), Some(&HashSet::new()), &Record::default(), "k");
        assert_eq!(picks.len(), MAX_BOOKS);
        assert_eq!(&paths(&picks)[..2], ["/b/00.epub", "/b/01.epub"]);
        assert_eq!(&paths(&picks)[6..], ["/b/19.epub", "/b/18.epub"], "newest unopened after Home");
    }

    #[test]
    fn record_starts_over_at_other_settings_and_stays_bounded() {
        let mut record = Record::default();
        let stamp = Stamp { size: 1, ..Default::default() };
        assert!(record.note("a", Path::new("/x.epub"), stamp.clone(), false));
        assert!(!record.note("a", Path::new("/x.epub"), stamp.clone(), false), "unchanged");
        assert!(record.has("a", Path::new("/x.epub"), &stamp));
        assert!(!record.has("b", Path::new("/x.epub"), &stamp));
        assert!(record.note("b", Path::new("/y.epub"), stamp.clone(), true));
        assert_eq!(record.books.len(), 1, "a layout at other settings replaces the old ones");
        for i in 0..100 {
            record.note("b", Path::new(&format!("/{i}.epub")), stamp.clone(), false);
        }
        assert_eq!(record.books.len(), RECORD_BOOKS);
        assert_eq!(record.books[0].path, Path::new("/99.epub"));

        let dir = std::env::temp_dir().join(format!("verso-prepared-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = Record::path(&dir);
        record.save(&path).unwrap();
        assert_eq!(Record::load(&path), record);
        std::fs::write(&path, b"{").unwrap();
        assert_eq!(Record::load(&path), Record::default(), "a damaged record reads as empty");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn key_follows_what_changes_the_layout() {
        let layout = Layout { width: 1272, height: 1696, margins: [83; 4], font_size: 42, shaping: TextShaping::Light };
        let (t, p) = (Typography::default(), PageLayout::default());
        let k = key(&layout, &t, &p, 300);
        assert_eq!(k, key(&layout, &t, &p, 300));
        assert_ne!(k, key(&layout, &Typography { size_pt: 11.0, ..t.clone() }, &p, 300));
        assert_ne!(k, key(&layout, &Typography { justify: false, ..t.clone() }, &p, 300));
        assert_ne!(k, key(&Layout { width: 1696, height: 1272, ..layout.clone() }, &t, &p, 300));
        assert_ne!(k, key(&Layout { shaping: TextShaping::Full, ..layout.clone() }, &t, &p, 300));
        let job = Job { layout, typography: t, page: p, dpi: 300 };
        assert_eq!(job.key(), k);
        let json = serde_json::to_string(&job).unwrap();
        assert_eq!(serde_json::from_str::<Job>(&json).unwrap(), job);
    }

    #[test]
    fn reports_round_trip_as_lines() {
        let report = Report::Prepared {
            key: "k".into(),
            path: "/a b.epub".into(),
            stamp: Stamp::default(),
            parse_ms: 1,
            layout_ms: 2,
            close_ms: 3,
            pages: 4,
        };
        let line = serde_json::to_string(&report).unwrap();
        assert!(!line.contains('\n'));
        assert_eq!(serde_json::from_str::<Report>(&line).unwrap(), report);
    }
}
