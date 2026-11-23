//! Ordered I/O service. Failed snapshots remain pending until retry/flush;
//! success is reported only after the durable manifest commit.
use super::import::Preview;
use super::*;
use crate::{koreader, library::Book, platform::Waker};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread::JoinHandle,
};
pub enum Command {
    Save(Collection),
    Preview(PathBuf, BTreeSet<String>, u64),
    Flush(mpsc::Sender<Result<(), String>>),
    /// Writes one book's Markdown export; never changes annotations.
    Export(export::Export),
    Stop,
}
pub enum Reply {
    Loaded(Result<(Collection, String), String>),
    Saved(Result<(), String>),
    Preview(u64, Result<Preview, String>),
    /// The export's destination, or why it couldn't be written.
    Exported(Result<PathBuf, String>),
    /// KOReader's position for the book (`koreader::check`), sent before
    /// `Loaded` when the worker was started with a check.
    Koreader(Result<Option<(koreader::OnOpen, koreader::Progress)>, String>),
}
/// The KOReader check on open: the book's
/// sidecar metadata file and its state as loaded for this open.
pub struct KoreaderCheck {
    pub path: PathBuf,
    pub state: crate::store::BookState,
}
pub struct Worker {
    tx: mpsc::Sender<Command>,
    rx: mpsc::Receiver<Reply>,
    join: Option<JoinHandle<()>>,
    pub book: Book,
    import_id: Arc<AtomicU64>,
}
impl Worker {
    /// Starts the book's I/O thread. A KOReader `check` runs first, before
    /// the book is hashed, so its reply can arrive with the first page.
    pub fn new(store: Store, book: Book, waker: Waker, check: Option<KoreaderCheck>) -> Self {
        let (tx, commands) = mpsc::channel();
        let (results, rx) = mpsc::channel();
        let mut b = book.clone();
        let import_id = Arc::new(AtomicU64::new(0));
        let current = import_id.clone();
        let join = std::thread::Builder::new()
            .name("verso-annotations".into())
            .spawn(move || {
                let send = |r| {
                    let _ = results.send(r);
                    waker.wake();
                };
                if let Some(check) = check {
                    let started = std::time::Instant::now();
                    let result = koreader::check(&check.path, &check.state, &|| false).map_err(|e| format!("{e:#}"));
                    log::info!(
                        "koreader check: found={:?} elapsed_us={}",
                        result.as_ref().map(|r| r.as_ref().map(|(found, _)| *found)),
                        started.elapsed().as_micros()
                    );
                    send(Reply::Koreader(result));
                }
                let loaded = (|| -> Result<_> {
                    use std::io::Read;
                    let mut file = std::fs::File::open(&b.path)?;
                    let mut hash = Sha256::new();
                    let mut buffer = [0; 64 * 1024];
                    loop {
                        let n = file.read(&mut buffer)?;
                        if n == 0 {
                            break;
                        }
                        hash.update(&buffer[..n]);
                    }
                    Ok((Collection::load(&store, &b.annotation_id)?, format!("{:x}", hash.finalize())))
                })()
                .map_err(|e| format!("{e:#}"));
                if let Ok((_, fingerprint)) = &loaded {
                    b.fingerprint = fingerprint.clone();
                }
                send(Reply::Loaded(loaded));
                let mut pending: Option<Collection> = None;
                for command in commands {
                    match command {
                        Command::Save(c) => {
                            pending = Some(c);
                            let result = pending
                                .as_ref()
                                .unwrap()
                                .commit(&store, &b.annotation_id)
                                .map_err(|e| format!("{e:#}"));
                            if result.is_ok() {
                                pending = None;
                            }
                            send(Reply::Saved(result));
                        }
                        Command::Preview(path, known, id) => send(Reply::Preview(
                            id,
                            import::read(&b.annotation_id, &b.fingerprint, &path, &known, &|| {
                                current.load(Ordering::Relaxed) != id
                            })
                            .map_err(|e| format!("{e:#}")),
                        )),
                        Command::Flush(ack) => {
                            let result = pending
                                .as_ref()
                                .map(|c| c.commit(&store, &b.annotation_id))
                                .unwrap_or(Ok(()))
                                .map_err(|e| format!("{e:#}"));
                            if result.is_ok() && pending.take().is_some() {
                                // A snapshot is pending only after a reported
                                // failure. Report its retry in reply order so the
                                // UI clears that error after earlier replies.
                                send(Reply::Saved(Ok(())));
                            }
                            let _ = ack.send(result);
                        }
                        Command::Export(e) => send(Reply::Exported(
                            export::write(&store, &b.annotation_id, &e).map_err(|e| format!("{e:#}")),
                        )),
                        Command::Stop => break,
                    }
                }
            })
            .expect("start annotation writer");
        Self { tx, rx, join: Some(join), book, import_id }
    }
    pub fn cancel_preview(&self) {
        self.import_id.fetch_add(1, Ordering::Relaxed);
    }
    pub fn send(&self, c: Command) {
        if let Command::Preview(_, _, id) = &c {
            self.import_id.store(*id, Ordering::Relaxed);
        }
        let _ = self.tx.send(c);
    }
    pub fn drain(&self) -> impl Iterator<Item = Reply> + '_ {
        self.rx.try_iter()
    }
    pub fn flush(&self) -> Result<()> {
        self.cancel_preview();
        let (tx, rx) = mpsc::channel();
        self.tx.send(Command::Flush(tx))?;
        rx.recv()?.map_err(anyhow::Error::msg)
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.flush();
        let _ = self.tx.send(Command::Stop);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn flush_retry_is_acknowledged_in_order_without_losing_queued_replies() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-annotation-flush-{}", std::process::id()));
        let store = Store::at(root.clone());
        std::fs::create_dir_all(store.annotation_dir().join("book-book.json.tmp"))?;
        let mut book = crate::library::selection_tests::book(1);
        book.annotation_id = "book".into();
        book.path = root.join("book.epub");
        std::fs::write(&book.path, b"book bytes")?;
        let source = root.join("metadata.epub.lua");
        std::fs::write(
            &source,
            "return { annotations = { { drawer='lighten', pos0='/a', pos1='/b', text='Quote' } } }",
        )?;
        let worker = Worker::new(store.clone(), book, Waker::new(|| {}), None);
        // A barrier that, unlike flush(), doesn't cancel the queued preview.
        let barrier = || {
            let (tx, rx) = mpsc::channel();
            worker.send(Command::Flush(tx));
            rx.recv().unwrap()
        };
        let mut collection = Collection::load(&store, "book")?;
        collection.add(super::super::tests::record("first"));
        worker.send(Command::Save(collection));
        worker.send(Command::Preview(source, BTreeSet::new(), 1));
        assert!(barrier().is_err(), "still blocked: the snapshot stays pending");
        std::fs::remove_dir(store.annotation_dir().join("book-book.json.tmp"))?;
        assert!(barrier().is_ok());
        assert!(barrier().is_ok(), "nothing pending: no further acknowledgement");
        let replies: Vec<_> = worker.drain().collect();
        assert!(matches!(
            replies.as_slice(),
            [Reply::Loaded(Ok(_)), Reply::Saved(Err(_)), Reply::Preview(1, Ok(p)), Reply::Saved(Ok(()))]
                if p.records.len() == 1
        ));
        assert_eq!(Collection::load(&store, "book")?.records.len(), 1);
        drop(worker);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
