//! One coalescing library worker. Progress/errors and index content are published
//! separately: status is small and always delivered, while an index is cloned,
//! published and saved only after its content actually changed.
use super::collections::{self, Collections, Navigation};
use super::home::Summary;
use super::previews;
use super::{Book, Cover, LibraryIndex, Metadata, PathChange, Stamp, assign_ids, covers, epub};
use crate::platform::Waker;
use crate::store::{BookState, FolderPreview, Store, book_id};
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, UNIX_EPOCH};

/// Scan and cover progress plus errors. Carries no index and is delivered even
/// when the library content is unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub scanning: bool,
    pub working: bool,
    pub errors: Vec<String>,
}
impl Default for Status {
    /// Matches the UI's initial state: a launch always starts with a pass.
    fn default() -> Self {
        Self { scanning: true, working: true, errors: Vec::new() }
    }
}
/// A changed library index. `partial` marks a scan checkpoint whose metadata or
/// folders may still be incomplete.
#[derive(Clone, Default)]
pub struct Snapshot {
    pub index: LibraryIndex,
    pub partial: bool,
    pub revision: u64,
    /// The Collections hierarchy of `index`, built here instead of on the UI
    /// thread, with the folder list it was built for.
    pub collections: Option<(Vec<PathBuf>, Arc<Collections>)>,
    /// The index as saved before this launch, unchanged. The first pass
    /// publishes it so browsing needn't read `library.json` on the UI thread.
    pub cached: bool,
}
/// Above this many uncached books, one directory listing replaces failed opens
/// of books never read; below it, single reads are cheaper.
pub(crate) const LISTED_STATES: usize = 32;

/// Input for preparing the remembered browsing page off the UI thread.
#[derive(Clone, Debug)]
pub struct WarmRequest {
    pub seq: u64,
    pub navigation: Navigation,
    pub per_page: usize,
    pub folder_preview: FolderPreview,
    /// True when the screen has not yet loaded all saved reading states.
    pub need_states: bool,
    /// Already-loaded `opened.json` preview ranks, if any.
    pub known_ranks: Option<Arc<HashMap<String, u32>>>,
}

/// One tile on the prepared browsing page: only what the UI needs to compute
/// `CoverSpec`s for `verso-thumb`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WarmTile {
    Folder {
        parent_line: bool,
        /// Preview picks (up to `folder_preview.slots()`), in pick order.
        picks: Vec<(String, Cover)>,
    },
    Book {
        id: String,
        cover: Cover,
    },
}

/// Result of preparing the remembered browsing page off the UI thread.
pub struct WarmResult {
    pub seq: u64,
    pub page: usize,
    pub total_items: usize,
    pub tiles: Vec<WarmTile>,
    /// Saved states read from disk. When `states_complete` is true, every
    /// existing state record in `books/` was read and absent ids have default
    /// state.
    pub states: Option<HashMap<String, BookState>>,
    pub states_complete: bool,
    /// `opened.json` ranks when read off-thread for folder previews.
    pub opened_ranks: Option<Arc<HashMap<String, u32>>>,
    pub reads: usize,
    pub listed: bool,
    pub elapsed: Duration,
}

#[derive(Default)]
struct Shared {
    stop: bool,
    requested: bool,
    retry: bool,
    paused: bool,
    /// Number of active [`CoverPause`] guards (e.g. Wi-Fi transfer open).
    covers_paused: usize,
    priority: Vec<String>,
    latest: Option<Snapshot>,
    /// The worker's current index, readable without emptying `latest`.
    current: Option<Arc<LibraryIndex>>,
    status: Option<Status>,
    home_requested: bool,
    latest_home: Option<(Summary, u64)>,
    /// `opened.json` was rebuilt with a different order.
    opened_changed: bool,
    /// Book ids whose visible thumbnail failed to load, and those since repaired.
    repairs: Vec<String>,
    repaired: Vec<String>,
    /// Queued index edits: `(revision, change, rescan)`.
    changes: Vec<(u64, PathChange, bool)>,
    revision: u64,
    /// Highest `revision` applied to `current` and persisted.
    applied_revision: u64,
    /// A new folder list from Settings, used from the next pass.
    roots: Option<Vec<PathBuf>>,
    /// Cover sizes the library screen drew since the worker last looked.
    drawn_sizes: Vec<(u32, u32)>,
    /// Pending Home warm-up request and its completed result.
    warm_request: Option<WarmRequest>,
    warm_result: Option<WarmResult>,
}

#[derive(Default)]
struct SyncState {
    state: Mutex<Shared>,
    wake: Condvar,
    /// Notified whenever `current` becomes ready, `applied_revision` advances,
    /// or the worker stops.
    applied: Condvar,
}
const REPAIR_LIMIT: usize = 32;
/// A page of book covers plus a page of folder previews (8 tiles × 4) and
/// the Continue cover.
const PRIORITY_LIMIT: usize = 48;
const ERROR_LIMIT: usize = 32;
/// Real cover changes are batched into one index publication per interval.
const PUBLISH_INTERVAL: Duration = Duration::from_millis(250);
/// Changed books between index checkpoints, so an interrupted pass keeps progress.
const SAVE_EVERY: usize = 64;

fn cover_key(book: &Book) -> String {
    match &book.cover {
        Cover::Ready(key) | Cover::Unavailable(key) => key.clone(),
        Cover::Pending => covers::key(book),
    }
}
/// Cover keys the old index used that no book of the new one does: a changed
/// file or a reassigned identity gets a new key and strands the old files.
fn stale_cover_keys(old: &LibraryIndex, new: &LibraryIndex) -> Vec<String> {
    let kept: HashSet<String> = new.books.iter().flat_map(|b| [cover_key(b), covers::key(b)]).collect();
    let stale: HashSet<String> = old.books.iter().map(cover_key).filter(|k| !kept.contains(k)).collect();
    stale.into_iter().collect()
}
fn push_error(errors: &mut Vec<String>, error: String) {
    if errors.len() < ERROR_LIMIT {
        errors.push(error);
    }
}

/// The worker thread's side of the shared state.
struct Channel {
    shared: Arc<SyncState>,
    waker: Waker,
}
impl Channel {
    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared.state.lock().unwrap()
    }
    fn wait<'a>(&self, state: MutexGuard<'a, Shared>) -> MutexGuard<'a, Shared> {
        self.shared.wake.wait(state).unwrap()
    }
    fn notify_applied(&self) {
        self.shared.applied.notify_all();
    }
    fn stopped(&self) -> bool {
        self.lock().stop
    }
    /// Scan reads block while a book is open; true on shutdown or queued edits.
    fn yield_point(&self) -> bool {
        let mut state = self.lock();
        while state.paused && !state.stop && state.changes.is_empty() {
            state = self.wait(state);
        }
        if !state.changes.is_empty() {
            // A scan was interrupted before completing; run it again after the
            // queued changes are applied.
            state.requested = true;
        }
        state.stop || !state.changes.is_empty()
    }
    /// Fingerprint reads also yield while cover work is paused (Wi-Fi transfer)
    /// and never escalate a non-scanning edit into a full scan pass.
    fn fingerprint_yield(&self) -> bool {
        let mut state = self.lock();
        while (state.paused || state.covers_paused > 0) && !state.stop && !state.requested && state.changes.is_empty() {
            state = self.wait(state);
        }
        if state.changes.iter().any(|(_, _, rescan)| *rescan) {
            state.requested = true;
        }
        state.stop || state.requested || !state.changes.is_empty()
    }
    /// Cover and identity work stop for a new scan request or queued edits.
    fn interrupted(&self) -> bool {
        self.fingerprint_yield()
    }
    fn content(&self, snapshot: Snapshot) {
        let current = Arc::new(snapshot.index.clone());
        let mut state = self.lock();
        state.current = Some(current);
        // A replaced, unconsumed publication already has a pending wake.
        let wake = state.latest.replace(snapshot).is_none();
        drop(state);
        self.notify_applied();
        if wake {
            self.waker.wake();
        }
    }
    fn status(&self, status: Status) {
        if self.lock().status.replace(status).is_none() {
            self.waker.wake();
        }
    }
    fn home(&self, summary: Summary, revision: u64) {
        self.lock().latest_home = Some((summary, revision));
        self.waker.wake();
    }
    /// The Home rebuild returns whether it changed the opened order.
    fn rebuilt(&self, (summary, opened_changed): (Summary, bool), revision: u64) {
        if opened_changed {
            self.lock().opened_changed = true;
        }
        self.home(summary, revision);
    }
    fn take_home_request(&self) -> bool {
        std::mem::take(&mut self.lock().home_requested)
    }
    fn take_warm_request(&self) -> Option<WarmRequest> {
        let mut state = self.lock();
        if state.paused || state.stop || !state.changes.is_empty() {
            return None;
        }
        state.warm_request.take()
    }
    fn warm(&self, result: WarmResult) {
        if self.lock().warm_result.replace(result).is_none() {
            self.waker.wake();
        }
    }
}

