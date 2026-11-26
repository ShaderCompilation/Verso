//! Small, versioned JSON records. Rename replaces a complete file atomically.
//!
//! `Store` owns paths under `<data>/` (`settings.json`, `library.json`,
//! `home.json`, `opened.json`, `library-navigation.json`, `reading-speed.json`,
//! `books/<id>.json`, `covers/`, `annotations/`, `dictionaries.json`,
//! `dictionary-folders.json`), caches normalized `Settings`, and coordinates
//! shared lazy handles (`dictionary_service`, `library_worker`, `updates`).
//! Atomic counters (`settings_saves`, `book_saves`, `bulk_saves`) keep cached
//! readers/writers coherent without polling FUSE. Worker clones strip service
//! handles so threads never hold `Arc` cycles. `Store::freeze` silences all
//! writes before `EXIT_RESTART` after developer live-data edits.
mod dictionary_folders;
mod home;
pub mod navigation;
pub mod position;
use crate::document::{Position, TextShaping, Typography};
use crate::library::LibraryIndex;
use crate::library::collections::{Navigation, distinct_roots, normalize};
pub use crate::online::translate::Provider as TranslateProvider;
use crate::page_layout::PageLayout;
pub use crate::platform::Orientation;
pub use crate::reader::TapZones;
use anyhow::{Context, Result};
pub use dictionary_folders::{
    KOREADER_DICTIONARY_FOLDER, MAX_DICTIONARY_FOLDERS, default_dictionary_folders, find_dictionary_folder,
    with_dictionary_folder, without_dictionary_folder,
};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub typography: Typography,
    /// Margins and the reader's top and bottom bars.
    pub page_layout: PageLayout,
    /// The old progress-footer toggle, read once to migrate into
    /// `page_layout` and never written again.
    #[serde(rename = "footer", skip_serializing)]
    pub legacy_footer: Option<bool>,
    pub sleep_screen: SleepScreen,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub library_folder: Option<PathBuf>, // Legacy single-folder setting.
    pub library_folders: Option<Vec<PathBuf>>,
    /// Page turns between full flashes that clear ghosting; 0 never flashes
    /// periodically (chapter starts and pictures still do).
    pub page_flash: u32,
    pub tap_zones: TapZones,
    /// A tap in the top-right corner of a page toggles its bookmark.
    /// Settings → Reading.
    pub bookmark_corner: bool,
    /// The device's page-turn buttons the other way round; remotes have their
    /// own mapping. Settings → Reading, on devices with buttons.
    pub swap_page_buttons: bool,
    pub orientation: Orientation,
    /// Developer options in Settings (tap the version five times).
    pub developer: bool,
    /// The name the device is found by on Wi-Fi while SSH runs, without
    /// `.local`. Changes only when another device already has it.
    pub hostname: String,
    /// Settings → Advanced. Applies from the next book open.
    pub text_shaping: TextShaping,
    /// Check for updates when Verso starts or the Kindle wakes (at most every
    /// `CHECK_EVERY`, never turning Wi-Fi on). Settings → Device.
    pub check_updates: bool,
    /// How folder tiles in Collections look. Older files read as Covers.
    pub folder_preview: FolderPreview,
    /// Untested Kindle models whose notice was dismissed
    /// (`DeviceModel::key`), so it shows once per model.
    pub untested_seen: Vec<String>,
    /// When a book opens, offer KOReader's newer position. Settings → Reading.
    pub koreader_progress: bool,
    /// The lookup card's size in portrait as ⤢ last left it: it opens
    /// expanded until collapsed.
    pub dictionary_expanded: bool,
    /// The compact lookup card stays at the bottom (the right in landscape)
    /// rather than moving away from the word. Dictionaries → Lookup card.
    pub dictionary_at_bottom: bool,
    /// The language Translate translates into, in every book; unset until
    /// the reader chooses one, and nothing is translated before.
    pub translate_language: Option<String>,
    /// Translation service: Google Translate (default) or DeepL.
    pub translate_provider: TranslateProvider,
    /// Optional DeepL API key (Free keys end with `:fx`). Omitted/redacted from
    /// problem reports (`report.sh`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deepl_api_key: Option<String>,
    /// First-time setup was finished or skipped; until then it shows over
    /// Home on every launch (`screens/setup.rs`).
    pub setup_done: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            typography: Typography::default(),
            page_layout: PageLayout::default(),
            legacy_footer: None,
            sleep_screen: SleepScreen::Cover,
            library_folder: None,
            library_folders: None,
            page_flash: DEFAULT_PAGE_FLASH,
            tap_zones: TapZones::Standard,
            bookmark_corner: true,
            swap_page_buttons: false,
            orientation: Orientation::Portrait,
            developer: false,
            hostname: verso_mdns::DEFAULT_LABEL.to_owned(),
            text_shaping: TextShaping::default(),
            check_updates: true,
            folder_preview: FolderPreview::default(),
            untested_seen: Vec::new(),
            koreader_progress: true,
            dictionary_expanded: true,
            dictionary_at_bottom: true,
            translate_language: None,
            translate_provider: TranslateProvider::default(),
            deepl_api_key: None,
            setup_done: false,
        }
    }
}
impl Settings {
    /// Moves old fields into their replacements and clamps out-of-range values.
    pub fn normalize(&mut self) {
        self.page_layout.migrate(self.legacy_footer.take(), self.typography.legacy_margins.take());
        self.page_layout.normalize();
        self.typography.normalize();
        if !FolderPreview::OFFERED.contains(&self.folder_preview) {
            self.folder_preview = FolderPreview::default();
        }
        self.deepl_api_key =
            self.deepl_api_key.as_deref().and_then(|key| crate::online::translate::parse_deepl_key(key).ok().flatten());
    }
}
pub const DEFAULT_PAGE_FLASH: u32 = 0;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SleepScreen {
    #[default]
    Cover,
    Amazon,
}
/// What a folder tile in Collections shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderPreview {
    /// A 2×2 grid of four covers. Disabled for now: it isn't in `OFFERED`,
    /// so Settings doesn't show it and a saved choice reads as the default.
    Covers,
    /// Three covers, each shifted behind the one in front.
    #[default]
    Stack,
    /// The cover of the book read most recently, under a folder tab.
    Latest,
    /// The folder icon and count.
    Icon,
}
impl FolderPreview {
    /// Every mode, including disabled ones, for tests.
    #[cfg(test)]
    pub const ALL: [FolderPreview; 4] =
        [FolderPreview::Covers, FolderPreview::Stack, FolderPreview::Latest, FolderPreview::Icon];
    /// The modes Settings offers, in its order. Covers is left out until its
    /// grid looks right on the panel; add it back
    /// here, with its label in Settings, to enable it again.
    pub const OFFERED: [FolderPreview; 3] = [FolderPreview::Stack, FolderPreview::Latest, FolderPreview::Icon];
    /// How many of a folder's preview books the tile shows: a prefix of its pick.
    pub fn slots(self) -> usize {
        match self {
            FolderPreview::Covers => 4,
            FolderPreview::Stack => 3,
            FolderPreview::Latest => 1,
            FolderPreview::Icon => 0,
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BookState {
    pub position: Position,
    pub page: usize,
    pub progress: u32,
    pub last_opened: u64,
    /// I have read this book. Sticky: opening the book doesn't clear it, only
    /// *Mark unfinished*.
    pub finished: bool,
    /// When `finished` last changed, in either direction (Unix seconds, 0 =
    /// never). While finished, this is the finished date.
    pub status_at: u64,
    /// Finished, and being read again: back in Continue reading with its real
    /// progress. Meaningless unless `finished` is set.
    pub rereading: bool,
    pub hidden_from_continue: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dictionary_set: Option<String>,
    /// The Wikipedia chosen for this book, instead of its language's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wikipedia_language: Option<String>,
    /// Characters per second read in this book, for the time-left items.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reading_speed: Option<crate::reader::speed::ReadingSpeed>,
    /// The language detected from the book's text on its first open.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<crate::library::language::DetectedLanguage>,
    /// The last KOReader position offered, taken or imported for this book.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub koreader_seen: Option<crate::koreader::Seen>,
}
#[derive(Clone)]
pub struct Store {
    root: PathBuf,
    /// Bundled word-form packs (`<lang>.vwf`), outside the user's data.
    wordforms: PathBuf,
    pub(crate) dictionary_service:
        std::sync::Arc<std::sync::OnceLock<std::sync::Arc<crate::dictionary::worker::Service>>>,
    pub(crate) dictionary_folder_revision: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Shared library worker: screens and Wi-Fi transfer share one handle while
    /// any is open, and dropping the last handle stops and joins its thread.
    library_worker: std::sync::Arc<std::sync::Mutex<std::sync::Weak<crate::library::worker::WorkerInner>>>,
    selected_home: std::sync::Arc<std::sync::Mutex<Option<crate::library::Book>>>,
    /// The book known to lead `opened.json`, so its saves (page turns) skip
    /// reading the file. None when unknown.
    opened_leader: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    /// Counts settings saves through this store and its clones, so screens
    /// can notice a change without rereading the file.
    settings_saves: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Cached normalized `Settings`, populated on first read and updated on
    /// `save_settings` so repeated lookups avoid FUSE reads and JSON parses.
    settings_cache: std::sync::Arc<std::sync::Mutex<Option<Settings>>>,
    /// Counts external book-state writes through this store and its clones, so
    /// the position writer knows when to re-read its cached `BookState`.
    book_saves: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Counts bulk state writes (the KOReader import), so the library
    /// reloads every cached state when it comes back from Settings.
    bulk_saves: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// The update check, shared by startup and Settings; made on first use.
    updates: std::sync::Arc<std::sync::OnceLock<std::sync::Arc<crate::update::Updates>>>,
    /// Online lookups, made on first use; it holds no `Store`.
    online: std::sync::Arc<std::sync::OnceLock<std::sync::Arc<crate::online::Service>>>,
    /// Set when the web app's Files tab wrote into Verso's live data
    /// (developer mode): Verso still holds the old values and would save
    /// over the edits, so they wait for Restart to apply.
    live_data_edited: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Display color mode for cover cache keys, thumbnails and sleep covers.
    color: crate::gfx::ColorMode,
    /// The device has page-turn buttons, so Settings offers to swap them.
    page_buttons: bool,
}
impl Store {
    pub fn new() -> Self {
        let root = std::env::var_os("VERSO_DATA").map(PathBuf::from).unwrap_or_else(|| {
            #[cfg(feature = "kindle")]
            {
                PathBuf::from("/mnt/us/verso/data")
            }
            #[cfg(feature = "desktop")]
            {
                std::env::var_os("XDG_DATA_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"))
                    .join("verso")
            }
        });
        // Installed next to the fonts on the Kindle; the simulator uses the
        // packs `scripts/build-wordforms.sh` writes.
        let wordforms = std::env::var_os("VERSO_WORDFORMS").map(PathBuf::from).unwrap_or_else(|| {
            #[cfg(feature = "kindle")]
            {
                PathBuf::from("/mnt/us/verso/wordforms")
            }
            #[cfg(feature = "desktop")]
            {
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/wordforms")
            }
        });
        Self {
            root,
            wordforms,
            selected_home: Default::default(),
            opened_leader: Default::default(),
            settings_saves: Default::default(),
            settings_cache: Default::default(),
            book_saves: Default::default(),
            bulk_saves: Default::default(),
            dictionary_service: Default::default(),
            dictionary_folder_revision: Default::default(),
            library_worker: Default::default(),
            updates: Default::default(),
            online: Default::default(),
            live_data_edited: Default::default(),
            color: crate::gfx::ColorMode::Gray8,
            page_buttons: false,
        }
    }
    pub fn with_color(mut self, color: crate::gfx::ColorMode) -> Self {
        self.color = color;
        self
    }
    pub fn with_page_buttons(mut self, page_buttons: bool) -> Self {
        self.page_buttons = page_buttons;
        self
    }
    pub fn has_page_buttons(&self) -> bool {
        self.page_buttons
    }
    pub fn color_mode(&self) -> crate::gfx::ColorMode {
        self.color
    }
    /// A worker keeps the data paths and counters, without owning its service registry.
    pub(crate) fn dictionary_context(&self) -> Self {
        Self { dictionary_service: Default::default(), ..self.clone() }
    }
    /// Returns the shared library worker, starting one if none is running.
    pub fn library_worker(&self, waker: crate::platform::Waker) -> crate::library::worker::Worker {
        let mut slot = self.library_worker.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(inner) = slot.upgrade() {
            return crate::library::worker::Worker::from_inner(inner);
        }
        let context =
            Self { dictionary_service: Default::default(), library_worker: Default::default(), ..self.clone() };
        let worker = crate::library::worker::Worker::new(context, self.library_folders(), waker);
        *slot = worker.weak();
        worker
    }
    pub fn settings(&self) -> Settings {
        let mut slot = self.settings_cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cached) = slot.as_ref() {
            return cached.clone();
        }
        let s = normalize_settings(read_settings(&self.root.join("settings.json")));
        *slot = Some(s.clone());
        s
    }
    pub fn save_settings(&self, s: &Settings) -> Result<()> {
        let result = atomic(&self.root.join("settings.json"), s);
        if result.is_ok() {
            *self.settings_cache.lock().unwrap_or_else(|e| e.into_inner()) = Some(normalize_settings(s.clone()));
        }
        self.settings_saves.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        result
    }
    /// Changes whenever settings were saved; compare to notice edits.
    pub fn settings_saves(&self) -> u64 {
        self.settings_saves.load(std::sync::atomic::Ordering::Relaxed)
    }
    pub fn navigation(&self) -> Navigation {
        read(&self.root.join("library-navigation.json")).unwrap_or_default()
    }
    pub fn save_navigation(&self, navigation: &Navigation) -> Result<()> {
        let started = std::time::Instant::now();
        let result = atomic(&self.root.join("library-navigation.json"), navigation);
        log::debug!("navigation write: {:?}, ok={}", started.elapsed(), result.is_ok());
        result
    }
    /// The reading speed carried over from previous books.
    pub fn reading_speed(&self) -> Option<crate::reader::speed::ReadingSpeed> {
        read(&self.root.join("reading-speed.json"))
    }
    pub fn save_reading_speed(&self, speed: &crate::reader::speed::ReadingSpeed) -> Result<()> {
        atomic(&self.root.join("reading-speed.json"), speed)
    }
    pub fn book(&self, id: &str) -> BookState {
        read(&self.book_path(id)).unwrap_or_default()
    }
    /// Changes whenever an external caller writes or removes a book record.
    pub(crate) fn book_saves(&self) -> u64 {
        self.book_saves.load(std::sync::atomic::Ordering::Acquire)
    }
    pub fn save_book(&self, id: &str, s: &BookState) -> Result<()> {
        let result = self.save_book_position(id, s, true);
        if result.is_ok() {
            self.book_saves.fetch_add(1, std::sync::atomic::Ordering::Release);
        }
        result
    }
    /// Writes authoritative `books/<id>.json`, optionally updating derived
    /// `home.json` and `opened.json`. Routine page turns pass `update_home =
    /// false` and sync Home on open and flush instead.
    pub(crate) fn save_book_position(&self, id: &str, s: &BookState, update_home: bool) -> Result<()> {
        let started = std::time::Instant::now();
        let result = atomic(&self.book_path(id), s);
        log::debug!("position write: {:?}, ok={}", started.elapsed(), result.is_ok());
        if result.is_ok() && update_home {
            self.sync_home(id, s);
        }
        result
    }
    /// Writes `books/<id>.json` without updating derived Home caches or
    /// emitting a position-turn timing log (for metadata like detected language).
    pub(crate) fn save_book_record(&self, id: &str, s: &BookState) -> Result<()> {
        atomic(&self.book_path(id), s)
    }
    /// Updates derived `home.json` and `opened.json` for a book whose state is
    /// already durable. Failures only log: a rebuildable display cache must
    /// never make a durable position look failed, cause an old position to
    /// replay, or erase book state.
    pub(crate) fn sync_home(&self, id: &str, s: &BookState) {
        if let Err(error) = self.update_home_after_save(id, s) {
            log::warn!("updating Home summary: {error:#}");
        }
    }
    /// Sets the finished flag of many books, skipping those that already have
    /// it (see `BookState::mark_finished`). Home is updated once, for the books it shows, rather than per
    /// book. Returns the saved states, and the error that stopped the rest.
    pub fn set_finished(&self, ids: &[String], finished: bool) -> (Vec<(String, BookState)>, Option<anyhow::Error>) {
        let mut changed = Vec::new();
        let mut error = None;
        let now = crate::update::unix_now();
        for id in ids {
            let mut state = self.book(id);
            if !state.mark_finished(finished, now) {
                continue;
            }
            if let Err(e) = atomic(&self.book_path(id), &state) {
                error = Some(e);
                break;
            }
            changed.push((id.clone(), state));
        }
        if !changed.is_empty() {
            self.book_saves.fetch_add(1, std::sync::atomic::Ordering::Release);
        }
        let shown: std::collections::HashSet<String> = self
            .home_summary()
            .map(|summary| summary.books.iter().map(|entry| entry.book.id.clone()).collect())
            .unwrap_or_default();
        for (id, state) in changed.iter().filter(|(id, _)| shown.contains(id)) {
            if let Err(e) = self.update_home_after_save(id, state) {
                log::warn!("updating Home summary: {e:#}");
            }
        }
        (changed, error)
    }
    /// Writes many books' states in one pass (the KOReader import), stopping
    /// at the first failure. Home and the opened order aren't updated per
    /// book: the caller rebuilds them once. Returns how many were written,
    /// and the error that stopped the rest.
    pub fn save_books(&self, states: &[(String, BookState)]) -> (usize, Option<anyhow::Error>) {
        let mut written = 0;
        let mut error = None;
        for (id, state) in states {
            if let Err(e) = atomic(&self.book_path(id), state) {
                error = Some(e.context(format!("saving the reading state of {id}")));
                break;
            }
            written += 1;
        }
        if written > 0 {
            self.book_saves.fetch_add(1, std::sync::atomic::Ordering::Release);
            self.bulk_saves.fetch_add(1, std::sync::atomic::Ordering::Release);
        }
        (written, error)
    }
    /// Changes whenever `save_books` wrote something; compare to notice it.
    pub fn bulk_saves(&self) -> u64 {
        self.bulk_saves.load(std::sync::atomic::Ordering::Acquire)
    }
    /// Ids that have a reading-state record, from one directory listing, so
    /// books never opened cost no failed file open. None when listing fails.
    pub fn book_state_ids(&self) -> Option<std::collections::HashSet<String>> {
        let entries = match std::fs::read_dir(self.root.join("books")) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(Default::default()),
            Err(e) => {
                log::warn!("listing reading states: {e}");
                return None;
            }
        };
        let mut ids = std::collections::HashSet::new();
        for entry in entries {
            let name = entry.ok()?.file_name();
            if let Some(id) = name.to_str().and_then(|name| name.strip_suffix(".json")) {
                ids.insert(id.to_owned());
            }
        }
        Some(ids)
    }
    /// Removes a deleted book's reading state. A missing record is fine.
    pub fn remove_book(&self, id: &str) -> Result<()> {
        if frozen(&self.book_path(id)) {
            return Ok(());
        }
        match std::fs::remove_file(self.book_path(id)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => {
                self.book_saves.fetch_add(1, std::sync::atomic::Ordering::Release);
                Ok(())
            }
        }
    }
    pub fn library(&self) -> LibraryIndex {
        let started = std::time::Instant::now();
        let index = read::<LibraryIndex>(&self.root.join("library.json"))
            .filter(|index| index.version == LibraryIndex::VERSION)
            .map(|mut index| {
                // Older desktop defaults included `crates/verso/../..`.
                // Normalize cached paths too, so a spelling change in the
                // scan roots cannot be mistaken for a move/replacement.
                for folder in &mut index.empty_folders {
                    *folder = normalize(folder);
                }
                for book in &mut index.books {
                    book.path = normalize(&book.path);
                    if book.annotation_id.is_empty() {
                        book.annotation_id = book.id.clone();
                    }
                }
                index
            })
            .unwrap_or_default();
        log::info!(
            "startup stage: cached index books={} elapsed_us={}",
            index.books.len(),
            started.elapsed().as_micros()
        );
        index
    }
    pub fn save_library(&self, index: &LibraryIndex) -> Result<()> {
        let started = std::time::Instant::now();
        let result = atomic(&self.root.join("library.json"), index);
        // Every batch of covers saves the index: only a slow or failed save
        // is worth a line in a problem report.
        let elapsed = started.elapsed();
        let notable = result.is_err() || elapsed >= std::time::Duration::from_millis(500);
        log::log!(
            if notable { log::Level::Warn } else { log::Level::Debug },
            "startup stage: index persistence books={} elapsed_us={} ok={}",
            index.books.len(),
            elapsed.as_micros(),
            result.is_ok()
        );
        result
    }
    pub fn cover_path(&self, key: &str) -> PathBuf {
        self.root.join("covers").join(format!("{key}.png"))
    }
    /// The quarter-size thumbnail for folder previews; same key as the full one.
    pub fn small_cover_path(&self, key: &str) -> PathBuf {
        self.root.join("covers").join(format!("{key}-s.png"))
    }
    /// A thumbnail at a tile size the library draws covers at.
    pub fn tile_cover_path(&self, key: &str, width: u32, height: u32) -> PathBuf {
        self.root.join("covers").join(format!("{key}-{width}x{height}.png"))
    }
    /// The tile sizes `tile_cover_path` files are kept for.
    pub fn tile_sizes_path(&self) -> PathBuf {
        self.root.join("covers").join("sizes.json")
    }
    pub fn annotation_dir(&self) -> PathBuf {
        self.root.join("annotations")
    }
    /// Markdown exports of highlights and notes, reachable over USB.
    pub fn export_dir(&self) -> PathBuf {
        self.root.join("exports")
    }
    pub fn dictionary_dir(&self) -> PathBuf {
        self.root.join("dictionaries")
    }
    /// User-installed Hunspell dictionaries (`.aff`/`.dic` pairs) for base
    /// forms, next to the native dictionaries folder.
    pub fn hunspell_dir(&self) -> PathBuf {
        self.root.join("hunspell")
    }
    pub fn dictionary_cache(&self) -> PathBuf {
        self.root.join("dictionary-cache")
    }
    pub fn dictionary_preferences(&self) -> PathBuf {
        self.root.join("dictionaries.json")
    }
    pub fn updates(&self) -> std::sync::Arc<crate::update::Updates> {
        self.updates
            .get_or_init(|| {
                let paths = crate::update::Paths::for_device(&self.root);
                std::sync::Arc::new(crate::update::Updates::new(paths, crate::update::Source::configured()))
            })
            .clone()
    }
    pub fn online(&self) -> &std::sync::Arc<crate::online::Service> {
        self.online.get_or_init(crate::online::Service::new)
    }
    /// Looks things up through `service` instead of the internet.
    #[cfg(test)]
    pub fn with_online(self, service: std::sync::Arc<crate::online::Service>) -> Self {
        Self { online: std::sync::Arc::new(std::sync::OnceLock::from(service)), ..self }
    }
    /// Notes that the web app wrote into Verso's live data (`*.json`,
    /// `books`, `annotations`): see `live_data_edited`.
    pub fn note_live_data_edit(&self) {
        if !self.live_data_edited.swap(true, std::sync::atomic::Ordering::SeqCst) {
            log::info!("Verso's data was edited through the web app; it applies after a restart");
        }
    }
    /// True once the web app wrote into Verso's live data, until Verso
    /// starts again: Wi-Fi transfer offers Restart to apply and keeps its
    /// Books tab read-only.
    pub fn live_data_edited(&self) -> bool {
        self.live_data_edited.load(std::sync::atomic::Ordering::SeqCst)
    }
    /// Restart to apply: from now on nothing under the data folder is
    /// written, by this store, its clones or anything else that saves there
    /// (`atomic_bytes`). Verso is leaving so that it reads hand-edited data
    /// again, and its saves on the way out (screens, the library worker,
    /// the navigation and position writers, the dictionary service) would
    /// overwrite the edits. Never undone: the process exits.
    pub fn freeze(&self) {
        log::info!("not saving anything more in {}: restarting to apply edits", self.root.display());
        FROZEN.lock().unwrap_or_else(|e| e.into_inner()).push(self.root.clone());
    }
    /// Verso's data folder (`/mnt/us/verso/data` on the Kindle).
    pub fn root(&self) -> PathBuf {
        self.root.clone()
    }
    pub fn wordforms_dir(&self) -> PathBuf {
        self.wordforms.clone()
    }
    pub fn library_folders(&self) -> Vec<PathBuf> {
        folder_paths(self.settings(), std::env::var_os("VERSO_BOOKS"))
    }
    /// `VERSO_BOOKS` replaces the saved folder list (for development).
    pub fn library_folders_overridden() -> bool {
        std::env::var_os("VERSO_BOOKS").is_some()
    }
    /// Saves the folder list edited in Settings.
    pub fn save_library_folders(&self, folders: Vec<PathBuf>) -> Result<()> {
        let mut settings = self.settings();
        settings.library_folders = Some(folders);
        settings.library_folder = None;
        self.save_settings(&settings)
    }
    #[cfg(test)]
    pub fn at(root: PathBuf) -> Self {
        Self {
            wordforms: root.join("wordforms"),
            root,
            selected_home: Default::default(),
            opened_leader: Default::default(),
            settings_saves: Default::default(),
            settings_cache: Default::default(),
            book_saves: Default::default(),
            bulk_saves: Default::default(),
            dictionary_service: Default::default(),
            dictionary_folder_revision: Default::default(),
            library_worker: Default::default(),
            updates: Default::default(),
            online: Default::default(),
            live_data_edited: Default::default(),
            color: crate::gfx::ColorMode::Gray8,
            page_buttons: false,
        }
    }
    fn book_path(&self, id: &str) -> PathBuf {
        self.root.join("books").join(format!("{id}.json"))
    }
}
pub fn book_id(path: &Path) -> Result<String> {
    let mut hash = 0xcbf29ce484222325u64;
    let name = path.file_name().context("book has no filename")?.as_encoded_bytes();
    for &b in name.iter().chain([0].iter()).chain(std::fs::metadata(path)?.len().to_le_bytes().iter()) {
        hash = (hash ^ b as u64).wrapping_mul(0x100000001b3);
    }
    Ok(format!("{hash:016x}"))
}
/// Normalizes settings and enforces a valid mDNS hostname label.
fn normalize_settings(mut s: Settings) -> Settings {
    s.normalize();
    if !verso_mdns::valid_label(&s.hostname) {
        s.hostname = verso_mdns::DEFAULT_LABEL.to_owned();
    }
    s
}

