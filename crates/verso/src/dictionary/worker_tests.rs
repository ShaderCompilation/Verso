use super::*;
use crate::dictionary::{basename, sibling};
use crate::store::atomic;
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
        let id = self.worker.lookup(query.into(), false);
        loop {
            let reply = self.reply(id).0;
            if reply.complete {
                return reply;
            }
        }
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
    install_entries(ifo, name, &[(word, &format!("{name} text"))], &[]);
}
fn install_entries(ifo: &Path, name: &str, entries: &[(&str, &str)], aliases: &[(&str, u32)]) {
    fs::create_dir_all(ifo.parent().unwrap()).unwrap();
    let mut idx = Vec::new();
    let mut data = Vec::new();
    for (word, text) in entries {
        idx.extend_from_slice(word.as_bytes());
        idx.push(0);
        idx.extend_from_slice(&(data.len() as u32).to_be_bytes());
        idx.extend_from_slice(&(text.len() as u32).to_be_bytes());
        data.extend_from_slice(text.as_bytes());
    }
    let base = basename(ifo).unwrap();
    fs::write(sibling(&base, "idx"), &idx).unwrap();
    fs::write(sibling(&base, "dict"), data).unwrap();
    if !aliases.is_empty() {
        let mut syn = Vec::new();
        for (word, ordinal) in aliases {
            syn.extend_from_slice(word.as_bytes());
            syn.push(0);
            syn.extend_from_slice(&ordinal.to_be_bytes());
        }
        fs::write(sibling(&base, "syn"), syn).unwrap();
    }
    fs::write(ifo, format!(
        "StarDict's dict ifo file\nversion=3.0.0\nbookname={name}\nwordcount={}\nsynwordcount={}\nidxfilesize={}\nsametypesequence=m\n",
        entries.len(), aliases.len(), idx.len())).unwrap();
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
    assert!(
        matches!(&reply.state, Some(State::Definition { entries: t, .. }) if t.iter().any(|e| e.blocks.iter().any(|b| b.text.contains("Native text"))))
    );
    assert!(reply.dictionaries.iter().any(|d| d.name == "broken" && d.error.is_some()), "broken stays listed");
    assert!(reply.warnings[0].contains("gone") && reply.warnings[0].contains("not available"));
    if !readable {
        assert_eq!(reply.warnings.len(), 2);
        assert!(reply.warnings[1].contains("locked"), "{:?}", reply.warnings);
    }

    // An invalid configuration is a warning; the native folder still works.
    fs::write(f.join("dictionary-folders.json"), b"{broken").unwrap();
    let reply = f.reply(f.worker.lookup("word".into(), true)).0;
    assert!(matches!(reply.state, Some(State::Definition { .. })));
    assert_eq!(reply.sources.len(), 1);
    assert!(reply.warnings[0].contains("only Verso's folder"), "{:?}", reply.warnings);

    // Only broken sources: Missing, with the reasons alongside.
    fs::remove_dir_all(f.store.dictionary_dir()).unwrap();
    f.store.save_dictionary_folders(&[f.join("gone")]).unwrap();
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
    assert!(
        matches!(f.lookup("word").state, Some(State::Definition { entries: t, .. }) if t.iter().any(|e| e.blocks.iter().any(|b| b.text.contains("External"))))
    );
    // A lookup in flight when the folder is removed must not be published
    // after the new catalog, nor answer from the removed source.
    let stale = f.worker.lookup("word".into(), true);
    f.store.save_dictionary_folders(&[]).unwrap();
    let id = f.worker.lookup("word".into(), false);
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
    edit(&f.worker, Edit::MoveDictionary { set: DEFAULT.into(), dictionary: b.clone(), position: 0 }); // B, A, Native
    edit(&f.worker, Edit::Disabled { set: DEFAULT.into(), dictionary: a.clone(), disabled: true }); // A disabled
    assert_eq!(names(&f.catalog()), ["B", "A", "Native"]);

    f.store.save_dictionary_folders(&[]).unwrap();
    assert_eq!(names(&f.catalog()), ["Native"]);
    // A change while the folder is gone keeps the removed dictionaries' preferences.
    edit(&f.worker, Edit::Disabled { set: DEFAULT.into(), dictionary: "n.ifo".into(), disabled: true });
    f.worker.flush().unwrap();

    // Re-adding restores flags and relative order, also after a restart.
    f.store.save_dictionary_folders(&[f.join("ext")]).unwrap();
    let mut f = f;
    (f.worker, f.wake) = Fixture::worker(&f.store);
    let reply = f.catalog();
    assert_eq!(names(&reply), ["B", "A", "Native"]);
    let flags: Vec<_> = reply.dictionaries.iter().map(|d| d.disabled).collect();
    assert_eq!(flags, [false, true, true]);
}

#[test]
fn preferred_base_form_beats_lower_exact_and_selection_and_disabled_are_respected() {
    let f = Fixture::new();
    install(&f.join("dictionaries/a.ifo"), "A", "box");
    install(&f.join("dictionaries/b.ifo"), "B", "boxes");
    let reply = f.lookup("boxes");
    assert_eq!(reply.selected.as_deref(), Some("a.ifo"));
    assert!(matches!(reply.state, Some(State::Definition { base_forms: Some(_), .. })));
    f.worker.open("b.ifo");
    let reply = f.reply(reply.id).0;
    assert_eq!(reply.selected.as_deref(), Some("b.ifo"));
    assert!(matches!(reply.state, Some(State::Definition { base_forms: None, .. })));
    edit(&f.worker, Edit::MoveDictionary { set: DEFAULT.into(), dictionary: "b.ifo".into(), position: 0 });
    assert_eq!(f.lookup("boxes").selected.as_deref(), Some("b.ifo"), "preference changes apply immediately");
    edit(&f.worker, Edit::Disabled { set: DEFAULT.into(), dictionary: "b.ifo".into(), disabled: true });
    let reply = f.lookup("boxes");
    assert_eq!(reply.selected.as_deref(), Some("a.ifo"), "disabled dictionaries are not searched");
    assert_eq!(reply.matches.len(), 1);
    edit(&f.worker, Edit::Disabled { set: DEFAULT.into(), dictionary: "a.ifo".into(), disabled: true });
    assert_eq!(f.lookup("boxes").state, Some(State::Disabled));
}