enum Next {
    Interrupted,
    Repairs,
    Cover(usize),
}

struct Background {
    channel: Channel,
    store: Store,
    roots: Vec<PathBuf>,
    /// The sizes tile thumbnails are kept at, most recently drawn first.
    tile_sizes: Vec<(u32, u32)>,
    index: LibraryIndex,
    /// Content changes not yet delivered to the UI / written to `library.json`.
    unpublished: bool,
    unsaved: bool,
    /// Displayed metadata or covers changed since the Home summary was refreshed.
    home_stale: bool,
    /// Pending cover generation (from added books, new tile sizes, or an
    /// interrupted cover pass), run once cover work isn't paused, without a scan.
    covers_needed: bool,
    /// The last status sent to the UI; identical updates are not resent.
    status: Status,
    last_errors: Vec<String>,
    /// Each cover key is repaired at most once per session; repeated failures
    /// keep the title cover instead of looping.
    attempted: HashSet<String>,
    startup: Instant,
    revision: u64,
    /// Whether any index went to the UI yet.
    published: bool,
    /// The last hierarchy built, keyed by a hash of what it depends on, so
    /// cover and metadata changes reuse it.
    collections: Option<(u64, Arc<Collections>)>,
    /// Roots changed via Settings: refresh Home's cached library counts even if
    /// the book index itself is unchanged.
    roots_changed: bool,
}
impl Background {
    fn run(mut self) {
        {
            self.channel.lock().current = Some(Arc::new(self.index.clone()));
            self.channel.notify_applied();
        }
        if self.store.home_summary().is_none_or(|summary| !summary.complete)
            || self.store.opened().is_none_or(|opened| !opened.complete)
        {
            self.rebuild_home();
        }
        covers::sweep_temps(&self.store);
        self.tile_sizes = covers::tile_sizes(&self.store);
        loop {
            let (retry, scan, home, run_covers, repairs) = {
                let mut state = self.channel.lock();
                while !state.stop
                    && !state.requested
                    && !state.home_requested
                    && state.changes.is_empty()
                    && state.drawn_sizes.is_empty()
                    && ((state.paused || state.covers_paused > 0) || (!self.covers_needed && state.repairs.is_empty()))
                    && (state.paused || state.warm_request.is_none())
                {
                    state = self.channel.wait(state);
                }
                if state.stop {
                    break;
                }
                let changes = std::mem::take(&mut state.changes);
                if let Some(roots) = state.roots.take()
                    && roots != self.roots
                {
                    self.roots = roots;
                    self.roots_changed = true;
                }
                let drawn = std::mem::take(&mut state.drawn_sizes);
                drop(state);
                if self.use_tile_sizes(&drawn) {
                    self.covers_needed = true;
                }
                let needs_scan = changes.iter().any(|(_, _, rescan)| *rescan);
                self.change_paths(&changes);
                let mut state = self.channel.lock();
                let covers_ok = !state.paused && state.covers_paused == 0;
                let scan = std::mem::take(&mut state.requested) || needs_scan;
                let run_covers = !scan && covers_ok && std::mem::take(&mut self.covers_needed);
                let repairs = !scan && !run_covers && covers_ok && !state.repairs.is_empty();
                (std::mem::take(&mut state.retry), scan, std::mem::take(&mut state.home_requested), run_covers, repairs)
            };
            if home {
                self.rebuild_home();
            }
            if scan {
                if !self.pass(retry) {
                    break;
                }
            } else if run_covers {
                // Missing covers (from added books or a new tile size), without a scan.
                let mut errors = self.last_errors.clone();
                self.set_status(false, true, &errors);
                let mut interrupted = self.covers(&mut errors);
                if !interrupted {
                    // Identity work a scan-free edit interrupted resumes here.
                    interrupted = self.fingerprint_books(&mut errors).0;
                }
                if interrupted && !self.channel.stopped() {
                    self.covers_needed = true;
                }
                self.save(&mut errors, "covers");
                if std::mem::take(&mut self.home_stale) {
                    self.refresh_home_metadata();
                }
                self.publish();
                self.set_status(false, interrupted && !self.channel.stopped(), &errors);
                self.last_errors = errors;
            } else if repairs {
                // Outside a pass, only an actual cover change is saved and published.
                let mut errors = self.last_errors.clone();
                if self.repair_requested(&mut errors).1 {
                    self.save(&mut errors, "covers");
                    self.refresh_home_metadata();
                    self.publish();
                }
                self.set_status(false, false, &errors);
                self.last_errors = errors;
            } else if let Some(request) = self.channel.take_warm_request() {
                self.warm(request);
            }
        }
        // File operations already happened on the UI thread. Persist their
        // queued index edits even when shutdown interrupted the current pass.
        let changes = std::mem::take(&mut self.channel.lock().changes);
        self.change_paths(&changes);
        self.channel.notify_applied();
    }

    /// One scan, cover and identity pass. Returns false on shutdown.
    fn pass(&mut self, retry: bool) -> bool {
        let mut errors = Vec::new();
        self.set_status(true, true, &errors);
        let mut checkpoint_index = None;
        let result = {
            let (channel, store, roots, built) = (&self.channel, &self.store, &self.roots, &mut self.collections);
            // Checkpoints happen only when the scan found new files to read.
            scan_roots(roots, &self.index, retry, &|| channel.yield_point(), |checkpoint| {
                if let Err(e) = store.save_library(checkpoint) {
                    push_error(&mut errors, format!("Couldn't save library: {e}"));
                }
                channel.content(Snapshot {
                    index: checkpoint.clone(),
                    partial: true,
                    revision: self.revision,
                    collections: Some((roots.clone(), hierarchy(built, checkpoint, roots))),
                    cached: false,
                });
                checkpoint_index = Some(checkpoint.clone());
            })
        };
        let checkpointed = checkpoint_index.is_some();
        self.published |= checkpointed;
        let Some(scan) = result else {
            // A folder edit or shutdown cancelled the scan after it showed new
            // books. Keep that index, which is already saved and published, so
            // the edit applies to it and the restarted scan continues from it
            // instead of hiding the books and reading them all again.
            if let Some(checkpoint) = checkpoint_index {
                self.index = checkpoint;
            }
            return !self.channel.stopped();
        };
        log::debug!("scan completed with {} metadata reads", scan.metadata_reads);
        errors.extend(scan.errors);
        if checkpointed || self.index != scan.index || std::mem::take(&mut self.roots_changed) {
            if checkpointed || self.index != scan.index {
                let stale = stale_cover_keys(&self.index, &scan.index);
                self.index = scan.index;
                for key in stale {
                    covers::remove(&self.store, &key, &self.tile_sizes);
                }
                self.unsaved = true;
                self.unpublished = true;
            }
            self.home_stale = false;
            self.rebuild_home();
        }
        self.save(&mut errors, "library");
        self.publish();
        self.set_status(false, true, &errors);
        if let Some(request) = self.channel.take_warm_request() {
            self.warm(request);
        }

        let mut interrupted = self.covers(&mut errors);
        if interrupted && !self.channel.stopped() {
            self.covers_needed = true;
        } else if !interrupted {
            self.covers_needed = false;
        }

        let identity_started = Instant::now();
        // Fingerprinting new additions is deferred until metadata and covers are
        // visible. Only possible moves need it before identity assignment. Identity
        // is persisted but not displayed, so it changes no UI publication.
        let mut fingerprinted = 0;
        if !interrupted {
            (interrupted, fingerprinted) = self.fingerprint_books(&mut errors);
        }
        if self.channel.stopped() {
            return false;
        }
        if interrupted {
            // A scan edit restarts the pass at once. A scan-free edit does not, so
            // leave the remaining identity work to the idle cover branch.
            self.covers_needed = true;
            self.publish();
            self.set_status(false, true, &errors);
            return true;
        }
        log::info!("library identities: {fingerprinted} fingerprinted in {:?}", identity_started.elapsed());
        self.save(&mut errors, "library");
        if std::mem::take(&mut self.home_stale) {
            self.refresh_home_metadata();
        }
        self.publish();
        self.set_status(false, false, &errors);
        self.last_errors = errors;
        log::info!("startup stage: background complete elapsed_us={}", self.startup.elapsed().as_micros());
        true
    }

