//! Ordered I/O service. Failed snapshots remain pending until retry/flush;
//! success is reported only after the durable manifest commit.
use super::*;
use crate::{document::Position, koreader, library::Book, platform::Waker};
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
#[derive(Clone, Default, Debug)]
pub struct Preview {
    pub records: Vec<Annotation>,
    pub duplicates: usize,
    pub unsupported: usize,
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
                            preview_cancellable(&b, &path, &known, &|| current.load(Ordering::Relaxed) != id)
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
pub fn preview(book: &Book, path: &std::path::Path, known: &BTreeSet<String>) -> Result<Preview> {
    preview_cancellable(book, path, known, &|| false)
}
fn preview_cancellable(
    book: &Book,
    path: &std::path::Path,
    known: &BTreeSet<String>,
    cancelled: &dyn Fn() -> bool,
) -> Result<Preview> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    let data = lua::parse_cancellable(&bytes, cancelled)?;
    let root = data.table()?;
    let table = root
        .get("annotations")
        .context("unsupported legacy KOReader format: modern annotations table required")?
        .table()?;
    let mut result = Preview::default();
    let mut seen = known.clone();
    for value in table.values() {
        ensure!(!cancelled(), "import cancelled");
        let Ok(entry) = value.table() else {
            result.unsupported += 1;
            continue;
        };
        let string = |key| entry.get(key).and_then(lua::Value::string).unwrap_or("").to_owned();
        // KOReader's own test (ReaderBookmark:getBookmarkType): an entry with
        // a drawer is a highlight or note, one without is a page bookmark.
        // A bookmark's `text` is KOReader's generated "in <chapter>", so it is
        // ignored; its opening words are read at its position when the
        // preview is checked against the book (the quotation stays empty
        // until then). A PDF bookmark's numeric page stays unsupported.
        let kind = if entry.contains_key("drawer") { Kind::Highlight } else { Kind::Bookmark };
        let (start, end, quotation) = match kind {
            Kind::Highlight => (string("pos0"), string("pos1"), string("text")),
            Kind::Bookmark => (string("page"), String::new(), String::new()),
        };
        let complete = match kind {
            Kind::Highlight => !start.is_empty() && !end.is_empty() && !quotation.is_empty(),
            Kind::Bookmark => !start.is_empty(),
        };
        if !complete {
            result.unsupported += 1;
            continue;
        }
        // Stable semantic source identity; formatting, path and row order don't
        // affect it. Changed source entries produce new independent copies.
        let created = string("datetime");
        let chapter = string("chapter");
        let note = string("note");
        let identity = digest(&serde_json::to_vec(&(&book.annotation_id, entry))?);
        if !seen.insert(identity.clone()) {
            result.duplicates += 1;
            continue;
        }
        result.records.push(Annotation {
            version: 1,
            kind,
            id: format!("import-{identity}"),
            book: book.annotation_id.clone(),
            fingerprint: book.fingerprint.clone(),
            range: TextRange { start: Position(start), end: Position(end) },
            quotation,
            created,
            chapter,
            note: (!note.is_empty()).then_some(note),
            provenance: Some(Provenance { source: path.display().to_string(), entry: identity }),
        });
    }
    Ok(result)
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
    #[test]
    fn imports_page_bookmarks_and_reimports_only_them_after_a_highlights_only_import() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-import-bookmarks-{}", std::process::id()));
        std::fs::create_dir_all(&root)?;
        let path = root.join("metadata.epub.lua");
        let mut book = crate::library::selection_tests::book(1);
        book.annotation_id = "book".into();
        book.fingerprint = "content".into();
        let highlight =
            "{ drawer='lighten',pos0='/body/p/text().0',pos1='/body/p/text().9',text='Quote',chapter='One' }";
        let bookmark = "{ page='/body/DocFragment[2]/body/p[3]/text().0',text='in Two',chapter='Two',datetime='2026-11-20 08:00:00',note='Come back',pageno=12 }";
        // What an earlier import saw and kept: the highlight only.
        std::fs::write(&path, format!("return {{ annotations = {{ {highlight} }} }}"))?;
        let earlier = preview(&book, &path, &BTreeSet::new())?;
        let known = BTreeSet::from([earlier.records[0].provenance.as_ref().unwrap().entry.clone()]);
        std::fs::write(&path, format!("return {{ annotations = {{ {highlight}, {bookmark} }} }}"))?;
        let again = preview(&book, &path, &known)?;
        assert_eq!((again.records.len(), again.duplicates, again.unsupported), (1, 1, 0));
        let b = &again.records[0];
        assert_eq!(b.kind, Kind::Bookmark);
        assert_eq!(b.range.start.0, "/body/DocFragment[2]/body/p[3]/text().0");
        assert!(b.range.end.0.is_empty() && b.quotation.is_empty(), "opening words come from the book thread");
        assert_eq!((b.chapter.as_str(), b.created.as_str()), ("Two", "2026-11-20 08:00:00"));
        assert_eq!(b.note.as_deref(), Some("Come back"));
        assert_eq!(b.fingerprint, "content");
        assert!(b.id.starts_with("import-"));
        // Importing the same file again finds nothing new.
        let mut all = known.clone();
        all.insert(b.provenance.as_ref().unwrap().entry.clone());
        assert_eq!(preview(&book, &path, &all)?.duplicates, 2);
        // A bookmark without a position is unsupported.
        std::fs::write(&path, "return { annotations = { { text='in Two' } } }")?;
        assert_eq!(preview(&book, &path, &BTreeSet::new())?.unsupported, 1);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn imports_unicode_notes_duplicates_changes_and_legacy() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-import-{}", std::process::id()));
        std::fs::create_dir_all(&root)?;
        let path = root.join("metadata.epub.lua");
        let mut book = crate::library::selection_tests::book(1);
        book.annotation_id = "book".into();
        let data = "return { annotations = { [1] = { drawer='lighten',pos0='/body/p/text().0',pos1='/body/p/text().9',text='Привет!',note='My \\\"note\\\"',datetime='2026-11-15 12:00:00',chapter='Chapter',text_edited=true }, [2]={drawer='lighten',pos0={x=1},pos1={x=2},text='PDF'},[3]={page=12,text='in PDF'} } }";
        std::fs::write(&path, data)?;
        let first = preview(&book, &path, &BTreeSet::new())?;
        assert_eq!(first.records.len(), 1);
        assert_eq!(first.unsupported, 2, "PDF highlights and PDF bookmarks");
        assert_eq!(first.records[0].quotation, "Привет!");
        assert_eq!(first.records[0].note.as_deref(), Some("My \"note\""));
        let known = BTreeSet::from([first.records[0].provenance.as_ref().unwrap().entry.clone()]);
        assert_eq!(preview(&book, &path, &known)?.duplicates, 1);
        std::fs::write(&path, data.replace("Привет!", "Привет?"))?;
        assert_eq!(preview(&book, &path, &known)?.records.len(), 1);
        std::fs::write(&path, "return {highlight={}}")?;
        assert!(preview(&book, &path, &known).unwrap_err().to_string().contains("legacy"));
        // Optional local-only sample copied read-only from the installed version;
        // no personal quotations become repository fixtures.
        if let Ok(sample) = std::fs::read("/tmp/verso-koreader-sample.lua") {
            lua::parse(&sample)?;
        }
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