#[test]
fn exact_homographs_and_aliases_stay_together_without_base_forms() {
    let f = Fixture::new();
    install_entries(
        &f.join("dictionaries/a.ifo"),
        "A",
        &[("boxes", "First exact"), ("boxes", "Second exact"), ("box", "Base form")],
        &[("boxes", 0), ("containers", 1)],
    );
    install(&f.join("dictionaries/b.ifo"), "B", "boxes");
    let reply = f.lookup("boxes");
    assert_eq!(reply.selected.as_deref(), Some("a.ifo"));
    let Some(State::Definition { entries, base_forms }) = reply.state else { panic!("expected exact entries") };
    assert_eq!(base_forms, None);
    assert_eq!(entries.iter().map(|e| e.headword.as_str()).collect::<Vec<_>>(), ["boxes", "boxes"]);
    assert_eq!(entries[0].blocks[0].text, "First exact");
    assert_eq!(entries[1].blocks[0].text, "Second exact");
    let reply = f.lookup("containers");
    let Some(State::Definition { entries, base_forms }) = reply.state else { panic!("expected alias") };
    assert_eq!(base_forms, None);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].headword, "boxes");
    assert_eq!(entries[0].blocks[0].text, "Second exact");
}

#[test]
fn worker_forms_exceptions_possessives_function_words_and_non_english_exact() {
    let f = Fixture::new();
    install_entries(
        &f.join("dictionaries/a.ifo"),
        "A",
        &[
            ("box", "container"),
            ("study", "learn"),
            ("run", "move"),
            ("hope", "expect"),
            ("tree", "plant"),
            ("lie", "recline"),
            ("lye", "decoy"),
            ("good", "fine"),
            ("well", "fine"),
            ("be", "decoy"),
            ("ax", "tool"),
            ("axe", "tool"),
            ("axis", "line"),
            ("hi", "decoy for his"),
            ("again", "decoy for against"),
            ("Слово", "word"),
        ],
        &[],
    );
    for (query, expected) in [
        ("boxes", vec!["box"]),
        ("studies", vec!["study"]),
        ("running", vec!["run"]),
        ("hoping", vec!["hope"]),
        ("tree's", vec!["tree"]),
        ("trees’", vec!["tree"]),
        ("lying", vec!["lie"]),
        ("best", vec!["good", "well"]),
        ("axes", vec!["ax", "axe", "axis"]),
    ] {
        let reply = f.lookup(query);
        let Some(State::Definition { entries, base_forms }) = reply.state else { panic!("no forms for {query}") };
        assert_eq!(entries.iter().map(|e| e.headword.as_str()).collect::<Vec<_>>(), expected, "{query}");
        assert_eq!(base_forms, Some(BaseForms { query: query.into(), count: expected.len() }));
    }
    for query in ["zzqxyings", "his", "against", "this", "two words"] {
        assert_eq!(f.lookup(query).state, Some(State::NoResult), "{query}");
    }
    let reply = f.lookup("СЛОВО");
    let Some(State::Definition { entries, base_forms }) = reply.state else { panic!("no non-English exact") };
    assert_eq!(base_forms, None);
    assert_eq!(entries[0].headword, "Слово");
}

#[test]
fn forms_search_dictionary_order_and_passages_get_no_base_forms() {
    let f = Fixture::new();
    install(&f.join("dictionaries/a.ifo"), "A", "thunder");
    install(&f.join("dictionaries/b.ifo"), "B", "thunder");
    let reply = f.lookup("THUNDERS");
    assert_eq!(reply.selected.as_deref(), Some("a.ifo"));
    assert!(
        matches!(reply.state, Some(State::Definition { base_forms: Some(ref forms), .. }) if forms.query == "THUNDERS")
    );
    for query in ["two thunders", "thunders\n", "привет", "café"] {
        assert_eq!(f.lookup(query).state, Some(State::NoResult), "{query:?}");
    }
}

#[test]
fn oversized_queries_are_misses_without_dictionary_errors() {
    let f = Fixture::new();
    install(&f.join("dictionaries/a.ifo"), "A", "thunder");
    for query in ["a".repeat(4097), "İ".repeat(1366)] {
        let reply = f.lookup(&query);
        assert!(reply.matches.is_empty());
        assert_eq!(reply.selected, None);
        assert_eq!(reply.state, Some(State::NoResult));
        assert!(reply.dictionaries.iter().all(|d| d.error.is_none()));
    }
}

