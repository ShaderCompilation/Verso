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
    let reply = f.reply(f.worker.lookup("word".into(), None, true)).0;
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
    let stale = f.worker.lookup("word".into(), None, true);
    f.store.save_dictionary_folders(&[]).unwrap();
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
    let reply = f.reply(f.worker.lookup("boxes".into(), Some("b.ifo".into()), false)).0;
    assert_eq!(reply.selected.as_deref(), Some("b.ifo"));
    assert!(matches!(reply.state, Some(State::Definition { base_forms: None, .. })));
    edit(&f.worker, Edit::MoveDictionary { set: DEFAULT.into(), dictionary: "b.ifo".into(), position: 0 });
    assert_eq!(f.lookup("boxes").selected.as_deref(), Some("b.ifo"), "preference changes apply immediately");
    edit(&f.worker, Edit::Disabled { set: DEFAULT.into(), dictionary: "b.ifo".into(), disabled: true });
    let reply = f.reply(f.worker.lookup("boxes".into(), Some("b.ifo".into()), false)).0;
    assert_eq!(reply.selected.as_deref(), Some("a.ifo"), "disabled selection falls back to enabled dictionaries");
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
    install(&f.join("dictionaries/b.ifo"), "B", "thunder");
    // Over 4 KiB as typed, and 2732 bytes that fold to 4098 (U+0130 grows).
    for query in ["a".repeat(4097), "İ".repeat(1366)] {
        let reply = f.lookup(&query);
        assert_eq!(reply.selected.as_deref(), Some("a.ifo"));
        assert_eq!(reply.state, Some(State::NoResult));
        assert!(reply.dictionaries.iter().all(|d| d.error.is_none()), "no index errors recorded");
        let reply = f.reply(f.worker.lookup(query, Some("b.ifo".into()), false)).0;
        assert_eq!(reply.selected.as_deref(), Some("b.ifo"));
        assert_eq!(reply.state, Some(State::NoResult));
    }
}

#[test]
fn error_and_no_result_fallback_remain_in_preference_order_in_one_pass() {
    for query in ["absent", "thunders", "two words"] {
        let f = Fixture::new();
        install(&f.join("dictionaries/a.ifo"), "A", "other");
        install(&f.join("dictionaries/b.ifo"), "B", "other");
        fs::write(f.join("dictionaries/a.idx"), b"broken").unwrap();
        let reply = f.lookup(query);
        assert_eq!(reply.selected.as_deref(), Some("b.ifo"));
        assert_eq!(reply.state, Some(State::NoResult), "{query}");
        assert!(reply.dictionaries[0].error.is_some(), "index errors remain visible");
        // An explicitly selected broken dictionary reports its error.
        let reply = f.reply(f.worker.lookup(query.into(), Some("a.ifo".into()), false)).0;
        assert!(matches!(reply.state, Some(State::Error(_))));
        // A no-result before an error keeps the same priority.
        edit(&f.worker, Edit::MoveDictionary { set: DEFAULT.into(), dictionary: "b.ifo".into(), position: 0 });
        let reply = f.lookup(query);
        assert_eq!(reply.selected.as_deref(), Some("b.ifo"));
        assert_eq!(reply.state, Some(State::NoResult));
        fs::write(f.join("dictionaries/b.idx"), b"broken").unwrap();
        let reply = f.reply(f.worker.lookup(query.into(), None, true)).0;
        assert_eq!(reply.selected.as_deref(), Some("b.ifo"), "first error wins if all fail");
        assert!(matches!(reply.state, Some(State::Error(_))));
    }
    // An index error does not prevent a later base-form hit.
    let f = Fixture::new();
    install(&f.join("dictionaries/a.ifo"), "A", "thunder");
    install(&f.join("dictionaries/b.ifo"), "B", "thunder");
    fs::write(f.join("dictionaries/a.idx"), b"broken").unwrap();
    let reply = f.lookup("thunders");
    assert_eq!(reply.selected.as_deref(), Some("b.ifo"));
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
    worker.lookup("thunders".into(), None, false);
    worker.cancel();
    release.send(()).unwrap();
    worker.flush().unwrap();
    assert!(worker.receive().is_none());
    assert!(wake.try_recv().is_err(), "a cancelled lookup never wakes the UI");
}

