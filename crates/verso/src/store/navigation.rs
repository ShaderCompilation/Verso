//! Library navigation and sort choice are written on their own thread. A
//! synced write on the Kindle's FUSE storage took 5–951 ms (measured), and the
//! UI thread must keep taking touches meanwhile. One thread owns every
//! navigation write, and sort updates serialize with other `settings.json`
//! writers through `Store::update_settings`. Snapshots and sort choices that
//! queue behind a slow write collapse to the newest.
use super::Store;
use crate::{library::collections::Navigation, library::sort::Sort, platform::Waker};
use anyhow::{Context, Result};
use std::sync::mpsc;
use std::thread::JoinHandle;

enum Command {
    Save(Navigation),
    SaveSort(Sort),
    Flush(mpsc::Sender<Result<()>>),
    Stop,
}
pub struct Writer {
    tx: mpsc::Sender<Command>,
    results: mpsc::Receiver<Result<()>>,
    sort_saves: mpsc::Receiver<(u64, u64)>,
    join: Option<JoinHandle<()>>,
}
impl Writer {
    pub fn new(store: Store, waker: Waker) -> Self {
        let sort_store = store.clone();
        Self::start(
            waker,
            move |n| store.save_navigation(n),
            move |sort| sort_store.update_settings(|s| s.library_sort = sort),
        )
    }
    fn start(
        waker: Waker,
        mut write_nav: impl FnMut(&Navigation) -> Result<()> + Send + 'static,
        mut write_sort: impl FnMut(Sort) -> Result<(u64, u64)> + Send + 'static,
    ) -> Self {
        let (tx, commands) = mpsc::channel();
        let (results, rx) = mpsc::channel();
        let (sort_saves_tx, sort_saves) = mpsc::channel();
        let join = std::thread::Builder::new()
            .name("verso-navigation".into())
            .spawn(move || {
                // Independent retry slots for navigation and sort; flushes retry whichever failed.
                let mut failed_nav: Option<Navigation> = None;
                let mut failed_sort: Option<Sort> = None;
                let report = |result: &Result<()>| {
                    if let Err(e) = result {
                        let _ = results.send(Err(anyhow::anyhow!("{e:#}")));
                        waker.wake();
                    }
                };
                while let Ok(first) = commands.recv() {
                    // Take everything queued behind the last write. Only the
                    // newest navigation and newest sort are written; flushes
                    // are acknowledged after both, covering everything sent
                    // before them.
                    let mut newest_nav = None;
                    let mut newest_sort = None;
                    let mut flushes = Vec::new();
                    let mut stop = false;
                    for command in std::iter::once(first).chain(commands.try_iter()) {
                        match command {
                            Command::Save(n) => newest_nav = Some(n),
                            Command::SaveSort(s) => newest_sort = Some(s),
                            Command::Flush(done) => flushes.push(done),
                            Command::Stop => stop = true,
                        }
                    }
                    if !flushes.is_empty() {
                        if newest_nav.is_none() {
                            newest_nav = failed_nav.take();
                        }
                        if newest_sort.is_none() {
                            newest_sort = failed_sort.take();
                        }
                    }
                    let nav_result = newest_nav.as_ref().map_or(Ok(()), &mut write_nav).map_err(|e| {
                        log::error!("saving library navigation: {e:#}");
                        anyhow::anyhow!("Couldn't save library navigation: {e:#}")
                    });
                    if nav_result.is_err() {
                        failed_nav = newest_nav;
                    } else if newest_nav.is_some() {
                        failed_nav = None;
                    }
                    report(&nav_result);

                    let sort_result = match newest_sort {
                        Some(sort) => match write_sort(sort) {
                            Ok(saves) => {
                                failed_sort = None;
                                let _ = sort_saves_tx.send(saves);
                                Ok(())
                            }
                            Err(e) => {
                                failed_sort = Some(sort);
                                log::error!("saving library sort: {e:#}");
                                Err(anyhow::anyhow!("Couldn't save the sort: {e:#}"))
                            }
                        },
                        None => Ok(()),
                    };
                    report(&sort_result);

                    let flush_result = nav_result.and(sort_result);
                    for done in flushes {
                        let _ = done.send(flush_result.as_ref().map_err(|e| anyhow::anyhow!("{e:#}")).copied());
                    }
                    if stop {
                        break;
                    }
                }
            })
            .expect("start navigation thread");
        Self { tx, results: rx, sort_saves, join: Some(join) }
    }
    /// Queues a snapshot and returns at once.
    pub fn save(&self, navigation: Navigation) -> Result<()> {
        self.tx.send(Command::Save(navigation)).context("navigation writer stopped")
    }
    /// Queues a sort choice to persist in `settings.json` and returns at once.
    pub fn save_sort(&self, sort: Sort) -> Result<()> {
        self.tx.send(Command::SaveSort(sort)).context("navigation writer stopped")
    }
    /// Failed writes since the last call; each one also wakes the UI.
    pub fn errors(&self) -> impl Iterator<Item = anyhow::Error> + '_ {
        self.results.try_iter().filter_map(Result::err)
    }
    /// `(before, after)` `Store::settings_saves` counters for completed sort
    /// writes since the last call.
    pub fn sort_saves(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        self.sort_saves.try_iter()
    }
    /// Blocks until the newest navigation and sort sent so far are durably
    /// written, retrying whichever failed.
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
        let writer = Writer::start(
            Waker::new(|| {}),
            move |n| {
                entered.send(n.page()).unwrap();
                gate.recv().unwrap();
                saved.lock().unwrap().push(n.page());
                Ok(())
            },
            |_| Ok((0, 1)),
        );
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
            |_| Ok((0, 1)),
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
        let writer = Writer::start(
            Waker::new(|| {}),
            move |n| {
                saved.lock().unwrap().push(n.page());
                Ok(())
            },
            |_| Ok((0, 1)),
        );
        writer.save(at(7)).unwrap();
        drop(writer);
        assert_eq!(seen.lock().unwrap().last(), Some(&7));
    }
    #[test]
    fn sort_saves_coalesce_retry_independently_on_flush_and_report_errors() {
        // Rapid sort changes while a write is in flight collapse to the newest.
        let (entered, started) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let saved = seen.clone();
        let mut count = 0u64;
        let writer = Writer::start(
            Waker::new(|| {}),
            |_| Ok(()),
            move |sort| {
                entered.send(sort).unwrap();
                gate.recv().unwrap();
                saved.lock().unwrap().push(sort);
                let before = count;
                count += 1;
                Ok((before, count))
            },
        );
        writer.save_sort(Sort::Author).unwrap();
        assert_eq!(started.recv().unwrap(), Sort::Author);
        for sort in [Sort::Series, Sort::RecentlyAdded, Sort::RecentlyRead] {
            writer.save_sort(sort).unwrap();
        }
        let (complete, completed) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            writer.flush().unwrap();
            complete.send(()).unwrap();
            writer
        });
        release.send(()).unwrap();
        assert_eq!(started.recv().unwrap(), Sort::RecentlyRead, "intermediate sorts are skipped");
        assert!(completed.try_recv().is_err(), "flush waits for the newest sort write");
        release.send(()).unwrap();
        completed.recv().unwrap();
        let writer = waiter.join().unwrap();
        assert_eq!(*seen.lock().unwrap(), vec![Sort::Author, Sort::RecentlyRead]);
        assert_eq!(writer.sort_saves().collect::<Vec<_>>(), vec![(0, 1), (1, 2)]);
        drop(writer);

        // A failing sort write wakes the UI, reports the error, and retries on
        // flush without re-writing an already-succeeded navigation snapshot.
        let nav_seen = Arc::new(Mutex::new(Vec::new()));
        let nav_saved = nav_seen.clone();
        let sort_seen = Arc::new(Mutex::new(Vec::new()));
        let sort_saved = sort_seen.clone();
        let (wake, wakes) = mpsc::channel();
        let mut attempt = 0;
        let writer = Writer::start(
            Waker::new(move || {
                let _ = wake.send(());
            }),
            move |n| {
                nav_saved.lock().unwrap().push(n.page());
                Ok(())
            },
            move |sort| {
                attempt += 1;
                sort_saved.lock().unwrap().push(sort);
                anyhow::ensure!(attempt > 2, "disk full");
                Ok((5, 6))
            },
        );
        writer.save(at(2)).unwrap();
        writer.save_sort(Sort::Author).unwrap();
        wakes.recv().unwrap();
        let err = writer.errors().next().unwrap().to_string();
        assert!(err.starts_with("Couldn't save the sort") && err.contains("disk full"), "{err}");
        assert!(writer.flush().is_err(), "a failing sort retry is never acknowledged as saved");
        assert_eq!(writer.errors().count(), 1);
        writer.flush().unwrap();
        assert_eq!(writer.sort_saves().collect::<Vec<_>>(), vec![(5, 6)]);
        writer.flush().unwrap(); // Nothing left to retry.
        assert_eq!(*nav_seen.lock().unwrap(), vec![2], "succeeded navigation is not retried");
        assert_eq!(*sort_seen.lock().unwrap(), vec![Sort::Author, Sort::Author, Sort::Author]);
        assert_eq!(writer.errors().count(), 0);
    }
    #[test]
    fn flush_makes_the_newest_snapshot_durable_on_disk() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-navigation-writer-{}", std::process::id()));
        let store = Store::at(root.clone());
        let writer = Writer::new(store.clone(), Waker::new(|| {}));
        for page in 1..=3 {
            writer.save(at(page))?;
        }
        writer.save_sort(Sort::Series)?;
        writer.flush()?;
        assert_eq!(store.navigation().page(), 3);
        assert_eq!(store.settings().library_sort, Sort::Series);
        assert_eq!(writer.sort_saves().collect::<Vec<_>>(), vec![(0, 1)]);
        drop(writer);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