    /// Hashes books whose identity is still unattempted. Returns whether
    /// shutdown or a queued edit interrupted it, and how many were hashed.
    fn fingerprint_books(&mut self, errors: &mut Vec<String>) -> (bool, usize) {
        let mut fingerprinted = 0;
        let mut interrupted = false;
        for i in 0..self.index.books.len() {
            if let Some(request) = self.channel.take_warm_request() {
                self.warm(request);
            }
            if interrupted || self.channel.interrupted() {
                interrupted = true;
                break;
            }
            let book = &self.index.books[i];
            if book.available && book.fingerprint.is_empty() && !book.fingerprint_attempted {
                let channel = &self.channel;
                let result = fingerprint(&book.path, &|| channel.fingerprint_yield());
                // Shutdown and queued folder edits cancel the read. Leave the
                // book unattempted so the restarted pass hashes it again.
                if result.as_ref().is_err_and(|e| e.is::<Cancelled>()) {
                    interrupted = true;
                    break;
                }
                match result {
                    Ok(fingerprint) => {
                        let book = &mut self.index.books[i];
                        book.fingerprint = fingerprint;
                        book.fingerprint_attempted = true;
                        self.unsaved = true;
                        fingerprinted += 1;
                        if fingerprinted % SAVE_EVERY == 0 {
                            self.save(errors, "library identity");
                        }
                    }
                    Err(e) => push_error(errors, format!("Couldn't fingerprint {}: {e}", book.path.display())),
                }
            }
        }
        (interrupted, fingerprinted)
    }

    /// Moves the sizes the library drew covers at to the front of the kept
    /// list, dropping the least recent beyond [`covers::TILE_SIZES`] with
    /// their files. True when a size is new and its thumbnails are needed.
    fn use_tile_sizes(&mut self, drawn: &[(u32, u32)]) -> bool {
        let mut sizes = self.tile_sizes.clone();
        let mut added = false;
        for &size in drawn.iter().filter(|&&s| covers::tile_size(s)) {
            match sizes.iter().position(|&s| s == size) {
                Some(i) => {
                    sizes.remove(i);
                }
                None => added = true,
            }
            sizes.insert(0, size);
        }
        if sizes == self.tile_sizes {
            return false;
        }
        for &size in sizes.iter().skip(covers::TILE_SIZES) {
            log::info!("cover size {}x{}: no longer kept", size.0, size.1);
            covers::remove_tile_size(&self.store, size);
        }
        sizes.truncate(covers::TILE_SIZES);
        if let Err(e) = covers::save_tile_sizes(&self.store, &sizes) {
            log::warn!("saving cover sizes: {e:#}");
        }
        self.tile_sizes = sizes;
        added
    }

    /// Makes missing thumbnails, visible books first. True when a scan
    /// request, folder edit or shutdown interrupted it.
    fn covers(&mut self, errors: &mut Vec<String>) -> bool {
        let covers_started = Instant::now();
        let mut pending: BTreeSet<_> =
            self.index.books.iter().enumerate().filter(|(_, b)| b.available).map(|(i, _)| i).collect();
        let id_indices: HashMap<_, _> = self.index.books.iter().enumerate().map(|(i, b)| (b.id.clone(), i)).collect();
        let mut since_publish = Instant::now();
        let (mut confirmed, mut changed, mut unsaved) = (0, 0, 0);
        while !pending.is_empty() {
            if self.channel.take_home_request() {
                self.rebuild_home();
            }
            if let Some(request) = self.channel.take_warm_request() {
                self.warm(request);
            }
            let (repaired, repair_changed) = self.repair_requested(errors);
            for i in repaired {
                pending.remove(&i);
            }
            if repair_changed {
                self.changed();
                unsaved += 1;
            }
            let next = match self.next_cover(&pending, &id_indices) {
                Next::Interrupted => return true,
                Next::Repairs => continue,
                Next::Cover(i) => i,
            };
            pending.remove(&next);
            match covers::generate(&self.index.books[next], &self.store, &self.tile_sizes) {
                Ok(cover) if cover != self.index.books[next].cover => {
                    self.index.books[next].cover = cover;
                    self.changed();
                    changed += 1;
                    unsaved += 1;
                }
                Ok(_) => confirmed += 1,
                Err(e) => push_error(errors, format!("Couldn't cache cover: {e}")),
            }
            if unsaved >= SAVE_EVERY {
                self.save(errors, "covers");
                unsaved = 0;
            }
            if self.unpublished && since_publish.elapsed() >= PUBLISH_INTERVAL {
                self.publish();
                since_publish = Instant::now();
            }
            self.set_status(false, true, errors);
        }
        log::info!("library covers: {confirmed} confirmed, {changed} changed in {:?}", covers_started.elapsed());
        false
    }

    fn next_cover(&self, pending: &BTreeSet<usize>, id_indices: &HashMap<String, usize>) -> Next {
        let mut state = self.channel.lock();
        while (state.paused || state.covers_paused > 0) && !state.stop && !state.requested && state.changes.is_empty() {
            state = self.channel.wait(state);
        }
        // Removing or adding shifts book indices, so stop this cover pass. Only
        // edits that asked for a scan pass trigger one.
        if !state.changes.is_empty() {
            if state.changes.iter().any(|(_, _, rescan)| *rescan) {
                state.requested = true;
            }
            return Next::Interrupted;
        }
        if state.stop || state.requested {
            return Next::Interrupted;
        }
        if !state.repairs.is_empty() {
            return Next::Repairs;
        }
        let visible = state.priority.iter().filter_map(|id| id_indices.get(id).copied()).find(|i| pending.contains(i));
        Next::Cover(visible.unwrap_or_else(|| *pending.first().unwrap()))
    }

    fn change_paths(&mut self, changes: &[(u64, PathChange, bool)]) {
        if changes.is_empty() {
            return;
        }
        let old_keys = changes
            .iter()
            .any(|(_, c, _)| matches!(c, PathChange::Add(_)))
            .then(|| self.index.books.iter().map(cover_key).collect::<HashSet<_>>());
        for (revision, change, _) in changes {
            match change {
                PathChange::Remove(path) => {
                    for book in self.index.books.iter().filter(|b| b.path.starts_with(path)) {
                        if let Err(e) = self.store.remove_book(&book.id) {
                            log::warn!("removing deleted reading state: {e}");
                        }
                        covers::remove(&self.store, &cover_key(book), &self.tile_sizes);
                    }
                }
                PathChange::Add(_) => {
                    self.covers_needed = true;
                }
                PathChange::Move(_, _) | PathChange::AddEmpty(_) => {}
            }
            change.apply(&mut self.index);
            self.revision = *revision;
        }
        if let Some(old_keys) = old_keys {
            let kept: HashSet<String> = self.index.books.iter().flat_map(|b| [cover_key(b), covers::key(b)]).collect();
            for key in old_keys.into_iter().filter(|k| !kept.contains(k)) {
                covers::remove(&self.store, &key, &self.tile_sizes);
            }
        }
        self.unsaved = true;
        self.unpublished = true;
        let mut errors = Vec::new();
        self.save(&mut errors, "library");
        if !errors.is_empty() {
            errors.iter().for_each(|e| log::warn!("{e}"));
            let mut all = self.status.errors.clone();
            for e in errors {
                if !all.contains(&e) {
                    push_error(&mut all, e);
                }
            }
            self.set_status(self.status.scanning, self.status.working, &all);
        }
        // This also runs during shutdown; stop between records then.
        let counts = hierarchy(&mut self.collections, &self.index, &self.roots).top_level_counts(&self.roots);
        let channel = &self.channel;
        match self.store.rebuild_home_summary_until(&self.index, counts, &|| channel.stopped()) {
            Ok(rebuilt) => self.channel.rebuilt(rebuilt, self.revision),
            Err(_) if self.channel.stopped() => {}
            Err(e) => log::warn!("rebuilding Home after file edits: {e:#}"),
        }
        self.publish();
        {
            let mut state = self.channel.lock();
            state.applied_revision = state.applied_revision.max(self.revision);
        }
        self.channel.notify_applied();
    }
    /// Records a displayed content change.
    fn changed(&mut self) {
        self.unsaved = true;
        self.unpublished = true;
        self.home_stale = true;
    }
    /// Sends a changed index to the UI. The first call sends it even when
    /// unchanged (`cached`), with its hierarchy, so the first visit to
    /// Collections or All books does no index work on the UI thread.
    fn publish(&mut self) {
        let changed = std::mem::take(&mut self.unpublished);
        if changed || !self.published {
            let started = Instant::now();
            let collections = hierarchy(&mut self.collections, &self.index, &self.roots);
            self.channel.content(Snapshot {
                index: self.index.clone(),
                partial: false,
                revision: self.revision,
                collections: Some((self.roots.clone(), collections)),
                cached: !changed && !self.published,
            });
            self.published = true;
            log::debug!("library published: {} books in {:?}", self.index.books.len(), started.elapsed());
        }
    }
    fn save(&mut self, errors: &mut Vec<String>, what: &str) {
        if !self.unsaved {
            return;
        }
        match self.store.save_library(&self.index) {
            Ok(()) => self.unsaved = false,
            Err(e) => push_error(errors, format!("Couldn't save {what}: {e}")),
        }
    }
    fn set_status(&mut self, scanning: bool, working: bool, errors: &[String]) {
        if self.status.scanning == scanning && self.status.working == working && self.status.errors == errors {
            return;
        }
        self.status = Status { scanning, working, errors: errors.to_vec() };
        self.channel.status(self.status.clone());
    }

