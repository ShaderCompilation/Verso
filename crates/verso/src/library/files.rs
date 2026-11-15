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

/// Clash policy applies to both the book name and its reserved sidecar name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClashChoice {
    Skip,
    KeepBoth,
}
#[derive(Clone, Debug)]
pub enum MoveItem {
    Book(PathBuf),
    Folder(PathBuf),
}
impl MoveItem {
    pub fn path(&self) -> &Path {
        match self {
            Self::Book(p) | Self::Folder(p) => p,
        }
    }
    pub fn move_to(&self, destination: &Path, choice: Option<ClashChoice>) -> Result<MoveOutcome> {
        match self {
            Self::Book(path) => move_book(path, destination, choice),
            Self::Folder(path) => move_folder(path, destination, choice),
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum MoveOutcome {
    Clash,
    Skipped,
    Moved(PathBuf),
}
fn occupied(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}
fn target_free(path: &Path, book: bool) -> Result<bool> {
    // Reserve a sidecar name even for a book without one: an orphan sidecar
    // must never become associated with the incoming book.
    Ok(!occupied(path)? && (!book || !occupied(&sidecar(path))?))
}
pub fn next_free_name(parent: &Path, name: &std::ffi::OsStr, book: bool) -> Result<PathBuf> {
    let name = Path::new(name);
    let stem = if book { name.file_stem().unwrap_or(name.as_os_str()) } else { name.as_os_str() };
    for n in 2u64.. {
        let mut numbered = stem.to_os_string();
        numbered.push(format!(" ({n})"));
        if book && let Some(extension) = name.extension() {
            numbered.push(".");
            numbered.push(extension);
        }
        let candidate = parent.join(numbered);
        if target_free(&candidate, book)? {
            return Ok(candidate);
        }
    }
    bail!("No free name available")
}
fn move_target(path: &Path, destination: &Path, book: bool, choice: Option<ClashChoice>) -> Result<MoveOutcome> {
    let source = std::fs::canonicalize(path).context("The source no longer exists")?;
    let dest = std::fs::canonicalize(destination).context("The destination no longer exists")?;
    ensure!(dest.is_dir(), "Choose a destination folder");
    ensure!(source.parent() != Some(dest.as_path()), "Choose a different folder");
    if !book {
        ensure!(!dest.starts_with(&source), "Cannot move a folder inside itself");
    }
    let meta = std::fs::symlink_metadata(path)?;
    ensure!(if book { meta.is_file() } else { meta.is_dir() }, "The source has changed");
    let name = path.file_name().context("Cannot move a library root")?;
    let target = destination.join(name);
    if target_free(&target, book)? {
        return Ok(MoveOutcome::Moved(target));
    }
    match choice {
        None => Ok(MoveOutcome::Clash),
        Some(ClashChoice::Skip) => Ok(MoveOutcome::Skipped),
        Some(ClashChoice::KeepBoth) => Ok(MoveOutcome::Moved(next_free_name(destination, name, book)?)),
    }
}
/// Refuse replacement atomically where available. Kindle's FUSE mount rejects
/// RENAME_NOREPLACE, so recheck immediately before ordinary rename there, as for
/// folder renaming. That fallback cannot exclude a concurrent external writer.
fn rename_free(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let from_c = std::ffi::CString::new(from.as_os_str().as_bytes())?;
    let to_c = std::ffi::CString::new(to.as_os_str().as_bytes())?;
    // SAFETY: both C strings live through the syscall; it retains no pointers.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from_c.as_ptr(),
            libc::AT_FDCWD,
            to_c.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if matches!(error.raw_os_error(), Some(libc::EINVAL | libc::ENOSYS | libc::EOPNOTSUPP)) {
        return rename_checked(from, to);
    }
    Err(error)
}
fn rename_checked(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(to) {
        Ok(_) => return Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    std::fs::rename(from, to)
}
/// Move the book and KOReader data together. Never copy across filesystems.
pub fn move_book(path: &Path, destination: &Path, choice: Option<ClashChoice>) -> Result<MoveOutcome> {
    move_book_with(path, destination, choice, rename_free)
}
fn move_book_with(
    path: &Path,
    destination: &Path,
    choice: Option<ClashChoice>,
    mut rename: impl FnMut(&Path, &Path) -> std::io::Result<()>,
) -> Result<MoveOutcome> {
    let outcome = move_target(path, destination, true, choice)?;
    let MoveOutcome::Moved(target) = &outcome else { return Ok(outcome) };
    let data = sidecar(path);
    // Decide before touching anything, so a failed read changes nothing.
    // A plain file with the sidecar's name is not KOReader data and stays.
    enum Data {
        None,
        Whole,
        Own(Vec<PathBuf>),
    }
    let plan = if !std::fs::symlink_metadata(&data).is_ok_and(|m| m.is_dir()) {
        Data::None
    } else if sidecar_shared(path)? {
        Data::Own(own_metadata(path)?)
    } else {
        Data::Whole
    };
    rename(path, target).context("Couldn't move book")?;
    let moved_data = match &plan {
        Data::None => Ok(()),
        Data::Own(files) if files.is_empty() => Ok(()),
        Data::Whole => rename(&data, &sidecar(target)).map_err(Into::into),
        Data::Own(files) => move_into_new_dir(files, &sidecar(target), &mut rename),
    };
    if let Err(error) = moved_data {
        if let Err(rollback) = rename(target, path) {
            bail!(
                "Couldn't move KOReader data: {error}; couldn't restore book: {rollback}. Book is at {}",
                target.display()
            );
        }
        bail!("Couldn't move KOReader data; book restored: {error:#}");
    }
    Ok(outcome)
}
/// Moves a shared sidecar's files belonging to one book into a new sidecar.
/// On failure, files already moved go back and the new folder is removed.
fn move_into_new_dir(
    files: &[PathBuf],
    dir: &Path,
    rename: &mut impl FnMut(&Path, &Path) -> std::io::Result<()>,
) -> Result<()> {
    std::fs::create_dir(dir)?;
    let mut moved: Vec<(&Path, PathBuf)> = Vec::new();
    for from in files {
        let to = dir.join(from.file_name().context("Unnamed KOReader file")?);
        if let Err(error) = rename(from, &to) {
            let restored = moved.iter().rev().all(|(from, to)| rename(to, from).is_ok());
            if restored {
                let _ = std::fs::remove_dir(dir);
                return Err(error.into());
            }
            bail!("{error}; some KOReader files stayed in {}", dir.display());
        }
        moved.push((from, to));
    }
    Ok(())
}
pub fn move_folder(path: &Path, destination: &Path, choice: Option<ClashChoice>) -> Result<MoveOutcome> {
    let outcome = move_target(path, destination, false, choice)?;
    if let MoveOutcome::Moved(target) = &outcome {
        rename_free(path, target).context("Couldn't move folder")?;
    }
    Ok(outcome)
}

/// KOReader keeps a book's progress and highlights in `Name.sdr` beside it.
pub fn sidecar(book: &Path) -> PathBuf {
    book.with_extension("sdr")
}

/// KOReader names a sidecar after the file name without its extension, so
/// `Book.epub` and `Book.pdf` in one folder share `Book.sdr`, each keeping its
/// own `metadata.<ext>.lua` there. Works whether or not the book still exists.
fn sidecar_shared(book: &Path) -> Result<bool> {
    let (Some(parent), Some(name), Some(stem)) = (book.parent(), book.file_name(), book.file_stem()) else {
        return Ok(false);
    };
    let own = sidecar(book);
    for entry in std::fs::read_dir(parent)? {
        let path = entry?.path();
        if path.file_name() != Some(name) && path != own && path.extension().is_some() && path.file_stem() == Some(stem)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// This book's files in its sidecar: `metadata.<ext>.lua` and its `.old` backup.
fn own_metadata(book: &Path) -> Result<Vec<PathBuf>> {
    let Some(ext) = book.extension().and_then(|e| e.to_str()) else { return Ok(Vec::new()) };
    let dir = sidecar(book);
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let name = entry?.file_name();
        let Some(rest) = name.to_str().and_then(|n| n.strip_prefix("metadata.")) else { continue };
        let format = rest.strip_suffix(".lua.old").or_else(|| rest.strip_suffix(".lua"));
        if format.is_some_and(|f| f.eq_ignore_ascii_case(ext)) {
            files.push(dir.join(&name));
        }
    }
    files.sort();
    Ok(files)
}

/// Removes a deleted book's KOReader sidecar, if it has one. A sidecar shared
/// with another format of the same book loses only this book's files.
pub fn delete_sidecar(book: &Path) -> Result<()> {
    let path = sidecar(book);
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_dir() => {
            if sidecar_shared(book).context("Couldn't remove KOReader data")? {
                for file in own_metadata(book).context("Couldn't remove KOReader data")? {
                    std::fs::remove_file(file).context("Couldn't remove KOReader data")?;
                }
                return Ok(());
            }
            std::fs::remove_dir_all(&path).context("Couldn't remove KOReader data")
        }
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

    fn temp_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ))
    }
    #[test]
    fn moves_sidecars_folders_and_refuses_cycles_and_clashes() -> Result<()> {
        let root = temp_root("verso-file-moves");
        let source = root.join("Source");
        let dest = root.join("Destination");
        std::fs::create_dir_all(source.join("Tree/Child"))?;
        std::fs::create_dir_all(&dest)?;
        std::fs::write(source.join("Book.epub"), b"book")?;
        std::fs::create_dir(source.join("Book.sdr"))?;
        std::fs::write(source.join("Book.sdr/metadata.epub.lua"), b"reading data")?;
        std::fs::write(dest.join("Book.sdr"), b"orphan")?;
        assert_eq!(move_book(&source.join("Book.epub"), &dest, None)?, MoveOutcome::Clash);
        assert_eq!(move_book(&source.join("Book.epub"), &dest, Some(ClashChoice::Skip))?, MoveOutcome::Skipped);
        assert_eq!(
            move_book(&source.join("Book.epub"), &dest, Some(ClashChoice::KeepBoth))?,
            MoveOutcome::Moved(dest.join("Book (2).epub"))
        );
        assert_eq!(std::fs::read(dest.join("Book (2).sdr/metadata.epub.lua"))?, b"reading data");
        assert_eq!(std::fs::read(dest.join("Book.sdr"))?, b"orphan");
        std::fs::write(source.join("Plain.EPUB"), b"plain")?;
        assert_eq!(move_book(&source.join("Plain.EPUB"), &dest, None)?, MoveOutcome::Moved(dest.join("Plain.EPUB")));
        assert!(!dest.join("Plain.sdr").exists());
        let tree = source.join("Tree");
        assert!(move_folder(&tree, &tree.join("Child"), None).is_err());
        assert!(move_folder(&tree, &tree, None).is_err());
        assert!(move_folder(&tree, &source, None).is_err());
        std::fs::create_dir(dest.join("Tree"))?;
        std::fs::create_dir(dest.join("Tree (2)"))?;
        assert_eq!(move_folder(&tree, &dest, None)?, MoveOutcome::Clash);
        assert_eq!(move_folder(&tree, &dest, Some(ClashChoice::Skip))?, MoveOutcome::Skipped);
        assert_eq!(move_folder(&tree, &dest, Some(ClashChoice::KeepBoth))?, MoveOutcome::Moved(dest.join("Tree (3)")));
        assert!(dest.join("Tree (3)/Child").is_dir());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn sidecar_failure_rolls_back_and_rename_never_overwrites() -> Result<()> {
        let root = temp_root("verso-move-rollback");
        std::fs::create_dir_all(root.join("Source/Book.sdr"))?;
        std::fs::create_dir_all(root.join("Dest"))?;
        let book = root.join("Source/Book.epub");
        std::fs::write(&book, b"book")?;
        let dest = root.join("Dest");
        let error = move_book_with(&book, &dest, None, |from, to| {
            if from == sidecar(&book) {
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            } else {
                rename_free(from, to)
            }
        })
        .unwrap_err();
        assert!(error.to_string().contains("book restored"));
        assert!(book.exists() && sidecar(&book).exists() && !dest.join("Book.epub").exists());
        std::fs::write(dest.join("Book.epub"), b"existing")?;
        assert!(rename_free(&book, &dest.join("Book.epub")).is_err());
        assert!(rename_checked(&book, &dest.join("Book.epub")).is_err());
        assert_eq!(std::fs::read(dest.join("Book.epub"))?, b"existing");
        assert_eq!(std::fs::read(book)?, b"book");
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn batch_delete_continues_after_failure_and_keeps_survivor_progress() -> Result<()> {
        let root = temp_root("verso-batch-delete");
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
        let root = temp_root("verso-folder-files");
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
        let root = temp_root("verso-partial-delete");
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
        let root = temp_root("verso-sidecar");
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

    /// `Book.epub` and `Book.pdf` beside each other share `Book.sdr`.
    fn shared_sidecar(dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir.join("Book.sdr"))?;
        std::fs::write(dir.join("Book.epub"), b"epub")?;
        std::fs::write(dir.join("Book.pdf"), b"pdf")?;
        for name in ["metadata.epub.lua", "metadata.epub.lua.old", "metadata.pdf.lua", "cover.jpg"] {
            std::fs::write(dir.join("Book.sdr").join(name), name)?;
        }
        Ok(())
    }
    #[test]
    fn a_shared_sidecar_keeps_the_other_formats_data() -> Result<()> {
        let root = temp_root("verso-shared-sidecar");
        let (source, dest) = (root.join("Source"), root.join("Dest"));
        shared_sidecar(&source)?;
        std::fs::create_dir_all(&dest)?;
        std::fs::write(dest.join("Book.epub"), b"existing")?;
        assert_eq!(
            move_book(&source.join("Book.epub"), &dest, Some(ClashChoice::KeepBoth))?,
            MoveOutcome::Moved(dest.join("Book (2).epub"))
        );
        let mut moved: Vec<_> = std::fs::read_dir(dest.join("Book (2).sdr"))?.map(|e| e.unwrap().file_name()).collect();
        moved.sort();
        assert_eq!(moved, ["metadata.epub.lua", "metadata.epub.lua.old"]);
        let mut kept: Vec<_> = std::fs::read_dir(source.join("Book.sdr"))?.map(|e| e.unwrap().file_name()).collect();
        kept.sort();
        assert_eq!(kept, ["cover.jpg", "metadata.pdf.lua"]);

        // A failure partway puts the moved files and the book back.
        let other = root.join("Other");
        shared_sidecar(&other)?;
        let book = other.join("Book.epub");
        let error = move_book_with(&book, &dest, Some(ClashChoice::KeepBoth), |from, to| {
            if from.ends_with("metadata.epub.lua.old") {
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            } else {
                rename_free(from, to)
            }
        })
        .unwrap_err();
        assert!(error.to_string().contains("book restored"), "{error:#}");
        assert!(book.is_file() && other.join("Book.sdr/metadata.epub.lua").is_file());
        assert!(!dest.join("Book (3).epub").exists() && !dest.join("Book (3).sdr").exists());

        // Deleting one format removes only its own files.
        std::fs::remove_file(&book)?;
        delete_sidecar(&book)?;
        let mut left: Vec<_> = std::fs::read_dir(other.join("Book.sdr"))?.map(|e| e.unwrap().file_name()).collect();
        left.sort();
        assert_eq!(left, ["cover.jpg", "metadata.pdf.lua"]);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
