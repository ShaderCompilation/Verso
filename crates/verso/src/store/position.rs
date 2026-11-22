//! One FIFO for presented positions. Barriers retry the latest failed save and
//! acknowledge only after the durable write; speculative pages never enter it.
use super::{BookState, Store};
use crate::{document::Position, library::language::DetectedLanguage, platform::Waker, reader::speed::ReadingSpeed};
use anyhow::{Context, Result};
use std::sync::mpsc;
use std::thread::JoinHandle;

#[derive(Clone)]
pub struct Presented {
    pub position: Position,
    pub page: usize,
    pub progress: u32,
    /// The book's reading speed, when it has samples.
    pub speed: Option<ReadingSpeed>,
    pub at: u64,
}
enum Command {
    Save(Presented),
    /// The global reading speed that seeds books opened for the first time.
    Seed(ReadingSpeed),
    /// The book's detected language, saved in order with its positions so
    /// neither write loses the other.
    Language(DetectedLanguage),
    Flush(mpsc::Sender<Result<()>>),
    Stop,
}
trait Backend: Send + 'static {
    fn save(&mut self, p: &Presented) -> Result<()>;
    fn flush_home(&mut self) {}
    fn seed(&mut self, _speed: &ReadingSpeed) -> Result<()> {
        Ok(())
    }
    fn language(&mut self, _detected: &DetectedLanguage) -> Result<()> {
        Ok(())
    }
}
struct StoreBackend {
    store: Store,
    id: String,
    fresh_open: bool,
    home_dirty: bool,
    /// Last durable `BookState` written by this worker and the external
    /// `Store::book_saves` generation it came from, so routine page turns
    /// avoid re-reading `books/<id>.json`.
    cached: Option<(BookState, u64)>,
}
impl StoreBackend {
    fn new(store: Store, id: String) -> Self {
        Self { store, id, fresh_open: true, home_dirty: false, cached: None }
    }
    fn take_state(&mut self) -> (BookState, u64) {
        let generation = self.store.book_saves();
        if let Some((state, cached_gen)) = self.cached.take()
            && cached_gen == generation
        {
            return (state, generation);
        }
        (self.store.book(&self.id), generation)
    }
}
impl Backend for StoreBackend {
    fn save(&mut self, p: &Presented) -> Result<()> {
        let (mut state, generation) = self.take_state();
        state.presented(p, self.fresh_open);
        // Update Home on the first presented save of an open session so this
        // book becomes the resume book; routine page turns defer Home until Flush.
        let update_home = self.fresh_open;
        self.store.save_book_position(&self.id, &state, update_home)?;
        self.cached = Some((state, generation));
        if self.fresh_open {
            self.fresh_open = false;
            self.home_dirty = false;
        } else {
            self.home_dirty = true;
        }
        Ok(())
    }
    fn flush_home(&mut self) {
        if !self.home_dirty {
            return;
        }
        self.home_dirty = false;
        let generation = self.store.book_saves();
        match &self.cached {
            Some((state, cached_gen)) if *cached_gen == generation => {
                self.store.sync_home(&self.id, state);
            }
            Some(_) => {
                // An external save_book/set_finished/remove_book superseded
                // our cached state and already updated Home.
                self.cached = None;
            }
            None => {
                // Cache was dropped after a best-effort error (e.g. Language);
                // re-read the durable record so Home catches up.
                let state = self.store.book(&self.id);
                self.store.sync_home(&self.id, &state);
                self.cached = Some((state, generation));
            }
        }
    }
    fn seed(&mut self, speed: &ReadingSpeed) -> Result<()> {
        self.store.save_reading_speed(speed)
    }
    fn language(&mut self, detected: &DetectedLanguage) -> Result<()> {
        let (mut state, generation) = self.take_state();
        state.language = Some(detected.clone());
        self.store.save_book_record(&self.id, &state)?;
        self.cached = Some((state, generation));
        Ok(())
    }
}
fn coalesce(mut c: Command, rx: &mpsc::Receiver<Command>, pending: &mut Option<Command>) -> Command {
    while matches!(c, Command::Save(_)) {
        match rx.try_recv() {
            Ok(next @ Command::Save(_)) => c = next,
            Ok(next) => {
                *pending = Some(next);
                break;
            }
            Err(_) => break,
        }
    }
    c
}
pub struct Writer {
    tx: mpsc::Sender<Command>,
    results: mpsc::Receiver<Result<()>>,
    join: Option<JoinHandle<()>>,
}
impl Writer {
    pub fn new(store: Store, id: String, waker: Waker) -> Self {
        Self::start_backend(waker, StoreBackend::new(store, id))
    }
    #[cfg(test)]
    fn start(waker: Waker, write: impl FnMut(&Presented) -> Result<()> + Send + 'static) -> Self {
        struct FnBackend<F>(F);
        impl<F: FnMut(&Presented) -> Result<()> + Send + 'static> Backend for FnBackend<F> {
            fn save(&mut self, p: &Presented) -> Result<()> {
                (self.0)(p)
            }
        }
        Self::start_backend(waker, FnBackend(write))
    }
    fn start_backend(waker: Waker, mut backend: impl Backend) -> Self {
        let (tx, commands) = mpsc::channel();
        let (results, rx) = mpsc::channel();
        let join = std::thread::Builder::new()
            .name("verso-position".into())
            .spawn(move || {
                let mut failed = None;
                let mut was_error = false;
                let mut pending = None;
                let report = |result: Result<()>, was_error: &mut bool| match &result {
                    Err(e) => {
                        log::error!("saving position: {e:#}");
                        *was_error = true;
                        let _ = results.send(result);
                        waker.wake();
                    }
                    Ok(()) if *was_error => {
                        *was_error = false;
                        let _ = results.send(result);
                        waker.wake();
                    }
                    Ok(()) => {}
                };
                loop {
                    let command = match pending.take() {
                        Some(command) => command,
                        None => match commands.recv() {
                            Ok(command) => command,
                            Err(_) => break,
                        },
                    };
                    match coalesce(command, &commands, &mut pending) {
                        Command::Save(p) => {
                            let result = backend.save(&p);
                            failed = result.is_err().then_some(p);
                            report(result, &mut was_error);
                        }
                        Command::Flush(done) => {
                            let result = failed.as_ref().map_or(Ok(()), |p| backend.save(p));
                            if result.is_ok() {
                                failed = None;
                                was_error = false;
                                backend.flush_home();
                            }
                            // The UI can consume failure/recovery notifications even
                            // while waiting at a barrier. Keep the acknowledgement separate.
                            if let Err(e) = &result {
                                report(Err(anyhow::anyhow!("{e:#}")), &mut was_error);
                            }
                            let _ = done.send(result);
                        }
                        // Best effort: a missing seed only delays time left for a new book.
                        Command::Seed(speed) => {
                            if let Err(e) = backend.seed(&speed) {
                                log::warn!("saving reading speed: {e:#}");
                            }
                        }
                        // Best effort: without it the book is detected again on its next open.
                        Command::Language(detected) => {
                            if let Err(e) = backend.language(&detected) {
                                log::warn!("saving book language: {e:#}");
                            }
                        }
                        Command::Stop => {
                            if failed.is_none() {
                                backend.flush_home();
                            }
                            break;
                        }
                    }
                }
            })
            .expect("start position thread");
        Self { tx, results: rx, join: Some(join) }
    }
    pub fn save(&self, position: Presented) -> Result<()> {
        self.tx.send(Command::Save(position)).context("position worker stopped")
    }
    /// Queues the global reading speed; nothing waits for it.
    pub fn seed(&self, speed: ReadingSpeed) {
        let _ = self.tx.send(Command::Seed(speed));
    }
    /// Queues the book's detected language; nothing waits for it.
    pub fn save_language(&self, detected: DetectedLanguage) {
        let _ = self.tx.send(Command::Language(detected));
    }
    pub fn drain(&self) -> impl Iterator<Item = Result<()>> + '_ {
        self.results.try_iter()
    }
    pub fn flush(&self) -> Result<()> {
        let (done, result) = mpsc::channel();
        self.tx.send(Command::Flush(done)).context("position worker stopped")?;
        result.recv().context("position worker stopped before completing saves")?
    }
}
impl Drop for Writer {
    fn drop(&mut self) {
        // Also drain submitted saves during error unwinding / an unexpected close.
        let _ = self.tx.send(Command::Stop);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    fn presented(page: usize) -> Presented {
        Presented { position: Position(format!("page-{page}")), page, progress: 1234, speed: None, at: 42 }
    }
    #[test]
    fn slow_writes_leave_submission_free_and_barrier_waits_for_every_save_in_order() {
        let (entered, started) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let saved = seen.clone();
        let writer = Writer::start(Waker::new(|| {}), move |p| {
            entered.send(p.page).unwrap();
            gate.recv().unwrap();
            saved.lock().unwrap().push(p.page);
            Ok(())
        });
        writer.save(presented(1)).unwrap();
        assert_eq!(started.recv().unwrap(), 1);
        writer.save(presented(2)).unwrap(); // Must return while save 1 is blocked.
        let (complete, completed) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            writer.flush().unwrap();
            complete.send(()).unwrap();
        });
        assert!(completed.try_recv().is_err());
        release.send(()).unwrap();
        assert_eq!(started.recv().unwrap(), 2);
        assert!(completed.try_recv().is_err());
        release.send(()).unwrap();
        completed.recv().unwrap();
        waiter.join().unwrap();
        assert_eq!(*seen.lock().unwrap(), vec![1, 2]);
    }
    #[test]
    fn failures_wake_ui_and_flush_retries_latest_without_replaying_an_old_position() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let saved = seen.clone();
        let (wake, wakes) = mpsc::channel();
        let mut attempt = 0;
        let writer = Writer::start(
            Waker::new(move || {
                let _ = wake.send(());
            }),
            move |p| {
                attempt += 1;
                saved.lock().unwrap().push(p.page);
                anyhow::ensure!(attempt > 2, "disk full");
                Ok(())
            },
        );
        writer.save(presented(1)).unwrap();
        wakes.recv().unwrap();
        assert!(writer.drain().next().unwrap().is_err());
        assert!(writer.flush().is_err()); // Still failing, must not acknowledge success.
        writer.save(presented(2)).unwrap();
        writer.flush().unwrap();
        assert_eq!(*seen.lock().unwrap(), vec![1, 1, 2]);
        assert!(writer.drain().last().unwrap().is_ok());
    }
    #[test]
    fn drop_completes_submitted_saves() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let saved = seen.clone();
        let writer = Writer::start(Waker::new(|| {}), move |p| {
            saved.lock().unwrap().push(p.page);
            Ok(())
        });
        writer.save(presented(7)).unwrap();
        drop(writer);
        assert_eq!(*seen.lock().unwrap(), vec![7]);
    }
    #[test]
    fn initial_failure_keeps_fresh_open_flags_until_a_successful_write() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-position-first-failure-{}", std::process::id()));
        let store = Store::at(root.clone());
        store.save_book(
            "book",
            &super::super::BookState { finished: true, hidden_from_continue: true, ..Default::default() },
        )?;
        let tmp = store.book_path("book").with_extension("json.tmp");
        std::fs::create_dir(&tmp)?;
        let writer = Writer::new(store.clone(), "book".into(), Waker::new(|| {}));
        writer.save(presented(1))?;
        assert!(writer.flush().is_err());
        assert!(store.book("book").finished);
        std::fs::remove_dir(&tmp)?;
        writer.save(presented(2))?;
        writer.flush()?;
        let saved = store.book("book");
        assert_eq!(saved.page, 2);
        assert!(!saved.finished && !saved.hidden_from_continue);
        drop(writer);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn coalesces_queued_saves_without_dropping_flush_or_metadata() {
        let (entered, started) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let events = Arc::new(Mutex::new(Vec::new()));
        struct Probe {
            entered: mpsc::Sender<usize>,
            gate: mpsc::Receiver<()>,
            events: Arc<Mutex<Vec<String>>>,
        }
        impl Backend for Probe {
            fn save(&mut self, p: &Presented) -> Result<()> {
                let _ = self.entered.send(p.page);
                self.gate.recv().unwrap();
                self.events.lock().unwrap().push(format!("save:{}", p.page));
                Ok(())
            }
            fn flush_home(&mut self) {
                self.events.lock().unwrap().push("flush_home".into());
            }
            fn language(&mut self, detected: &DetectedLanguage) -> Result<()> {
                self.events.lock().unwrap().push(format!("lang:{}", detected.code));
                Ok(())
            }
        }
        let writer = Writer::start_backend(Waker::new(|| {}), Probe { entered, gate, events: events.clone() });
        writer.save(presented(1)).unwrap();
        assert_eq!(started.recv().unwrap(), 1);
        // Queue rapid saves, a language update, more rapid saves, then Flush.
        writer.save(presented(2)).unwrap();
        writer.save(presented(3)).unwrap();
        writer.save(presented(4)).unwrap();
        writer.save_language(DetectedLanguage { code: "de".into(), reliable: true, version: 1 });
        writer.save(presented(5)).unwrap();
        writer.save(presented(6)).unwrap();
        let (complete, completed) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            writer.flush().unwrap();
            complete.send(()).unwrap();
            writer
        });
        // Release save 1 -> coalesces 2,3,4 into 4; runs lang:de; coalesces 5,6 into 6; flushes.
        release.send(()).unwrap();
        assert_eq!(started.recv().unwrap(), 4);
        release.send(()).unwrap();
        assert_eq!(started.recv().unwrap(), 6);
        release.send(()).unwrap();
        completed.recv().unwrap();
        let writer = waiter.join().unwrap();
        assert_eq!(*events.lock().unwrap(), vec!["save:1", "save:4", "lang:de", "save:6", "flush_home"]);
        drop(writer);
    }
    #[test]
    fn routine_ok_saves_do_not_wake_ui_while_error_and_recovery_do() {
        let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let wakes_bg = wakes.clone();
        let writer = Writer::start(
            Waker::new(move || {
                wakes_bg.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }),
            move |p| {
                anyhow::ensure!(p.page != 3, "disk full");
                Ok(())
            },
        );
        // Two routine successful saves: no wakes, no queued results.
        writer.save(presented(1)).unwrap();
        writer.flush().unwrap();
        writer.save(presented(2)).unwrap();
        writer.flush().unwrap();
        assert_eq!(wakes.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(writer.drain().next().is_none());
        // Page 3 fails -> wakes once with Err.
        writer.save(presented(3)).unwrap();
        let mut drained = Vec::new();
        while drained.is_empty() {
            drained.extend(writer.drain());
            std::thread::yield_now();
        }
        assert_eq!(drained.len(), 1);
        assert!(drained[0].is_err());
        assert_eq!(wakes.load(std::sync::atomic::Ordering::SeqCst), 1);
        // Page 4 recovers -> wakes once more with Ok(()).
        writer.save(presented(4)).unwrap();
        writer.flush().unwrap();
        let recovery: Vec<_> = writer.drain().collect();
        assert_eq!(recovery.len(), 1);
        assert!(recovery[0].is_ok());
        assert_eq!(wakes.load(std::sync::atomic::Ordering::SeqCst), 2);
        // Subsequent routine save after recovery -> no further wakeups.
        writer.save(presented(5)).unwrap();
        writer.flush().unwrap();
        assert_eq!(wakes.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert!(writer.drain().next().is_none());
    }
    #[test]
    fn cached_state_preserves_language_and_reloads_on_external_save() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::at(dir.path().to_path_buf());
        store.save_book("book", &BookState { dictionary_set: Some("english".into()), ..Default::default() })?;
        let writer = Writer::new(store.clone(), "book".into(), Waker::new(|| {}));
        writer.save(presented(1))?;
        writer.save_language(DetectedLanguage { code: "fr".into(), reliable: true, version: 1 });
        writer.save(presented(2))?;
        writer.flush()?;
        let after_lang = store.book("book");
        assert_eq!(after_lang.page, 2);
        assert_eq!(after_lang.dictionary_set.as_deref(), Some("english"));
        assert_eq!(after_lang.language.as_ref().map(|l| l.code.as_str()), Some("fr"));
        // External dictionary_set save (via Store::save_book) invalidates cached BookState.
        let mut updated = after_lang;
        updated.dictionary_set = Some("french".into());
        store.save_book("book", &updated)?;
        writer.save(presented(3))?;
        writer.flush()?;
        let after_ext = store.book("book");
        assert_eq!(after_ext.page, 3);
        assert_eq!(after_ext.dictionary_set.as_deref(), Some("french"));
        assert_eq!(after_ext.language.as_ref().map(|l| l.code.as_str()), Some("fr"));
        Ok(())
    }
    #[test]
    fn home_updates_on_first_save_and_flush_not_routine_turns() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::at(dir.path().to_path_buf());
        let book = crate::library::selection_tests::book(5);
        store.select_home_book(&book);
        let writer = Writer::new(store.clone(), book.id.clone(), Waker::new(|| {}));
        let turn = |page, progress, at| Presented {
            position: Position(format!("page-{page}")),
            page,
            progress,
            speed: None,
            at,
        };
        // First presented save of the session updates home.json so the book becomes resume.
        writer.save(turn(1, 1000, 100))?;
        writer.flush()?;
        assert_eq!(store.home_summary().unwrap().resume().unwrap().status.progress, 1000);
        // Routine subsequent turn writes books/<id>.json without rewriting home.json.
        writer.save(turn(2, 2000, 101))?;
        // Wait until the worker has written page 2 to books/<id>.json without calling flush().
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while store.book(&book.id).page != 2 {
            assert!(std::time::Instant::now() < deadline, "timed out waiting for page 2 save");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(store.book(&book.id).progress, 2000);
        assert_eq!(
            store.home_summary().unwrap().resume().unwrap().status.progress,
            1000,
            "routine page turn must not rewrite home.json"
        );
        // Flush (close/sleep/exit) brings home.json up to date.
        writer.flush()?;
        assert_eq!(store.home_summary().unwrap().resume().unwrap().status.progress, 2000);
        Ok(())
    }
}