#[test]
fn misses_and_failures_are_skipped_and_diagnostics_remain_separate() {
    for query in ["absent", "thunders", "two words"] {
        let f = Fixture::new();
        install(&f.join("dictionaries/a.ifo"), "A", "other");
        install(&f.join("dictionaries/b.ifo"), "B", "other");
        fs::write(f.join("dictionaries/a.idx"), b"broken").unwrap();
        let reply = f.lookup(query);
        assert_eq!(reply.selected, None);
        assert_eq!(reply.state, Some(State::NoResult));
        assert!(reply.matches.is_empty());
        assert!(reply.dictionaries[0].error.is_some());
        fs::write(f.join("dictionaries/b.idx"), b"broken").unwrap();
        let reply = f.reply(f.worker.lookup(query.into(), true)).0;
        assert_eq!(reply.state, Some(State::NoResult));
        assert!(reply.dictionaries.iter().all(|d| d.error.is_some()));
    }
    let f = Fixture::new();
    install(&f.join("dictionaries/a.ifo"), "A", "thunder");
    install(&f.join("dictionaries/b.ifo"), "B", "thunder");
    fs::write(f.join("dictionaries/a.idx"), b"broken").unwrap();
    let reply = f.lookup("thunders");
    assert_eq!(reply.selected.as_deref(), Some("b.ifo"));
    assert_eq!(reply.matches.len(), 1);
    assert!(matches!(reply.state, Some(State::Definition { base_forms: Some(_), .. })));
}

#[test]
fn lookup_cancelled_before_it_starts_publishes_nothing() {
    let f = Fixture::new();
    install(&f.join("dictionaries/a.ifo"), "A", "thunder");
    // Gate the thread at its catalog wake so cancellation is deterministic,
    // rather than racing a very short fixture lookup against the test thread.
    let (woke, wake) = mpsc::channel();
    let (release, blocked) = mpsc::channel();
    let blocked = Mutex::new(blocked);
    let worker = Worker::new(
        f.store.clone(),
        Waker::new(move || {
            woke.send(()).unwrap();
            blocked.lock().unwrap().recv().unwrap();
        }),
    );
    worker.catalog(false);
    wake.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(worker.receive().is_some());
    worker.lookup("thunders".into(), false);
    worker.cancel();
    release.send(()).unwrap();
    worker.flush().unwrap();
    assert!(worker.receive().is_none());
    assert!(wake.try_recv().is_err(), "a cancelled lookup never wakes the UI");
}

#[test]
fn unreadable_definition_keeps_its_index_hit_and_management_diagnostic() {
    let f = Fixture::new();
    install(&f.join("dictionaries/a.ifo"), "A", "thunder");
    fs::write(f.join("dictionaries/a.dict"), [0xff; 6]).unwrap();
    let reply = f.lookup("thunders");
    // Not "no definition" next to "1 found": the match opens on its error page.
    assert_eq!(reply.selected.as_deref(), Some("a.ifo"));
    assert!(matches!(reply.state, Some(State::Error(_))));
    assert_eq!(reply.matches.len(), 1, "counts do not require readable definitions");
    assert!(reply.dictionaries[0].error.is_some());
    install(&f.join("dictionaries/b.ifo"), "B", "thunder");
    let reply = f.reply(f.worker.lookup("thunders".into(), true)).0;
    assert_eq!(reply.selected.as_deref(), Some("b.ifo"));
    assert_eq!(reply.matches.len(), 2);
    assert!(matches!(reply.state, Some(State::Definition { .. })));
}

#[test]
fn single_dictionary_reuses_its_backend_for_exact_and_forms() {
    let f = Fixture::new();
    let path = f.join("dictionaries/a.ifo");
    install(&path, "A", "thunder");
    let mut inner = inner(&f);
    assert_eq!(inner.lookup("other", &|| true).1, State::NoResult);
    // The cached backend has everything needed. Reopening during either lookup
    // would fail because its metadata is no longer on disk.
    fs::remove_file(path).unwrap();
    let (selected, state) = inner.lookup("thunders", &|| true);
    assert_eq!(selected.as_deref(), Some("a.ifo"));
    assert!(matches!(state, State::Definition { base_forms: Some(_), .. }));
}

#[test]
fn cancellation_at_any_point_marks_no_errors() {
    use std::cell::Cell;
    let f = Fixture::new();
    install(&f.join("dictionaries/a.ifo"), "A", "other");
    install(&f.join("dictionaries/b.ifo"), "B", "thunder");
    let mut inner = inner(&f);
    // Cancel at the nth check, through index building, exact and forms, the
    // dictionary switches and every base-form candidate, until one completes.
    // A first definition may already have been published before a later
    // cancellation. The client clears that mailbox; cancellation must never
    // turn an interrupted read into a dictionary error.
    for n in 0.. {
        assert!(n < 1000, "the lookup never completed");
        let checks = Cell::new(0);
        let current = || {
            checks.set(checks.get() + 1);
            checks.get() <= n
        };
        let (_, state) = inner.lookup("thunders", &current);
        if checks.get() <= n {
            assert!(matches!(state, State::Definition { base_forms: Some(_), .. }), "{state:?}");
            break;
        }
        if let State::Definition { base_forms, .. } = state {
            assert!(base_forms.is_some(), "cancelled at check {n}");
        }
        assert!(inner.catalog.dictionaries.iter().all(|d| d.error.is_none()), "cancelled at check {n}");
    }
}

fn inner(f: &Fixture) -> Inner {
    let mut inner = Inner {
        store: f.store.clone(),
        prefs_path: f.store.dictionary_preferences(),
        preferences: Preferences::default(),
        folders_seen: 0,
        active_set: DEFAULT.into(),
        catalog: Catalog::default(),
        warnings: Vec::new(),
        fresh: false,
        backend: None,
        save_error: None,
        failed_queries: Default::default(),
        prefs_error: None,
    };
    assert!(inner.discover(&|| true));
    inner
}