    fn rebuild_home(&mut self) {
        let counts = hierarchy(&mut self.collections, &self.index, &self.roots).top_level_counts(&self.roots);
        let channel = &self.channel;
        match self.store.rebuild_home_summary_until(&self.index, counts, &|| channel.stopped()) {
            Ok(rebuilt) => channel.rebuilt(rebuilt, self.revision),
            Err(_) if channel.stopped() => {}
            Err(error) => log::warn!("rebuilding Home summary: {error:#}"),
        }
    }
    fn refresh_home_metadata(&mut self) {
        match self.store.refresh_home_metadata(&self.index) {
            Ok(Some(summary)) => self.channel.home(summary, self.revision),
            Ok(None) => self.rebuild_home(),
            Err(error) => log::warn!("refreshing Home metadata: {error:#}"),
        }
    }

    /// Prepares the remembered browsing page off the UI thread: loads saved
    /// reading states and `opened.json`, picks folder previews, and selects
    /// only the single page's books without cloning the whole index.
    fn warm(&mut self, request: WarmRequest) {
        let started = Instant::now();
        let cancelled = || {
            let state = self.channel.lock();
            state.paused || state.stop || !state.changes.is_empty()
        };
        if cancelled() {
            return;
        }
        let collections = hierarchy(&mut self.collections, &self.index, &self.roots);
        let mut navigation = request.navigation;
        navigation.folder = collections.restore_for(navigation.folder.as_deref(), &self.roots);
        let folders = collections.children(navigation.folder.as_deref());
        let folder_node = navigation.folder.as_ref().and_then(|p| collections.folders.get(p));
        let book_indices: Vec<usize> = folder_node.map_or_else(Vec::new, |f| f.indices.clone());

        let (mut states, mut states_complete, mut reads, mut listed) = (None, false, 0, false);
        if request.need_states && !book_indices.is_empty() {
            let Some((loaded, complete, r, l)) =
                load_warm_states(&self.store, &self.index.books, &book_indices, &cancelled)
            else {
                return;
            };
            states = Some(loaded);
            states_complete = complete;
            reads = r;
            listed = l;
        }

        let total_items = folders.len() + book_indices.len();
        let per_page = request.per_page.max(1);
        let pages = total_items.div_ceil(per_page).max(1);
        let page = navigation.page().min(pages - 1);
        let start = page * per_page;
        let end = (start + per_page).min(total_items);

        let page_folders_end = end.min(folders.len());
        let page_folders = if start < page_folders_end { &folders[start..page_folders_end] } else { &[] };
        let mut opened_ranks = None;
        let ranks_for_picks = if request.folder_preview != FolderPreview::Icon && !page_folders.is_empty() {
            if let Some(known) = request.known_ranks {
                Some(known)
            } else {
                if cancelled() {
                    return;
                }
                let opened = self.store.opened().unwrap_or_default();
                let ranks = Arc::new(opened.ranks());
                opened_ranks = Some(ranks.clone());
                Some(ranks)
            }
        } else {
            None
        };

        let shared_names: HashSet<String> = if navigation.folder.is_none() && !page_folders.is_empty() {
            let mut seen = HashSet::new();
            folders.iter().map(|p| collections::name(p)).filter(|name| !seen.insert(name.clone())).collect()
        } else {
            HashSet::new()
        };

        let mut tiles = Vec::with_capacity(end.saturating_sub(start));
        for path in page_folders {
            let parent_line = shared_names.contains(&collections::name(path));
            let picks = match &ranks_for_picks {
                Some(ranks) => previews::pick(&self.index, &collections, path, ranks)
                    .into_iter()
                    .take(request.folder_preview.slots())
                    .filter_map(|i| self.index.books.get(i))
                    .map(|b| (b.id.clone(), b.cover.clone()))
                    .collect(),
                None => Vec::new(),
            };
            tiles.push(WarmTile::Folder { parent_line, picks });
        }

        let b_start = start.saturating_sub(folders.len());
        let b_end = end.saturating_sub(folders.len());
        if b_start < b_end {
            let selected = super::select_page_indices(&self.index.books, book_indices, b_start, b_end);
            for i in selected {
                let book = &self.index.books[i];
                tiles.push(WarmTile::Book { id: book.id.clone(), cover: book.cover.clone() });
            }
        }

        if cancelled() {
            return;
        }
        self.channel.warm(WarmResult {
            seq: request.seq,
            page,
            total_items,
            tiles,
            states,
            states_complete,
            opened_ranks,
            reads,
            listed,
            elapsed: started.elapsed(),
        });
    }

    /// Returns the books processed and whether any cover changed.
    fn repair_requested(&mut self, errors: &mut Vec<String>) -> (Vec<usize>, bool) {
        let ids = std::mem::take(&mut self.channel.lock().repairs);
        if ids.is_empty() {
            return (Vec::new(), false);
        }
        let (repaired, changed) =
            repair_covers(&mut self.index, &ids, &self.store, &self.tile_sizes, &mut self.attempted, errors);
        if !repaired.is_empty() {
            self.channel.lock().repaired.extend(repaired.iter().map(|&i| self.index.books[i].id.clone()));
            self.channel.waker.wake();
        }
        (repaired, changed)
    }
}

/// Loads reading states off the UI thread. Above [`LISTED_STATES`] books, a
/// single `books/` directory listing reads only existing records (so unread
/// books never take map space) and returns `complete = true`. Stops early on
/// `cancelled`.
fn load_warm_states(
    store: &Store,
    books: &[Book],
    indices: &[usize],
    cancelled: &dyn Fn() -> bool,
) -> Option<(HashMap<String, BookState>, bool, usize, bool)> {
    let mut states = HashMap::new();
    let mut reads = 0;
    if indices.len() > LISTED_STATES
        && let Some(saved) = store.book_state_ids()
    {
        for id in saved {
            if cancelled() {
                return None;
            }
            reads += 1;
            states.insert(id.clone(), store.book(&id));
        }
        return Some((states, true, reads, true));
    }
    let mut seen = HashSet::new();
    for &i in indices {
        let id = &books[i].id;
        if !seen.insert(id.as_str()) {
            continue;
        }
        if cancelled() {
            return None;
        }
        reads += 1;
        states.insert(id.clone(), store.book(id));
    }
    Some((states, false, reads, false))
}

/// `index`'s Collections hierarchy under `roots`, reusing `cache` when no
/// book path, availability, empty folder or root changed.
fn hierarchy(cache: &mut Option<(u64, Arc<Collections>)>, index: &LibraryIndex, roots: &[PathBuf]) -> Arc<Collections> {
    let mut hasher = DefaultHasher::new();
    roots.hash(&mut hasher);
    for book in &index.books {
        (book.available, &book.path).hash(&mut hasher);
    }
    index.empty_folders.hash(&mut hasher);
    let key = hasher.finish();
    if let Some((cached, collections)) = cache
        && *cached == key
    {
        return collections.clone();
    }
    let collections = Arc::new(Collections::from_index(index, roots));
    *cache = Some((key, collections.clone()));
    collections
}

pub(crate) struct WorkerInner {
    shared: Arc<SyncState>,
    join: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for WorkerInner {
    fn drop(&mut self) {
        {
            let mut state = self.shared.state.lock().unwrap();
            state.stop = true;
            self.shared.wake.notify_all();
            self.shared.applied.notify_all();
        }
        if let Some(join) = self.join.lock().unwrap().take() {
            let _ = join.join();
        }
    }
}

/// Pauses background cover and fingerprint work while held (for example while
/// Wi-Fi transfer is open); dropping resumes it and wakes the worker so covers
/// for newly added books are generated without a scan pass.
pub struct CoverPause {
    shared: Arc<SyncState>,
}

impl Drop for CoverPause {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock().unwrap();
        state.covers_paused = state.covers_paused.saturating_sub(1);
        self.shared.wake.notify_all();
    }
}

