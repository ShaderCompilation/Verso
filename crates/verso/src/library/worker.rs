//! One coalescing library worker. A single replaceable snapshot bounds pending results.
use super::{Book, Cover, LibraryIndex, Metadata, Stamp, assign_ids, epub};
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
    priority: Vec<String>,
    latest: Option<Snapshot>,
}
pub struct Worker {
    shared: Arc<(Mutex<Shared>, Condvar)>,
    join: Option<JoinHandle<()>>,
}
impl Worker {
    pub fn new(store: Store, root: PathBuf, waker: Waker) -> Self {
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
                    let result = scan(&root, &index, retry, &cancelled, |checkpoint| {
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
                    index = scan.index;
                    errors.extend(scan.errors);
                    if let Err(e) = store.save_library(&index) {
                        errors.push(format!("Couldn't save library: {e}"));
                    }
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

pub fn scan(
    root: &Path,
    previous: &LibraryIndex,
    retry: bool,
    cancelled: &impl Fn() -> bool,
    mut checkpoint: impl FnMut(&LibraryIndex),
) -> Option<Scan> {
    let started = Instant::now();
    let mut paths = Vec::new();
    let mut uncertain = Vec::new();
    let mut errors = Vec::new();
    enumerate(root, cancelled, &mut paths, &mut uncertain, &mut errors);
    paths.sort();
    let old: HashMap<_, _> = previous.books.iter().map(|b| (&b.path, b)).collect();
    let found: HashSet<_> = paths.iter().cloned().collect();
    let mut books = Vec::new();
    let mut dirty = HashSet::new();
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
        let fingerprint = if changed {
            fingerprint(&path, cancelled).unwrap_or_default()
        } else {
            cached.map_or(String::new(), |b| b.fingerprint.clone())
        };
        dirty.insert(path.clone());
        books.push(Book {
            metadata: Metadata {
                title: path.file_stem().unwrap_or_default().to_string_lossy().into_owned(),
                ..Metadata::default()
            },
            path,
            id: String::new(),
            legacy_id,
            fingerprint,
            stamp: file_stamp,
            error: None,
            cover: Cover::Pending,
            available: true,
            metadata_pending: true,
        });
    }
    if cancelled() {
        return None;
    }
    // Retain tombstones, and leave inaccessible subtrees visible.
    for cached in &previous.books {
        if !found.contains(&cached.path) || uncertain.iter().any(|p| cached.path.starts_with(p)) {
            let mut b = cached.clone();
            b.available = cached.path.starts_with(root) && uncertain.iter().any(|p| cached.path.starts_with(p));
            if !books.iter().any(|entry| entry.path == b.path) {
                books.push(b);
            }
        }
    }
    assign_ids(&mut books, &previous.books);
    // A moved entry supersedes its unavailable old path.
    let available_ids: HashSet<_> = books.iter().filter(|b| b.available).map(|b| b.id.clone()).collect();
    books.retain(|b| b.available || !available_ids.contains(&b.id));
    let mut index = LibraryIndex { books, ..LibraryIndex::default() };
    let mut reads = 0;
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
mod tests {
    use super::*;
    use crate::library::epub::tests::fixture;
    fn run(root: &Path, old: &LibraryIndex, retry: bool) -> Scan {
        scan(root, old, retry, &|| false, |_| {}).unwrap()
    }
    #[test]
    fn incremental_discovery_failures_moves_and_missing_subtrees() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-scan-{}", std::process::id()));
        std::fs::create_dir_all(root.join("nested"))?;
        let a = root.join("nested/one.EPUB");
        fixture(&a, "<package/>", &[])?;
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
        fixture(&moved_path, "<package><metadata><title>Edited</title></metadata></package>", &[])?;
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
        let worker = Worker::new(Store::at(root.join("state")), root.clone(), Waker::new(|| {}));
        for _ in 0..100 {
            worker.rescan();
        }
        drop(worker); // Does not block on an unconsumed result channel.
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