fn wait_client(worker: &Worker, id: u64) -> Reply {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(reply) = worker.receive() {
            assert_eq!(reply.id, id);
            return reply;
        }
        assert!(Instant::now() < deadline, "no client reply to {id}");
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn edit(worker: &Worker, edit: Edit) -> Reply {
    let reply = wait_client(worker, worker.edit(edit));
    assert!(reply.change_error.is_none(), "{:?}", reply.change_error);
    assert!(reply.save_error.is_none(), "{:?}", reply.save_error);
    reply
}
fn set(worker: &Worker, id: &str, language: &str, dictionary: &str) {
    edit(worker, Edit::Create { id: id.into(), name: id.into() });
    edit(worker, Edit::Languages { set: id.into(), languages: vec![language.into()] });
    edit(worker, Edit::Disabled { set: id.into(), dictionary: dictionary.into(), disabled: false });
}
#[test]
fn shared_service_is_lazy_and_routes_overlapping_requests_and_cancellation_per_client() {
    let f = Fixture::new();
    let (settings, _) = Fixture::worker(&f.store);
    assert!(Arc::ptr_eq(&f.worker.service, &settings.service));
    assert!(!f.store.dictionary_preferences().exists(), "client construction does not discover dictionaries");
    install(&f.join("dictionaries/a.ifo"), "A", "word");
    let lookup = f.worker.lookup("word".into(), false);
    let catalog = settings.catalog(false);
    let reply = wait_client(&settings, catalog);
    assert!(reply.state.is_none());
    let reply = wait_client(&f.worker, lookup);
    assert!(matches!(reply.state, Some(State::Definition { .. })));
    assert!(settings.receive().is_none());
    let lookup = f.worker.lookup("word".into(), false);
    settings.catalog(false);
    settings.flush().unwrap(); // Does not cancel another client's lookup.
    assert!(matches!(wait_client(&f.worker, lookup).state, Some(State::Definition { .. })));
    let catalog = settings.catalog(false);
    f.worker.cancel();
    assert!(wait_client(&settings, catalog).state.is_none());
    // Preference changes cancel obsolete lookups; management keeps its own mailbox.
    let lookup = f.worker.lookup("word".into(), false);
    let created = settings.edit(Edit::Create { id: "set".into(), name: "Set".into() });
    f.store.save_dictionary_folders(&[]).unwrap();
    let reply = wait_client(&settings, created);
    // An already completed reply may be waiting; there is no later publication.
    if let Some(old) = f.worker.receive() {
        assert_eq!(old.id, lookup);
    }
    let lookup = f.worker.lookup("word".into(), false);
    assert!(matches!(wait_client(&f.worker, lookup).state, Some(State::Definition { .. })));
    assert_eq!(reply.active_set, "set", "an edit replies with the edited set");
    assert!(reply.dictionaries.iter().all(|d| d.disabled));
}
#[test]
fn migration_new_dictionary_placement_and_missing_positions_survive_restart() {
    let f = Fixture::new();
    install(&f.join("dictionaries/a.ifo"), "A", "word");
    let legacy = vec![
        super::super::Preference { id: "gone.ifo".into(), disabled: true },
        super::super::Preference { id: "a.ifo".into(), disabled: true },
    ];
    atomic(&f.store.dictionary_preferences(), &legacy).unwrap();
    let reply = f.catalog();
    assert_eq!(reply.sets[0].dictionaries, legacy);
    set(&f.worker, "english", "en", "a.ifo");
    install(&f.join("dictionaries/b.ifo"), "B", "word");
    let reply = f.reply(f.worker.catalog(true)).0;
    let saved: Preferences = serde_json::from_slice(&fs::read(f.store.dictionary_preferences()).unwrap()).unwrap();
    assert_eq!(
        saved.set(DEFAULT).dictionaries.iter().map(|d| (&*d.id, d.disabled)).collect::<Vec<_>>(),
        [("gone.ifo", true), ("a.ifo", true), ("b.ifo", false)]
    );
    assert_eq!(
        saved.set("english").dictionaries.iter().map(|d| (&*d.id, d.disabled)).collect::<Vec<_>>(),
        [("gone.ifo", true), ("a.ifo", false), ("b.ifo", true)]
    );
    assert_eq!(reply.sets, saved.sets);
    // A new Store models a process restart, not just another client.
    let restarted = Worker::new(Store::at(f.root.clone()), Waker::new(|| {}));
    let reply = wait_client(&restarted, restarted.catalog(false));
    assert_eq!(reply.sets, saved.sets);
    install(&f.join("dictionaries/gone.ifo"), "Gone", "word");
    let reply = wait_client(&restarted, restarted.catalog(true));
    assert_eq!(
        reply.dictionaries.iter().map(|d| (&*d.id, d.disabled)).collect::<Vec<_>>(),
        [("gone.ifo", true), ("a.ifo", true), ("b.ifo", false)]
    );
}
#[test]
fn set_language_gates_forms_and_script_fallback_uses_the_resulting_sets_languages() {
    let f = Fixture::new();
    install_entries(
        &f.join("dictionaries/a.ifo"),
        "A",
        &[("box", "base"), ("boxes", "exact"), ("thunder", "form"), ("слово", "word")],
        &[],
    );
    f.catalog();
    set(&f.worker, "english", "en", "a.ifo");
    set(&f.worker, "french", "fr", "a.ifo");
    set(&f.worker, "russian", "ru", "a.ifo");
    for (language, query, expected_set, definition, base_form) in [
        ("", "thunders", DEFAULT, true, true),
        ("eng-US", "thunders", "english", true, true),
        ("fr", "thunders", "french", false, false),
        ("fr", "boxes", "french", true, false),
        ("en", "слово", "russian", true, false),
        ("ru", "thunders", "english", true, true),
    ] {
        let selection = Selection { language: language.into(), ..Default::default() };
        let reply = f.reply(f.worker.lookup_in(query.into(), false, selection)).0;
        assert_eq!(reply.active_set, expected_set);
        assert_eq!(matches!(&reply.state, Some(State::Definition { .. })), definition, "{language} {query}");
        assert_eq!(matches!(&reply.state, Some(State::Definition { base_forms: Some(_), .. })), base_form);
    }
}
#[test]
fn settings_edits_apply_to_retained_reader_client_and_are_durable_before_reply() {
    let f = Fixture::new();
    install(&f.join("dictionaries/a.ifo"), "A", "word");
    install(&f.join("dictionaries/b.ifo"), "B", "word");
    assert_eq!(f.lookup("word").selected.as_deref(), Some("a.ifo"));
    let (settings, _) = Fixture::worker(&f.store);
    set(&settings, "english", "en-US", "a.ifo");
    edit(&settings, Edit::Disabled { set: "english".into(), dictionary: "b.ifo".into(), disabled: false });
    edit(&settings, Edit::MoveDictionary { set: "english".into(), dictionary: "b.ifo".into(), position: 0 });
    edit(&settings, Edit::Rename { set: "english".into(), name: "English books".into() });
    let reply = edit(&settings, Edit::MoveSet { set: "english".into(), position: 0 });
    assert_eq!(reply.sets[0].name, "English books");
    let persisted: Preferences = serde_json::from_slice(&fs::read(f.store.dictionary_preferences()).unwrap()).unwrap();
    assert_eq!(persisted.sets, reply.sets);
    let selection = Selection { book_set: Some("english".into()), language: "ru".into() };
    let lookup = || f.reply(f.worker.lookup_in("word".into(), false, selection.clone())).0;
    assert_eq!(lookup().selected.as_deref(), Some("b.ifo"));
    edit(&settings, Edit::Disabled { set: "english".into(), dictionary: "b.ifo".into(), disabled: true });
    assert_eq!(lookup().selected.as_deref(), Some("a.ifo"));
    // Editing a set never changes Default.
    edit(&settings, Edit::Disabled { set: "english".into(), dictionary: "a.ifo".into(), disabled: true });
    assert_eq!(lookup().state, Some(State::Disabled));
    assert_eq!(f.lookup("word").selected.as_deref(), Some("a.ifo"));
    edit(&settings, Edit::Delete { set: "english".into() });
    assert_eq!(lookup().active_set, DEFAULT);
    let reply = wait_client(&settings, settings.edit(Edit::Delete { set: DEFAULT.into() }));
    assert!(reply.change_error.is_some());
    assert_eq!(reply.sets.len(), 1);
}
#[test]
fn unreadable_preferences_use_empty_default_and_failed_saves_retry_at_barrier() {
    let f = Fixture::new();
    let path = f.store.dictionary_preferences();
    fs::write(&path, b"{broken").unwrap();
    install(&f.join("dictionaries/a.ifo"), "A", "word");
    install(&f.join("dictionaries/b.ifo"), "B", "word");
    let reply = f.lookup("word");
    assert_eq!(reply.active_set, DEFAULT);
    assert!(!reply.dictionaries[0].disabled);
    assert!(reply.warnings.iter().any(|w| w.contains("preferences couldn't be read")), "{:?}", reply.warnings);
    // Discovery never replaces a damaged file; a repaired one is picked up.
    assert_eq!(fs::read(&path).unwrap(), b"{broken");
    let repaired = vec![super::super::Preference { id: "b.ifo".into(), disabled: false }];
    atomic(&path, &repaired).unwrap();
    let reply = f.reply(f.worker.lookup("word".into(), true)).0;
    assert_eq!(reply.selected.as_deref(), Some("b.ifo"));
    assert!(reply.warnings.is_empty(), "{:?}", reply.warnings);
    let saved: Preferences = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved.set(DEFAULT).dictionaries.len(), 2, "migrated and new dictionary saved");
    // A user's change replaces a damaged file and clears the warning.
    fs::write(&path, b"[{\"id\":7}]").unwrap();
    f.reply(f.worker.catalog(true));
    assert_eq!(fs::read(&path).unwrap(), b"[{\"id\":7}]");
    edit(&f.worker, Edit::Disabled { set: DEFAULT.into(), dictionary: "a.ifo".into(), disabled: true });
    let reply = f.lookup("word");
    assert!(reply.warnings.is_empty(), "{:?}", reply.warnings);
    let saved: Preferences = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert!(saved.set(DEFAULT).dictionaries.iter().any(|d| d.id == "a.ifo" && d.disabled));
    fs::remove_file(f.store.dictionary_preferences()).unwrap();
    fs::create_dir(f.store.dictionary_preferences()).unwrap();
    let reply = wait_client(&f.worker, f.worker.edit(Edit::Create { id: "set".into(), name: "Set".into() }));
    assert!(reply.save_error.is_some());
    assert!(f.worker.flush().is_err());
    fs::remove_dir(f.store.dictionary_preferences()).unwrap();
    f.worker.flush().unwrap();
    let saved: Preferences = serde_json::from_slice(&fs::read(f.store.dictionary_preferences()).unwrap()).unwrap();
    assert_eq!(saved.set("set").name, "Set");
}

