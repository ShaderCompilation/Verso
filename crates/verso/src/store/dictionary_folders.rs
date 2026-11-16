//! External dictionary folders, owned separately from `settings.json` and
//! `dictionaries.json`. Verso's native folder is always searched and is never
//! listed here. Writes are synchronous: the file is tiny and only written on a
//! deliberate tap.
use super::{Store, atomic};
use crate::library::collections::normalize;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// KOReader's standard StarDict folder, the Kindle default and the Settings preset.
pub const KOREADER_DICTIONARY_FOLDER: &str = "/mnt/us/koreader/data/dict";
pub const MAX_DICTIONARY_FOLDERS: usize = 64;
const VERSION: u32 = 1;
const MAX_BYTES: u64 = 64 * 1024;
const FILE: &str = "dictionary-folders.json";
// One writer per process, also across independent Store handles for one root.
static WRITE: Mutex<()> = Mutex::new(());
/// Counts save attempts in this process, so screens and retained dictionary
/// workers can notice a change without rereading the file.
static SAVES: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize, Deserialize)]
struct Record {
    version: u32,
    folders: Vec<PathBuf>,
}

/// The configured external folders. When the file is invalid, `folders` is
/// empty (native-only) and `error` says why; the file is left untouched.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DictionaryFolders {
    pub folders: Vec<PathBuf>,
    pub error: Option<String>,
}

pub fn default_dictionary_folders() -> Vec<PathBuf> {
    #[cfg(feature = "kindle")]
    {
        vec![PathBuf::from(KOREADER_DICTIONARY_FOLDER)]
    }
    #[cfg(feature = "desktop")]
    {
        Vec::new()
    }
}

impl Store {
    pub fn dictionary_folders(&self) -> DictionaryFolders {
        load(&self.root.join(FILE), default_dictionary_folders)
    }
    /// Replaces the external folder list atomically. An empty list means
    /// native-only and is kept as such (defaults are never reseeded).
    pub fn save_dictionary_folders(&self, folders: &[PathBuf]) -> Result<()> {
        let folders = clean(folders.iter().cloned())?;
        let _guard = WRITE.lock().unwrap_or_else(|e| e.into_inner());
        let result = atomic(&self.root.join(FILE), &Record { version: VERSION, folders });
        SAVES.fetch_add(1, Ordering::Relaxed);
        self.dictionary_folder_revision.fetch_add(1, Ordering::Relaxed);
        result
    }
    /// Changes whenever the folder list was saved (or a save was attempted);
    /// compare to notice edits.
    pub fn dictionary_folder_saves() -> u64 {
        SAVES.load(Ordering::Relaxed)
    }
}

fn load(path: &Path, defaults: impl FnOnce() -> Vec<PathBuf>) -> DictionaryFolders {
    match parse(path) {
        Ok(Some(folders)) => DictionaryFolders { folders, error: None },
        Ok(None) => DictionaryFolders { folders: defaults(), error: None },
        Err(e) => {
            let error = format!("{e:#}");
            log::warn!("dictionary folders: {error}");
            DictionaryFolders { folders: Vec::new(), error: Some(error) }
        }
    }
}
/// `None` when the file does not exist.
fn parse(path: &Path) -> Result<Option<Vec<PathBuf>>> {
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        file => file.with_context(|| format!("Cannot read {}", path.display()))?,
    };
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= MAX_BYTES, "{} is too large", path.display());
    let record: Record = serde_json::from_slice(&bytes).with_context(|| format!("Invalid {}", path.display()))?;
    ensure!(record.version == VERSION, "Unsupported {} version {}", path.display(), record.version);
    clean(record.folders).map(Some).with_context(|| format!("Invalid {}", path.display()))
}
/// Normalizes, rejects relative paths and too many folders, and drops exact
/// duplicates. Parent/child folders are deliberately kept: removing a parent
/// must leave an explicitly added child.
fn clean(folders: impl IntoIterator<Item = PathBuf>) -> Result<Vec<PathBuf>> {
    let mut result: Vec<PathBuf> = Vec::new();
    for folder in folders {
        ensure!(folder.is_absolute(), "Dictionary folder is not an absolute path: {}", folder.display());
        let folder = normalize(&folder);
        if !result.contains(&folder) {
            result.push(folder);
        }
    }
    ensure!(result.len() <= MAX_DICTIONARY_FOLDERS, "More than {MAX_DICTIONARY_FOLDERS} dictionary folders");
    Ok(result)
}

