//! Latest-request cancellation; preference writes remain ordered even on close.
//!
//! Discovery is lazy: the first request (or one after `invalidate`, or with
//! `rescan`) reads the folder configuration and scans every source. A
//! cancelled scan publishes nothing and leaves the catalog stale, so a later
//! request scans again; a stale catalog is never sent to the UI.
use super::{
    Backend, Catalog, Dictionary, Entry, Preference, Source, SourceStatus, discover_catalog, forms, merged_preferences,
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
    /// `None` answers a catalog request.
    pub state: Option<State>,
    pub save_error: Option<String>,
}
enum Command {
    Lookup { id: u64, query: String, selected: Option<String>, rescan: bool },
    Catalog { id: u64, rescan: bool },
    Change { id: String, up: bool },
    Invalidate,
    Cancel,
    Flush(mpsc::Sender<Result<(), String>>),
    Stop,
}
pub struct Worker {
    tx: mpsc::Sender<Command>,
    result: Arc<Mutex<Option<Reply>>>,
    latest: Arc<AtomicU64>,
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
    preferences: Vec<Preference>,
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
        match discover_catalog(&self.store.dictionary_dir(), &folders.folders, &self.preferences, current) {
            Ok(catalog) => {
                self.warnings = warnings(&catalog.sources, folders.error.as_deref());
                self.catalog = catalog;
                self.fresh = true;
                true
            }
            Err(_) => false,
        }
    }
    fn change(&mut self, id: &str, up: bool) {
        let dictionaries = &mut self.catalog.dictionaries;
        if let Some(i) = dictionaries.iter().position(|d| d.id == id) {
            if up {
                if i > 0 {
                    dictionaries.swap(i, i - 1);
                }
            } else {
                dictionaries[i].disabled = !dictionaries[i].disabled;
            }
            // Keep preferences for temporarily removed installations too.
            self.preferences = merged_preferences(dictionaries, std::mem::take(&mut self.preferences));
            self.save_error = atomic(&self.prefs_path, &self.preferences).err().map(|e| format!("{e:#}"));
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
        let mut outcomes = vec![None; dictionaries.len()];
        let forms = forms::candidates(query);
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
            state,
            save_error: self.save_error.clone(),
        }
    }
}

impl Worker {
    pub fn new(store: Store, waker: Waker) -> Self {
        let (tx, commands) = mpsc::channel();
        let latest = Arc::new(AtomicU64::new(0));
        let result = Arc::new(Mutex::new(None));
        let generation = latest.clone();
        let output = result.clone();
        std::thread::Builder::new()
            .name("verso-dictionary".into())
            .spawn(move || {
                let prefs_path = store.dictionary_preferences();
                let preferences = super::bounded_read(&prefs_path, 1024 * 1024)
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default();
                let mut inner = Inner {
                    store,
                    prefs_path,
                    preferences,
                    catalog: Catalog::default(),
                    warnings: Vec::new(),
                    fresh: false,
                    backend: None,
                    save_error: None,
                };
                let publish = |reply: Reply, current: &dyn Fn() -> bool| {
                    if current() {
                        *output.lock().unwrap() = Some(reply);
                        waker.wake();
                    }
                };
                while let Ok(command) = commands.recv() {
                    match command {
                        Command::Stop => break,
                        Command::Cancel => inner.backend = None,
                        Command::Invalidate => {
                            // A removed source may own the open backend; the
                            // next request rediscovers and reopens anyway.
                            inner.backend = None;
                            inner.fresh = false;
                        }
                        Command::Flush(done) => {
                            if inner.save_error.is_some() {
                                inner.save_error =
                                    atomic(&inner.prefs_path, &inner.preferences).err().map(|e| format!("{e:#}"));
                            }
                            let _ = done.send(inner.save_error.clone().map_or(Ok(()), Err));
                        }
                        Command::Change { id, up } => inner.change(&id, up),
                        Command::Catalog { id, rescan } => {
                            let current = || generation.load(Ordering::Relaxed) == id;
                            if !current() || (!inner.fresh || rescan) && !inner.discover(&current) {
                                continue;
                            }
                            publish(inner.reply(id, None, None), &current);
                        }
                        Command::Lookup { id, query, selected, rescan } => {
                            let current = || generation.load(Ordering::Relaxed) == id;
                            if !current() || (!inner.fresh || rescan) && !inner.discover(&current) {
                                continue;
                            }
                            let (chosen, state) = inner.lookup(&query, selected, &current);
                            publish(inner.reply(id, chosen, Some(state)), &current);
                        }
                    }
                }
            })
            .expect("spawn dictionary worker");
        Self { tx, result, latest }
    }
    fn next_id(&self) -> u64 {
        self.latest.fetch_add(1, Ordering::Relaxed) + 1
    }
    pub fn lookup(&self, query: String, selected: Option<String>, rescan: bool) -> u64 {
        let id = self.next_id();
        let _ = self.tx.send(Command::Lookup { id, query, selected, rescan });
        id
    }
    /// The dictionaries and source status without a query, e.g. for Settings
    /// before any book is open. `rescan` forces a fresh discovery.
    pub fn catalog(&self, rescan: bool) -> u64 {
        let id = self.next_id();
        let _ = self.tx.send(Command::Catalog { id, rescan });
        id
    }
    /// The folder configuration changed: cancels work in flight, releases the
    /// open dictionary and rediscovers on the next request.
    pub fn invalidate(&self) {
        self.latest.fetch_add(1, Ordering::Relaxed);
        let _ = self.tx.send(Command::Invalidate);
    }
    pub fn change(&self, id: String, up: bool) {
        let _ = self.tx.send(Command::Change { id, up });
    }
    pub fn receive(&self) -> Option<Reply> {
        self.result.lock().unwrap().take()
    }
    #[cfg(test)]
    pub fn result_for_test(&self, reply: Reply) {
        *self.result.lock().unwrap() = Some(reply);
    }
    pub fn cancel(&self) {
        self.latest.fetch_add(1, Ordering::Relaxed);
        let _ = self.tx.send(Command::Cancel);
    }
    /// Close/exit barrier, after cancelling any obsolete lookup work.
    pub fn flush(&self) -> anyhow::Result<()> {
        self.cancel();
        let (tx, rx) = mpsc::channel();
        self.tx.send(Command::Flush(tx))?;
        rx.recv()?.map_err(|e| anyhow::anyhow!("Couldn't save dictionary preferences: {e}"))
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.latest.fetch_add(1, Ordering::Relaxed);
        let _ = self.tx.send(Command::Stop);
        // No UI-thread join: a slow removable-storage read may still be running.
        // Cancellation bounds further work; queued preference writes run before Stop.
    }
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
