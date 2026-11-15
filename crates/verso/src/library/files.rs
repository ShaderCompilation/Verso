//! Local folder operations. Symlinks are counted as files and never traversed.
use super::LibraryIndex;
use crate::store::Store;
use anyhow::{Context, Result, bail, ensure};
use std::path::{Path, PathBuf};

const WINDOWS_RESERVED: [char; 8] = ['\\', ':', '*', '?', '"', '<', '>', '|'];

pub fn validate_name(parent: &Path, name: &str) -> Result<PathBuf> {
    let name = name.trim();
    ensure!(!name.is_empty(), "Enter a folder name");
    ensure!(!name.starts_with('.'), "Names cannot begin with a dot");
    ensure!(!name.contains('/'), "Names cannot contain /");
    ensure!(!name.chars().any(char::is_control), "Names cannot contain control characters");
    // Linux allows these, but a Windows computer reading the Kindle over USB
    // can't show or copy a folder whose name has them.
    ensure!(!name.contains(WINDOWS_RESERVED), "Names cannot contain \\ : * ? \" < > |");
    ensure!(!name.ends_with(".sdr"), "Names ending in .sdr are reserved for reading data");
    let target = parent.join(name);
    match std::fs::symlink_metadata(&target) {
        Ok(_) => bail!("That name is already taken"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(target)
}

/// Shown instead of a raw "No such file or directory" when another device or
/// app removed the folder meanwhile.
const GONE: &str = "This folder no longer exists";

pub fn create_folder(parent: &Path, name: &str) -> Result<PathBuf> {
    let path = validate_name(parent, name)?;
    match std::fs::create_dir(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!(GONE),
        result => result.context("Couldn't create folder")?,
    }
    Ok(path)
}

pub fn rename_folder(path: &Path, name: &str) -> Result<PathBuf> {
    let parent = path.parent().context("Cannot rename a library root")?;
    if path.file_name().is_some_and(|old| old == name.trim()) {
        return Ok(path.to_owned());
    }
    let target = validate_name(parent, name)?;
    match std::fs::symlink_metadata(path) {
        Ok(meta) => ensure!(meta.is_dir(), "This is no longer a folder"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!(GONE),
        Err(e) => return Err(e).context("Couldn't rename folder"),
    }
    std::fs::rename(path, &target).context("Couldn't rename folder")?;
    Ok(target)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Contents {
    pub books: usize,
    pub folders: usize,
    pub other_files: usize,
}
/// What a delete confirmation names. Hidden entries and KOReader sidecars are
/// deleted with the folder but not counted: sidecars are the books' reading
/// progress, and "other files" should point at PDFs and Kindle books.
pub fn count_contents(path: &Path) -> Result<Contents> {
    let mut result = Contents::default();
    fn walk(path: &Path, result: &mut Contents) -> Result<()> {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            if entry.file_type()?.is_dir() {
                if name.ends_with(".sdr") {
                    continue;
                }
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

/// What a folder delete removed. `remove_dir_all` can stop partway, so a
/// failed delete still lists the indexed books whose files are already gone.
#[derive(Debug)]
pub struct Deletion {
    /// Ids and paths of the removed books; their reading state is gone too.
    pub books: Vec<(String, PathBuf)>,
    pub error: Option<anyhow::Error>,
}

/// The caller removes the returned books from its view; covers belong to the
/// worker. Only books whose files are gone lose their reading state, so a book
/// that a failed delete left in place keeps its progress.
pub fn delete_folder(path: &Path, index: &LibraryIndex, store: &Store) -> Deletion {
    let error = std::fs::remove_dir_all(path).context("Couldn't delete folder").err();
    let books: Vec<_> = index
        .books
        .iter()
        .filter(|b| b.path.starts_with(path))
        .filter(|b| {
            error.is_none()
                || std::fs::symlink_metadata(&b.path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        })
        .map(|b| (b.id.clone(), b.path.clone()))
        .collect();
    for (id, _) in &books {
        if let Err(e) = store.remove_book(id) {
            log::warn!("removing deleted book state {id}: {e}");
        }
    }
    Deletion { books, error }
}

/// A batch uses filesystem paths captured by the confirmation; it never deletes
/// newly discovered items merely because a list changed in the meantime.
#[derive(Clone, Debug)]
pub enum DeleteItem {
    Book { id: String, path: PathBuf },
    Folder(PathBuf),
}
impl DeleteItem {
    pub fn book(book: &super::Book) -> Self {
        Self::Book { id: book.id.clone(), path: book.path.clone() }
    }
    fn path(&self) -> &Path {
        match self {
            Self::Book { path, .. } => path,
            Self::Folder(path) => path,
        }
    }
}

pub fn selection_contents(items: &[DeleteItem]) -> Result<Contents> {
    let mut total = Contents::default();
    for item in items {
        match item {
            DeleteItem::Book { .. } => total.books += 1,
            DeleteItem::Folder(path) => {
                let contents = count_contents(path)?;
                total.books += contents.books;
                total.folders += contents.folders + 1;
                total.other_files += contents.other_files;
            }
        }
    }
    Ok(total)
}

#[derive(Default)]
pub struct BatchDeletion {
    pub deleted: usize,
    pub removed_books: Vec<String>,
    pub changes: Vec<super::PathChange>,
    pub errors: Vec<String>,
    /// Deleted books whose KOReader sidecar stayed behind. Like a single book
    /// delete, these count as deleted: the book itself is gone.
    pub sidecar_errors: Vec<String>,
}
/// Continue after failures and return one index batch, including books already
/// removed by a partially failed folder deletion. Parents are checked only after
/// all items, so deleting their last children keeps them visible immediately.
pub fn delete_selection(items: &[DeleteItem], index: &LibraryIndex, store: &Store) -> BatchDeletion {
    let mut result = BatchDeletion::default();
    let mut parents = std::collections::BTreeSet::new();
    for item in items {
        let path = item.path();
        if let Some(parent) = path.parent() {
            parents.insert(parent.to_owned());
        }
        let error = match item {
            DeleteItem::Book { id, .. } => match std::fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Some(anyhow::Error::from(e)),
                _ => {
                    result.changes.push(super::PathChange::Remove(path.to_owned()));
                    result.removed_books.push(id.clone());
                    if let Err(e) = delete_sidecar(path) {
                        log::warn!("deleting sidecar of {}: {e:#}", path.display());
                        result.sidecar_errors.push(format!("{}: {e:#}", path.display()));
                    }
                    // The worker retries this for every removed book.
                    if let Err(e) = store.remove_book(id) {
                        log::warn!("removing reading state of {}: {e:#}", path.display());
                    }
                    None
                }
            },
            DeleteItem::Folder(path) => {
                let deletion = delete_folder(path, index, store);
                result.removed_books.extend(deletion.books.iter().map(|(id, _)| id.clone()));
                if deletion.error.is_none() {
                    result.changes.push(super::PathChange::Remove(path.clone()));
                } else {
                    result.changes.extend(deletion.books.into_iter().map(|(_, p)| super::PathChange::Remove(p)));
                    result.changes.extend(
                        index
                            .empty_folders
                            .iter()
                            .filter(|p| p.starts_with(path))
                            .filter(|p| {
                                std::fs::symlink_metadata(p).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                            })
                            .cloned()
                            .map(super::PathChange::Remove),
                    );
                }
                deletion.error
            }
        };
        if let Some(error) = error {
            result.errors.push(format!("{}: {error:#}", path.display()));
        } else {
            result.deleted += 1;
        }
    }
    result.changes.extend(parents.into_iter().filter(|p| is_empty(p)).map(super::PathChange::AddEmpty));
    result
}

/// KOReader keeps a book's progress and highlights in `Name.sdr` beside it.
pub fn sidecar(book: &Path) -> PathBuf {
    book.with_extension("sdr")
}

/// Removes a deleted book's KOReader sidecar, if it has one.
pub fn delete_sidecar(book: &Path) -> Result<()> {
    let path = sidecar(book);
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(&path).context("Couldn't remove KOReader data"),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).context("Couldn't remove KOReader data"),
    }
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
    fn batch_delete_continues_after_failure_and_keeps_survivor_progress() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-batch-delete-{}", std::process::id()));
        std::fs::create_dir_all(root.join("blocked.epub"))?;
        std::fs::write(root.join("good.epub"), b"book")?;
        std::fs::create_dir_all(root.join("Folder"))?;
        std::fs::write(root.join("Folder/nested.epub"), b"book")?;
        // A sidecar that can't be removed: its locked folder holds a file.
        std::fs::write(root.join("kept.epub"), b"book")?;
        let locked = root.join("kept.sdr/locked");
        std::fs::create_dir_all(&locked)?;
        std::fs::write(locked.join("metadata.lua"), b"data")?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555))?;
        let mut books: Vec<_> = ["blocked.epub", "good.epub", "Folder/nested.epub", "kept.epub"]
            .iter()
            .enumerate()
            .map(|(i, path)| {
                let mut book = crate::library::selection_tests::book(i);
                book.path = root.join(path);
                book
            })
            .collect();
        let store = Store::at(root.join("state"));
        for book in &books {
            store.save_book(&book.id, &BookState { progress: 5200, ..Default::default() })?;
        }
        let mut index = LibraryIndex { books: books.clone(), ..Default::default() };
        let kept = books.pop().unwrap();
        let items = vec![
            DeleteItem::book(&books.remove(0)),
            DeleteItem::Folder(root.join("Folder")),
            DeleteItem::book(&books.remove(0)),
            DeleteItem::book(&kept),
        ];
        assert_eq!(selection_contents(&items)?, Contents { books: 4, folders: 1, other_files: 0 });
        let result = delete_selection(&items, &index, &store);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755))?;
        assert_eq!(result.deleted, 3, "a book whose sidecar stayed behind is still deleted");
        assert_eq!(result.errors.len(), 1);
        assert!(result.errors[0].contains("blocked.epub"));
        assert_eq!(result.sidecar_errors.len(), 1);
        assert!(result.sidecar_errors[0].contains("kept.epub"));
        assert!(!root.join("kept.epub").exists());
        assert_eq!(result.removed_books.len(), 3);
        for change in result.changes {
            change.apply(&mut index);
        }
        assert_eq!(index.books.len(), 1);
        assert_eq!(index.books[0].id, "0");
        assert_eq!(store.book("0").progress, 5200);
        assert_eq!(store.book("1").progress, 0);
        assert_eq!(store.book("2").progress, 0);
        assert_eq!(store.book("3").progress, 0);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn names_operations_counts_and_reading_identity() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-folder-files-{}", std::process::id()));
        std::fs::create_dir_all(&root)?;
        for name in ["", "  ", ".", "..", ".hidden", "a/b", "null\0", "a\tb", "book.sdr", "a:b", "why?", "a\\b", "x|y"]
        {
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
        std::fs::write(folder.join(".DS_Store"), b"hidden")?;
        std::fs::create_dir(folder.join(".git"))?;
        std::fs::write(folder.join(".git/config"), b"hidden")?;
        let contents = count_contents(&folder)?;
        // Sidecars and hidden entries are deleted but not named in the message.
        assert_eq!(contents, Contents { books: 1, folders: 1, other_files: 1 });
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
        assert_eq!(format!("{:#}", rename_folder(&root.join("missing"), "New").unwrap_err()), GONE);
        assert_eq!(format!("{:#}", create_folder(&root.join("missing"), "New").unwrap_err()), GONE);
        let missing = delete_folder(&root.join("missing"), &index, &store);
        assert!(missing.error.is_some() && missing.books.is_empty());
        assert_eq!(store.book("7").progress, 4200, "a failed removal retains progress");
        let deletion = delete_folder(&renamed, &index, &store);
        assert!(deletion.error.is_none());
        assert_eq!(deletion.books.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["7"]);
        index.remove_path(&renamed);
        assert!(index.books.is_empty() && !renamed.exists());
        assert_eq!(store.book("7").progress, 0);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn a_partial_delete_forgets_only_the_books_already_gone() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("verso-partial-delete-{}", std::process::id()));
        let folder = root.join("Folder");
        let locked = folder.join("locked");
        std::fs::create_dir_all(&locked)?;
        // "gone.epub" models a book removed before the delete stopped.
        std::fs::write(locked.join("stays.epub"), b"book")?;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555))?;
        let book = |id: &str, path: PathBuf| super::super::Book {
            id: id.into(),
            path,
            ..crate::library::selection_tests::book(0)
        };
        let index = LibraryIndex {
            books: vec![book("gone", folder.join("gone.epub")), book("stays", locked.join("stays.epub"))],
            ..Default::default()
        };
        let store = Store::at(root.join("state"));
        let state = BookState { progress: 1200, ..Default::default() };
        store.save_book("gone", &state)?;
        store.save_book("stays", &state)?;
        let deletion = delete_folder(&folder, &index, &store);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755))?;
        // Permissions don't stop root, so there is no partial delete to check.
        if deletion.error.is_some() {
            assert_eq!(deletion.books, [("gone".to_owned(), folder.join("gone.epub"))]);
            assert_eq!(store.book("gone").progress, 0);
            assert_eq!(store.book("stays").progress, 1200, "a book left in place keeps its progress");
        }
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn sidecars_are_removed_with_their_book_only() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-sidecar-{}", std::process::id()));
        std::fs::create_dir_all(root.join("Book.sdr"))?;
        std::fs::write(root.join("Book.sdr/metadata.epub.lua"), b"progress")?;
        std::fs::create_dir_all(root.join("Other.sdr"))?;
        assert_eq!(sidecar(&root.join("Book.epub")), root.join("Book.sdr"));
        delete_sidecar(&root.join("Book.epub"))?;
        assert!(!root.join("Book.sdr").exists() && root.join("Other.sdr").exists());
        delete_sidecar(&root.join("Missing.epub"))?;
        // A plain file with the sidecar's name is not KOReader data.
        std::fs::write(root.join("Note.sdr"), b"file")?;
        delete_sidecar(&root.join("Note.epub"))?;
        assert!(root.join("Note.sdr").exists());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
