//! One service per Store family, with independent cancellable client mailboxes.
//! Discovery is lazy; edits and durable writes share the lookup command queue.
use super::{
    Backend, Catalog, Dictionary, Entry, Hits, Source, SourceStatus, discover_catalog, forms,
    sets::{DEFAULT, Edit, Preferences, Selection, Set},
};
use crate::{
    platform::Waker,
    store::{Store, atomic, default_dictionary_folders},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc,
};

/// Shown when a matched entry can't be read; details live in management.
const UNREADABLE: &str = "Couldn’t read this entry. Open Manage dictionaries for details.";
/// How long a search may run before its first, incomplete publication.
const FIRST_PUBLICATION: std::time::Duration = std::time::Duration::from_millis(150);

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
/// Index hits only: definitions are loaded for the selected dictionary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    pub dictionary: String,
    pub hits: Hits,
}
impl Match {
    pub fn headwords(&self) -> Vec<String> {
        let mut words = Vec::new();
        for (word, _, _) in &self.hits.records {
            if !words.contains(word) {
                words.push(word.clone());
            }
        }
        words
    }
}
#[derive(Clone)]
struct Session {
    query: String,
    active_set: String,
    matches: Vec<Match>,
    revision: u64,
}
#[derive(Clone)]
pub struct Reply {
    pub id: u64,
    pub matches: Vec<Match>,
    pub complete: bool,
    /// The dictionary whose index is being built; never inferred from a delay.
    pub preparing: Option<String>,
    /// Catalog replies only: dictionaries whose index must be built first.
    pub unprepared: Vec<String>,
    /// Catalog replies only: the active set's saved dictionaries that are not
    /// on disk now, in set order.
    pub missing: Vec<String>,
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
    Lookup { request: Request, query: String, selection: Selection, rescan: bool },
    Catalog { request: Request, set: String, rescan: bool },
    Edit { request: Request, edit: Edit },
    Retry { request: Request, set: String, dictionary: String },
    Open { request: Request, dictionary: String },
    Release,
    Flush(mpsc::Sender<Result<(), String>>),
    Stop,
}
struct Mailbox {
    result: Mutex<Option<Reply>>,
    session: Mutex<Option<Session>>,
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
    /// Cancels obsolete searches on preferences or rescan.
    revision: Arc<AtomicU64>,
    /// Cancels every running command, including another client's, at shutdown.
    stopped: Arc<AtomicBool>,
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
    /// Search every enabled index in set order, keeping only compact index
    /// hits; opening another dictionary later never repeats the search.
    ///
    /// A search that outlasts `FIRST_PUBLICATION` publishes an incomplete
    /// snapshot, then again only when what it shows changes meaning: the
    /// first definition arrives, or index preparation starts or ends. More
    /// matches alone wait for the final count, to keep e-ink redraws few.
    fn lookup_session(&mut self, request: &Request, query: &str, current: &impl Fn() -> bool) {
        use std::{cell::RefCell, time::Instant};
        let started = Instant::now();
        // Matches live only in the session; publications copy them.
        *request.mailbox.session.lock().unwrap() = Some(Session {
            query: query.into(),
            active_set: self.active_set.clone(),
            matches: Vec::new(),
            revision: request.revision,
        });
        let ids: Vec<_> = self.catalog.dictionaries.iter().filter(|d| !d.disabled).map(|d| d.id.clone()).collect();
        let state = if self.catalog.dictionaries.is_empty() {
            State::Missing
        } else if ids.is_empty() {
            State::Disabled
        } else {
            State::NoResult
        };
        // Built once; the loop updates only the fields a search changes.
        let snapshot = RefCell::new(Reply { complete: false, ..self.reply(request.id, None, Some(state)) });
        let matches =
            || request.mailbox.session.lock().unwrap().as_ref().map(|s| s.matches.clone()).unwrap_or_default();
        // (selected, preparing) as last published.
        let shown = RefCell::new(None);
        let progress = || {
            let valid = current();
            if valid && started.elapsed() >= FIRST_PUBLICATION {
                let snapshot = snapshot.borrow();
                let key = (snapshot.selected.clone(), snapshot.preparing.clone());
                if shown.borrow().as_ref() != Some(&key) {
                    publish(request, Reply { matches: matches(), ..snapshot.clone() }, current);
                    *shown.borrow_mut() = Some(key);
                }
            }
            valid
        };
        let forms = self.preferences.set(&self.active_set).english_forms().then(|| forms::candidates(query)).flatten();
        if let Some(forms) = &forms {
            log::debug!("dictionary query: {} generated candidates", forms.count());
        }
        if super::index::query_fits(query) {
            for id in ids {
                if !progress() {
                    return;
                }
                let preparing = || snapshot.borrow_mut().preparing = Some(id.clone());
                let outcome = self.open_backend(&id, &progress, &preparing).and_then(|backend| {
                    if !backend.displayable() {
                        return Ok(Hits { records: vec![], forms: None });
                    }
                    let hits = backend.find(query)?;
                    if hits.records.is_empty()
                        && let Some(forms) = &forms
                    {
                        backend.find_forms(forms, &progress)
                    } else {
                        Ok(hits)
                    }
                });
                snapshot.borrow_mut().preparing = None;
                if !current() {
                    return;
                }
                match outcome {
                    Ok(hits) => {
                        // A readable index clears an earlier failure, as before
                        // sessions; an unreadable entry below records it again.
                        self.clear_error(&id);
                        if !hits.records.is_empty() {
                            let matched = Match { dictionary: id.clone(), hits };
                            // Only the first usable definition is rendered during search.
                            if snapshot.borrow().selected.is_none() {
                                match self.render_match(&matched, query, current) {
                                    Ok(definition) => {
                                        let mut snapshot = snapshot.borrow_mut();
                                        snapshot.selected = Some(id.clone());
                                        snapshot.state = Some(definition);
                                    }
                                    Err(error) if current() => self.read_error(&id, query, forms.is_some(), error),
                                    Err(_) => return,
                                }
                            }
                            if let Some(session) = request.mailbox.session.lock().unwrap().as_mut() {
                                session.matches.push(matched);
                            }
                        }
                    }
                    Err(error) => self.read_error(&id, query, forms.is_some(), error),
                }
                // Keep the snapshot's errors in step without cloning the catalog.
                let error = self.catalog.dictionaries.iter().find(|d| d.id == id).and_then(|d| d.error.clone());
                if let Some(d) = snapshot.borrow_mut().dictionaries.iter_mut().find(|d| d.id == id) {
                    d.error = error;
                }
            }
        }
        if !current() {
            return;
        }
        let mut reply = snapshot.into_inner();
        reply.complete = true;
        reply.matches = matches();
        // Every match failed to render: show the first one's error page, not
        // a miss next to a nonzero count.
        if reply.selected.is_none()
            && let Some(first) = reply.matches.first()
        {
            reply.selected = Some(first.dictionary.clone());
            reply.state = Some(State::Error(UNREADABLE.into()));
        }
        publish(request, reply, current);
    }
    #[cfg(test)]
    fn lookup(&mut self, query: &str, current: &impl Fn() -> bool) -> (Option<String>, State) {
        let mailbox = Arc::new(Mailbox {
            result: Mutex::new(None),
            session: Mutex::new(None),
            latest: AtomicU64::new(0),
            waker: Waker::new(|| {}),
        });
        let request = Request { id: 0, mailbox: mailbox.clone(), revision: 0 };
        self.lookup_session(&request, query, current);
        let Some(reply) = mailbox.result.lock().unwrap().take() else { return (None, State::NoResult) };
        (reply.selected, reply.state.unwrap())
    }
    fn open_backend(
        &mut self,
        id: &str,
        current: &impl Fn() -> bool,
        preparing: &impl Fn(),
    ) -> anyhow::Result<&mut Backend> {
        if self.backend.as_ref().is_none_or(|(open, _)| open != id) {
            self.backend = None;
            let dictionary = self
                .catalog
                .dictionaries
                .iter()
                .find(|d| d.id == id)
                .ok_or_else(|| anyhow::anyhow!("Dictionary disappeared"))?;
            self.backend = Some((
                id.into(),
                Backend::open_progress(&dictionary.path, &self.store.dictionary_cache(), current, preparing)?,
            ));
        }
        Ok(&mut self.backend.as_mut().unwrap().1)
    }
    fn render_match(&mut self, matched: &Match, query: &str, current: &impl Fn() -> bool) -> anyhow::Result<State> {
        let entries = self.open_backend(&matched.dictionary, current, &|| {})?.render(matched.hits.records.clone())?;
        Ok(State::Definition {
            entries,
            base_forms: matched.hits.forms.map(|count| BaseForms { query: query.into(), count }),
        })
    }
    fn clear_error(&mut self, id: &str) {
        self.failed_queries.remove(id);
        if let Some(d) = self.catalog.dictionaries.iter_mut().find(|d| d.id == id) {
            d.error = None;
        }
    }
    fn read_error(&mut self, id: &str, query: &str, english: bool, error: anyhow::Error) {
        self.failed_queries.insert(id.into(), (query.into(), english));
        if let Some(d) = self.catalog.dictionaries.iter_mut().find(|d| d.id == id) {
            d.error = Some(format!("{error:#}"));
        }
    }