/// Reads settings field by field when the whole file doesn't parse, so one
/// invalid value (a typo in a hand-edited enum) resets only that setting.
fn read_settings(path: &Path) -> Settings {
    if let Some(s) = read::<Settings>(path) {
        return s;
    }
    let Some(serde_json::Value::Object(mut fields)) =
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok())
    else {
        return Settings::default();
    };
    fields.retain(|key, value| {
        let single = serde_json::Map::from_iter([(key.clone(), value.clone())]);
        let ok = serde_json::from_value::<Settings>(serde_json::Value::Object(single)).is_ok();
        if !ok {
            log::warn!("ignoring invalid setting {key} in {}", path.display());
        }
        ok
    });
    serde_json::from_value(serde_json::Value::Object(fields)).unwrap_or_default()
}
fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(s) => Some(s),
            Err(e) => {
                log::warn!("ignoring invalid {}: {e}", path.display());
                None
            }
        },
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                log::warn!("reading {}: {e}", path.display());
            }
            None
        }
    }
}
pub(crate) fn atomic(path: &Path, value: &impl Serialize) -> Result<()> {
    atomic_bytes(path, &serde_json::to_vec(value)?)
}
/// Data folders frozen by `Store::freeze`. A list of folders, not one
/// switch, so tests with their own folders don't affect each other.
static FROZEN: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// Whether `path` is in a frozen data folder: its save is skipped.
fn frozen(path: &Path) -> bool {
    let frozen = FROZEN.lock().unwrap_or_else(|e| e.into_inner());
    if frozen.is_empty() || !frozen.iter().any(|root| path.starts_with(root)) {
        return false;
    }
    log::debug!("not saving {}: restarting to apply edits", path.display());
    true
}

