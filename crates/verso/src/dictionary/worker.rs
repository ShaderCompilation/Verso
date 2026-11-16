//! One service per Store family, with independent cancellable client mailboxes.
//! Discovery is lazy; edits and durable writes share the lookup command queue.
use super::{
    Backend, Catalog, Dictionary, Entry, Source, SourceStatus, discover_catalog, forms,
    sets::{DEFAULT, Edit, Preferences, Selection, Set},
};
use crate::{
    platform::Waker,
    store::{Store, atomic, default_dictionary_folders},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
    mpsc,
};

/// How a base-form definition was found, for the line above it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaseForms {
    /// The query as submitted, which the lookup field may no longer show.
    pub query: String,
    /// Distinct base forms shown, from one to three.
    pub count: usize,
}

/// The outcome of a lookup. Discovery problems are not states: they arrive as
/// `Reply::warnings` next to whatever the working sources found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Definition {
        entries: Vec<Entry>,
        /// Present only when the definition is for base forms of the query.
        base_forms: Option<BaseForms>,
    },
    Missing,
    Disabled,
    NoResult,
    /// The dictionary itself could not be read.
    Error(String),
}
pub struct Reply {
    pub id: u64,
    pub dictionaries: Vec<Dictionary>,
    /// The native source first, then external folders in configured order.
    pub sources: Vec<Source>,
    /// Source and configuration problems, readable as they are.
    pub warnings: Vec<String>,
    pub selected: Option<String>,
    #[allow(dead_code)] // Management consumes the set catalog in Phase 4.
    pub sets: Vec<Set>,
    pub active_set: String,
    pub change_error: Option<String>,
    /// `None` answers a catalog request.
    pub state: Option<State>,
    pub save_error: Option<String>,
}
enum Command {
    Lookup { request: Request, query: String, selected: Option<String>, selection: Selection, rescan: bool },
    Catalog { request: Request, set: String, rescan: bool },
    Edit { request: Request, edit: Edit },
    Change { set: String, id: String, up: bool },
    Release,
    Flush(mpsc::Sender<Result<(), String>>),
    Stop,
}
struct Mailbox {
    result: Mutex<Option<Reply>>,
    latest: AtomicU64,
    waker: Waker,
}
struct Request {
    id: u64,
    mailbox: Arc<Mailbox>,
    revision: u64,
    folders: u64,
}
/// The only owner of dictionary backends and preferences. Store creates this
/// on first use and retains it across screen/client lifetimes.
pub struct Service {
    tx: mpsc::Sender<Command>,
    revision: Arc<AtomicU64>,
    folders: Arc<AtomicU64>,
}
/// A screen's client, not a separate thread. Its replies and cancellation are
/// private; a Settings catalog can never replace a reader lookup.
pub struct Worker {
    service: Arc<Service>,
    mailbox: Arc<Mailbox>,
}

/// Source and configuration problems worth showing. An absent native or
/// default folder is an ordinary empty installation; an absent folder the
/// user added is reported.
pub fn warnings(sources: &[Source], config_error: Option<&str>) -> Vec<String> {
    let defaults = default_dictionary_folders();
    let config = config_error
        .map(|e| format!("Dictionary folder settings couldn't be read, so only Verso's folder is searched. {e}"));
    let sources = sources.iter().filter_map(|s| match &s.status {
        SourceStatus::Available => None,
        SourceStatus::Absent if s.native || defaults.contains(&s.root) => None,
        SourceStatus::Absent => Some(format!("{} is not available.", s.root.display())),
        SourceStatus::Unreadable(e) => Some(e.clone()),
        SourceStatus::LimitReached => {
            Some(format!("{} has too many files; some dictionaries may be missing.", s.root.display()))
        }
    });
    config.into_iter().chain(sources).collect()
}