#[test]
fn oversized_set_edits_preserve_saved_sets_and_survive_restart() {
    let f = Fixture::new();
    let mut preferences = Preferences::default();
    // A large collection's retained IDs are copied into each custom set.
    preferences.sets[0].dictionaries = (0..1024)
        .map(|i| super::super::Preference { id: format!("{i:04}{}", "d".repeat(96)), disabled: false })
        .collect();
    for i in 0.. {
        let mut candidate = preferences.clone();
        candidate.edit(Edit::Create { id: format!("set-{i}"), name: format!("Set {i}") }).unwrap();
        if preference_bytes(&candidate).is_err() {
            break;
        }
        preferences = candidate;
    }
    assert!(preferences.sets.len() > 1);
    let path = f.store.dictionary_preferences();
    atomic(&path, &preferences).unwrap();
    let saved = fs::read(&path).unwrap();
    assert_eq!(f.catalog().sets, preferences.sets);
    let reply = wait_client(&f.worker, f.worker.edit(Edit::Create { id: "overflow".into(), name: "Extra".into() }));
    assert!(reply.change_error.as_ref().is_some_and(|e| e.contains("1 MiB")));
    assert!(reply.save_error.is_none());
    assert_eq!(reply.sets, preferences.sets, "rejected candidate never becomes active");
    f.worker.flush().unwrap();
    assert_eq!(fs::read(&path).unwrap(), saved);

    let restarted = Worker::new(Store::at(f.root.clone()), Waker::new(|| {}));
    let reply = wait_client(&restarted, restarted.catalog(false));
    assert!(reply.warnings.is_empty());
    assert_eq!(reply.sets, preferences.sets);
    // A later valid edit retains every custom set instead of replacing them.
    let reply = edit(
        &restarted,
        Edit::Disabled {
            set: DEFAULT.into(),
            dictionary: preferences.sets[0].dictionaries[0].id.clone(),
            disabled: true,
        },
    );
    assert_eq!(reply.sets.len(), preferences.sets.len());
    assert_eq!(read_preferences(&path).unwrap().0.sets, reply.sets);
}

