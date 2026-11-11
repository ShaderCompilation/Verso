//! Small, versioned JSON records. Rename replaces a complete file atomically.
use crate::document::{Position, Typography};
use crate::library::{LibraryIndex, Sort};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub typography: Typography,
    pub footer: bool,
    pub library_folder: Option<PathBuf>,
    pub library_sort: Sort,
}
impl Default for Settings {
    fn default() -> Self {
        Self { typography: Typography::default(), footer: true, library_folder: None, library_sort: Sort::Recent }
    }
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
}
#[allow(dead_code)] // Library worker is wired in the next Phase 5 steps.
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
        Self { root }
    }
    pub fn settings(&self) -> Settings {
        let mut s: Settings = read(&self.root.join("settings.json")).unwrap_or_default();
        s.typography.normalize();
        s
    }
    pub fn save_settings(&self, s: &Settings) -> Result<()> {
        atomic(&self.root.join("settings.json"), s)
    }
    pub fn book(&self, id: &str) -> BookState {
        read(&self.book_path(id)).unwrap_or_default()
    }
    pub fn save_book(&self, id: &str, s: &BookState) -> Result<()> {
        atomic(&self.book_path(id), s)
    }
    pub fn library(&self) -> LibraryIndex {
        read::<LibraryIndex>(&self.root.join("library.json"))
            .filter(|index| index.version == LibraryIndex::VERSION)
            .unwrap_or_default()
    }
    pub fn save_library(&self, index: &LibraryIndex) -> Result<()> {
        atomic(&self.root.join("library.json"), index)
    }
    pub fn cover_path(&self, key: &str) -> PathBuf {
        self.root.join("covers").join(format!("{key}.png"))
    }
    pub fn library_folder(&self) -> PathBuf {
        std::env::var_os("VERSO_BOOKS")
            .map(PathBuf::from)
            .or_else(|| self.settings().library_folder)
            .unwrap_or_else(default_library_folder)
    }
    #[cfg(test)]
    pub fn at(root: PathBuf) -> Self {
        Self { root }
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
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn records_survive_interrupted_writes_and_moves() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-store-{}", std::process::id()));
        std::fs::create_dir_all(&root)?;
        let store = Store { root: root.join("data") };
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

pub fn default_library_folder() -> PathBuf {
    #[cfg(feature = "kindle")]
    {
        PathBuf::from("/mnt/us/documents")
    }
    #[cfg(feature = "desktop")]
    {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testbooks")
    }
}