/// Folders `atomic_bytes` has written to, and whether each takes a
/// directory fsync. Every page turn saves a record, and on the Kindle's FUSE
/// storage each check is a round trip, so they are made once per folder.
static FOLDERS: std::sync::Mutex<std::collections::BTreeMap<PathBuf, bool>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Every record under the data folder is written here (or by `atomic`), so
/// `Store::freeze` stops all of them at once.
pub(crate) fn atomic_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    if frozen(path) {
        return Ok(());
    }
    let parent = path.parent().context("record has no parent directory")?;
    let folders = || FOLDERS.lock().unwrap_or_else(|e| e.into_inner());
    let mut known = folders().get(parent).copied();
    if known.is_none() {
        std::fs::create_dir_all(parent)?;
    }
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut f = match std::fs::File::create(&tmp) {
        // Removed since it was last written to.
        Err(e) if known.is_some() && e.kind() == std::io::ErrorKind::NotFound => {
            known = None;
            std::fs::create_dir_all(parent)?;
            std::fs::File::create(&tmp)?
        }
        f => f?,
    };
    f.write_all(bytes)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)?;
    // The FAT/FUSE filesystem on the Kindle may not support directory fsync.
    let syncs = known.unwrap_or(true)
        && match std::fs::File::open(parent)?.sync_all() {
            Ok(()) => true,
            Err(e) if matches!(e.raw_os_error(), Some(libc::EINVAL | libc::ENOSYS)) => false,
            Err(e) => return Err(e.into()),
        };
    if known != Some(syncs) {
        folders().insert(parent.to_owned(), syncs);
    }
    Ok(())
}
/// The Kindle's user storage, `/mnt/us`. On the desktop
/// `$VERSO_STORAGE` stands in for it when set (setup walks it for books and
/// takes the default folder from it); None otherwise.
pub fn storage_root() -> Option<PathBuf> {
    #[cfg(feature = "kindle")]
    {
        Some(PathBuf::from("/mnt/us"))
    }
    #[cfg(feature = "desktop")]
    {
        std::env::var_os("VERSO_STORAGE").map(PathBuf::from)
    }
}
/// Every Kindle has `documents`; setup finds the other folders with books.
pub fn default_library_folders() -> Vec<PathBuf> {
    match storage_root() {
        Some(root) => vec![root.join("documents")],
        #[cfg(feature = "desktop")]
        None => vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testbooks")],
        #[cfg(feature = "kindle")]
        None => Vec::new(),
    }
}

