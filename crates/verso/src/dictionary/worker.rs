//! One service per Store family, with independent cancellable client mailboxes.
//! Discovery is lazy; edits and durable writes share the lookup command queue.
use super::{
    Catalog, Dictionary, Entry, Hits, Source, SourceStatus, discover_catalog,
    forms::{self, WordForms, WordFormsStatus},
    sets::{DEFAULT, Edit, Preferences, Selection, Set},
};
use super::{Probe, dictzip::Definitions};
use crate::{
    platform::Waker,
    store::{Store, atomic_bytes, default_dictionary_folders},
};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc,
};

/// Shown when a matched entry can't be read; details live in management.
const UNREADABLE: &str = "Couldn’t read this entry. Open Manage dictionaries for details.";
/// How long a search may run before it publishes status-only progress
/// ("Searching…", index preparation). A first definition never waits for
/// this: it is published as soon as it is rendered. Why: most searches finish
/// sooner, and on e-ink a status shown briefly and then replaced is an extra
/// panel update.
const PROGRESS_DELAY: std::time::Duration = std::time::Duration::from_millis(150);
/// Every file we write must fit the same bound used when reopening it.
const PREFERENCES_LIMIT: usize = 1024 * 1024;

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
    /// The query's word-form languages, for retrying a failed read.
    form_languages: Vec<String>,
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
    /// Catalog replies only: every word-form language and its source or error.
    pub word_forms: Vec<WordFormsStatus>,
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
    Prepare(u64),
    Release,
    Close { id: String, done: mpsc::Sender<()> },
    Rescan,
    Flush(mpsc::Sender<Result<(), String>>),
    Stop,
}
struct Mailbox {
    result: Mutex<Option<Reply>>,
    ready: Condvar,
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
    /// Commands sent other than Prepare, which a running Prepare yields to.
    sent: Arc<AtomicU64>,
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
    /// Indexes opened since the last discovery, kept across lookups: a file
    /// handle each, so a search no longer reopens and revalidates every
    /// dictionary. Discovery, Retry and a failed read drop them.
    probes: std::collections::HashMap<String, Probe>,
    /// The one open definitions file, for the dictionary last shown; it keeps
    /// up to eight decompressed blocks. Closing the lookup releases it.
    definitions: Option<(String, Definitions)>,
    save_error: Option<String>,
    /// The query that failed in each dictionary, with its word-form languages.
    failed_queries: std::collections::HashMap<String, (String, Vec<String>)>,
    /// Base-form analyzers; packs open on first use.
    word_forms: WordForms,
    /// The preference file exists but couldn't be read or parsed. Automatic
    /// saves then leave it alone; the user's next change replaces it.
    prefs_error: Option<String>,
}
/// Absent preferences are empty; an unreadable or invalid file is an error.
fn read_preferences(path: &std::path::Path) -> anyhow::Result<(Preferences, bool)> {
    if !path.try_exists()? {
        return Ok((Preferences::default(), false));
    }
    Preferences::parse(&super::bounded_read(path, PREFERENCES_LIMIT)?)
}
fn preference_bytes(preferences: &Preferences) -> anyhow::Result<Vec<u8>> {
    let bytes = serde_json::to_vec(preferences)?;
    anyhow::ensure!(
        bytes.len() <= PREFERENCES_LIMIT,
        "Dictionary preferences exceed the 1 MiB limit. Remove unused sets or forget missing dictionaries first."
    );
    Ok(bytes)
}
/// Reopen files and repeat the failed query (including base forms), so an
/// error disappears only after the operation that failed can succeed.
fn retry_read(
    dictionary: &Dictionary,
    cache: &std::path::Path,
    query: Option<&(String, Vec<String>)>,
    word_forms: &mut WordForms,
    current: &impl Fn() -> bool,
) -> anyhow::Result<()> {
    let mut backend = super::Backend::open(&dictionary.path, cache, current)?;
    if let Some((query, languages)) = query
        && backend.lookup(query)?.is_empty()
        && let Some(forms) = word_forms.candidates(languages, query)
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
        // Files may have changed; reopen from the new catalog. Word-form
        // packs reopen too, which retries one that couldn't be read.
        self.probes.clear();
        self.definitions = None;
        self.word_forms.reset();
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
                        dictionary.error = retry_read(
                            dictionary,
                            &cache,
                            self.failed_queries.get(&dictionary.id),
                            &mut self.word_forms,
                            current,
                        )
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
                let mut candidate = self.preferences.clone();
                if candidate.discover(&self.catalog.dictionaries) {
                    match preference_bytes(&candidate) {
                        Ok(_) => {
                            self.preferences = candidate;
                            if self.prefs_error.is_none() {
                                self.save();
                            }
                        }
                        Err(e) => self.warnings.push(format!("New dictionaries couldn't be added to sets. {e:#}")),
                    }
                }
                self.fresh = true;
                true
            }
            Err(_) => false,
        }
    }
    /// Writes the whole preference file, replacing a damaged one.
    fn save(&mut self) {
        self.save_error = preference_bytes(&self.preferences)
            .and_then(|bytes| atomic_bytes(&self.prefs_path, &bytes))
            .err()
            .map(|e| format!("{e:#}"));
        if self.save_error.is_none() {
            self.prefs_error = None;
        }
    }
    /// A size rejection leaves both the accepted settings and saved file alone.
    /// I/O failures still retain the accepted edit for a later flush retry.
    fn edit_preferences(&mut self, edit: Edit) -> anyhow::Result<()> {
        let mut candidate = self.preferences.clone();
        candidate.edit(edit)?;
        preference_bytes(&candidate)?;
        self.preferences = candidate;
        self.save();
        Ok(())
    }
    fn apply_set(&mut self, set: &str) {
        let set = self.preferences.set(set);
        self.active_set = set.id.clone();
        set.apply(&mut self.catalog.dictionaries);
    }
    /// Search every enabled index in set order, keeping only compact index
    /// hits; opening another dictionary later never repeats the search.
    ///
    /// Publish the first usable definition as soon as it is ready, while
    /// counting other dictionaries. Long searches also publish preparation
    /// progress after `PROGRESS_DELAY`; more matches alone wait for the
    /// final count to keep e-ink redraws few.
    fn lookup_session(&mut self, request: &Request, query: &str, book_language: &str, current: &impl Fn() -> bool) {
        use std::{cell::RefCell, time::Instant};
        let started = Instant::now();
        let languages = self.preferences.set(&self.active_set).form_languages(book_language, query);
        // Matches live only in the session; publications copy them.
        *request.mailbox.session.lock().unwrap() = Some(Session {
            query: query.into(),
            active_set: self.active_set.clone(),
            form_languages: languages.clone(),
            matches: Vec::new(),
            revision: request.revision,
        });
        let ids: Vec<_> = self.catalog.dictionaries.iter().filter(|d| !d.disabled).map(|d| d.id.clone()).collect();
        log::debug!("dictionary search started: set {}, {} enabled", self.active_set, ids.len());
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
        let publish_snapshot = |first_definition: bool| {
            let valid = current();
            if valid && (first_definition || started.elapsed() >= PROGRESS_DELAY) {
                let snapshot = snapshot.borrow();
                let key = (snapshot.selected.clone(), snapshot.preparing.clone());
                if shown.borrow().as_ref() != Some(&key) {
                    publish(request, Reply { matches: matches(), ..snapshot.clone() }, current);
                    *shown.borrow_mut() = Some(key);
                }
            }
            valid
        };
        let progress = || publish_snapshot(false);
        // Generated on the first miss, once per query, so a pack opens only
        // when a dictionary lacks the exact word.
        let mut forms: Option<Option<forms::Candidates>> = None;
        if super::index::query_fits(query) {
            for id in ids {
                if !progress() {
                    return;
                }
                let preparing = || snapshot.borrow_mut().preparing = Some(id.clone());
                let outcome = self.open_probe(&id, &progress, &preparing).and_then(|probe| {
                    if !probe.displayable() {
                        return Ok(None);
                    }
                    probe.find(query, &progress).map(Some)
                });
                let outcome = match outcome {
                    Ok(Some(hits)) if hits.records.is_empty() => {
                        let forms = forms.get_or_insert_with(|| {
                            let generating = Instant::now();
                            let forms = self.word_forms.candidates(&languages, query);
                            log::debug!(
                                "dictionary query: languages {languages:?}, {} generated candidates in {:?}",
                                forms.as_ref().map_or(0, forms::Candidates::count),
                                generating.elapsed()
                            );
                            forms
                        });
                        match forms {
                            Some(forms) => {
                                let checking = Instant::now();
                                let hits = self
                                    .open_probe(&id, &progress, &preparing)
                                    .and_then(|probe| probe.find_forms(forms, &progress));
                                log::debug!("dictionary base forms checked: {:?}", checking.elapsed());
                                hits
                            }
                            None => Ok(hits),
                        }
                    }
                    Ok(hits) => Ok(hits.unwrap_or(Hits { records: vec![], forms: None })),
                    Err(error) => Err(error),
                };
                snapshot.borrow_mut().preparing = None;
                if !current() {
                    return;
                }
                let mut first_definition = false;
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
                                        log::debug!("dictionary first definition: {:?}", started.elapsed());
                                        let mut snapshot = snapshot.borrow_mut();
                                        snapshot.selected = Some(id.clone());
                                        snapshot.state = Some(definition);
                                        first_definition = true;
                                    }
                                    Err(error) if current() => self.read_error(&id, query, &languages, error),
                                    Err(_) => return,
                                }
                            }
                            if let Some(session) = request.mailbox.session.lock().unwrap().as_mut() {
                                session.matches.push(matched);
                            }
                        }
                    }
                    Err(error) => self.read_error(&id, query, &languages, error),
                }
                // Keep the snapshot's errors in step without cloning the catalog.
                let error = self.catalog.dictionaries.iter().find(|d| d.id == id).and_then(|d| d.error.clone());
                if let Some(d) = snapshot.borrow_mut().dictionaries.iter_mut().find(|d| d.id == id) {
                    d.error = error;
                }
                if first_definition && !publish_snapshot(true) {
                    return;
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
        log::debug!(
            "dictionary search complete: {:?}, {} matches, set {}",
            started.elapsed(),
            reply.matches.len(),
            self.active_set
        );
        publish(request, reply, current);
    }
    /// Opens (building if needed) the index of every dictionary that some
    /// set searches, and the word-form packs. Failures are left for the
    /// next lookup to find and report.
    fn prepare(&mut self, current: &impl Fn() -> bool) {
        let started = std::time::Instant::now();
        let searched =
            |id: &str| self.preferences.sets.iter().any(|s| s.dictionaries.iter().any(|p| p.id == id && !p.disabled));
        let ids: Vec<_> = self
            .catalog
            .dictionaries
            .iter()
            .filter(|d| d.error.is_none() && !self.probes.contains_key(&d.id) && searched(&d.id))
            .map(|d| d.id.clone())
            .collect();
        let mut opened = 0;
        for id in ids {
            if !current() {
                return;
            }
            match self.open_probe(&id, current, &|| {}) {
                Ok(_) => opened += 1,
                Err(e) => log::debug!("dictionary prepare: {id}: {e:#}"),
            }
        }
        if !current() {
            return;
        }
        self.word_forms.status();
        log::info!("dictionary prepare: {opened} indexes opened in {:?}", started.elapsed());
    }
    #[cfg(test)]
    fn lookup(&mut self, query: &str, current: &impl Fn() -> bool) -> (Option<String>, State) {
        let mailbox = Arc::new(Mailbox {
            result: Mutex::new(None),
            ready: Condvar::new(),
            session: Mutex::new(None),
            latest: AtomicU64::new(0),
            waker: Waker::new(|| {}),
        });
        let request = Request { id: 0, mailbox: mailbox.clone(), revision: 0 };
        self.lookup_session(&request, query, "", current);
        let Some(reply) = mailbox.result.lock().unwrap().take() else { return (None, State::NoResult) };
        (reply.selected, reply.state.unwrap())
    }
    fn path(&self, id: &str) -> anyhow::Result<std::path::PathBuf> {
        let dictionary = self.catalog.dictionaries.iter().find(|d| d.id == id);
        Ok(dictionary.ok_or_else(|| anyhow::anyhow!("Dictionary disappeared"))?.path.clone())
    }
    /// The dictionary's open index, opening (and if needed building) it once.
    fn open_probe(
        &mut self,
        id: &str,
        current: &impl Fn() -> bool,
        preparing: &impl Fn(),
    ) -> anyhow::Result<&mut Probe> {
        if !self.probes.contains_key(id) {
            let probe = Probe::open_progress(&self.path(id)?, &self.store.dictionary_cache(), current, preparing)?;
            self.probes.insert(id.into(), probe);
        }
        Ok(self.probes.get_mut(id).unwrap())
    }
    fn render_match(&mut self, matched: &Match, query: &str, current: &impl Fn() -> bool) -> anyhow::Result<State> {
        let id = &matched.dictionary;
        self.open_probe(id, current, &|| {})?;
        if self.definitions.as_ref().is_none_or(|(open, _)| open != id) {
            self.definitions = None;
            self.definitions = Some((id.clone(), super::open_definitions(&self.path(id)?)?));
        }
        let definitions = &mut self.definitions.as_mut().unwrap().1;
        let entries = self.probes[id].render(definitions, matched.hits.records.clone())?;
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
    /// Closes the dictionary's files, so its next use reopens and revalidates them.
    fn close(&mut self, id: &str) {
        self.probes.remove(id);
        if self.definitions.as_ref().is_some_and(|(open, _)| open == id) {
            self.definitions = None;
        }
    }
    fn read_error(&mut self, id: &str, query: &str, form_languages: &[String], error: anyhow::Error) {
        self.close(id);
        self.failed_queries.insert(id.into(), (query.into(), form_languages.to_vec()));
        if let Some(d) = self.catalog.dictionaries.iter_mut().find(|d| d.id == id) {
            d.error = Some(format!("{error:#}"));
        }
    }

    /// A catalog reply: everything management shows, including the per-file
    /// preparation check that lookups never need.
    fn catalog_reply(&mut self, id: u64) -> Reply {
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
        let word_forms = self.word_forms.status();
        Reply { unprepared, missing, word_forms, ..self.reply(id, None, None) }
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
            word_forms: vec![],
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
        let sent = Arc::new(AtomicU64::new(0));
        let sent_seen = sent.clone();
        // Do not retain Store's service registry from its own thread.
        let store = store.dictionary_context();
        std::thread::Builder::new()
            .name("verso-dictionary".into())
            .spawn(move || {
                // Even preference reads wait for an actual request.
                let Ok(first) = commands.recv() else { return };
                let prefs_path = store.dictionary_preferences();
                let word_forms = WordForms::new(store.wordforms_dir());
                let mut inner = Inner {
                    store,
                    prefs_path,
                    preferences: Preferences::default(),
                    folders_seen: 0,
                    active_set: DEFAULT.into(),
                    catalog: Catalog::default(),
                    warnings: Vec::new(),
                    fresh: false,
                    probes: Default::default(),
                    definitions: None,
                    save_error: None,
                    failed_queries: Default::default(),
                    word_forms,
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
                                        inner.read_error(&dictionary, &session.query, &session.form_languages, error);
                                        State::Error(UNREADABLE.into())
                                    }
                                    Err(_) => continue,
                                };
                                let mut reply = inner.reply(request.id, Some(dictionary), Some(state));
                                reply.matches = session.matches;
                                publish(&request, reply, &current);
                            }
                        }
                        // Any command sent after it moves the send count on and stops it.
                        Command::Prepare(ticket) => {
                            let current = || alive() && sent_seen.load(Ordering::Relaxed) == ticket;
                            if (!inner.fresh || folders_changed(&inner)) && !inner.discover(&current) {
                                continue;
                            }
                            inner.prepare(&current);
                        }
                        Command::Release => {
                            // A queued request from another client reopens what it needs.
                            // Open indexes stay: they are file handles, and the next
                            // lookup would otherwise reopen every one.
                            inner.definitions = None;
                        }
                        Command::Close { id, done } => {
                            inner.close(&id);
                            inner.fresh = false;
                            let _ = done.send(());
                        }
                        Command::Rescan => {
                            inner.discover(&alive);
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
                                inner.edit_preferences(edit).err().map(|e| e.to_string())
                            };
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
                            inner.close(&dictionary);
                            if let Some(d) = inner.catalog.dictionaries.iter_mut().find(|d| d.id == dictionary) {
                                let result = retry_read(
                                    d,
                                    &inner.store.dictionary_cache(),
                                    inner.failed_queries.get(&dictionary),
                                    &mut inner.word_forms,
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
                            inner.lookup_session(&request, &query, &selection.language, &current);
                        }
                    }
                }
            })
            .expect("spawn dictionary worker");
        Self { tx, revision, stopped, folders, sent }
    }
    fn send(&self, command: Command) {
        if !matches!(command, Command::Prepare(_)) {
            self.sent.fetch_add(1, Ordering::Relaxed);
        }
        let _ = self.tx.send(command);
    }
    /// Closes any open index or definitions file for `id` and waits until the
    /// worker thread confirms they are dropped (before deleting on FUSE).
    pub(crate) fn close_sync(&self, id: &str) {
        self.revision.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.send(Command::Close { id: id.into(), done: tx });
        let _ = rx.recv();
    }
    /// Triggers a catalog rescan on the worker thread without tying it to a
    /// short-lived client mailbox.
    pub(crate) fn rescan(&self) {
        self.revision.fetch_add(1, Ordering::Relaxed);
        self.send(Command::Rescan);
    }
}
fn publish(request: &Request, reply: Reply, current: &dyn Fn() -> bool) {
    if current() {
        *request.mailbox.result.lock().unwrap() = Some(reply);
        request.mailbox.ready.notify_one();
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
                ready: Condvar::new(),
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
        self.service.send(Command::Lookup { request, query, selection, rescan });
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
        self.service.send(Command::Catalog { request, set: set.into(), rescan });
        id
    }
    pub fn edit(&self, edit: Edit) -> u64 {
        self.service.revision.fetch_add(1, Ordering::Relaxed);
        let request = self.request();
        let id = request.id;
        self.service.send(Command::Edit { request, edit });
        id
    }
    pub fn retry(&self, set: &str, dictionary: &str) -> u64 {
        self.service.revision.fetch_add(1, Ordering::Relaxed);
        let request = self.request();
        let id = request.id;
        self.service.send(Command::Retry { request, set: set.into(), dictionary: dictionary.into() });
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
        self.service.send(Command::Open { request, dictionary: dictionary.into() });
    }
    /// Opens the indexes a lookup will search, in the background, so the
    /// first lookup in a book is as quick as the ones after it. Any request
    /// from any client stops it.
    pub fn prepare(&self) {
        let ticket = self.service.sent.load(Ordering::Relaxed);
        let _ = self.service.tx.send(Command::Prepare(ticket));
    }
    pub fn receive(&self) -> Option<Reply> {
        self.mailbox.result.lock().unwrap().take().filter(|r| r.id == self.mailbox.latest.load(Ordering::Relaxed))
    }
    /// Give a first draw a short chance to include an already nearly ready
    /// definition. The worker signals this condition when it publishes. A
    /// reply left by an older request doesn't end the wait.
    pub fn receive_wait(&self, timeout: std::time::Duration) -> Option<Reply> {
        let latest = || self.mailbox.latest.load(Ordering::Relaxed);
        let result = self.mailbox.result.lock().unwrap();
        let (mut result, _) = self
            .mailbox
            .ready
            .wait_timeout_while(result, timeout, |reply| reply.as_ref().is_none_or(|r| r.id != latest()))
            .unwrap();
        result.take().filter(|r| r.id == latest())
    }
    #[cfg(test)]
    pub fn result_for_test(&self, reply: Reply) {
        *self.mailbox.result.lock().unwrap() = Some(reply);
        self.mailbox.ready.notify_one();
    }
    pub fn cancel(&self) {
        self.mailbox.latest.fetch_add(1, Ordering::Relaxed);
        *self.mailbox.result.lock().unwrap() = None;
        *self.mailbox.session.lock().unwrap() = None;
        self.service.send(Command::Release);
    }
    pub fn flush(&self) -> anyhow::Result<()> {
        self.cancel();
        let (tx, rx) = mpsc::channel();
        self.service.send(Command::Flush(tx));
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