/// Everything the worker thread owns.
struct Inner {
    store: Store,
    prefs_path: std::path::PathBuf,
    preferences: Preferences,
    folders_seen: u64,
    active_set: String,
    /// The last discovered catalog. When stale it is only used to apply
    /// preference changes, never published.
    catalog: Catalog,
    warnings: Vec<String>,
    fresh: bool,
    backend: Option<(String, Backend)>,
    save_error: Option<String>,
}
impl Inner {
    /// Rediscovers every source with the current folder configuration.
    /// `false` when cancelled: the catalog stays stale and nothing is published.
    fn discover(&mut self, current: &impl Fn() -> bool) -> bool {
        // Files may have changed; reopen from the new catalog.
        self.backend = None;
        self.fresh = false;
        let folders = self.store.dictionary_folders();
        self.folders_seen = self.store.dictionary_folder_revision.load(Ordering::Relaxed);
        match discover_catalog(
            &self.store.dictionary_dir(),
            &folders.folders,
            &self.preferences.set(DEFAULT).dictionaries,
            current,
        ) {
            Ok(catalog) => {
                self.warnings = warnings(&catalog.sources, folders.error.as_deref());
                self.catalog = catalog;
                if self.preferences.discover(&self.catalog.dictionaries) {
                    self.save();
                }
                self.fresh = true;
                true
            }
            Err(_) => false,
        }
    }
    fn save(&mut self) {
        self.save_error = atomic(&self.prefs_path, &self.preferences).err().map(|e| format!("{e:#}"));
    }
    fn apply_set(&mut self, set: &str) {
        let set = self.preferences.set(set);
        self.active_set = set.id.clone();
        set.apply(&mut self.catalog.dictionaries);
    }
    fn change(&mut self, set: &str, id: &str, up: bool) {
        let entries = &self.preferences.set(set).dictionaries;
        if let Some(i) = entries.iter().position(|d| d.id == id) {
            let edit = if up {
                Edit::MoveDictionary { set: set.into(), dictionary: id.into(), position: i.saturating_sub(1) }
            } else {
                Edit::Disabled { set: set.into(), dictionary: id.into(), disabled: !entries[i].disabled }
            };
            if self.preferences.edit(edit).is_ok() {
                self.save();
            }
        }
    }
    /// Returns the chosen dictionary and the state.
    fn lookup(
        &mut self,
        query: &str,
        selected: Option<String>,
        current: &impl Fn() -> bool,
    ) -> (Option<String>, State) {
        let dictionaries = &self.catalog.dictionaries;
        if dictionaries.is_empty() {
            return (None, State::Missing);
        }
        if dictionaries.iter().all(|d| d.disabled) {
            return (None, State::Disabled);
        }
        let requested = selected.filter(|s| dictionaries.iter().any(|d| d.id == *s && !d.disabled));
        // No headword can match an oversized query. Answer it as a plain miss
        // instead of recording an index error against every dictionary.
        if !super::index::query_fits(query) {
            let id = requested.or_else(|| dictionaries.iter().find(|d| !d.disabled).map(|d| d.id.clone()));
            return (id, State::NoResult);
        }
        let mut outcomes = vec![None; dictionaries.len()];
        let forms = self.preferences.set(&self.active_set).english_forms().then(|| forms::candidates(query)).flatten();
        if let Some(found) = self.search(query, forms.as_ref(), requested.as_deref(), &mut outcomes, current) {
            return found;
        }
        // Keep each dictionary's final outcome in preference order. A readable
        // dictionary with no result wins over an error, as in the exact-only lookup.
        let mut fallback = None;
        for (dictionary, state) in self.catalog.dictionaries.iter().zip(outcomes).filter_map(|(d, s)| s.map(|s| (d, s)))
        {
            if fallback.is_none() || matches!(&fallback, Some((_, State::Error(_)))) && state == State::NoResult {
                fallback = Some((dictionary.id.clone(), state));
            }
        }
        fallback.map_or((None, State::NoResult), |(id, state)| (Some(id), state))
    }
    /// Each enabled (or requested) dictionary tries exact, then base forms,
    /// before moving to the next dictionary. Records misses/errors for fallback.
    fn search(
        &mut self,
        query: &str,
        forms: Option<&forms::Candidates>,
        requested: Option<&str>,
        outcomes: &mut [Option<State>],
        current: &impl Fn() -> bool,
    ) -> Option<(Option<String>, State)> {
        let backend = &mut self.backend;
        let cache = self.store.dictionary_cache();
        for (i, dictionary) in self
            .catalog
            .dictionaries
            .iter_mut()
            .enumerate()
            .filter(|(_, d)| !d.disabled && requested.is_none_or(|s| d.id == s))
        {
            if !current() {
                break;
            }
            // The entries, plus the base-form count for a base-form lookup.
            let lookup = (|| -> anyhow::Result<(Vec<Entry>, Option<usize>)> {
                if backend.as_ref().is_none_or(|(id, _)| id != &dictionary.id) {
                    // Release the old index and eight-block cache before opening another.
                    *backend = None;
                    *backend = Some((dictionary.id.clone(), Backend::open(&dictionary.path, &cache, current)?));
                }
                let backend = &mut backend.as_mut().unwrap().1;
                let entries = backend.lookup(query)?;
                if !current() {
                    anyhow::bail!("Lookup cancelled");
                }
                if entries.is_empty()
                    && let Some(forms) = forms
                {
                    let started = std::time::Instant::now();
                    let result = backend.lookup_forms(forms, current);
                    log::debug!(
                        "dictionary base forms: {:?}, {} generated candidates",
                        started.elapsed(),
                        forms.count()
                    );
                    return result.map(|(entries, count)| (entries, Some(count)));
                }
                Ok((entries, None))
            })();
            outcomes[i] = Some(match lookup {
                Ok((text, _)) if text.is_empty() => {
                    dictionary.error = None;
                    State::NoResult
                }
                Ok((entries, count)) => {
                    dictionary.error = None;
                    let base_forms = count.map(|count| BaseForms { query: query.to_owned(), count });
                    return Some((Some(dictionary.id.clone()), State::Definition { entries, base_forms }));
                }
                Err(e) => {
                    if !current() {
                        break;
                    }
                    let error = format!("{e:#}");
                    dictionary.error = Some(error.clone());
                    State::Error(error)
                }
            });
        }
        None
    }