#[test]
fn discovery_respects_the_preference_read_limit_without_partial_updates() {
    let f = Fixture::new();
    let mut preferences = Preferences::default();
    preferences.sets[0].name.clear();
    let overhead = preference_bytes(&preferences).unwrap().len();
    preferences.sets[0].name = "x".repeat(PREFERENCES_LIMIT - overhead);
    assert_eq!(preference_bytes(&preferences).unwrap().len(), PREFERENCES_LIMIT);
    let path = f.store.dictionary_preferences();
    atomic(&path, &preferences).unwrap();
    assert_eq!(read_preferences(&path).unwrap().0, preferences, "exactly the limit is readable");
    assert_eq!(f.catalog().sets, preferences.sets);
    let saved = fs::read(&path).unwrap();

    install(&f.join("dictionaries/a.ifo"), "A", "word");
    install(&f.join("dictionaries/b.ifo"), "B", "word");
    let reply = f.reply(f.worker.catalog(true)).0;
    assert!(reply.warnings.iter().any(|w| w.contains("1 MiB")));
    assert_eq!(reply.sets, preferences.sets);
    assert_eq!(reply.dictionaries.len(), 2, "the catalog remains available");
    f.worker.flush().unwrap();
    assert_eq!(fs::read(&path).unwrap(), saved);
    assert_eq!(read_preferences(&path).unwrap().0, preferences);

    // Make space and rescan: both additions can now be accepted and reopened.
    edit(&f.worker, Edit::Rename { set: DEFAULT.into(), name: "Default".into() });
    let reply = f.reply(f.worker.catalog(true)).0;
    assert!(reply.warnings.is_empty());
    assert_eq!(reply.sets[0].dictionaries.len(), 2);
    assert!(reply.dictionaries.iter().all(|d| !d.disabled));
    assert_eq!(read_preferences(&path).unwrap().0.sets, reply.sets);
}

#[test]
fn retry_reopens_a_repaired_dictionary_and_clears_errors_even_when_disabled() {
    let f = Fixture::new();
    let ifo = f.store.dictionary_dir().join("repair.ifo");
    install(&ifo, "Repair", "Hello");
    let data = ifo.with_extension("dict");
    fs::write(&data, b"").unwrap();
    let reply = f.lookup("Hello");
    assert!(reply.dictionaries[0].error.is_some());
    let reply =
        edit(&f.worker, Edit::Disabled { set: DEFAULT.into(), dictionary: "repair.ifo".into(), disabled: true });
    assert!(reply.dictionaries[0].disabled);
    let failed = wait_client(&f.worker, f.worker.retry(DEFAULT, "repair.ifo"));
    assert!(failed.dictionaries[0].error.is_some(), "retry must still read the failed entry");
    let rescan = wait_client(&f.worker, f.worker.catalog(true));
    assert!(rescan.dictionaries[0].error.is_some(), "rescan must not hide an unrepaired read error");
    fs::write(&data, b"Repair text").unwrap();
    let repaired = wait_client(&f.worker, f.worker.retry(DEFAULT, "repair.ifo"));
    assert!(repaired.dictionaries[0].error.is_none());
    assert!(repaired.dictionaries[0].disabled, "retry does not change set membership");
}

#[test]
fn rescan_retries_read_errors_and_refreshes_repaired_metadata() {
    let f = Fixture::new();
    let ifo = f.store.dictionary_dir().join("repair.ifo");
    install(&ifo, "Repair", "Hello");
    fs::write(ifo.with_extension("idx"), b"broken").unwrap();
    assert!(f.lookup("Hello").dictionaries[0].error.is_some());
    install(&ifo, "Renamed full dictionary", "Hello");
    let reply = wait_client(&f.worker, f.worker.catalog(true));
    assert!(reply.dictionaries[0].error.is_none());
    assert_eq!(reply.dictionaries[0].name, "Renamed full dictionary");
}

