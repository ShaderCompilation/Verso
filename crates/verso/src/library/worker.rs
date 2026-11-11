//! One coalescing library worker. A single replaceable snapshot bounds pending results.
use super::{Book, Cover, LibraryIndex, Metadata, Stamp, assign_ids, covers, epub};
use crate::platform::Waker;
use crate::store::{Store, book_id};
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Instant, UNIX_EPOCH};

#[derive(Clone, Default)]
pub struct Snapshot {
    pub index: LibraryIndex,
    pub scanning: bool,
    pub working: bool,
    pub errors: Vec<String>,
}
#[derive(Default)]
struct Shared {
    stop: bool,
    requested: bool,
    retry: bool,
    paused: bool,
    priority: Vec<String>,
    latest: Option<Snapshot>,
}
pub struct Worker {
    shared: Arc<(Mutex<Shared>, Condvar)>,
    join: Option<JoinHandle<()>>,
}
impl Worker {
    pub fn new(store: Store, roots: Vec<PathBuf>, waker: Waker) -> Self {
        let shared = Arc::new((Mutex::new(Shared { requested: true, ..Shared::default() }), Condvar::new()));
        let thread_shared = shared.clone();
        let join = std::thread::Builder::new()
            .name("verso-library".into())
            .spawn(move || {
                let shared = thread_shared;
                let mut index = store.library();
                let cancelled = || shared.0.lock().unwrap().stop;
                let publish = |snapshot| {
                    let wake = shared.0.lock().unwrap().latest.replace(snapshot).is_none();
                    if wake {
                        waker.wake();
                    }
                };
                loop {
                    let retry = {
                        let mut state = shared.0.lock().unwrap();
                        while !state.stop && !state.requested {
                            state = shared.1.wait(state).unwrap();
                        }
                        if state.stop {
                            break;
                        }
                        state.requested = false;
                        std::mem::take(&mut state.retry)
                    };
                    let mut errors = Vec::new();
                    let result = scan_roots(&roots, &index, retry, &cancelled, |checkpoint| {
                        let mut snapshot = Snapshot {
                            index: checkpoint.clone(),
                            scanning: true,
                            working: true,
                            errors: errors.clone(),
                        };
                        if let Err(e) = store.save_library(checkpoint) {
                            snapshot.errors.push(format!("Couldn't save library: {e}"));
                        }
                        publish(snapshot);
                    });
                    let Some(scan) = result else { break };
                    log::debug!("scan completed with {} metadata reads", scan.metadata_reads);
                    index = scan.index;
                    errors.extend(scan.errors);
                    if let Err(e) = store.save_library(&index) {
                        errors.push(format!("Couldn't save library: {e}"));
                    }
                    publish(Snapshot { index: index.clone(), scanning: false, working: true, errors: errors.clone() });
                    let covers_started = Instant::now();
                    let mut pending: std::collections::BTreeSet<_> =
                        index.books.iter().enumerate().filter(|(_, b)| b.available).map(|(i, _)| i).collect();
                    let id_indices: HashMap<_, _> =
                        index.books.iter().enumerate().map(|(i, b)| (b.id.clone(), i)).collect();
                    let mut since_publish = Instant::now();
                    let mut completed = 0;
                    while !pending.is_empty() {
                        let next = {
                            let mut state = shared.0.lock().unwrap();
                            while state.paused && !state.stop && !state.requested {
                                state = shared.1.wait(state).unwrap();
                            }
                            if state.stop || state.requested {
                                break;
                            }
                            state
                                .priority
                                .iter()
                                .filter_map(|id| id_indices.get(id).copied())
                                .find(|i| pending.contains(i))
                                .unwrap_or_else(|| *pending.first().unwrap())
                        };
                        pending.remove(&next);
                        match covers::generate(&index.books[next], &store) {
                            Ok(cover) => index.books[next].cover = cover,
                            Err(e) => {
                                if errors.len() < 32 {
                                    errors.push(format!("Couldn't cache cover: {e}"));
                                }
                            }
                        }
                        completed += 1;
                        if completed % 64 == 0
                            && let Err(e) = store.save_library(&index)
                            && errors.len() < 32
                        {
                            errors.push(format!("Couldn't save covers: {e}"));
                        }
                        if completed % 4 == 0 || since_publish.elapsed().as_millis() >= 100 {
                            publish(Snapshot {
                                index: index.clone(),
                                scanning: false,
                                working: true,
                                errors: errors.clone(),
                            });
                            since_publish = Instant::now();
                        }
                    }
                    log::info!("library covers: {completed} processed in {:?}", covers_started.elapsed());
                    let identity_started = Instant::now();
                    // Fingerprinting new additions is deferred until metadata and covers
                    // are visible. Only possible moves need it before identity assignment.
                    for i in 0..index.books.len() {
                        {
                            let mut state = shared.0.lock().unwrap();
                            while state.paused && !state.stop && !state.requested {
                                state = shared.1.wait(state).unwrap();
                            }
                            if state.stop || state.requested {
                                break;
                            }
                        }
                        if index.books[i].available
                            && index.books[i].fingerprint.is_empty()
                            && !index.books[i].fingerprint_attempted
                        {
                            index.books[i].fingerprint =
                                fingerprint(&index.books[i].path, &cancelled).unwrap_or_default();
                            index.books[i].fingerprint_attempted = !cancelled();
                            if i % 64 == 0
                                && let Err(e) = store.save_library(&index)
                            {
                                errors.push(format!("Couldn't save library identity: {e}"));
                            }
                        }
                    }
                    if cancelled() {
                        break;
                    }
                    if let Err(e) = store.save_library(&index) {
                        errors.push(format!("Couldn't save covers: {e}"));
                    }
                    log::info!("library identities complete in {:?}", identity_started.elapsed());
                    publish(Snapshot { index: index.clone(), scanning: false, working: false, errors });
                }
            })
            .expect("start library thread");
        Self { shared, join: Some(join) }
    }
    pub fn rescan(&self) {
        let mut state = self.shared.0.lock().unwrap();
        state.requested = true;
        state.retry = true;
        self.shared.1.notify_one();
    }
    pub fn pause(&self, paused: bool) {
        self.shared.0.lock().unwrap().paused = paused;
        self.shared.1.notify_one();
    }
    pub fn prioritize(&self, ids: Vec<String>) {
        self.shared.0.lock().unwrap().priority = ids.into_iter().take(32).collect();
    }
    pub fn take(&self) -> Option<Snapshot> {
        self.shared.0.lock().unwrap().latest.take()
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.shared.0.lock().unwrap().stop = true;
        self.shared.1.notify_one();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

pub struct Scan {
    pub index: LibraryIndex,
    pub errors: Vec<String>,
    pub metadata_reads: usize,
}
fn stamp(meta: &std::fs::Metadata) -> Result<Stamp> {
    let modified = meta.modified()?.duration_since(UNIX_EPOCH).unwrap_or_default();
    Ok(Stamp { size: meta.len(), modified_secs: modified.as_secs(), modified_nanos: modified.subsec_nanos() })
}
fn fingerprint(path: &Path, cancelled: &impl Fn() -> bool) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        anyhow::ensure!(!cancelled(), "scan cancelled");
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn enumerate(
    root: &Path,
    cancelled: &impl Fn() -> bool,
    files: &mut Vec<PathBuf>,
    uncertain: &mut Vec<PathBuf>,
    errors: &mut Vec<String>,
) {
    if cancelled() {
        return;
    }
    let read = match std::fs::read_dir(root) {
        Ok(read) => read,
        Err(e) => {
            uncertain.push(root.to_owned());
            errors.push(format!("Couldn't read {}: {e}", root.display()));
            return;
        }
    };
    for item in read {
        if cancelled() {
            return;
        }
        let item = match item {
            Ok(item) => item,
            Err(e) => {
                uncertain.push(root.to_owned());
                errors.push(format!("Couldn't enumerate {}: {e}", root.display()));
                continue;
            }
        };
        match item.file_type() {
            Ok(kind) if kind.is_dir() => enumerate(&item.path(), cancelled, files, uncertain, errors),
            Ok(kind)
                if kind.is_file() && item.path().extension().is_some_and(|ext| ext.eq_ignore_ascii_case("epub")) =>
            {
                files.push(item.path())
            }
            Ok(_) => {}
            Err(e) => {
                uncertain.push(item.path());
                errors.push(format!("Couldn't inspect {}: {e}", item.path().display()));
            }
        }
    }
}

pub fn scan_roots(
    roots: &[PathBuf],
    previous: &LibraryIndex,
    retry: bool,
    cancelled: &impl Fn() -> bool,
    mut checkpoint: impl FnMut(&LibraryIndex),
) -> Option<Scan> {
    let started = Instant::now();
    let mut paths = Vec::new();
    let mut uncertain = Vec::new();
    let mut errors = Vec::new();
    for root in roots {
        enumerate(root, cancelled, &mut paths, &mut uncertain, &mut errors);
    }
    paths.sort();
    paths.dedup();
    let old: HashMap<_, _> = previous.books.iter().map(|b| (&b.path, b)).collect();
    let found: HashSet<_> = paths.iter().cloned().collect();
    let mut books = Vec::new();
    let mut dirty = HashSet::new();
    let mut reads = 0;
    let possible_moves = previous.books.iter().any(|b| !found.contains(&b.path));
    for path in paths {
        if cancelled() {
            return None;
        }
        let file_stamp = match std::fs::metadata(&path).map_err(anyhow::Error::from).and_then(|m| stamp(&m)) {
            Ok(stamp) => stamp,
            Err(e) => {
                uncertain.push(path.clone());
                errors.push(format!("Couldn't inspect {}: {e}", path.display()));
                continue;
            }
        };
        let cached = old.get(&path);
        let changed = cached.is_none_or(|old| old.stamp != file_stamp);
        if let Some(cached) = cached
            && !changed
            && !(retry && cached.error.is_some())
            && !cached.metadata_pending
        {
            let mut b = (*cached).clone();
            b.available = true;
            books.push(b);
            continue;
        }
        let legacy_id = book_id(&path).unwrap_or_default();
        let fingerprint_attempted =
            if changed { cached.is_some() || possible_moves } else { cached.is_some_and(|b| b.fingerprint_attempted) };
        let fingerprint = if changed && (cached.is_some() || possible_moves) {
            fingerprint(&path, cancelled).unwrap_or_default()
        } else if changed {
            String::new()
        } else {
            cached.map_or(String::new(), |b| b.fingerprint.clone())
        };
        let mut metadata = Metadata {
            title: path.file_stem().unwrap_or_default().to_string_lossy().into_owned(),
            ..Metadata::default()
        };
        let mut metadata_pending = true;
        let mut error = None;
        if changed && cached.is_some() {
            reads += 1;
            match epub::read(&path) {
                Ok(parsed) => metadata = parsed,
                Err(e) => error = Some(format!("{e:#}")),
            }
            metadata_pending = false;
        } else {
            dirty.insert(path.clone());
        }
        books.push(Book {
            metadata,
            path,
            id: String::new(),
            legacy_id,
            fingerprint,
            fingerprint_attempted,
            stamp: file_stamp,
            error,
            cover: Cover::Pending,
            available: true,
            metadata_pending,
        });
    }
    if cancelled() {
        return None;
    }
    // Retain tombstones, and leave inaccessible subtrees visible. Path lookups
    // must stay linear overall for large cached collections, including on exit.
    let mut present: HashSet<_> = books.iter().map(|b| b.path.clone()).collect();
    for cached in &previous.books {
        if cancelled() {
            return None;
        }
        if !found.contains(&cached.path) || uncertain.iter().any(|p| cached.path.starts_with(p)) {
            let mut b = cached.clone();
            b.available = roots.iter().any(|root| cached.path.starts_with(root))
                && uncertain.iter().any(|p| cached.path.starts_with(p));
            if present.insert(b.path.clone()) {
                books.push(b);
            }
        }
    }
    assign_ids(&mut books, &previous.books);
    // A moved entry supersedes its unavailable old path.
    let available_ids: HashSet<_> = books.iter().filter(|b| b.available).map(|b| b.id.clone()).collect();
    books.retain(|b| b.available || !available_ids.contains(&b.id));
    let mut index = LibraryIndex { books, ..LibraryIndex::default() };
    checkpoint(&index);
    for i in 0..index.books.len() {
        if cancelled() {
            return None;
        }
        let book = &mut index.books[i];
        if !dirty.contains(&book.path) {
            continue;
        }
        reads += 1;
        match epub::read(&book.path) {
            Ok(metadata) => book.metadata = metadata,
            Err(e) => book.error = Some(format!("{e:#}")),
        }
        book.metadata_pending = false;
        if reads % 64 == 0 {
            checkpoint(&index);
        }
    }
    log::info!(
        "library scan: {} available, {reads} metadata reads, {:?}",
        index.books.iter().filter(|b| b.available).count(),
        started.elapsed()
    );
    Some(Scan { index, errors, metadata_reads: reads })
}

#[cfg(test)]
pub fn scan(
    root: &Path,
    previous: &LibraryIndex,
    retry: bool,
    cancelled: &impl Fn() -> bool,
    checkpoint: impl FnMut(&LibraryIndex),
) -> Option<Scan> {
    scan_roots(&[root.to_owned()], previous, retry, cancelled, checkpoint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::epub::tests::fixture;
    #[test]
    fn multiple_folders_spaces_overlaps_and_partial_failure() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-multi-folders-{}", std::process::id()));
        let documents = root.join("documents");
        let library = root.join("My Library");
        std::fs::create_dir_all(&documents)?;
        std::fs::create_dir_all(&library)?;
        fixture(&documents.join("same.epub"), "<package/>", &[])?;
        fixture(&library.join("same.epub"), "<package/>", &[])?;
        let roots = vec![root.clone(), documents, library.clone()];
        let first = scan_roots(&roots, &Default::default(), false, &|| false, |_| {}).unwrap();
        assert_eq!(first.index.books.len(), 2);
        assert_ne!(first.index.books[0].id, first.index.books[1].id);
        std::fs::rename(&library, root.join("offline"))?;
        // Enumerate only the original roots: the inaccessible one retains its entry.
        let next = scan_roots(&roots[1..], &first.index, false, &|| false, |_| {}).unwrap();
        assert_eq!(next.index.books.iter().filter(|b| b.available).count(), 2);
        assert!(!next.errors.is_empty());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    fn run(root: &Path, old: &LibraryIndex, retry: bool) -> Scan {
        let mut result = scan(root, old, retry, &|| false, |_| {}).unwrap();
        for book in &mut result.index.books {
            if book.available && book.fingerprint.is_empty() {
                book.fingerprint = fingerprint(&book.path, &|| false).unwrap_or_default();
            }
        }
        result
    }
    #[test]
    fn incremental_discovery_failures_moves_and_missing_subtrees() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-scan-{}", std::process::id()));
        std::fs::create_dir_all(root.join("nested"))?;
        let a = root.join("nested/one.EPUB");
        fixture(
            &a,
            r#"<package unique-identifier="id"><metadata><identifier id="id">one</identifier></metadata></package>"#,
            &[],
        )?;
        std::fs::write(root.join("bad.epub"), b"broken")?;
        let initial = run(&root, &LibraryIndex::default(), false);
        assert_eq!(initial.metadata_reads, 2);
        assert_eq!(initial.index.books.len(), 2);
        assert!(initial.index.books.iter().any(|b| b.error.is_some()));
        let unchanged = run(&root, &initial.index, false);
        assert_eq!(unchanged.metadata_reads, 0);
        assert_eq!(run(&root, &unchanged.index, true).metadata_reads, 1);
        let id = initial.index.books.iter().find(|b| b.path == a).unwrap().id.clone();
        let moved_path = root.join("renamed.epub");
        std::fs::rename(&a, &moved_path)?;
        let moved = run(&root, &initial.index, false);
        assert_eq!(moved.metadata_reads, 1);
        assert_eq!(moved.index.books.iter().find(|b| b.path == moved_path).unwrap().id, id);
        assert!(!moved.index.books.iter().any(|b| b.path == a));
        fixture(
            &moved_path,
            r#"<package unique-identifier="id"><metadata><identifier id="id">one</identifier><title>Edited</title></metadata></package>"#,
            &[],
        )?;
        let changed = run(&root, &moved.index, false);
        assert_eq!(changed.metadata_reads, 1);
        assert_eq!(changed.index.books.iter().find(|b| b.path == moved_path).unwrap().id, id);
        let hidden_root = root.with_extension("unavailable");
        std::fs::rename(&root, &hidden_root)?;
        let inaccessible = run(&root, &changed.index, false);
        assert!(!inaccessible.errors.is_empty());
        assert_eq!(inaccessible.index.books.iter().filter(|b| b.available).count(), 2);
        std::fs::rename(&hidden_root, &root)?;
        std::fs::remove_file(&moved_path)?;
        let missing = run(&root, &changed.index, false);
        assert!(!missing.index.books.iter().find(|b| b.path == moved_path).unwrap().available);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&root, root.join("nested/loop"))?;
            assert_eq!(run(&root, &missing.index, false).metadata_reads, 0);
        }
        assert!(scan(&root, &missing.index, false, &|| true, |_| {}).is_none());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn large_offline_index_retains_identities_and_cancels_before_publishing() {
        use std::cell::Cell;
        let root = std::env::temp_dir().join(format!("verso-offline-search-{}", std::process::id()));
        let previous = LibraryIndex {
            books: (0..10_000)
                .map(|i| {
                    let mut b = crate::library::selection_tests::book(i);
                    b.path = root.join(format!("Group{}/Book{i}.epub", i % 20));
                    b
                })
                .collect(),
            ..Default::default()
        };
        let scanned = scan_roots(std::slice::from_ref(&root), &previous, false, &|| false, |_| {}).unwrap();
        assert_eq!(scanned.metadata_reads, 0);
        assert_eq!(scanned.index.books.len(), previous.books.len());
        for (new, old) in scanned.index.books.iter().zip(&previous.books) {
            assert!(new.available);
            assert_eq!(new.id, old.id);
        }
        let checks = Cell::new(0);
        let published = Cell::new(false);
        let cancelled = || {
            checks.set(checks.get() + 1);
            checks.get() > 20
        };
        assert!(scan_roots(&[root], &previous, false, &cancelled, |_| published.set(true)).is_none());
        assert!(!published.get(), "a cancelled retained-index rebuild must not publish a checkpoint");
    }

    #[test]
    fn checkpoints_can_resume_and_worker_stops_without_consuming_results() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-scan-worker-{}", std::process::id()));
        std::fs::create_dir_all(&root)?;
        for i in 0..70 {
            fixture(&root.join(format!("{i}.epub")), "<package/>", &[])?;
        }
        let mut checkpoint = None;
        let full =
            scan(&root, &LibraryIndex::default(), false, &|| false, |index| checkpoint = Some(index.clone())).unwrap();
        assert_eq!(full.metadata_reads, 70);
        assert_eq!(run(&root, &checkpoint.unwrap(), false).metadata_reads, 6);
        let worker = Worker::new(Store::at(root.join("state")), vec![root.clone()], Waker::new(|| {}));
        worker.pause(true);
        for _ in 0..100 {
            worker.rescan();
        }
        drop(worker); // Does not block on an unconsumed result channel.
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