impl BookState {
    pub fn presented(&mut self, p: &position::Presented, fresh_open: bool) {
        self.position = p.position.clone();
        self.page = p.page;
        self.progress = p.progress;
        self.last_opened = p.at;
        if p.speed.is_some() {
            self.reading_speed = p.speed;
        }
        // Finished is sticky: opening a finished book to look something up
        // keeps it.
        if fresh_open {
            self.hidden_from_continue = false;
        }
    }
    /// Marks the book finished or unfinished at `now`, ending any re-read.
    /// False when nothing changed: already in that state and not re-reading.
    pub fn mark_finished(&mut self, finished: bool, now: u64) -> bool {
        if self.finished == finished && !self.rereading {
            return false;
        }
        self.finished = finished;
        self.status_at = now;
        self.rereading = false;
        true
    }
    /// Whether the book belongs in Continue reading: it has a place, isn't
    /// hidden, and is unfinished or being re-read.
    pub fn continues(&self) -> bool {
        !self.position.0.is_empty() && !self.hidden_from_continue && (!self.finished || self.rereading)
    }
    /// Finished and not being re-read: the solid 100% bar.
    pub fn done(&self) -> bool {
        self.finished && !self.rereading
    }
}

/// Adds `folder` to `folders`, replacing folders inside it. None when it is
/// already part of the library (listed, or inside a listed folder).
pub fn with_folder(folders: &[PathBuf], folder: &Path) -> Option<Vec<PathBuf>> {
    let folder = normalize(folder);
    if folders.iter().any(|f| folder.starts_with(normalize(f))) {
        return None;
    }
    let mut result: Vec<_> = folders.iter().filter(|f| !normalize(f).starts_with(&folder)).cloned().collect();
    result.push(folder);
    Some(result)
}

