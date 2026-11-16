//! Library navigation is written on its own thread. A synced write on the
//! Kindle's FUSE storage took 5–951 ms (measured), and the UI thread must
//! keep taking touches meanwhile. One thread owns every write, so writes never
//! overlap on the shared `.tmp` file or land out of order. Snapshots that
//! queue behind a slow write collapse to the newest.
use super::Store;
use crate::{library::collections::Navigation, platform::Waker};
use anyhow::{Context, Result};
use std::sync::mpsc;
use std::thread::JoinHandle;

enum Command {
    Save(Navigation),
    Flush(mpsc::Sender<Result<()>>),
    Stop,
}
pub struct Writer {
    tx: mpsc::Sender<Command>,
    results: mpsc::Receiver<Result<()>>,
    join: Option<JoinHandle<()>>,
}
impl Writer {
    pub fn new(store: Store, waker: Waker) -> Self {
        Self::start(waker, move |n| store.save_navigation(n))
    }
    fn start(waker: Waker, mut write: impl FnMut(&Navigation) -> Result<()> + Send + 'static) -> Self {
        let (tx, commands) = mpsc::channel();
        let (results, rx) = mpsc::channel();
        let join = std::thread::Builder::new()
            .name("verso-navigation".into())
            .spawn(move || {
                // The newest snapshot whose write failed; flushes retry it.
                let mut failed: Option<Navigation> = None;
                let report = |result: &Result<()>| {
                    if let Err(e) = result {
                        log::error!("saving library navigation: {e:#}");
                        let _ = results.send(Err(anyhow::anyhow!("{e:#}")));
                        waker.wake();
                    }
                };
                while let Ok(first) = commands.recv() {
                    // Take everything queued behind the last write. Only the
                    // newest snapshot is written; flushes are acknowledged
                    // after it, so they cover every snapshot sent before them.
                    let mut newest = None;
                    let mut flushes = Vec::new();
                    let mut stop = false;
                    for command in std::iter::once(first).chain(commands.try_iter()) {
                        match command {
                            Command::Save(n) => newest = Some(n),
                            Command::Flush(done) => flushes.push(done),
                            Command::Stop => stop = true,
                        }
                    }
                    if newest.is_none() && !flushes.is_empty() {
                        newest = failed.take();
                    }
                    let result = newest.as_ref().map_or(Ok(()), &mut write);
                    if result.is_err() {
                        failed = newest;
                    } else if newest.is_some() {
                        failed = None;
                    }
                    report(&result);
                    for done in flushes {
                        let _ = done.send(result.as_ref().map_err(|e| anyhow::anyhow!("{e:#}")).copied());
                    }
                    if stop {
                        break;
                    }
                }
            })
            .expect("start navigation thread");
        Self { tx, results: rx, join: Some(join) }
    }
    /// Queues a snapshot and returns at once.
    pub fn save(&self, navigation: Navigation) -> Result<()> {
        self.tx.send(Command::Save(navigation)).context("navigation writer stopped")
    }
    /// Failed writes since the last call; each one also wakes the UI.
    pub fn errors(&self) -> impl Iterator<Item = anyhow::Error> + '_ {
        self.results.try_iter().filter_map(Result::err)
    }
    /// Blocks until the newest snapshot sent so far is durably written,
    /// retrying it if its write failed.
    pub fn flush(&self) -> Result<()> {
        let (done, result) = mpsc::channel();
        self.tx.send(Command::Flush(done)).context("navigation writer stopped")?;
        result.recv().context("navigation writer stopped before saving")?
    }
}
impl Drop for Writer {
    fn drop(&mut self) {
        // Snapshots sent before Stop are still written.
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
    fn at(page: usize) -> Navigation {
        let mut n = Navigation::default();
        n.set_page(page);
        n
    }
    #[test]
    fn saves_return_during_a_slow_write_and_queued_snapshots_collapse_to_the_newest() {
        let (entered, started) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let saved = seen.clone();
        let writer = Writer::start(Waker::new(|| {}), move |n| {
            entered.send(n.page()).unwrap();
            gate.recv().unwrap();
            saved.lock().unwrap().push(n.page());
            Ok(())
        });
        writer.save(at(1)).unwrap();
        assert_eq!(started.recv().unwrap(), 1);
        // These return while page 1 is still being written.
        for page in 2..=5 {
            writer.save(at(page)).unwrap();
        }
        let (complete, completed) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            writer.flush().unwrap();
            complete.send(()).unwrap();
            writer
        });
        release.send(()).unwrap();
        assert_eq!(started.recv().unwrap(), 5, "pages 2–4 are skipped");
        assert!(completed.try_recv().is_err(), "flush waits for the newest write");
        release.send(()).unwrap();
        completed.recv().unwrap();
        drop(waiter.join().unwrap());
        assert_eq!(*seen.lock().unwrap(), vec![1, 5]);
    }
    #[test]
    fn failures_wake_the_ui_and_flush_retries_the_newest_snapshot() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let saved = seen.clone();
        let (wake, wakes) = mpsc::channel();
        let mut attempt = 0;
        let writer = Writer::start(
            Waker::new(move || {
                let _ = wake.send(());
            }),
            move |n| {
                attempt += 1;
                saved.lock().unwrap().push(n.page());
                anyhow::ensure!(attempt > 2, "disk full");
                Ok(())
            },
        );
        writer.save(at(1)).unwrap();
        wakes.recv().unwrap();
        assert!(writer.errors().next().unwrap().to_string().contains("disk full"));
        assert!(writer.flush().is_err(), "a failing retry is never acknowledged as saved");
        assert_eq!(writer.errors().count(), 1);
        writer.flush().unwrap();
        writer.flush().unwrap(); // Nothing left to retry.
        assert_eq!(*seen.lock().unwrap(), vec![1, 1, 1]);
        assert_eq!(writer.errors().count(), 0);
    }
    #[test]
    fn drop_writes_snapshots_sent_before_it() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let saved = seen.clone();
        let writer = Writer::start(Waker::new(|| {}), move |n| {
            saved.lock().unwrap().push(n.page());
            Ok(())
        });
        writer.save(at(7)).unwrap();
        drop(writer);
        assert_eq!(seen.lock().unwrap().last(), Some(&7));
    }
    #[test]
    fn flush_makes_the_newest_snapshot_durable_on_disk() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-navigation-writer-{}", std::process::id()));
        let store = Store::at(root.clone());
        let writer = Writer::new(store.clone(), Waker::new(|| {}));
        for page in 1..=3 {
            writer.save(at(page))?;
        }
        writer.flush()?;
        assert_eq!(store.navigation().page(), 3);
        drop(writer);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
