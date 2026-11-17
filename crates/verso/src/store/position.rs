//! One FIFO for presented positions. Barriers retry the latest failed save and
//! acknowledge only after the durable write; speculative pages never enter it.
use super::Store;
use crate::{document::Position, platform::Waker, reader::speed::ReadingSpeed};
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
    Flush(mpsc::Sender<Result<()>>),
    Stop,
}
pub struct Writer {
    tx: mpsc::Sender<Command>,
    results: mpsc::Receiver<Result<()>>,
    join: Option<JoinHandle<()>>,
}
impl Writer {
    pub fn new(store: Store, id: String, waker: Waker) -> Self {
        let mut fresh_open = true;
        let seeds = store.clone();
        Self::start_with_seed(
            waker,
            move |p| {
                // Keep library flags, including records written before this open.
                let mut state = store.book(&id);
                state.presented(p, fresh_open);
                store.save_book(&id, &state)?;
                fresh_open = false;
                Ok(())
            },
            move |speed| seeds.save_reading_speed(speed),
        )
    }
    #[cfg(test)]
    fn start(waker: Waker, write: impl FnMut(&Presented) -> Result<()> + Send + 'static) -> Self {
        Self::start_with_seed(waker, write, |_| Ok(()))
    }
    fn start_with_seed(
        waker: Waker,
        mut write: impl FnMut(&Presented) -> Result<()> + Send + 'static,
        mut seed: impl FnMut(&ReadingSpeed) -> Result<()> + Send + 'static,
    ) -> Self {
        let (tx, commands) = mpsc::channel();
        let (results, rx) = mpsc::channel();
        let join = std::thread::Builder::new()
            .name("verso-position".into())
            .spawn(move || {
                let mut failed = None;
                let report = |result: Result<()>| {
                    if let Err(e) = &result {
                        log::error!("saving position: {e:#}");
                    }
                    let _ = results.send(result);
                    waker.wake();
                };
                while let Ok(command) = commands.recv() {
                    match command {
                        Command::Save(p) => {
                            let result = write(&p);
                            failed = result.is_err().then_some(p);
                            report(result);
                        }
                        Command::Flush(done) => {
                            let result = failed.as_ref().map_or(Ok(()), &mut write);
                            if result.is_ok() {
                                failed = None;
                            }
                            // The UI can consume failure/recovery notifications even
                            // while waiting at a barrier. Keep the acknowledgement separate.
                            if let Err(e) = &result {
                                report(Err(anyhow::anyhow!("{e:#}")));
                            }
                            let _ = done.send(result);
                        }
                        // Best effort: a missing seed only delays time left for a new book.
                        Command::Seed(speed) => {
                            if let Err(e) = seed(&speed) {
                                log::warn!("saving reading speed: {e:#}");
                            }
                        }
                        Command::Stop => break,
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
}