#[derive(Clone)]
pub struct Worker {
    inner: Arc<WorkerInner>,
}

impl Worker {
    pub fn new(store: Store, roots: Vec<PathBuf>, waker: Waker) -> Self {
        let shared = Arc::new(SyncState {
            state: Mutex::new(Shared { requested: true, ..Shared::default() }),
            wake: Condvar::new(),
            applied: Condvar::new(),
        });
        let channel = Channel { shared: shared.clone(), waker };
        let join = std::thread::Builder::new()
            .name("verso-library".into())
            .spawn(move || {
                let startup = Instant::now();
                let index = store.library();
                Background {
                    channel,
                    store,
                    roots,
                    tile_sizes: Vec::new(),
                    index,
                    unpublished: false,
                    unsaved: false,
                    home_stale: false,
                    covers_needed: false,
                    status: Status::default(),
                    last_errors: Vec::new(),
                    attempted: HashSet::new(),
                    startup,
                    revision: 0,
                    published: false,
                    collections: None,
                    roots_changed: false,
                }
                .run()
            })
            .expect("start library thread");
        Self { inner: Arc::new(WorkerInner { shared, join: Mutex::new(Some(join)) }) }
    }
    pub(crate) fn weak(&self) -> Weak<WorkerInner> {
        Arc::downgrade(&self.inner)
    }
    pub(crate) fn from_inner(inner: Arc<WorkerInner>) -> Self {
        Self { inner }
    }
    fn state(&self) -> MutexGuard<'_, Shared> {
        self.inner.shared.state.lock().unwrap()
    }
    /// The worker's current index, readable without emptying the UI's `take()`
    /// mailbox. Waits briefly if the worker has not yet read `library.json`.
    pub fn current(&self) -> Arc<LibraryIndex> {
        let mut state = self.state();
        while state.current.is_none() && !state.stop {
            state = self.inner.shared.applied.wait(state).unwrap();
        }
        state.current.clone().unwrap_or_else(|| Arc::new(LibraryIndex::default()))
    }
    /// Pauses cover and fingerprint work until the returned guard is dropped.
    pub fn pause_covers(&self) -> CoverPause {
        let mut state = self.state();
        state.covers_paused += 1;
        self.inner.shared.wake.notify_all();
        CoverPause { shared: self.inner.shared.clone() }
    }
    /// Applies index edits directly without triggering a filesystem scan pass,
    /// and waits until the worker has applied, persisted and published them.
    pub fn apply_sync(&self, changes: Vec<PathChange>) -> u64 {
        let mut state = self.state();
        if changes.is_empty() {
            return state.applied_revision;
        }
        for change in changes {
            state.revision += 1;
            let revision = state.revision;
            state.changes.push((revision, change, false));
        }
        let target = state.revision;
        self.inner.shared.wake.notify_all();
        while state.applied_revision < target && !state.stop {
            state = self.inner.shared.applied.wait(state).unwrap();
        }
        target
    }
    /// Inserts one newly received book through `PathChange::Add` without a scan
    /// pass, returning the entry with its assigned id and annotation id.
    pub fn add_book(&self, book: Book) -> Option<Book> {
        let path = collections::normalize(&book.path);
        self.apply_sync(vec![PathChange::Add(Box::new(book))]);
        self.current().books.iter().find(|b| b.path == path && b.available).cloned()
    }
    /// A worker without its background thread: requests are recorded but
    /// nothing processes them, exactly like a worker that never catches up.
    /// For tests that exercise the browsing UI, not background scanning.
    #[cfg(test)]
    pub fn stub() -> Self {
        Self {
            inner: Arc::new(WorkerInner {
                shared: Arc::new(SyncState {
                    state: Mutex::new(Shared::default()),
                    wake: Condvar::new(),
                    applied: Condvar::new(),
                }),
                join: Mutex::new(None),
            }),
        }
    }
    /// Cover sizes the library drew, each once a session. The most recent
    /// [`covers::TILE_SIZES`] get thumbnails of their own, so paging decodes
    /// covers without resizing them.
    pub fn drew_cover_sizes(&self, sizes: Vec<(u32, u32)>) {
        let mut state = self.state();
        state.drawn_sizes.extend(sizes);
        self.inner.shared.wake.notify_all();
    }
    pub fn rescan(&self) {
        let mut state = self.state();
        state.requested = true;
        state.retry = true;
        self.inner.shared.wake.notify_all();
    }
    /// Scan a new folder list. A running pass stops at its next cover or
    /// identity check; books outside the new folders become unavailable but
    /// keep their entries, so adding a folder back restores them.
    pub fn set_roots(&self, roots: Vec<PathBuf>) {
        let mut state = self.state();
        state.roots = Some(roots);
        state.requested = true;
        self.inner.shared.wake.notify_all();
    }
    /// Recompute resume selection after finished/hidden flags change without a
    /// filesystem rescan. Authoritative book records stay on the save path.
    pub fn refresh_home(&self) {
        self.state().home_requested = true;
        self.inner.shared.wake.notify_all();
    }
    pub fn take_home(&self) -> Option<(Summary, u64)> {
        self.state().latest_home.take()
    }
    /// True once after a rebuild changed `opened.json`; read it again then.
    pub fn take_opened_changed(&self) -> bool {
        std::mem::take(&mut self.state().opened_changed)
    }
    /// While paused (a book is opening or open), scan, cover, warm-up and
    /// identity work stops at its next check: between files, covers or 64 KiB
    /// fingerprint reads.
    pub fn pause(&self, paused: bool) {
        let mut state = self.state();
        state.paused = paused;
        if paused {
            state.warm_request = None;
        }
        self.inner.shared.wake.notify_all();
    }
    /// Prepares the remembered browsing page off the UI thread. Replaces any
    /// earlier pending request or unconsumed result.
    pub fn warm(&self, request: WarmRequest) {
        let mut state = self.state();
        state.warm_request = Some(request);
        state.warm_result = None;
        self.inner.shared.wake.notify_all();
    }
    /// The finished Home warm-up, if any.
    pub fn take_warm(&self) -> Option<WarmResult> {
        self.state().warm_result.take()
    }
    pub fn change_paths(&self, changes: Vec<PathChange>) -> Vec<(u64, PathChange)> {
        let mut state = self.state();
        let mut queued = Vec::new();
        let mut rescan = false;
        for change in changes {
            let needs_scan = !matches!(change, PathChange::Add(_));
            rescan |= needs_scan;
            state.revision += 1;
            let rev = state.revision;
            state.changes.push((rev, change.clone(), needs_scan));
            queued.push((rev, change));
        }
        if rescan {
            // Wake/interrupt even a scan paused behind an open book.
            state.requested = true;
        }
        self.inner.shared.wake.notify_all();
        queued
    }
    pub fn move_paths(&self, paths: Vec<(PathBuf, PathBuf)>) -> Vec<(u64, PathChange)> {
        self.change_paths(paths.into_iter().map(|(from, to)| PathChange::Move(from, to)).collect())
    }
    pub fn remove_paths(&self, paths: Vec<PathBuf>) -> Vec<(u64, PathChange)> {
        self.change_paths(paths.into_iter().map(PathChange::Remove).collect())
    }
    pub fn add_empty_folder(&self, path: PathBuf) -> Vec<(u64, PathChange)> {
        self.change_paths(vec![PathChange::AddEmpty(path)])
    }
    pub fn prioritize(&self, ids: Vec<String>) {
        self.state().priority = ids.into_iter().take(PRIORITY_LIMIT).collect();
    }
    /// The newest changed index, if any.
    pub fn take(&self) -> Option<Snapshot> {
        self.state().latest.take()
    }
    /// The newest scan/cover status and errors, if they changed.
    pub fn take_status(&self) -> Option<Status> {
        self.state().status.take()
    }
    /// Request regeneration of thumbnails whose cache failed to load on screen.
    pub fn repair(&self, ids: impl IntoIterator<Item = String>) {
        let mut state = self.state();
        for id in ids {
            if state.repairs.len() < REPAIR_LIMIT && !state.repairs.contains(&id) {
                state.repairs.push(id);
            }
        }
        self.inner.shared.wake.notify_all();
    }
    /// Ids of repaired thumbnails; their visible load can be retried once.
    pub fn take_repaired(&self) -> Vec<String> {
        std::mem::take(&mut self.state().repaired)
    }
}