#[test]
fn base_form_read_error_is_reported_unless_another_dictionary_has_no_result() {
    let f = Fixture::new();
    install(&f.join("dictionaries/a.ifo"), "A", "thunder");
    // The exact query misses without reading this malformed definition.
    fs::write(f.join("dictionaries/a.dict"), [0xff; 6]).unwrap();
    let reply = f.lookup("thunders");
    assert!(matches!(reply.state, Some(State::Error(_))), "the base-form failure is not a no-result");
    install(&f.join("dictionaries/b.ifo"), "B", "other");
    let reply = f.reply(f.worker.lookup("thunders".into(), None, true)).0;
    assert_eq!(reply.selected.as_deref(), Some("b.ifo"));
    assert_eq!(reply.state, Some(State::NoResult));
}

#[test]
fn single_dictionary_reuses_its_backend_for_exact_and_forms() {
    let f = Fixture::new();
    let path = f.join("dictionaries/a.ifo");
    install(&path, "A", "thunder");
    let mut inner = inner(&f);
    assert_eq!(inner.lookup("other", None, &|| true).1, State::NoResult);
    // The cached backend has everything needed. Reopening during either lookup
    // would fail because its metadata is no longer on disk.
    fs::remove_file(path).unwrap();
    let (selected, state) = inner.lookup("thunders", None, &|| true);
    assert_eq!(selected.as_deref(), Some("a.ifo"));
    assert!(matches!(state, State::Definition { base_forms: Some(_), .. }));
}

#[test]
fn cancellation_at_any_point_returns_no_definition_and_marks_no_errors() {
    use std::cell::Cell;
    let f = Fixture::new();
    install(&f.join("dictionaries/a.ifo"), "A", "other");
    install(&f.join("dictionaries/b.ifo"), "B", "thunder");
    let mut inner = inner(&f);
    // Cancel at the nth check, through index building, exact and forms, the
    // dictionary switches and every base-form candidate, until one completes.
    // The worker drops whatever a cancelled lookup returns; it must not
    // return a definition or record cancellation as a dictionary error.
    for n in 0.. {
        assert!(n < 1000, "the lookup never completed");
        let checks = Cell::new(0);
        let current = || {
            checks.set(checks.get() + 1);
            checks.get() <= n
        };
        let (_, state) = inner.lookup("thunders", None, &current);
        if checks.get() <= n {
            assert!(matches!(state, State::Definition { base_forms: Some(_), .. }), "{state:?}");
            break;
        }
        assert!(!matches!(state, State::Definition { .. }), "cancelled at check {n}: {state:?}");
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
    let lookup = f.worker.lookup("word".into(), None, false);
    let catalog = settings.catalog(false);
    let reply = wait_client(&settings, catalog);
    assert!(reply.state.is_none());
    let reply = wait_client(&f.worker, lookup);
    assert!(matches!(reply.state, Some(State::Definition { .. })));
    assert!(settings.receive().is_none());
    let lookup = f.worker.lookup("word".into(), None, false);
    settings.catalog(false);
    settings.flush().unwrap(); // Does not cancel another client's lookup.
    assert!(matches!(wait_client(&f.worker, lookup).state, Some(State::Definition { .. })));
    let catalog = settings.catalog(false);
    f.worker.cancel();
    assert!(wait_client(&settings, catalog).state.is_none());
    // Another client's edits and folder saves are queued, not cancellations.
    let lookup = f.worker.lookup("word".into(), None, false);
    let created = settings.edit(Edit::Create { id: "set".into(), name: "Set".into() });
    f.store.save_dictionary_folders(&[]).unwrap();
    assert!(matches!(wait_client(&f.worker, lookup).state, Some(State::Definition { .. })));
    let reply = wait_client(&settings, created);
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
        let reply = f.reply(f.worker.lookup_in(query.into(), None, false, selection)).0;
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
    let lookup = || f.reply(f.worker.lookup_in("word".into(), None, false, selection.clone())).0;
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
    let reply = f.reply(f.worker.lookup("word".into(), None, true)).0;
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