#[test]
fn edits_and_retries_publish_a_fresh_catalog_after_folder_changes() {
    let f = Fixture::new();
    install(&f.join("dictionaries/n.ifo"), "Native", "word");
    install(&f.join("ext/a.ifo"), "A", "word");
    assert_eq!(names(&f.catalog()), ["Native"]);
    f.store.save_dictionary_folders(&[f.join("ext")]).unwrap();
    let reply = edit(&f.worker, Edit::Rename { set: DEFAULT.into(), name: "All".into() });
    assert_eq!(names(&reply), ["Native", "A"], "the edit reply includes the added folder");
    f.store.save_dictionary_folders(&[]).unwrap();
    let reply = wait_client(&f.worker, f.worker.retry(DEFAULT, "n.ifo"));
    assert_eq!(names(&reply), ["Native"], "the retry reply drops the removed folder");
}

#[test]
fn sessions_count_index_hits_without_reading_other_definitions() {
    let f = Fixture::new();
    install_entries(&f.join("dictionaries/a.ifo"), "A", &[("box", "first"), ("box", "second")], &[("box", 0)]);
    install(&f.join("dictionaries/b.ifo"), "B", "boxes");
    // A text read would fail; counting its index hit must succeed.
    fs::write(f.join("dictionaries/b.dict"), [0xff; 6]).unwrap();
    install(&f.join("dictionaries/c.ifo"), "C", "absent");
    install(&f.join("dictionaries/d.ifo"), "D", "boxes");
    let path = f.join("dictionaries/d.ifo");
    fs::write(&path, fs::read_to_string(&path).unwrap().replace("sametypesequence=m", "sametypesequence=W")).unwrap();
    let reply = f.lookup("boxes");
    assert!(reply.complete);
    assert_eq!(reply.matches.len(), 2);
    assert_eq!(reply.matches[0].hits.records.len(), 2, "alias is deduplicated");
    assert_eq!(reply.matches[0].hits.forms, Some(1));
    assert_eq!(reply.matches[0].headwords(), ["box"]);
    assert_eq!(reply.matches[1].hits.forms, None);
    assert!(reply.dictionaries[1].error.is_none(), "unopened definition has not failed");
    let session = reply.id;
    f.worker.open("b.ifo");
    let opened = f.reply(session).0;
    assert_eq!(opened.matches, reply.matches);
    assert!(matches!(opened.state, Some(State::Error(_))));
    assert!(opened.dictionaries[1].error.is_some());
    f.worker.open("a.ifo");
    let opened = f.reply(session).0;
    assert!(matches!(opened.state, Some(State::Definition { entries, .. }) if entries.len() == 2));
    let reply = f.lookup("absent");
    assert_eq!(reply.matches.len(), 1);
    assert_eq!(reply.matches[0].dictionary, "c.ifo");
}

/// Capture every publication synchronously, without racing mailbox replacement.
fn publications(f: &Fixture, delay: impl Fn(&Mailbox) -> bool) -> Vec<Reply> {
    use std::cell::Cell;
    let holder: Arc<Mutex<Option<std::sync::Weak<Mailbox>>>> = Arc::new(Mutex::new(None));
    let results = Arc::new(Mutex::new(Vec::new()));
    let captured = results.clone();
    let weak = holder.clone();
    let mailbox = Arc::new(Mailbox {
        result: Mutex::new(None),
        ready: Condvar::new(),
        session: Mutex::new(None),
        latest: AtomicU64::new(1),
        waker: Waker::new(move || {
            let mailbox = weak.lock().unwrap().as_ref().unwrap().upgrade().unwrap();
            captured.lock().unwrap().push(mailbox.result.lock().unwrap().as_ref().unwrap().clone());
        }),
    });
    *holder.lock().unwrap() = Some(Arc::downgrade(&mailbox));
    let request = Request { id: 1, mailbox: mailbox.clone(), revision: 0 };
    let delayed = Cell::new(false);
    let current = || {
        if !delayed.get() && delay(&mailbox) {
            delayed.set(true);
            std::thread::sleep(Duration::from_millis(170));
        }
        true
    };
    inner(f).lookup_session(&request, "boxes", &current);
    results.lock().unwrap().clone()
}

#[test]
fn fast_search_publishes_first_definition_then_final_count() {
    let f = Fixture::new();
    install(&f.join("dictionaries/a.ifo"), "A", "box");
    install(&f.join("dictionaries/b.ifo"), "B", "boxes");
    let fast = publications(&f, |_| false);
    assert_eq!(fast.len(), 2);
    assert!(!fast[0].complete && fast[0].matches.len() == 1);
    assert!(matches!(fast[0].state, Some(State::Definition { .. })));
    assert!(fast[1].complete && fast[1].matches.len() == 2);
    let delayed = publications(&f, |m| m.session.lock().unwrap().as_ref().is_some_and(|s| s.matches.len() == 1));
    assert_eq!(delayed.len(), 2);
    assert!(!delayed[0].complete);
    assert_eq!(delayed[0].matches.len(), 1);
    assert!(matches!(delayed[0].state, Some(State::Definition { .. })));
    assert!(delayed[1].complete);
    assert_eq!(delayed[1].matches.len(), 2);
    assert_eq!(delayed[0].state, delayed[1].state);
    assert_eq!(delayed[0].selected, delayed[1].selected);
}

