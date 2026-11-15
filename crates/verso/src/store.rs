//! Small, versioned JSON records. Rename replaces a complete file atomically.
mod dictionary_folders;
mod home;
pub mod position;
use crate::document::{Position, Typography};
use crate::library::collections::{Navigation, distinct_roots, normalize};
use crate::library::{LibraryIndex, Sort};
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
    pub footer: bool,
    pub sleep_screen: SleepScreen,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub library_folder: Option<PathBuf>, // Legacy single-folder setting.
    pub library_folders: Option<Vec<PathBuf>>,
    pub library_sort: Sort,
    /// Page turns between full flashes that clear ghosting; 0 never flashes
    /// periodically (chapter starts and pictures still do).
    pub page_flash: u32,
    pub tap_zones: TapZones,
    pub orientation: Orientation,
    /// Developer options in Settings (tap the version five times).
    pub developer: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            typography: Typography::default(),
            footer: true,
            sleep_screen: SleepScreen::Cover,
            library_folder: None,
            library_folders: None,
            library_sort: Sort::Recent,
            page_flash: DEFAULT_PAGE_FLASH,
            tap_zones: TapZones::Standard,
            orientation: Orientation::Portrait,
            developer: false,
        }
    }
}
pub const DEFAULT_PAGE_FLASH: u32 = 6;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SleepScreen {
    #[default]
    Cover,
    Amazon,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BookState {
    pub position: Position,
    pub page: usize,
    pub progress: u32,
    pub last_opened: u64,
    pub finished: bool,
    pub hidden_from_continue: bool,
}
#[derive(Clone)]
pub struct Store {
    root: PathBuf,
    selected_home: std::sync::Arc<std::sync::Mutex<Option<crate::library::Book>>>,
    /// Counts settings saves through this store and its clones, so screens
    /// can notice a change without rereading the file.
    settings_saves: std::sync::Arc<std::sync::atomic::AtomicU64>,
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
        Self { root, selected_home: Default::default(), settings_saves: Default::default() }
    }
    pub fn settings(&self) -> Settings {
        let mut s: Settings = read(&self.root.join("settings.json")).unwrap_or_default();
        s.typography.normalize();
        s
    }
    pub fn save_settings(&self, s: &Settings) -> Result<()> {
        let result = atomic(&self.root.join("settings.json"), s);
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
        atomic(&self.root.join("library-navigation.json"), navigation)
    }
    pub fn book(&self, id: &str) -> BookState {
        read(&self.book_path(id)).unwrap_or_default()
    }
    pub fn save_book(&self, id: &str, s: &BookState) -> Result<()> {
        let started = std::time::Instant::now();
        let result = atomic(&self.book_path(id), s);
        log::debug!("position write: {:?}, ok={}", started.elapsed(), result.is_ok());
        if result.is_ok()
            && let Err(error) = self.update_home_after_save(id, s)
        {
            // A rebuildable display cache must never make a durable position
            // look failed, cause an old position to replay, or erase book state.
            log::warn!("updating Home summary: {error:#}");
        }
        result
    }
    /// Removes a deleted book's reading state. A missing record is fine.
    pub fn remove_book(&self, id: &str) -> Result<()> {
        match std::fs::remove_file(self.book_path(id)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
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
        log::info!(
            "startup stage: index persistence books={} elapsed_us={} ok={}",
            index.books.len(),
            started.elapsed().as_micros(),
            result.is_ok()
        );
        result
    }
    pub fn cover_path(&self, key: &str) -> PathBuf {
        self.root.join("covers").join(format!("{key}.png"))
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
    pub fn dictionary_cache(&self) -> PathBuf {
        self.root.join("dictionary-cache")
    }
    pub fn dictionary_preferences(&self) -> PathBuf {
        self.root.join("dictionaries.json")
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
        Self { root, selected_home: Default::default(), settings_saves: Default::default() }
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
pub(crate) fn atomic_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("record has no parent directory")?;
    std::fs::create_dir_all(parent)?;
    let tmp = path.with_extension("json.tmp");
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)?;
    // The FAT/FUSE filesystem on the Kindle may not support directory fsync.
    match std::fs::File::open(parent)?.sync_all() {
        Ok(()) => {}
        Err(e) if matches!(e.raw_os_error(), Some(libc::EINVAL | libc::ENOSYS)) => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
pub fn default_library_folders() -> Vec<PathBuf> {
    #[cfg(feature = "kindle")]
    {
        vec![PathBuf::from("/mnt/us/documents"), PathBuf::from("/mnt/us/My Library")]
    }
    #[cfg(feature = "desktop")]
    {
        vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testbooks")]
    }
}

impl BookState {
    pub fn presented(&mut self, position: Position, page: usize, progress: u32, now: u64, fresh_open: bool) {
        self.position = position;
        self.page = page;
        self.progress = progress;
        self.last_opened = now;
        if fresh_open {
            self.finished = false;
            self.hidden_from_continue = false;
        }
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
    fn existing_settings_default_to_covers_and_amazon_round_trips() -> Result<()> {
        let mut settings: Settings = serde_json::from_str(r#"{"footer":false}"#)?;
        assert_eq!(settings.sleep_screen, SleepScreen::Cover);
        assert!(!settings.footer);
        settings.sleep_screen = SleepScreen::Amazon;
        let restored: Settings = serde_json::from_slice(&serde_json::to_vec(&settings)?)?;
        assert_eq!(restored.sleep_screen, SleepScreen::Amazon);
        assert!(!restored.footer);
        Ok(())
    }
    #[test]
    fn reader_settings_default_and_round_trip() -> Result<()> {
        let mut settings: Settings = serde_json::from_str(r#"{"footer":true}"#)?;
        assert_eq!((settings.page_flash, settings.tap_zones), (6, TapZones::Standard));
        settings.page_flash = 0;
        settings.tap_zones = TapZones::Mirrored;
        let json = serde_json::to_string(&settings)?;
        assert!(json.contains(r#""tap_zones":"mirrored""#), "{json}");
        let restored: Settings = serde_json::from_str(&json)?;
        assert_eq!((restored.page_flash, restored.tap_zones), (0, TapZones::Mirrored));
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
    fn records_survive_interrupted_writes_and_moves() -> Result<()> {
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