    fn reply(&self, id: u64, selected: Option<String>, state: Option<State>) -> Reply {
        Reply {
            id,
            dictionaries: self.catalog.dictionaries.clone(),
            sources: self.catalog.sources.clone(),
            warnings: self.warnings.clone(),
            selected,
            sets: self.preferences.sets.clone(),
            active_set: self.active_set.clone(),
            change_error: None,
            state,
            save_error: self.save_error.clone(),
        }
    }
}

impl Service {
    pub(crate) fn new(store: Store) -> Self {
        let (tx, commands) = mpsc::channel();
        let revision = Arc::new(AtomicU64::new(0));
        let generation = revision.clone();
        let folders = store.dictionary_folder_revision.clone();
        // Do not retain Store's service registry from its own thread.
        let store = store.dictionary_context();
        std::thread::Builder::new()
            .name("verso-dictionary".into())
            .spawn(move || {
                // Even preference reads wait for an actual request.
                let Ok(first) = commands.recv() else { return };
                let prefs_path = store.dictionary_preferences();
                let (preferences, migrated) = super::bounded_read(&prefs_path, 1024 * 1024)
                    .ok()
                    .and_then(|s| Preferences::parse(&s).ok())
                    .unwrap_or_default();
                let mut inner = Inner {
                    store,
                    prefs_path,
                    preferences,
                    folders_seen: 0,
                    active_set: DEFAULT.into(),
                    catalog: Catalog::default(),
                    warnings: Vec::new(),
                    fresh: false,
                    backend: None,
                    save_error: None,
                };
                if migrated {
                    inner.save();
                }
                for command in std::iter::once(first).chain(commands) {
                    // Clone the counter so cancellation closures don't borrow Inner.
                    let folder_revision = inner.store.dictionary_folder_revision.clone();
                    let is_current = |r: &Request| {
                        r.mailbox.latest.load(Ordering::Relaxed) == r.id
                            && generation.load(Ordering::Relaxed) == r.revision
                            && folder_revision.load(Ordering::Relaxed) == r.folders
                    };
                    match command {
                        Command::Stop => break,
                        Command::Release => {
                            // A queued request from another client owns its own backend work.
                            // Releasing this cache is safe and bounds idle retained memory.
                            inner.backend = None;
                        }
                        Command::Flush(done) => {
                            if inner.save_error.is_some() {
                                inner.save();
                            }
                            let _ = done.send(inner.save_error.clone().map_or(Ok(()), Err));
                        }
                        Command::Change { set, id, up } => inner.change(&set, &id, up),
                        Command::Edit { request, edit } => {
                            // Edits survive closing the requesting screen.
                            let error = inner.preferences.edit(edit).err().map(|e| e.to_string());
                            if error.is_none() {
                                inner.save();
                            }
                            inner.apply_set(DEFAULT);
                            let mut reply = inner.reply(request.id, None, None);
                            reply.change_error = error;
                            publish(&request, reply, &|| is_current(&request));
                        }
                        Command::Catalog { request, set, rescan } => {
                            if !is_current(&request) {
                                continue;
                            }
                            let current = || is_current(&request);
                            if (!inner.fresh || rescan || inner.folders_seen != request.folders)
                                && !inner.discover(&current)
                            {
                                continue;
                            }
                            inner.apply_set(&set);
                            publish(&request, inner.reply(request.id, None, None), &current);
                        }
                        Command::Lookup { request, query, selected, selection, rescan } => {
                            if !is_current(&request) {
                                continue;
                            }
                            let current = || is_current(&request);
                            if (!inner.fresh || rescan || inner.folders_seen != request.folders)
                                && !inner.discover(&current)
                            {
                                continue;
                            }
                            let set = inner.preferences.choose(&selection, &query).id.clone();
                            inner.apply_set(&set);
                            let (chosen, state) = inner.lookup(&query, selected, &current);
                            publish(&request, inner.reply(request.id, chosen, Some(state)), &current);
                        }
                    }
                }
            })
            .expect("spawn dictionary worker");
        Self { tx, revision, folders }
    }
}
fn publish(request: &Request, reply: Reply, current: &dyn Fn() -> bool) {
    if current() {
        *request.mailbox.result.lock().unwrap() = Some(reply);
        request.mailbox.waker.wake();
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        self.revision.fetch_add(1, Ordering::Relaxed);
        let _ = self.tx.send(Command::Stop);
        // Never join a potentially slow removable-storage read on the UI thread.
    }
}
impl Worker {
    pub fn new(store: Store, waker: Waker) -> Self {
        let service = store.dictionary_service.get_or_init(|| Arc::new(Service::new(store.clone()))).clone();
        Self { service, mailbox: Arc::new(Mailbox { result: Mutex::new(None), latest: AtomicU64::new(0), waker }) }
    }
    fn request(&self) -> Request {
        let id = self.mailbox.latest.fetch_add(1, Ordering::Relaxed) + 1;
        *self.mailbox.result.lock().unwrap() = None;
        Request {
            id,
            mailbox: self.mailbox.clone(),
            revision: self.service.revision.load(Ordering::Relaxed),
            folders: self.service.folders.load(Ordering::Relaxed),
        }
    }
    #[cfg(test)]
    pub fn lookup(&self, query: String, selected: Option<String>, rescan: bool) -> u64 {
        self.lookup_in(query, selected, rescan, Selection::default())
    }
    pub fn lookup_in(&self, query: String, selected: Option<String>, rescan: bool, selection: Selection) -> u64 {
        let request = self.request();
        let id = request.id;
        let _ = self.service.tx.send(Command::Lookup { request, query, selected, selection, rescan });
        id
    }
    pub fn catalog(&self, rescan: bool) -> u64 {
        self.catalog_in(DEFAULT, rescan)
    }
    pub fn catalog_in(&self, set: &str, rescan: bool) -> u64 {
        let request = self.request();
        let id = request.id;
        let _ = self.service.tx.send(Command::Catalog { request, set: set.into(), rescan });
        id
    }
    #[allow(dead_code)] // Management uses the full edit API in Phase 4.
    pub fn edit(&self, edit: Edit) -> u64 {
        self.service.revision.fetch_add(1, Ordering::Relaxed);
        let request = self.request();
        let id = request.id;
        let _ = self.service.tx.send(Command::Edit { request, edit });
        id
    }
    #[cfg(test)]
    pub fn change(&self, id: String, up: bool) {
        self.change_in(DEFAULT, id, up);
    }
    pub fn change_in(&self, set: &str, id: String, up: bool) {
        self.service.revision.fetch_add(1, Ordering::Relaxed);
        let _ = self.service.tx.send(Command::Change { set: set.into(), id, up });
    }
    pub fn receive(&self) -> Option<Reply> {
        self.mailbox.result.lock().unwrap().take().filter(|r| r.id == self.mailbox.latest.load(Ordering::Relaxed))
    }
    #[cfg(test)]
    pub fn result_for_test(&self, reply: Reply) {
        *self.mailbox.result.lock().unwrap() = Some(reply);
    }
    pub fn cancel(&self) {
        self.mailbox.latest.fetch_add(1, Ordering::Relaxed);
        *self.mailbox.result.lock().unwrap() = None;
        let _ = self.service.tx.send(Command::Release);
    }
    pub fn flush(&self) -> anyhow::Result<()> {
        self.cancel();
        let (tx, rx) = mpsc::channel();
        self.service.tx.send(Command::Flush(tx))?;
        rx.recv()?.map_err(|e| anyhow::anyhow!("Couldn't save dictionary preferences: {e}"))
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