#[test]
fn first_index_build_reports_preparation_and_cancellation_cleans_its_temporary_files() {
    let f = Fixture::new();
    install(&f.join("dictionaries/a.ifo"), "A", "boxes");
    let reply = f.catalog();
    assert_eq!(reply.unprepared, ["a.ifo"]);
    let cache = f.store.dictionary_cache();
    let preparing = publications(&f, |_| {
        fs::read_dir(&cache)
            .is_ok_and(|mut files| files.any(|e| e.unwrap().path().extension().is_some_and(|x| x == "tmp")))
    });
    assert_eq!(preparing.len(), 3);
    assert_eq!(preparing[0].preparing.as_deref(), Some("a.ifo"));
    assert!(!preparing[0].complete);
    assert!(!preparing[1].complete && matches!(preparing[1].state, Some(State::Definition { .. })));
    assert!(preparing[2].complete && preparing[2].preparing.is_none());
    assert!(preparing[2].unprepared.is_empty());
    assert!(fs::read_dir(cache).unwrap().all(|e| e.unwrap().path().extension().unwrap() == "index"));
}

#[test]
fn forgetting_a_missing_dictionary_removes_all_sets_but_never_a_present_dictionary() {
    let f = Fixture::new();
    install(&f.join("dictionaries/a.ifo"), "A", "word");
    f.catalog();
    set(&f.worker, "english", "en", "a.ifo");
    let reply = wait_client(&f.worker, f.worker.edit(Edit::Forget { set: DEFAULT.into(), dictionary: "a.ifo".into() }));
    assert!(reply.change_error.is_some());
    fs::remove_file(f.join("dictionaries/a.ifo")).unwrap();
    f.reply(f.worker.catalog(true));
    let reply = edit(&f.worker, Edit::Forget { set: DEFAULT.into(), dictionary: "a.ifo".into() });
    assert!(reply.sets.iter().all(|s| s.dictionaries.is_empty()));
    f.worker.flush().unwrap();
    let saved = read_preferences(&f.store.dictionary_preferences()).unwrap().0;
    assert!(saved.sets.iter().all(|s| s.dictionaries.is_empty()));
}

#[test]
fn delayed_search_republishes_when_its_first_definition_arrives() {
    let f = Fixture::new();
    install(&f.join("dictionaries/a.ifo"), "A", "absent");
    install(&f.join("dictionaries/b.ifo"), "B", "boxes");
    install(&f.join("dictionaries/c.ifo"), "C", "absent");
    f.lookup("boxes"); // Build the indexes, so preparation adds no publications.
    let published = publications(&f, |_| true);
    assert_eq!(published.len(), 3, "searching, first definition, final count");
    assert!(published[0].matches.is_empty() && published[0].selected.is_none());
    assert!(!published[1].complete);
    assert_eq!(published[1].selected.as_deref(), Some("b.ifo"));
    assert!(matches!(published[1].state, Some(State::Definition { .. })));
    assert!(published[2].complete && published[2].selected == published[1].selected);
}

#[test]
fn a_readable_search_clears_an_earlier_read_error() {
    let f = Fixture::new();
    let idx = f.join("dictionaries/a.idx");
    install(&f.join("dictionaries/a.ifo"), "A", "word");
    f.catalog();
    // A passing failure, for example while USB storage is away.
    fs::rename(&idx, f.join("away")).unwrap();
    assert!(f.lookup("word").dictionaries[0].error.is_some());
    fs::rename(f.join("away"), &idx).unwrap();
    let reply = f.lookup("other");
    assert_eq!(reply.state, Some(State::NoResult));
    assert!(reply.dictionaries[0].error.is_none(), "a successful miss clears the error");
}

#[test]
fn lookups_skip_catalog_only_checks() {
    let f = Fixture::new();
    install(&f.join("dictionaries/a.ifo"), "A", "word");
    let reply = f.lookup("word");
    assert!(reply.unprepared.is_empty() && reply.missing.is_empty());
    fs::remove_file(f.join("dictionaries/a.ifo")).unwrap();
    let reply = wait_client(&f.worker, f.worker.catalog(true));
    assert_eq!(reply.missing, ["a.ifo"]);
}

#[test]
fn forget_survives_its_screen_closing_during_discovery() {
    let f = Fixture::new();
    install(&f.join("dictionaries/a.ifo"), "A", "word");
    f.catalog();
    fs::remove_file(f.join("dictionaries/a.ifo")).unwrap();
    // Folder changes force discovery before the edit; the client then moves on.
    f.store.save_dictionary_folders(&[f.join("ext")]).unwrap();
    f.worker.edit(Edit::Forget { set: DEFAULT.into(), dictionary: "a.ifo".into() });
    f.worker.cancel();
    f.worker.flush().unwrap();
    let saved = read_preferences(&f.store.dictionary_preferences()).unwrap().0;
    assert!(saved.sets.iter().all(|s| s.dictionaries.is_empty()));
}

#[test]
fn preparation_starting_and_ending_after_the_first_publication_is_shown() {
    let f = Fixture::new();
    install(&f.join("dictionaries/a.ifo"), "A", "absent");
    install(&f.join("dictionaries/c.ifo"), "C", "absent");
    f.lookup("boxes");
    // Only b needs its first index build, after the first publication.
    install(&f.join("dictionaries/b.ifo"), "B", "absent");
    f.catalog();
    let published = publications(&f, |_| true);
    let preparing: Vec<_> = published.iter().map(|r| (r.preparing.as_deref(), r.complete)).collect();
    assert_eq!(preparing, [(None, false), (Some("b.ifo"), false), (None, false), (None, true)]);
}