/// Returns the indices processed and whether a cover changed. Unknown,
/// unavailable and already attempted books are skipped without a reply, so a
/// persistent failure cannot loop.
fn repair_covers(
    index: &mut LibraryIndex,
    ids: &[String],
    store: &Store,
    tile_sizes: &[(u32, u32)],
    attempted: &mut HashSet<String>,
    errors: &mut Vec<String>,
) -> (Vec<usize>, bool) {
    let mut repaired = Vec::new();
    let mut changed = false;
    for id in ids {
        let Some(i) = index.books.iter().position(|b| &b.id == id && b.available) else { continue };
        if !attempted.insert(covers::key(&index.books[i])) {
            continue;
        }
        match covers::repair(&index.books[i], store, tile_sizes) {
            Ok(cover) if cover != index.books[i].cover => {
                index.books[i].cover = cover;
                changed = true;
            }
            Ok(_) => {}
            Err(e) => push_error(errors, format!("Couldn't cache cover: {e}")),
        }
        repaired.push(i);
    }
    (repaired, changed)
}

pub struct Scan {
    pub index: LibraryIndex,
    pub errors: Vec<String>,
    pub metadata_reads: usize,
}
pub(crate) fn stamp(meta: &std::fs::Metadata) -> Result<Stamp> {
    let modified = meta.modified()?.duration_since(UNIX_EPOCH).unwrap_or_default();
    Ok(Stamp { size: meta.len(), modified_secs: modified.as_secs(), modified_nanos: modified.subsec_nanos() })
}
/// A fingerprint read stopped by shutdown or a queued edit, not by the file.
#[derive(Debug)]
struct Cancelled;
impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("scan cancelled")
    }
}
impl std::error::Error for Cancelled {}

#[cfg(test)]
pub(crate) fn fingerprint_for_test(path: &Path) -> Result<String> {
    fingerprint(path, &|| false)
}

