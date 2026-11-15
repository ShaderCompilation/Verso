//! Local folder operations. Symlinks are counted as files and never traversed.
use super::LibraryIndex;
use crate::store::Store;
use anyhow::{Context, Result, bail, ensure};
use std::path::{Path, PathBuf};

pub fn validate_name(parent: &Path, name: &str) -> Result<PathBuf> {
    let name = name.trim();
    ensure!(!name.is_empty(), "Enter a folder name");
    ensure!(!name.starts_with('.'), "Names cannot begin with a dot");
    ensure!(!name.contains('/'), "Names cannot contain /");
    ensure!(!name.contains('\0'), "Names cannot contain a null character");
    ensure!(!name.ends_with(".sdr"), "Names ending in .sdr are reserved for reading data");
    let target = parent.join(name);
    match std::fs::symlink_metadata(&target) {
        Ok(_) => bail!("That name is already taken"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(target)
}

pub fn create_folder(parent: &Path, name: &str) -> Result<PathBuf> {
    let path = validate_name(parent, name)?;
    std::fs::create_dir(&path).context("Couldn't create folder")?;
    Ok(path)
}

pub fn rename_folder(path: &Path, name: &str) -> Result<PathBuf> {
    let parent = path.parent().context("Cannot rename a library root")?;
    if path.file_name().is_some_and(|old| old == name.trim()) {
        return Ok(path.to_owned());
    }
    let target = validate_name(parent, name)?;
    ensure!(std::fs::symlink_metadata(path)?.is_dir(), "This is no longer a folder");
    std::fs::rename(path, &target).context("Couldn't rename folder")?;
    Ok(target)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Contents {
    pub books: usize,
    pub folders: usize,
    pub other_files: usize,
}
pub fn count_contents(path: &Path) -> Result<Contents> {
    let mut result = Contents::default();
    fn walk(path: &Path, result: &mut Contents) -> Result<()> {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                result.folders += 1;
                walk(&entry.path(), result)?;
            } else if entry.file_type()?.is_file()
                && entry.path().extension().is_some_and(|ext| ext.eq_ignore_ascii_case("epub"))
            {
                result.books += 1;
            } else {
                result.other_files += 1;
            }
        }
        Ok(())
    }
    walk(path, &mut result).context("Couldn't count folder contents")?;
    Ok(result)
}

/// The caller removes the returned ids from its view; covers belong to the worker.
/// State cleanup follows successful removal, so a failed delete keeps progress.
pub fn delete_folder(path: &Path, index: &LibraryIndex, store: &Store) -> Result<Vec<String>> {
    std::fs::remove_dir_all(path).context("Couldn't delete folder")?;
    let ids: Vec<_> = index.books.iter().filter(|b| b.path.starts_with(path)).map(|b| b.id.clone()).collect();
    for id in &ids {
        if let Err(e) = store.remove_book(id) {
            log::warn!("removing deleted book state {id}: {e}");
        }
    }
    Ok(ids)
}

/// Dotfiles do not make a folder nonempty. Any visible entry, including a
/// sidecar or unsupported book, does. Inaccessible folders are never called empty.
pub fn is_empty(path: &Path) -> bool {
    std::fs::read_dir(path).is_ok_and(|mut entries| {
        entries.all(|entry| entry.is_ok_and(|e| e.file_name().to_string_lossy().starts_with('.')))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Position;
    use crate::library::worker::scan;
    use crate::store::BookState;

    #[test]
    fn names_operations_counts_and_reading_identity() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-folder-files-{}", std::process::id()));
        std::fs::create_dir_all(&root)?;
        for name in ["", "  ", ".", "..", ".hidden", "a/b", "null\0", "book.sdr"] {
            assert!(validate_name(&root, name).is_err(), "{name:?}");
        }
        let folder = create_folder(&root, "  Толкин  ")?;
        assert_eq!(folder.file_name().unwrap(), "Толкин");
        assert!(create_folder(&root, "Толкин").is_err());
        let occupied = create_folder(&root, "Occupied")?;
        assert!(rename_folder(&folder, "Occupied").is_err());
        assert!(occupied.exists(), "an empty destination must never be replaced");
        std::fs::create_dir(folder.join("nested"))?;
        std::fs::write(folder.join("nested/Book.EPUB"), b"book")?;
        std::fs::write(folder.join("paper.pdf"), b"pdf")?;
        std::fs::create_dir(folder.join("Book.sdr"))?;
        std::fs::write(folder.join("Book.sdr/metadata.epub.lua"), b"progress")?;
        let contents = count_contents(&folder)?;
        assert_eq!(contents, Contents { books: 1, folders: 2, other_files: 2 });
        let store = Store::at(root.join("state"));
        let state = BookState { position: Position("exact-anchor".into()), progress: 4200, ..Default::default() };
        store.save_book("7", &state)?;
        let mut b = scan(&folder, &Default::default(), false, &|| false, |_| {}).unwrap().index.books.remove(0);
        b.id = "7".into();
        let mut index = LibraryIndex { books: vec![b], ..Default::default() };
        let renamed = rename_folder(&folder, "Tolkien")?;
        index.move_path(&folder, &renamed);
        assert_eq!(index.books[0].id, "7");
        assert_eq!(store.book("7").position, state.position);
        assert_eq!(store.book("7").progress, 4200);
        let rescanned = scan(&root, &index, false, &|| false, |_| {}).unwrap().index;
        assert_eq!(rescanned.books.iter().find(|b| b.available).unwrap().id, "7");
        assert!(delete_folder(&root.join("missing"), &index, &store).is_err());
        assert_eq!(store.book("7").progress, 4200, "a failed removal retains progress");
        assert_eq!(delete_folder(&renamed, &index, &store)?, ["7"]);
        index.remove_path(&renamed);
        assert!(index.books.is_empty() && !renamed.exists());
        assert_eq!(store.book("7").progress, 0);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