/// The index of a configured folder that is the same location as `folder`:
/// the same normalized path, or the same real directory when both exist.
/// Missing folders are compared by spelling only.
pub fn find_dictionary_folder(folders: &[PathBuf], folder: &Path) -> Option<usize> {
    let folder = normalize(folder);
    if let Some(i) = folders.iter().position(|f| normalize(f) == folder) {
        return Some(i);
    }
    let real = std::fs::canonicalize(&folder).ok()?;
    folders.iter().position(|f| std::fs::canonicalize(f).is_ok_and(|r| r == real))
}

/// `folders` plus `folder`, or `Err(index)` of the row that already names it.
pub fn with_dictionary_folder(folders: &[PathBuf], folder: &Path) -> std::result::Result<Vec<PathBuf>, usize> {
    if let Some(i) = find_dictionary_folder(folders, folder) {
        return Err(i);
    }
    let mut result = folders.to_vec();
    result.push(normalize(folder));
    Ok(result)
}

/// `folders` without `folder`, compared by normalized spelling so a missing
/// folder can be removed without resolving it. Other folders, including
/// children of the removed one, stay.
pub fn without_dictionary_folder(folders: &[PathBuf], folder: &Path) -> Vec<PathBuf> {
    let folder = normalize(folder);
    folders.iter().filter(|f| normalize(f) != folder).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    fn temp() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "verso-dictionary-folders-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }
    fn kindle() -> Vec<PathBuf> {
        vec![PathBuf::from(KOREADER_DICTIONARY_FOLDER)]
    }

    #[test]
    fn missing_file_uses_defaults_and_empty_list_is_native_only() -> Result<()> {
        let root = temp();
        let path = root.join(FILE);
        assert_eq!(load(&path, kindle), DictionaryFolders { folders: kindle(), error: None });
        assert_eq!(Store::at(root.clone()).dictionary_folders().folders, default_dictionary_folders());
        let store = Store::at(root.clone());
        store.save_dictionary_folders(&[])?;
        assert_eq!(load(&path, kindle), DictionaryFolders::default(), "removed KOReader is not reseeded");
        store.save_dictionary_folders(&["/a/./b/../c".into(), "/a/c".into(), "/a".into()])?;
        assert_eq!(load(&path, kindle).folders, vec![PathBuf::from("/a/c"), PathBuf::from("/a")]);
        assert!(!root.join("dictionary-folders.json.tmp").exists());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn invalid_unsupported_and_oversized_files_are_reported_and_untouched() -> Result<()> {
        let root = temp();
        let path = root.join(FILE);
        let many = format!(
            r#"{{"version":1,"folders":[{}]}}"#,
            (0..=MAX_DICTIONARY_FOLDERS).map(|i| format!("\"/d{i}\"")).collect::<Vec<_>>().join(",")
        );
        let cases = [
            ("{broken".to_owned(), "Invalid"),
            (r#"{"version":2,"folders":[]}"#.to_owned(), "version 2"),
            (r#"{"folders":["/a"]}"#.to_owned(), "Invalid"),
            (r#"{"version":1,"folders":["relative/dict"]}"#.to_owned(), "absolute"),
            (many, "More than"),
            (format!(r#"{{"version":1,"folders":["/a"],"pad":"{}"}}"#, "x".repeat(70000)), "too large"),
        ];
        for (text, message) in cases {
            std::fs::write(&path, &text)?;
            let loaded = load(&path, kindle);
            assert!(loaded.folders.is_empty(), "{message}: native only, defaults not used");
            let error = loaded.error.unwrap_or_default();
            assert!(error.contains(message), "{message}: {error}");
            assert_eq!(std::fs::read_to_string(&path)?, text, "left untouched");
        }
        assert!(Store::at(root.clone()).save_dictionary_folders(&["relative".into()]).is_err());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn adding_recognizes_spellings_and_aliases_removing_keeps_children() -> Result<()> {
        let root = temp();
        let real = root.join("real");
        std::fs::create_dir_all(real.join("child"))?;
        std::os::unix::fs::symlink(&real, root.join("alias"))?;
        let folders = vec![real.clone(), root.join("gone")];
        assert_eq!(with_dictionary_folder(&folders, &root.join("real/./child/..")), Err(0));
        assert_eq!(with_dictionary_folder(&folders, &root.join("alias")), Err(0), "same real directory");
        assert_eq!(with_dictionary_folder(&folders, &root.join("x/../gone")), Err(1), "missing, by spelling");
        let added = with_dictionary_folder(&folders, &real.join("child")).unwrap();
        assert_eq!(added.len(), 3, "a child of a configured folder can be added");
        let removed = without_dictionary_folder(&added, &root.join("real/"));
        assert_eq!(removed, vec![root.join("gone"), real.join("child")], "child stays configured");
        assert_eq!(without_dictionary_folder(&removed, &root.join("gone")), vec![real.join("child")]);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