fn fingerprint(path: &Path, cancelled: &impl Fn() -> bool) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        if cancelled() {
            return Err(Cancelled.into());
        }
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn fingerprint_status(path: &Path, result: Result<String>, errors: &mut Vec<String>) -> (String, bool) {
    match result {
        Ok(fingerprint) => (fingerprint, true),
        Err(e) => {
            if !e.is::<Cancelled>() {
                push_error(errors, format!("Couldn't fingerprint {}: {e}", path.display()));
            }
            // A transient read failure must be retried on the next scan.
            (String::new(), false)
        }
    }
}
fn enumerate(
    root: &Path,
    cancelled: &impl Fn() -> bool,
    files: &mut Vec<PathBuf>,
    uncertain: &mut Vec<PathBuf>,
    empty_folders: &mut Vec<PathBuf>,
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
    let mut empty = true;
    for item in read {
        if cancelled() {
            return;
        }
        let item = match item {
            Ok(item) => item,
            Err(e) => {
                empty = false;
                uncertain.push(root.to_owned());
                errors.push(format!("Couldn't enumerate {}: {e}", root.display()));
                continue;
            }
        };
        let name = item.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        empty = false;
        match item.file_type() {
            Ok(kind) if kind.is_dir() && !name.to_string_lossy().ends_with(".sdr") => {
                enumerate(&item.path(), cancelled, files, uncertain, empty_folders, errors)
            }
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
    if empty {
        empty_folders.push(root.to_owned());
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
    let mut empty_folders = Vec::new();
    let mut errors = Vec::new();
    for root in roots {
        enumerate(root, cancelled, &mut paths, &mut uncertain, &mut empty_folders, &mut errors);
    }
    paths.sort();
    paths.dedup();
    log::info!(
        "startup stage: filesystem enumeration paths={} elapsed_us={}",
        paths.len(),
        started.elapsed().as_micros()
    );
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
        let (fingerprint, fingerprint_attempted) = if changed && (cached.is_some() || possible_moves) {
            fingerprint_status(&path, fingerprint(&path, cancelled), &mut errors)
        } else if changed {
            (String::new(), false)
        } else {
            cached.map_or_else(|| (String::new(), false), |b| (b.fingerprint.clone(), b.fingerprint_attempted))
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
            annotation_id: String::new(),
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
    // A moved entry supersedes its unavailable old path. Identity assignment
    // keeps every current ID unique, so compare the tombstone's previous ID:
    // its freshly assigned ID no longer identifies the transferred state.
    let available_ids: HashSet<_> = books.iter().filter(|b| b.available).map(|b| b.id.clone()).collect();
    books.retain(|b| b.available || old.get(&b.path).is_none_or(|cached| !available_ids.contains(&cached.id)));
    empty_folders.extend(
        previous
            .empty_folders
            .iter()
            .filter(|p| roots.iter().any(|r| p.starts_with(r)) && uncertain.iter().any(|r| p.starts_with(r)))
            .cloned(),
    );
    empty_folders.sort();
    empty_folders.dedup();
    let mut index = LibraryIndex { books, empty_folders, ..LibraryIndex::default() };
    // New books appear with filename titles before their metadata is read. An
    // unchanged or edit-only scan has nothing to show early.
    if !dirty.is_empty() {
        checkpoint(&index);
    }
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
    fn fingerprint_read_error_is_retryable() {
        let missing = Path::new("/this/verso-fingerprint-fixture-does-not-exist.epub");
        let mut errors = Vec::new();
        let (fingerprint, attempted) = fingerprint_status(missing, fingerprint(missing, &|| false), &mut errors);
        assert!(fingerprint.is_empty());
        assert!(!attempted);
        assert!(errors.iter().any(|e| e.contains("Couldn't fingerprint")));
        let (fingerprint, attempted) = fingerprint_status(missing, Ok("digest".into()), &mut errors);
        assert_eq!(fingerprint, "digest");
        assert!(attempted);
    }
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
    fn drawn_cover_sizes_get_tiles_for_every_book_and_the_oldest_size_goes() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-tile-sizes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let books = root.join("books");
        std::fs::create_dir_all(&books)?;
        for name in ["a", "b"] {
            std::fs::copy(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/library3.epub"),
                books.join(format!("{name}.epub")),
            )?;
        }
        let store = Store::at(root.join("data"));
        let wait = |done: &dyn Fn() -> bool| {
            let started = Instant::now();
            while !done() {
                assert!(started.elapsed() < Duration::from_secs(20), "timed out");
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        let keys = || -> Vec<String> {
            store
                .library()
                .books
                .iter()
                .filter_map(|b| match &b.cover {
                    Cover::Ready(key) => Some(key.clone()),
                    _ => None,
                })
                .collect()
        };
        let tiles =
            |size: (u32, u32)| keys().iter().filter(|k| store.tile_cover_path(k, size.0, size.1).exists()).count();
        let worker = Worker::new(store.clone(), vec![books.clone()], Waker::new(|| {}));
        wait(&|| keys().len() == 2);
        // Small and full sizes get no tiles of their own.
        let sizes: Vec<_> = (0..covers::TILE_SIZES as u32).map(|i| (200 + i, 300 + i)).collect();
        worker.drew_cover_sizes(vec![(100, 150), (covers::WIDTH, covers::HEIGHT), sizes[0]]);
        wait(&|| tiles(sizes[0]) == 2);
        assert_eq!(covers::tile_sizes(&store), [sizes[0]]);
        worker.drew_cover_sizes(sizes[1..].to_vec());
        wait(&|| sizes.iter().all(|&s| tiles(s) == 2));
        // One more drops the least recently drawn size and its files.
        worker.drew_cover_sizes(vec![(250, 375)]);
        wait(&|| tiles((250, 375)) == 2 && tiles(sizes[0]) == 0);
        let kept = covers::tile_sizes(&store);
        assert_eq!((kept.len(), kept[0]), (covers::TILE_SIZES, (250, 375)));
        assert!(!kept.contains(&sizes[0]));
        drop(worker);
        // A later session reads the kept sizes and backfills a missing tile.
        let key = keys()[0].clone();
        std::fs::remove_file(store.tile_cover_path(&key, 250, 375))?;
        let worker = Worker::new(store.clone(), vec![books.clone()], Waker::new(|| {}));
        wait(&|| store.tile_cover_path(&key, 250, 375).exists());
        drop(worker);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn unchanged_covers_skip_decoding_and_visible_failures_repair_once() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-cover-repair-{}", std::process::id()));
        let books = root.join("books");
        std::fs::create_dir_all(&books)?;
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/library3.epub"),
            books.join("book.epub"),
        )?;
        let store = Store::at(root.join("state"));
        fn wait(mut done: impl FnMut() -> bool) {
            let started = Instant::now();
            while !done() {
                assert!(started.elapsed().as_secs() < 20, "worker timed out");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        // Returns the last published snapshot (if any) and the final status.
        let finished = |worker: &Worker| {
            let (mut content, mut status) = (None, None);
            wait(|| {
                if let Some(snapshot) = worker.take() {
                    content = Some(snapshot);
                }
                if let Some(next) = worker.take_status() {
                    let done = !next.working;
                    status = Some(next);
                    return done;
                }
                false
            });
            // Content published just before the final status is already queued.
            if let Some(snapshot) = worker.take() {
                content = Some(snapshot);
            }
            (content, status.unwrap())
        };
        let worker = Worker::new(store.clone(), vec![books.clone()], Waker::new(|| {}));
        let index = finished(&worker).0.expect("a new library publishes its index").index;
        let book = index.books[0].clone();
        let Cover::Ready(key) = book.cover.clone() else { panic!("fixture cover missing") };
        assert!(covers::load_small(&store, &key).is_ok_and(|s| s.width() == covers::SMALL_WIDTH));
        drop(worker);
        // A cache from before small thumbnails is backfilled by the next pass.
        std::fs::remove_file(store.small_cover_path(&key))?;
        // An older installation with a reading record but no Home summary is
        // migrated on the worker without changing the authoritative record.
        let reading = crate::store::BookState {
            position: crate::document::Position("exact anchor".into()),
            progress: 2500,
            last_opened: 5,
            ..Default::default()
        };
        store.save_book(&book.id, &reading)?;
        let _ = std::fs::remove_file(root.join("state/home.json"));
        let _ = std::fs::remove_file(root.join("state/opened.json"));
        let record = std::fs::read(root.join("state/books").join(format!("{}.json", book.id)))?;
        let worker = Worker::new(store.clone(), vec![books.clone()], Waker::new(|| {}));
        finished(&worker);
        let (migrated, _) = worker.take_home().expect("a missing summary is rebuilt and published");
        assert!(migrated.complete && migrated.resume().is_some_and(|e| e.book.id == book.id));
        assert_eq!(store.home_summary(), Some(migrated));
        // The same reads build the opened order for folder previews.
        let opened = store.opened().expect("the opened order is rebuilt with Home");
        assert!(opened.complete && opened.leader() == Some(book.id.as_str()));
        assert!(worker.take_opened_changed() && !worker.take_opened_changed());
        assert_eq!(std::fs::read(root.join("state/books").join(format!("{}.json", book.id)))?, record);
        assert!(covers::load_small(&store, &key).is_ok_and(|s| s.width() == covers::SMALL_WIDTH));
        drop(worker);
        // A corrupt, current cache file is reused by the unchanged pass without
        // decoding. The pass writes no library file and hands over the saved
        // index only marked unchanged, with its hierarchy, so browsing needn't
        // read it again. It still delivers completion and errors (here an
        // unreadable extra folder).
        std::fs::write(store.cover_path(&key), b"corrupt")?;
        std::fs::write(store.small_cover_path(&key), b"corrupt")?;
        let library_file = root.join("state/library.json");
        let written = std::fs::metadata(&library_file)?.modified()?;
        let home_file = root.join("state/home.json");
        let home_written = std::fs::metadata(&home_file)?.modified()?;
        let roots = vec![books.clone(), root.join("missing")];
        let worker = Worker::new(store.clone(), roots.clone(), Waker::new(|| {}));
        let (content, status) = finished(&worker);
        let content = content.expect("the first pass hands over the saved index");
        assert!(content.cached && !content.partial && content.index == store.library());
        let (built_for, tree) = content.collections.expect("with its hierarchy");
        assert_eq!(built_for, roots);
        assert_eq!(tree.folders[&books].count, 1);
        assert!(worker.take_home().is_none(), "a complete summary is not rebuilt");
        assert_eq!(std::fs::metadata(&home_file)?.modified()?, home_written, "nor rewritten");
        assert!(!status.scanning && status.errors.iter().any(|e| e.contains("missing")));
        assert_eq!(std::fs::metadata(&library_file)?.modified()?, written, "an unchanged pass saves nothing");
        assert_eq!(store.library().books[0].cover, Cover::Ready(key.clone()));
        assert!(covers::load(&store.cover_path(&key)).is_err());
        // A visible-load failure regenerates it once, outside a pass.
        worker.repair(["unknown".to_owned(), book.id.clone()]);
        wait(|| !worker.take_repaired().is_empty());
        assert!(covers::load(&store.cover_path(&key)).is_ok());
        assert!(covers::load_small(&store, &key).is_ok(), "the same repair redoes the small thumbnail");
        std::fs::write(store.cover_path(&key), b"corrupt")?;
        worker.repair([book.id.clone()]);
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(worker.take_repaired().is_empty(), "a key is repaired at most once per session");
        assert!(worker.take().is_none(), "an unchanged repair publishes no index");
        assert_eq!(std::fs::metadata(&library_file)?.modified()?, written);
        drop(worker);
        // A changed book file gets a new cover key; the stale thumbnail is not reused.
        let file = std::fs::File::options().write(true).open(books.join("book.epub"))?;
        file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60))?;
        drop(file);
        let worker = Worker::new(store.clone(), roots.clone(), Waker::new(|| {}));
        let changed = finished(&worker).0.expect("a changed book publishes the index").index;
        let Cover::Ready(new_key) = changed.books[0].cover.clone() else { panic!("changed cover missing") };
        assert_ne!(new_key, key);
        assert!(covers::load(&store.cover_path(&new_key)).is_ok());
        assert!(store.small_cover_path(&new_key).exists());
        assert_eq!(store.book(&book.id).position, reading.position, "changed files keep reading state");
        drop(worker);
        // New and removed files are still discovered and published.
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/library2.epub"),
            books.join("added.epub"),
        )?;
        let worker = Worker::new(store.clone(), roots.clone(), Waker::new(|| {}));
        let added = finished(&worker).0.expect("an added book publishes the index").index;
        assert_eq!(added.books.iter().filter(|b| b.available).count(), 2);
        assert_eq!(store.library(), added);
        drop(worker);
        std::fs::remove_file(books.join("added.epub"))?;
        let worker = Worker::new(store.clone(), roots, Waker::new(|| {}));
        let removed = finished(&worker).0.expect("a removed book publishes the index").index;
        assert_eq!(removed.books.iter().filter(|b| b.available).count(), 1);
        // A new folder list applies without restarting the worker. Books in a
        // removed folder stay indexed as unavailable and return when re-added.
        let other = root.join("other");
        std::fs::create_dir_all(&other)?;
        worker.set_roots(vec![other.clone()]);
        let moved = finished(&worker).0.expect("a changed folder list publishes the index").index;
        assert!(moved.books.iter().all(|b| !b.available) && !moved.books.is_empty());
        worker.set_roots(vec![books.clone()]);
        let back = finished(&worker).0.expect("re-adding the folder publishes the index").index;
        assert_eq!(back.books.iter().filter(|b| b.available).count(), 1);
        assert_eq!(store.book(&book.id).position, reading.position, "reading state survives");
        drop(worker);
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

#[cfg(test)]
mod folder_tests {
    use super::*;
    use crate::library::{collections::Collections, files};
    #[test]
    fn empty_rule_nested_hidden_sidecars_and_other_formats() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-empty-rule-{}", std::process::id()));
        for path in ["nested/Empty", ".hidden/empty", "Book.sdr/empty", "PDF", "Dotfiles"] {
            std::fs::create_dir_all(root.join(path))?;
        }
        std::fs::write(root.join("PDF/book.pdf"), b"pdf")?;
        std::fs::write(root.join("Dotfiles/.hidden"), b"hidden")?;
        std::fs::write(root.join(".hidden/book.epub"), b"hidden")?;
        std::fs::write(root.join("Book.sdr/book.epub"), b"sidecar")?;
        let scan = scan(&root, &Default::default(), false, &|| false, |_| {}).unwrap();
        assert!(scan.index.books.is_empty());
        assert_eq!(scan.index.empty_folders, [root.join("Dotfiles"), root.join("nested/Empty")]);
        let tree = Collections::from_index(&scan.index, std::slice::from_ref(&root));
        assert!(tree.folders.contains_key(&root.join("nested")));
        assert!(!tree.folders.contains_key(&root.join("PDF")));
        assert!(files::is_empty(&root.join("Dotfiles")));
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn an_edit_cancelling_a_scan_keeps_the_books_it_already_showed() -> Result<()> {
        use std::sync::OnceLock;
        use std::sync::atomic::{AtomicBool, Ordering};
        let root = std::env::temp_dir().join(format!("verso-folder-checkpoint-{}", std::process::id()));
        let library = root.join("library");
        std::fs::create_dir_all(&library)?;
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/library3.epub");
        for name in ["a.epub", "b.epub", "c.epub"] {
            std::fs::copy(&fixture, library.join(name))?;
        }
        let empty = files::create_folder(&library, "Empty")?;
        let store = Store::at(root.join("state"));
        // The waker consumes every publication. On the first checkpoint (new
        // books shown with filename titles), it queues a folder edit, which
        // cancels the scan before any metadata is read.
        type SharedState = Arc<SyncState>;
        let cell: Arc<OnceLock<SharedState>> = Arc::default();
        let edited = Arc::new(AtomicBool::new(false));
        let seen: Arc<Mutex<Vec<(bool, u64, usize)>>> = Arc::default();
        let waker = {
            let (cell, edited, seen, empty) = (cell.clone(), edited.clone(), seen.clone(), empty.clone());
            Waker::new(move || {
                let mut state = cell.wait().state.lock().unwrap();
                state.status = None;
                if let Some(snapshot) = state.latest.take() {
                    seen.lock().unwrap().push((snapshot.partial, snapshot.revision, snapshot.index.books.len()));
                    if snapshot.partial && !edited.swap(true, Ordering::SeqCst) {
                        state.revision += 1;
                        let revision = state.revision;
                        state.changes.push((revision, PathChange::AddEmpty(empty.clone()), true));
                    }
                }
            })
        };
        let worker = Worker::new(store.clone(), vec![library.clone()], waker);
        cell.set(worker.inner.shared.clone()).ok();
        let deadline = Instant::now() + Duration::from_secs(10);
        // The edit's own publication is the first complete one at its revision.
        let first = loop {
            if let Some(&(_, _, books)) = seen.lock().unwrap().iter().find(|(partial, r, _)| !partial && *r >= 1) {
                break books;
            }
            assert!(Instant::now() < deadline, "the edit was never published");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(edited.load(Ordering::SeqCst));
        assert_eq!(first, 3, "the cancelled scan's books stay visible");
        drop(worker);
        let saved = store.library();
        assert_eq!(saved.books.len(), 3);
        assert!(saved.empty_folders.contains(&empty));
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn cancelled_fingerprints_are_told_apart_from_unreadable_files() -> Result<()> {
        // The identity pass retries only a cancelled read; a failed one is final.
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/library3.epub");
        assert!(fingerprint(&fixture, &|| true).unwrap_err().is::<Cancelled>());
        assert!(!fingerprint(Path::new("/missing/book.epub"), &|| false).unwrap_err().is::<Cancelled>());
        assert!(!fingerprint(&fixture, &|| false)?.is_empty());
        Ok(())
    }

    #[test]
    fn queued_mutations_interrupt_a_paused_scan_and_preserve_state_and_home() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-folder-queue-{}", std::process::id()));
        let old = root.join("library/Old");
        std::fs::create_dir_all(&old)?;
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/library3.epub");
        std::fs::copy(&fixture, old.join("book.epub"))?;
        let store = Store::at(root.join("state"));
        let initial = scan(&root.join("library"), &Default::default(), false, &|| false, |_| {}).unwrap().index;
        let id = initial.books[0].id.clone();
        store.save_library(&initial)?;
        store.save_book(
            &id,
            &crate::store::BookState {
                position: crate::document::Position("anchor".into()),
                last_opened: 1,
                progress: 2500,
                ..Default::default()
            },
        )?;
        let worker = Worker::new(store.clone(), vec![root.join("library")], Waker::new(|| {}));
        worker.pause(true);
        // A mutation must wake the paused scan, rather than wait for resume.
        let renamed = files::rename_folder(&old, "Renamed")?;
        worker.move_paths(vec![(old.clone(), renamed.clone())]);
        let empty = files::create_folder(&renamed, "Empty")?;
        let queued = worker.add_empty_folder(empty.clone());
        let revision = queued[0].0;
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut published = None;
        while published.as_ref().is_none_or(|s: &Snapshot| s.revision < revision) {
            if let Some(next) = worker.take() {
                published = Some(next);
            }
            assert!(Instant::now() < deadline, "mutations blocked behind a paused scan");
            std::thread::sleep(Duration::from_millis(5));
        }
        let snapshot = published.unwrap();
        assert_eq!(snapshot.index.books[0].id, id);
        assert_eq!(snapshot.index.books[0].path, renamed.join("book.epub"));
        assert!(snapshot.index.empty_folders.contains(&empty));
        assert_eq!(store.book(&id).progress, 2500);
        let (home, home_revision) = worker.take_home().unwrap();
        assert_eq!(home_revision, revision);
        assert_eq!(home.resume().unwrap().book.path, renamed.join("book.epub"));
        assert!(files::delete_folder(&renamed, &snapshot.index, &store).error.is_none());
        let removed_revision = worker.remove_paths(vec![renamed])[0].0;
        while store.library().books.iter().any(|b| b.id == id) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        worker.pause(false);
        // Drain through the completed restarted scan, which must keep removal.
        loop {
            if let Some(status) = worker.take_status()
                && !status.working
            {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let final_snapshot = worker.take().unwrap();
        assert!(final_snapshot.revision >= removed_revision);
        assert!(final_snapshot.index.books.is_empty());
        assert_eq!(final_snapshot.index.empty_folders, [root.join("library")]);
        // Model a batch arriving after a pass consumed `requested`, then
        // cancelled at enumeration. The remaining batch must still run a pass
        // and publish an idle status even though the flag is already false.
        let late = files::create_folder(&root.join("library"), "Late batch")?;
        {
            let mut state = worker.inner.shared.state.lock().unwrap();
            state.requested = false;
            state.revision += 1;
            let revision = state.revision;
            state.changes.push((revision, PathChange::AddEmpty(late.clone()), true));
        }
        worker.inner.shared.wake.notify_all();
        loop {
            if let Some(status) = worker.take_status()
                && !status.working
                && store.library().empty_folders.contains(&late)
            {
                break;
            }
            assert!(Instant::now() < deadline, "the final batch must complete its restarted scan");
            std::thread::sleep(Duration::from_millis(5));
        }
        let at_exit = files::create_folder(&root.join("library"), "At exit")?;
        worker.add_empty_folder(at_exit.clone());
        drop(worker);
        assert!(store.library().empty_folders.contains(&at_exit), "shutdown commits queued operations");
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn shared_store_handle_reads_current_without_emptying_take_and_adds_without_scan() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let library = dir.path().join("library");
        std::fs::create_dir_all(&library)?;
        let store = Store::at(dir.path().join("data"));
        store.save_library_folders(vec![library.clone()])?;

        let a = store.library_worker(Waker::new(|| {}));
        let b = store.clone().library_worker(Waker::new(|| {}));
        let started = Instant::now();
        loop {
            if let Some(status) = a.take_status()
                && !status.working
            {
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(10));
        }
        // Drain the initial empty snapshot so the next `take()` comes from `add_book`.
        let _ = a.take();

        let epub = library.join("added.epub");
        let unscanned = library.join("unscanned.epub");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/library3.epub");
        std::fs::copy(&fixture, &epub)?;
        std::fs::copy(&fixture, &unscanned)?;
        let metadata = epub::read(&epub)?;
        let file_stamp = stamp(&std::fs::metadata(&epub)?)?;
        let fp = fingerprint_for_test(&epub)?;

        let added = b
            .add_book(Book {
                path: epub.clone(),
                id: String::new(),
                annotation_id: String::new(),
                legacy_id: book_id(&epub)?,
                fingerprint: fp.clone(),
                fingerprint_attempted: true,
                stamp: file_stamp,
                metadata,
                error: None,
                cover: Cover::Pending,
                available: true,
                metadata_pending: false,
            })
            .expect("book inserted");
        assert_eq!(added.fingerprint, fp);

        // Both handles see the same updated index via `current()`, and reading
        // `current()` leaves `take()` intact for the UI screen.
        assert_eq!(a.current().books.len(), 1, "no scan pass ran");
        assert_eq!(b.current().books.len(), 1);
        assert_eq!(a.current().books[0].id, added.id);
        let snapshot = a.take().expect("current() must not empty the UI take() mailbox");
        assert_eq!(snapshot.index.books.len(), 1);
        assert_eq!(snapshot.index.books[0].id, added.id);
        assert!(!snapshot.index.books.iter().any(|book| book.path == unscanned));
        Ok(())
    }
}
