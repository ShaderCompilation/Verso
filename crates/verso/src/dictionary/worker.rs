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
    Retry { request: Request, set: String, dictionary: String },
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
}
/// The only owner of dictionary backends and preferences. Store creates this
/// on first use and retains it across screen/client lifetimes.
pub struct Service {
    tx: mpsc::Sender<Command>,
    /// Bumped only when the service stops.
    revision: Arc<AtomicU64>,
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
    failed_queries: std::collections::HashMap<String, (String, bool)>,
    /// The preference file exists but couldn't be read or parsed. Automatic
    /// saves then leave it alone; the user's next change replaces it.
    prefs_error: Option<String>,
}
/// Absent preferences are empty; an unreadable or invalid file is an error.
fn read_preferences(path: &std::path::Path) -> anyhow::Result<(Preferences, bool)> {
    if !path.try_exists()? {
        return Ok((Preferences::default(), false));
    }
    Preferences::parse(&super::bounded_read(path, 1024 * 1024)?)
}
/// Reopen files and repeat the failed query (including base forms), so an
/// error disappears only after the operation that failed can succeed.
fn retry_read(
    dictionary: &Dictionary,
    cache: &std::path::Path,
    query: Option<&(String, bool)>,
    current: &impl Fn() -> bool,
) -> anyhow::Result<()> {
    let mut backend = Backend::open(&dictionary.path, cache, current)?;
    if let Some((query, english_forms)) = query
        && backend.lookup(query)?.is_empty()
        && *english_forms
        && let Some(forms) = forms::candidates(query)
    {
        backend.lookup_forms(&forms, current)?;
    }
    Ok(())
}
impl Inner {
    /// Reads the preference file, keeping the in-memory copy if it fails.
    fn load(&mut self) {
        match read_preferences(&self.prefs_path) {
            Ok((preferences, migrated)) => {
                self.preferences = preferences;
                self.prefs_error = None;
                if migrated {
                    self.save();
                }
            }
            Err(e) => self.prefs_error = Some(format!("{e:#}")),
        }
    }
    /// Rediscovers every source with the current folder configuration.
    /// `false` when cancelled: the catalog stays stale and nothing is published.
    fn discover(&mut self, current: &impl Fn() -> bool) -> bool {
        // Files may have changed; reopen from the new catalog.
        self.backend = None;
        self.fresh = false;
        // Read the counter first: a save during the read then forces another scan.
        self.folders_seen = self.store.dictionary_folder_revision.load(Ordering::Relaxed);
        let folders = self.store.dictionary_folders();
        // A damaged file may have been a passing read failure (e.g. after USB).
        // Retry until the user replaces it, unless their own save is pending.
        if self.prefs_error.is_some() && self.save_error.is_none() {
            self.load();
        }
        match discover_catalog(
            &self.store.dictionary_dir(),
            &folders.folders,
            &self.preferences.set(DEFAULT).dictionaries,
            current,
        ) {
            Ok(mut catalog) => {
                let cache = self.store.dictionary_cache();
                for dictionary in &mut catalog.dictionaries {
                    if dictionary.error.is_none()
                        && self.catalog.dictionaries.iter().any(|d| d.id == dictionary.id && d.error.is_some())
                    {
                        dictionary.error =
                            retry_read(dictionary, &cache, self.failed_queries.get(&dictionary.id), current)
                                .err()
                                .map(|e| format!("{e:#}"));
                        if !current() {
                            return false;
                        }
                        if dictionary.error.is_none() {
                            self.failed_queries.remove(&dictionary.id);
                        }
                    }
                }
                self.failed_queries.retain(|id, _| catalog.dictionaries.iter().any(|d| d.id == *id));
                self.warnings = warnings(&catalog.sources, folders.error.as_deref());
                self.catalog = catalog;
                if self.preferences.discover(&self.catalog.dictionaries) && self.prefs_error.is_none() {
                    self.save();
                }
                self.fresh = true;
                true
            }
            Err(_) => false,
        }
    }
    /// Writes the whole preference file, replacing a damaged one.
    fn save(&mut self) {
        self.save_error = atomic(&self.prefs_path, &self.preferences).err().map(|e| format!("{e:#}"));
        if self.save_error.is_none() {
            self.prefs_error = None;
        }
    }
    fn apply_set(&mut self, set: &str) {
        let set = self.preferences.set(set);
        self.active_set = set.id.clone();
        set.apply(&mut self.catalog.dictionaries);
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
                    self.failed_queries.remove(&dictionary.id);
                    dictionary.error = None;
                    State::NoResult
                }
                Ok((entries, count)) => {
                    self.failed_queries.remove(&dictionary.id);
                    dictionary.error = None;
                    let base_forms = count.map(|count| BaseForms { query: query.to_owned(), count });
                    return Some((Some(dictionary.id.clone()), State::Definition { entries, base_forms }));
                }
                Err(e) => {
                    if !current() {
                        break;
                    }
                    let error = format!("{e:#}");
                    self.failed_queries.insert(dictionary.id.clone(), (query.into(), forms.is_some()));
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
            warnings: self
                .warnings
                .iter()
                .cloned()
                .chain(self.prefs_error.as_ref().map(|e| {
                    format!(
                        "Dictionary preferences couldn't be read, so every dictionary is on, in the order found. \
                     Changing a dictionary replaces the saved preferences. {e}"
                    )
                }))
                .collect(),
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
        // Do not retain Store's service registry from its own thread.
        let store = store.dictionary_context();
        std::thread::Builder::new()
            .name("verso-dictionary".into())
            .spawn(move || {
                // Even preference reads wait for an actual request.
                let Ok(first) = commands.recv() else { return };
                let prefs_path = store.dictionary_preferences();
                let mut inner = Inner {
                    store,
                    prefs_path,
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
                inner.load();
                for command in std::iter::once(first).chain(commands) {
                    // Only the asking client's newer request (or stopping the
                    // service) cancels; other clients' edits are queued in order.
                    let is_current = |r: &Request| {
                        r.mailbox.latest.load(Ordering::Relaxed) == r.id
                            && generation.load(Ordering::Relaxed) == r.revision
                    };
                    let folders_changed = |inner: &Inner| {
                        inner.folders_seen != inner.store.dictionary_folder_revision.load(Ordering::Relaxed)
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
                        Command::Edit { request, edit } => {
                            // Edits survive closing the requesting screen.
                            let set = edit.set().to_owned();
                            let error = inner.preferences.edit(edit).err().map(|e| e.to_string());
                            if error.is_none() {
                                inner.save();
                            }
                            // Only a fresh catalog is published. The edit itself
                            // is already applied and saved, even if this is cancelled.
                            let current = || is_current(&request);
                            if current() && (!inner.fresh || folders_changed(&inner)) && !inner.discover(&current) {
                                continue;
                            }
                            // Reply with the edited set (Default after a deletion).
                            inner.apply_set(&set);
                            let mut reply = inner.reply(request.id, None, None);
                            reply.change_error = error;
                            publish(&request, reply, &current);
                        }
                        Command::Retry { request, set, dictionary } => {
                            let current = || is_current(&request);
                            if !current() {
                                continue;
                            }
                            if (!inner.fresh || folders_changed(&inner)) && !inner.discover(&current) {
                                continue;
                            }
                            inner.backend = None;
                            if let Some(d) = inner.catalog.dictionaries.iter_mut().find(|d| d.id == dictionary) {
                                let result = retry_read(
                                    d,
                                    &inner.store.dictionary_cache(),
                                    inner.failed_queries.get(&dictionary),
                                    &current,
                                );
                                if !current() {
                                    continue;
                                }
                                d.error = result.err().map(|e| format!("{e:#}"));
                                if d.error.is_none() {
                                    inner.failed_queries.remove(&dictionary);
                                    if let Ok(info) = super::Info::read(&d.path) {
                                        d.name = info.name;
                                    }
                                }
                            }
                            inner.apply_set(&set);
                            publish(&request, inner.reply(request.id, None, None), &current);
                        }
                        Command::Catalog { request, set, rescan } => {
                            if !is_current(&request) {
                                continue;
                            }
                            let current = || is_current(&request);
                            if (!inner.fresh || rescan || folders_changed(&inner)) && !inner.discover(&current) {
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
                            if (!inner.fresh || rescan || folders_changed(&inner)) && !inner.discover(&current) {
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
        Self { tx, revision }
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
        Request { id, mailbox: self.mailbox.clone(), revision: self.service.revision.load(Ordering::Relaxed) }
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
    pub fn edit(&self, edit: Edit) -> u64 {
        let request = self.request();
        let id = request.id;
        let _ = self.service.tx.send(Command::Edit { request, edit });
        id
    }
    pub fn retry(&self, set: &str, dictionary: &str) -> u64 {
        let request = self.request();
        let id = request.id;
        let _ = self.service.tx.send(Command::Retry { request, set: set.into(), dictionary: dictionary.into() });
        id
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
