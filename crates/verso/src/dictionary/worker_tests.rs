use super::*;
use crate::dictionary::{basename, sibling};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::atomic::AtomicU64,
    time::{Duration, Instant},
};
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    store: Store,
    worker: Worker,
    wake: mpsc::Receiver<()>,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "verso-dictionary-worker-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        // Real path, so expected external IDs match where temp is a link.
        let root = fs::canonicalize(root).unwrap();
        let store = Store::at(root.clone());
        // Native-only unless a test configures folders (Kindle defaults off).
        store.save_dictionary_folders(&[]).unwrap();
        let (worker, wake) = Self::worker(&store);
        Self { root, store, worker, wake }
    }
    fn worker(store: &Store) -> (Worker, mpsc::Receiver<()>) {
        let (tx, wake) = mpsc::channel();
        let worker = Worker::new(
            store.clone(),
            Waker::new(move || {
                let _ = tx.send(());
            }),
        );
        (worker, wake)
    }
    fn join(&self, path: &str) -> PathBuf {
        self.root.join(path)
    }
    /// Waits for the reply to `id`, returning it and the IDs of any replies
    /// received before it.
    fn reply(&self, id: u64) -> (Reply, Vec<u64>) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut earlier = Vec::new();
        loop {
            if let Some(reply) = self.worker.receive() {
                if reply.id == id {
                    return (reply, earlier);
                }
                earlier.push(reply.id);
            }
            assert!(Instant::now() < deadline, "no reply to {id}");
            let _ = self.wake.recv_timeout(Duration::from_millis(20));
        }
    }
    fn lookup(&self, query: &str) -> Reply {
        self.reply(self.worker.lookup(query.into(), None, false)).0
    }
    fn catalog(&self) -> Reply {
        self.reply(self.worker.catalog(false)).0
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
/// A valid dictionary at `ifo` defining `word` as `name`'s text.
fn install(ifo: &Path, name: &str, word: &str) {
    fs::create_dir_all(ifo.parent().unwrap()).unwrap();
    let text = format!("{name} text");
    let mut idx = format!("{word}\0").into_bytes();
    idx.extend_from_slice(&0u32.to_be_bytes());
    idx.extend_from_slice(&(text.len() as u32).to_be_bytes());
    let base = basename(ifo).unwrap();
    fs::write(sibling(&base, "idx"), &idx).unwrap();
    fs::write(sibling(&base, "dict"), text).unwrap();
    fs::write(
        ifo,
        format!(
            "StarDict's dict ifo file\nversion=3.0.0\nbookname={name}\nwordcount=1\nidxfilesize={}\nsametypesequence=m\n",
            idx.len()
        ),
    )
    .unwrap();
}
fn names(reply: &Reply) -> Vec<&str> {
    reply.dictionaries.iter().map(|d| d.name.as_str()).collect()
}

#[test]
fn catalog_without_a_query_lists_dictionaries_and_sources() {
    let f = Fixture::new();
    install(&f.join("dictionaries/n.ifo"), "Native", "word");
    install(&f.join("koreader/en/e.ifo"), "External", "word");
    f.store.save_dictionary_folders(&[f.join("koreader")]).unwrap();
    let reply = f.catalog();
    assert_eq!(reply.state, None, "no lookup was made");
    assert_eq!(reply.selected, None);
    assert_eq!(names(&reply), ["External", "Native"]);
    assert_eq!(reply.dictionaries[0].id, f.join("koreader/en/e.ifo").to_string_lossy());
    assert_eq!((reply.dictionaries[0].source, reply.dictionaries[0].relative.as_path()), (1, Path::new("en/e.ifo")));
    let sources: Vec<_> = reply.sources.iter().map(|s| (s.root.clone(), s.native, s.status.clone())).collect();
    assert_eq!(
        sources,
        [
            (f.store.dictionary_dir(), true, SourceStatus::Available),
            (f.join("koreader"), false, SourceStatus::Available)
        ]
    );
    assert!(reply.warnings.is_empty());
    // Cached until invalidated or rescanned.
    install(&f.join("koreader/later.ifo"), "Later", "word");
    assert_eq!(f.catalog().dictionaries.len(), 2);
    assert_eq!(f.reply(f.worker.catalog(true)).0.dictionaries.len(), 3);
}

#[test]
fn source_problems_are_warnings_next_to_working_lookups() {
    let f = Fixture::new();
    install(&f.join("dictionaries/n.ifo"), "Native", "word");
    fs::write(f.join("dictionaries/broken.ifo"), b"broken").unwrap();
    install(&f.join("locked/l.ifo"), "Locked", "word");
    fs::set_permissions(f.join("locked"), fs::Permissions::from_mode(0o000)).unwrap();
    let readable = fs::read_dir(f.join("locked")).is_ok(); // running as root
    f.store.save_dictionary_folders(&[f.join("gone"), f.join("locked")]).unwrap();
    let reply = f.lookup("word");
    fs::set_permissions(f.join("locked"), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(&reply.state, Some(State::Definition(t)) if t.contains("Native text")));
    assert!(reply.dictionaries.iter().any(|d| d.name == "broken" && d.error.is_some()), "broken stays listed");
    assert!(reply.warnings[0].contains("gone") && reply.warnings[0].contains("not available"));
    if !readable {
        assert_eq!(reply.warnings.len(), 2);
        assert!(reply.warnings[1].contains("locked"), "{:?}", reply.warnings);
    }

    // An invalid configuration is a warning; the native folder still works.
    fs::write(f.join("dictionary-folders.json"), b"{broken").unwrap();
    f.worker.invalidate();
    let reply = f.lookup("word");
    assert!(matches!(reply.state, Some(State::Definition(_))));
    assert_eq!(reply.sources.len(), 1);
    assert!(reply.warnings[0].contains("only Verso's folder"), "{:?}", reply.warnings);

    // Only broken sources: Missing, with the reasons alongside.
    fs::remove_dir_all(f.store.dictionary_dir()).unwrap();
    f.store.save_dictionary_folders(&[f.join("gone")]).unwrap();
    f.worker.invalidate();
    let reply = f.lookup("word");
    assert_eq!(reply.state, Some(State::Missing));
    assert_eq!(reply.warnings.len(), 1);
}

#[test]
fn invalidation_cancels_in_flight_work_and_drops_removed_sources() {
    let f = Fixture::new();
    install(&f.join("dictionaries/n.ifo"), "Native", "native");
    install(&f.join("ext/e.ifo"), "External", "word");
    f.store.save_dictionary_folders(&[f.join("ext")]).unwrap();
    assert!(matches!(f.lookup("word").state, Some(State::Definition(t)) if t.contains("External")));
    // A lookup in flight when the folder is removed must not be published
    // after the new catalog, nor answer from the removed source.
    let stale = f.worker.lookup("word".into(), None, true);
    f.store.save_dictionary_folders(&[]).unwrap();
    f.worker.invalidate();
    let id = f.worker.lookup("word".into(), None, false);
    let (reply, earlier) = f.reply(id);
    assert!(earlier.iter().all(|&e| e == stale));
    assert_eq!(reply.state, Some(State::NoResult), "removed source no longer answers");
    assert_eq!(names(&reply), ["Native"]);
    assert_eq!(reply.sources.len(), 1);
    std::thread::sleep(Duration::from_millis(50));
    assert!(f.worker.receive().is_none(), "nothing published after the latest reply");
}

#[test]
fn preferences_survive_source_removal_and_re_adding() {
    let f = Fixture::new();
    install(&f.join("dictionaries/n.ifo"), "Native", "word");
    install(&f.join("ext/a.ifo"), "A", "word");
    install(&f.join("ext/b.ifo"), "B", "word");
    f.store.save_dictionary_folders(&[f.join("ext")]).unwrap();
    assert_eq!(names(&f.catalog()), ["A", "B", "Native"]);
    let b = f.join("ext/b.ifo").to_string_lossy().into_owned();
    let a = f.join("ext/a.ifo").to_string_lossy().into_owned();
    f.worker.change(b.clone(), true); // B, A, Native
    f.worker.change(a.clone(), false); // A disabled
    assert_eq!(names(&f.catalog()), ["B", "A", "Native"]);

    f.store.save_dictionary_folders(&[]).unwrap();
    f.worker.invalidate();
    assert_eq!(names(&f.catalog()), ["Native"]);
    // A change while the folder is gone keeps the removed dictionaries' preferences.
    f.worker.change("n.ifo".into(), false);
    f.worker.flush().unwrap();
    drop(f.worker.receive());

    // Re-adding restores flags and relative order, also after a restart.
    f.store.save_dictionary_folders(&[f.join("ext")]).unwrap();
    let mut f = f;
    (f.worker, f.wake) = Fixture::worker(&f.store);
    let reply = f.catalog();
    assert_eq!(names(&reply), ["Native", "B", "A"]);
    let flags: Vec<_> = reply.dictionaries.iter().map(|d| d.disabled).collect();
    assert_eq!(flags, [true, false, true]);
}