    /// A catalog reply: everything management shows, including the per-file
    /// preparation check that lookups never need.
    fn catalog_reply(&self, id: u64) -> Reply {
        let cache = self.store.dictionary_cache();
        let unprepared = self
            .catalog
            .dictionaries
            .iter()
            .filter(|d| d.error.is_none() && !super::index::Index::prepared(&d.path, &cache))
            .map(|d| d.id.clone())
            .collect();
        let missing = self
            .preferences
            .set(&self.active_set)
            .dictionaries
            .iter()
            .filter(|p| !self.catalog.dictionaries.iter().any(|d| d.id == p.id))
            .map(|p| p.id.clone())
            .collect();
        Reply { unprepared, missing, ..self.reply(id, None, None) }
    }
    fn reply(&self, id: u64, selected: Option<String>, state: Option<State>) -> Reply {
        Reply {
            id,
            matches: vec![],
            complete: true,
            preparing: None,
            unprepared: vec![],
            missing: vec![],
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
        let stopped = Arc::new(AtomicBool::new(false));
        let stopping = stopped.clone();
        let folders = store.dictionary_folder_revision.clone();
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
                    let alive = || !stopping.load(Ordering::Relaxed);
                    let is_current = |r: &Request| alive() && r.mailbox.latest.load(Ordering::Relaxed) == r.id;
                    let folders_changed = |inner: &Inner| {
                        inner.folders_seen != inner.store.dictionary_folder_revision.load(Ordering::Relaxed)
                    };
                    match command {
                        Command::Stop => break,
                        Command::Open { request, dictionary } => {
                            let current =
                                || is_current(&request) && generation.load(Ordering::Relaxed) == request.revision;
                            let session = request.mailbox.session.lock().unwrap().clone();
                            if let Some(session) = session.filter(|s| current() && s.revision == request.revision)
                                && let Some(matched) = session.matches.iter().find(|m| m.dictionary == dictionary)
                            {
                                inner.apply_set(&session.active_set);
                                let state = match inner.render_match(matched, &session.query, &current) {
                                    Ok(state) => {
                                        inner.clear_error(&dictionary);
                                        state
                                    }
                                    Err(error) if current() => {
                                        inner.read_error(
                                            &dictionary,
                                            &session.query,
                                            matched.hits.forms.is_some(),
                                            error,
                                        );
                                        State::Error(UNREADABLE.into())
                                    }
                                    Err(_) => continue,
                                };
                                let mut reply = inner.reply(request.id, Some(dictionary), Some(state));
                                reply.matches = session.matches;
                                publish(&request, reply, &current);
                            }
                        }
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
                            // Edits survive closing the requesting screen, so
                            // Forget's own check runs to the end unless stopping.
                            let set = edit.set().to_owned();
                            if matches!(edit, Edit::Forget { .. })
                                && (!inner.fresh || folders_changed(&inner))
                                && !inner.discover(&alive)
                            {
                                continue;
                            }
                            let error = if let Edit::Forget { dictionary, .. } = &edit
                                && inner.catalog.dictionaries.iter().any(|d| d.id == *dictionary)
                            {
                                Some("Only a missing dictionary can be forgotten. Rescan first.".into())
                            } else {
                                inner.preferences.edit(edit).err().map(|e| e.to_string())
                            };
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
                            let mut reply = inner.catalog_reply(request.id);
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
                            publish(&request, inner.catalog_reply(request.id), &current);
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
                            publish(&request, inner.catalog_reply(request.id), &current);
                        }
                        Command::Lookup { request, query, selection, rescan } => {
                            if !is_current(&request) {
                                continue;
                            }
                            let folders_revision = inner.store.dictionary_folder_revision.clone();
                            let folders = folders_revision.load(Ordering::Relaxed);
                            let current = || {
                                is_current(&request)
                                    && generation.load(Ordering::Relaxed) == request.revision
                                    && folders_revision.load(Ordering::Relaxed) == folders
                            };
                            if (!inner.fresh || rescan || folders_changed(&inner)) && !inner.discover(&current) {
                                continue;
                            }
                            let set = inner.preferences.choose(&selection, &query).id.clone();
                            inner.apply_set(&set);
                            inner.lookup_session(&request, &query, &current);
                        }
                    }
                }
            })
            .expect("spawn dictionary worker");
        Self { tx, revision, stopped, folders }
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
        self.stopped.store(true, Ordering::Relaxed);
        let _ = self.tx.send(Command::Stop);
        // Never join a potentially slow removable-storage read on the UI thread.
    }
}
impl Worker {
    pub fn new(store: Store, waker: Waker) -> Self {
        let service = store.dictionary_service.get_or_init(|| Arc::new(Service::new(store.clone()))).clone();
        Self {
            service,
            mailbox: Arc::new(Mailbox {
                result: Mutex::new(None),
                session: Mutex::new(None),
                latest: AtomicU64::new(0),
                waker,
            }),
        }
    }
    fn request(&self) -> Request {
        let id = self.mailbox.latest.fetch_add(1, Ordering::Relaxed) + 1;
        *self.mailbox.result.lock().unwrap() = None;
        Request { id, mailbox: self.mailbox.clone(), revision: self.service.revision.load(Ordering::Relaxed) }
    }
    #[cfg(test)]
    pub fn lookup(&self, query: String, rescan: bool) -> u64 {
        self.lookup_in(query, rescan, Selection::default())
    }
    pub fn lookup_in(&self, query: String, rescan: bool, selection: Selection) -> u64 {
        if rescan {
            self.service.revision.fetch_add(1, Ordering::Relaxed);
        }
        let request = self.request();
        *self.mailbox.session.lock().unwrap() = None;
        let id = request.id;
        let _ = self.service.tx.send(Command::Lookup { request, query, selection, rescan });
        id
    }
    pub fn catalog(&self, rescan: bool) -> u64 {
        self.catalog_in(DEFAULT, rescan)
    }
    pub fn catalog_in(&self, set: &str, rescan: bool) -> u64 {
        if rescan {
            self.service.revision.fetch_add(1, Ordering::Relaxed);
        }
        let request = self.request();
        let id = request.id;
        let _ = self.service.tx.send(Command::Catalog { request, set: set.into(), rescan });
        id
    }
    pub fn edit(&self, edit: Edit) -> u64 {
        self.service.revision.fetch_add(1, Ordering::Relaxed);
        let request = self.request();
        let id = request.id;
        let _ = self.service.tx.send(Command::Edit { request, edit });
        id
    }
    pub fn retry(&self, set: &str, dictionary: &str) -> u64 {
        self.service.revision.fetch_add(1, Ordering::Relaxed);
        let request = self.request();
        let id = request.id;
        let _ = self.service.tx.send(Command::Retry { request, set: set.into(), dictionary: dictionary.into() });
        id
    }
    pub fn revision(&self) -> u64 {
        self.service.revision.load(Ordering::Relaxed).wrapping_add(self.service.folders.load(Ordering::Relaxed))
    }
    /// Opening a hit queues behind the bounded search and keeps the session ID.
    pub fn open(&self, dictionary: &str) {
        let request = Request {
            id: self.mailbox.latest.load(Ordering::Relaxed),
            mailbox: self.mailbox.clone(),
            revision: self.service.revision.load(Ordering::Relaxed),
        };
        let _ = self.service.tx.send(Command::Open { request, dictionary: dictionary.into() });
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
        *self.mailbox.session.lock().unwrap() = None;
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