fn folder_paths(settings: Settings, override_folders: Option<std::ffi::OsString>) -> Vec<PathBuf> {
    let folders = override_folders
        .map(|value| std::env::split_paths(&value).collect())
        .or(settings.library_folders)
        .or_else(|| settings.library_folder.map(|p| vec![p]))
        .unwrap_or_else(default_library_folders);
    let mut resolved = Vec::new();
    for folder in folders {
        let folder =
            if folder.is_relative() { std::env::current_dir().unwrap_or_default().join(folder) } else { folder };
        if !resolved.contains(&folder) {
            resolved.push(folder);
        }
    }
    distinct_roots(&resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn koreader_seen_round_trips_and_is_left_out_when_unset() -> Result<()> {
        let plain = serde_json::to_string(&BookState::default())?;
        assert!(!plain.contains("koreader"), "{plain}");
        let seen = crate::koreader::Seen { modified: 1763800000, position: Position("/body/p[3]".into()) };
        let state = BookState { koreader_seen: Some(seen.clone()), ..Default::default() };
        let restored: BookState = serde_json::from_str(&serde_json::to_string(&state)?)?;
        assert_eq!(restored.koreader_seen, Some(seen));
        Ok(())
    }
    #[test]
    fn existing_settings_default_to_stack_and_amazon_round_trips() -> Result<()> {
        let mut settings: Settings = serde_json::from_str(r#"{"footer":false}"#)?;
        settings.normalize();
        assert_eq!(settings.sleep_screen, SleepScreen::Cover);
        assert_eq!(settings.folder_preview, FolderPreview::Stack);
        assert!(settings.koreader_progress, "KOReader progress is on by default, in existing files too");
        // Covers is disabled: a saved choice reads as Stack.
        let mut covers: Settings = serde_json::from_str(r#"{"folder_preview":"covers"}"#)?;
        covers.normalize();
        assert_eq!(covers.folder_preview, FolderPreview::Stack);
        assert!(!settings.page_layout.bottom.on);
        settings.sleep_screen = SleepScreen::Amazon;
        let restored: Settings = serde_json::from_slice(&serde_json::to_vec(&settings)?)?;
        assert_eq!(restored.sleep_screen, SleepScreen::Amazon);
        for mode in FolderPreview::ALL {
            settings.folder_preview = mode;
            let restored: Settings = serde_json::from_slice(&serde_json::to_vec(&settings)?)?;
            assert_eq!(restored.folder_preview, mode);
        }
        assert!(!restored.page_layout.bottom.on);
        Ok(())
    }
    #[test]
    fn one_invalid_setting_resets_only_itself() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("verso-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("settings.json");
        std::fs::write(&path, r#"{"sleep_screen":"amazon ","page_flash":7,"developer":true}"#)?;
        let s = read_settings(&path);
        assert_eq!(s.sleep_screen, SleepScreen::Cover);
        assert_eq!(s.page_flash, 7);
        assert!(s.developer);
        std::fs::write(&path, "not json")?;
        assert_eq!(read_settings(&path).page_flash, DEFAULT_PAGE_FLASH);
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
    #[test]
    fn old_footer_and_margins_migrate_once_and_are_not_written() -> Result<()> {
        let old = r#"{"typography":{"font":"Literata","size_pt":11.0,"spacing":0,"margins":2,"justify":false},"footer":false}"#;
        let mut settings: Settings = serde_json::from_str(old)?;
        settings.normalize();
        assert_eq!(settings.page_layout.margins.sides, 10);
        assert!(!settings.page_layout.bottom.on);
        assert_eq!((settings.typography.size_pt, settings.typography.justify), (11.0, false));
        let json = serde_json::to_string(&settings)?;
        assert!(!json.contains("footer") && !json.contains(r#""margins":2"#), "{json}");
        let mut restored: Settings = serde_json::from_str(&json)?;
        restored.normalize();
        assert_eq!(restored.page_layout, settings.page_layout);
        // Normal margins and the footer on are exactly the new defaults.
        let mut normal: Settings = serde_json::from_str(r#"{"typography":{"margins":1},"footer":true}"#)?;
        normal.normalize();
        assert_eq!(
            normal.page_layout,
            PageLayout {
                margins: crate::page_layout::Margins { sides: 7, ..Default::default() },
                ..PageLayout::default()
            }
        );
        // Out-of-range margins are clamped.
        let mut wild: Settings = serde_json::from_str(r#"{"page_layout":{"margins":{"sides":1,"top":50}}}"#)?;
        wild.normalize();
        assert_eq!((wild.page_layout.margins.sides, wild.page_layout.margins.top), (2, 20));
        Ok(())
    }
    #[test]
    fn reader_settings_default_and_round_trip() -> Result<()> {
        let mut settings: Settings = serde_json::from_str(r#"{"footer":true}"#)?;
        assert_eq!((settings.page_flash, settings.tap_zones, settings.bookmark_corner), (0, TapZones::Standard, true));
        settings.page_flash = 6;
        settings.tap_zones = TapZones::Mirrored;
        settings.bookmark_corner = false;
        let json = serde_json::to_string(&settings)?;
        assert!(json.contains(r#""tap_zones":"mirrored""#), "{json}");
        let restored: Settings = serde_json::from_str(&json)?;
        assert_eq!((restored.page_flash, restored.tap_zones, restored.bookmark_corner), (6, TapZones::Mirrored, false));
        Ok(())
    }
    #[test]
    fn text_shaping_defaults_to_light_and_round_trips() -> Result<()> {
        let mut settings: Settings = serde_json::from_str(r#"{"footer":true}"#)?;
        assert_eq!(settings.text_shaping, TextShaping::Light, "existing settings switch to light");
        settings.text_shaping = TextShaping::FreeType;
        let json = serde_json::to_string(&settings)?;
        assert!(json.contains(r#""text_shaping":"freetype""#), "{json}");
        assert_eq!(serde_json::from_str::<Settings>(&json)?.text_shaping, TextShaping::FreeType);
        let full: Settings = serde_json::from_str(r#"{"text_shaping":"full"}"#)?;
        assert_eq!(full.text_shaping, TextShaping::Full);
        Ok(())
    }
    #[test]
    fn adding_folders_skips_covered_ones_and_absorbs_subfolders() {
        let folders = vec![PathBuf::from("/mnt/us/documents"), PathBuf::from("/mnt/us/books/Tolkien")];
        assert_eq!(with_folder(&folders, Path::new("/mnt/us/documents")), None);
        assert_eq!(with_folder(&folders, Path::new("/mnt/us/documents/Fiction")), None);
        assert_eq!(
            with_folder(&folders, Path::new("/mnt/us/books")),
            Some(vec![PathBuf::from("/mnt/us/documents"), PathBuf::from("/mnt/us/books")])
        );
        // A shared prefix that isn't a parent folder is a different folder.
        assert_eq!(with_folder(&folders, Path::new("/mnt/us/doc")).map(|f| f.len()), Some(3));
    }
    #[test]
    fn freezing_stops_every_save_in_that_data_folder_only() -> Result<()> {
        let frozen = tempfile::tempdir()?;
        let other = tempfile::tempdir()?;
        let store = Store::at(frozen.path().to_path_buf());
        let elsewhere = Store::at(other.path().to_path_buf());
        store.save_book("kept", &BookState::default())?;
        assert!(!store.live_data_edited());
        store.clone().note_live_data_edit();
        assert!(store.live_data_edited(), "clones share it");
        store.clone().freeze();
        store.save_settings(&Settings::default())?;
        store.save_library(&LibraryIndex::default())?;
        store.save_navigation(&Navigation::default())?;
        store.save_book("new", &BookState::default())?;
        store.remove_book("kept")?;
        store.save_dictionary_folders(&[])?;
        for name in
            ["settings.json", "library.json", "library-navigation.json", "books/new.json", "dictionary-folders.json"]
        {
            assert!(!frozen.path().join(name).exists(), "{name} was written");
        }
        assert!(frozen.path().join("books/kept.json").exists(), "nor removed");
        elsewhere.save_settings(&Settings::default())?;
        assert!(other.path().join("settings.json").exists(), "another data folder still saves");
        assert!(!elsewhere.live_data_edited());
        Ok(())
    }
    #[test]
    fn settings_are_cached_across_clones_and_updated_on_save() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::at(dir.path().to_path_buf());
        let clone = store.clone();
        std::fs::write(dir.path().join("settings.json"), r#"{"page_flash":12,"hostname":"bad host!"}"#)?;
        let initial = store.settings();
        assert_eq!(initial.page_flash, 12);
        assert_eq!(initial.hostname, verso_mdns::DEFAULT_LABEL);
        // External edits on disk are not re-read once cached.
        std::fs::write(dir.path().join("settings.json"), r#"{"page_flash":3}"#)?;
        assert_eq!(clone.settings().page_flash, 12);
        // Saving updates the cache across clones and normalizes the hostname.
        let updated = Settings { page_flash: 4, hostname: "still bad!".into(), ..initial };
        store.save_settings(&updated)?;
        let read_back = clone.settings();
        assert_eq!(read_back.page_flash, 4);
        assert_eq!(read_back.hostname, verso_mdns::DEFAULT_LABEL);
        // A failed save leaves the cached settings untouched.
        std::fs::remove_file(dir.path().join("settings.json"))?;
        std::fs::create_dir_all(dir.path().join("settings.json"))?;
        let bad = Settings { page_flash: 99, ..read_back };
        assert!(store.save_settings(&bad).is_err());
        assert_eq!(clone.settings().page_flash, 4);
        Ok(())
    }
    #[test]
    fn saving_settings_is_noticed_by_clones() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-store-saves-{}", std::process::id()));
        let store = Store::at(root.clone());
        let other = store.clone();
        let before = other.settings_saves();
        store.save_library_folders(vec!["/books".into()])?;
        assert_ne!(other.settings_saves(), before);
        assert_eq!(other.settings().library_folders, Some(vec![PathBuf::from("/books")]));
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn folder_lists_preserve_spaces_and_override_legacy_settings() {
        let mut settings = Settings { library_folder: Some("/old".into()), ..Default::default() };
        assert_eq!(folder_paths(settings.clone(), None), vec![PathBuf::from("/old")]);
        settings.library_folders =
            Some(vec!["/mnt/us/documents".into(), "/mnt/us/My Library".into(), "/mnt/us/My Library".into()]);
        assert_eq!(
            folder_paths(settings.clone(), None),
            vec![PathBuf::from("/mnt/us/documents"), PathBuf::from("/mnt/us/My Library")]
        );
        assert_eq!(
            folder_paths(settings, Some("/override one:/override two".into())),
            vec![PathBuf::from("/override one"), PathBuf::from("/override two")]
        );
    }
    #[test]
    fn records_survive_interrupted_writes_removed_folders_and_moves() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-store-{}", std::process::id()));
        std::fs::create_dir_all(&root)?;
        let store = Store::at(root.join("data"));
        let s = BookState {
            position: Position("/body/p[5]/text().23".into()),
            page: 6,
            progress: 3210,
            last_opened: 42,
            ..BookState::default()
        };
        store.save_book("abc", &s)?;
        // Saves remember the folder; one removed since is made again.
        std::fs::remove_dir_all(store.book_path("abc").parent().unwrap())?;
        store.save_book("abc", &s)?;
        std::fs::write(store.book_path("abc").with_extension("json.tmp"), b"{broken")?;
        assert_eq!(store.book("abc").position, s.position);
        assert_eq!(store.book("absent").progress, 0);
        store.save_settings(&Settings::default())?;
        assert_eq!(store.settings().typography, Typography::default());
        let a = root.join("a.epub");
        std::fs::write(&a, b"book")?;
        let id = book_id(&a)?;
        std::fs::create_dir_all(root.join("moved"))?;
        let b = root.join("moved/a.epub");
        std::fs::rename(&a, &b)?;
        assert_eq!(id, book_id(&b)?);
        std::fs::write(&b, b"different edition")?;
        assert_ne!(id, book_id(&b)?);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
