//! Finding the folders that hold books, for the setup's Library page.
//! [`discover`] lists the fixed folders (the defaults and the saved list),
//! KOReader's home folder (`home_dir` in its `settings.reader.lua`), and
//! every top-level folder of the storage root with EPUBs anywhere below it,
//! counting each folder's EPUBs up to [`COUNT_CAP`]. It walks like the
//! library scanner (no dot entries, `.sdr` folders or symlinks) and skips
//! the Kindle's and the apps' own folders. [`Discovery`] runs it on its own
//! thread; the page fills in as folders arrive.
use crate::annotations::lua;
use crate::platform::Waker;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};

/// Counting stops here ("1,000+ books").
pub const COUNT_CAP: usize = 1000;

/// Top-level folders that belong to the Kindle or to apps.
const SKIPPED: [&str; 11] = [
    "system",
    "extensions",
    "koreader",
    "verso",
    "fonts",
    "audible",
    "voice",
    "screenshots",
    "lost+found",
    "mrpackages",
    "libkh",
];

/// A folder for the Library page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Folder {
    pub path: PathBuf,
    /// EPUBs below it, at most [`COUNT_CAP`].
    pub books: usize,
    /// KOReader's home folder.
    pub koreader_home: bool,
    /// Found by the walk rather than listed anyway: it starts ticked on a
    /// first setup.
    pub discovered: bool,
}

/// Where to look.
pub struct Sources {
    /// Listed even without books: the default folders and the saved list.
    pub fixed: Vec<PathBuf>,
    /// The storage root whose top-level folders are walked
    /// (`store::storage_root`); None discovers nothing (`VERSO_BOOKS`).
    pub root: Option<PathBuf>,
    /// KOReader's `settings.reader.lua`.
    pub koreader_settings: Option<PathBuf>,
}

/// KOReader's home folder, read as data. None when KOReader has no
/// settings file or no `home_dir` in it.
pub fn koreader_home(settings: &Path, cancelled: &dyn Fn() -> bool) -> Option<PathBuf> {
    let bytes = std::fs::read(settings).ok()?;
    match lua::fields_cancellable(&bytes, &["home_dir"], cancelled) {
        Ok(fields) => fields.get("home_dir")?.string().filter(|s| !s.is_empty()).map(|s| normalize(Path::new(s))),
        Err(e) => {
            log::warn!("reading KOReader's home folder from {}: {e:#}", settings.display());
            None
        }
    }
}

/// Without a trailing slash or `.` parts, so `/mnt/us/books/` matches.
fn normalize(path: &Path) -> PathBuf {
    path.components().collect()
}

/// EPUBs in `dir` and below, at most `cap`.
pub fn count_epubs(dir: &Path, cap: usize, cancelled: &dyn Fn() -> bool) -> usize {
    fn walk(dir: &Path, cap: usize, count: &mut usize, cancelled: &dyn Fn() -> bool) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.filter_map(Result::ok) {
            if *count >= cap || cancelled() {
                return;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            // The entry's own type: symlinks are neither.
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() && !name.ends_with(".sdr") {
                walk(&entry.path(), cap, count, cancelled);
            } else if kind.is_file() && Path::new(&*name).extension().is_some_and(|e| e.eq_ignore_ascii_case("epub")) {
                *count += 1;
            }
        }
    }
    let mut count = 0;
    walk(dir, cap, &mut count, cancelled);
    count
}

/// The storage root's top-level folders worth walking, sorted by name.
fn top_level(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else { return Vec::new() };
    let mut folders: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_lowercase();
            !name.starts_with('.') && !name.ends_with(".sdr") && !SKIPPED.contains(&name.as_str())
        })
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    folders.sort_by_cached_key(|p| p.file_name().unwrap_or_default().to_string_lossy().to_lowercase());
    folders
}

/// Sends each folder as it is counted: the fixed ones, then the storage
/// root's folders that hold books, then KOReader's home folder. A folder
/// inside one already sent is left out; KOReader's home folder labels a
/// listed folder it equals.
pub fn discover(sources: &Sources, cancelled: &dyn Fn() -> bool, mut send: impl FnMut(Folder)) {
    let home = sources.koreader_settings.as_deref().and_then(|s| koreader_home(s, cancelled));
    let mut listed: Vec<PathBuf> = Vec::new();
    let mut list = |listed: &mut Vec<PathBuf>, path: PathBuf, books: usize, discovered: bool| {
        let koreader_home = home.as_ref() == Some(&path);
        listed.push(path.clone());
        send(Folder { path, books, koreader_home, discovered });
    };
    let inside = |listed: &[PathBuf], path: &Path| listed.iter().any(|l| path.starts_with(l));
    for path in &sources.fixed {
        let path = normalize(path);
        if !inside(&listed, &path) {
            let books = count_epubs(&path, COUNT_CAP, cancelled);
            list(&mut listed, path, books, false);
        }
    }
    if let Some(root) = &sources.root {
        for path in top_level(root) {
            if cancelled() {
                return;
            }
            if inside(&listed, &path) {
                continue;
            }
            let books = count_epubs(&path, COUNT_CAP, cancelled);
            if books > 0 {
                list(&mut listed, path, books, true);
            }
        }
    }
    if let Some(home) = &home
        && !inside(&listed, home)
    {
        list(&mut listed, home.clone(), count_epubs(home, COUNT_CAP, cancelled), true);
    }
}

/// [`discover`] on its own thread. Dropping it stops the walk and waits
/// for the thread.
pub struct Discovery {
    folders: Receiver<Folder>,
    cancel: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Discovery {
    pub fn start(sources: Sources, waker: Waker) -> Self {
        let (tx, folders) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let thread = std::thread::Builder::new()
            .name("verso-discovery".into())
            .spawn(move || {
                let started = std::time::Instant::now();
                let cancelled = || flag.load(Ordering::Relaxed);
                discover(&sources, &cancelled, |folder| {
                    let _ = tx.send(folder);
                    waker.wake();
                });
                log::info!("folder discovery took {:?}", started.elapsed());
                // The end of the thread: the screen notices it finished.
                waker.wake();
            })
            .map_err(|e| log::error!("starting folder discovery: {e}"))
            .ok();
        Self { folders, cancel, thread }
    }
    /// Folders found since the last call.
    pub fn take(&self) -> Vec<Folder> {
        self.folders.try_iter().collect()
    }
    /// True once the walk has ended, or couldn't start.
    pub fn finished(&self) -> bool {
        self.thread.as_ref().is_none_or(std::thread::JoinHandle::is_finished)
    }
}
impl Drop for Discovery {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn epubs(dir: &Path, n: usize) -> anyhow::Result<()> {
        std::fs::create_dir_all(dir)?;
        for i in 0..n {
            std::fs::write(dir.join(format!("{i}.epub")), b"")?;
        }
        Ok(())
    }

    fn found(sources: &Sources) -> Vec<(String, usize, bool, bool)> {
        let root = sources.root.clone().unwrap_or_default();
        let mut out = Vec::new();
        discover(sources, &|| false, |f| {
            let name = f.path.strip_prefix(&root).unwrap_or(&f.path).display().to_string();
            out.push((name, f.books, f.koreader_home, f.discovered));
        });
        out
    }

    #[test]
    fn walks_top_level_folders_like_the_library_scanner() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        epubs(&root.join("documents"), 1)?;
        std::fs::create_dir_all(root.join("My Library"))?;
        epubs(&root.join("Books/Fiction/Tolkien"), 2)?;
        std::fs::write(root.join("Books/notes.txt"), b"")?;
        std::fs::write(root.join("Books/Upper.EPUB"), b"")?;
        // Not books: hidden, reading data, app folders, a folder without EPUBs.
        epubs(&root.join("Books/.hidden"), 3)?;
        epubs(&root.join("Books/Fiction/Tolkien/0.sdr"), 1)?;
        std::fs::write(root.join("Books/.dot.epub"), b"")?;
        for skipped in ["koreader", "System", "extensions", "lost+found", ".trash"] {
            epubs(&root.join(skipped), 1)?;
        }
        std::fs::create_dir_all(root.join("Empty"))?;
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("Books"), root.join("Linked"))?;
            std::os::unix::fs::symlink(root.join("Books/Upper.EPUB"), root.join("documents/link.epub"))?;
        }
        let sources = Sources {
            fixed: vec![root.join("documents"), root.join("My Library")],
            root: Some(root.to_path_buf()),
            koreader_settings: None,
        };
        assert_eq!(
            found(&sources),
            [
                ("documents".into(), 1, false, false),
                ("My Library".into(), 0, false, false),
                ("Books".into(), 3, false, true),
            ]
        );
        // VERSO_BOOKS: the fixed folders only.
        let sources = Sources { root: None, ..sources };
        assert_eq!(found(&sources).len(), 2);
        Ok(())
    }

    #[test]
    fn nested_folders_collapse_and_koreader_home_is_labelled() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        epubs(&root.join("Books/Fiction"), 1)?;
        epubs(&root.join("Other/Home"), 2)?;
        let lua = root.join("settings.reader.lua");
        let sources = |fixed: Vec<PathBuf>, home: &Path| -> anyhow::Result<Sources> {
            let text = format!(
                "-- KOReader\nreturn {{\n    [\"home_dir\"] = \"{}/\",\n    [\"x\"] = true,\n}}\n",
                home.display()
            );
            std::fs::write(&lua, text)?;
            Ok(Sources { fixed, root: Some(root.to_path_buf()), koreader_settings: Some(lua.clone()) })
        };
        // A saved folder inside another, and a home folder inside a listed
        // folder, are left out; the walk skips listed folders.
        let nested = vec![root.join("Books"), root.join("Books/Fiction")];
        assert_eq!(
            found(&sources(nested, &root.join("Other/Home"))?),
            [("Books".into(), 1, false, false), ("Other".into(), 2, false, true)]
        );
        // A home folder equal to a listed folder labels it.
        assert_eq!(found(&sources(vec![], &root.join("Other"))?)[1], ("Other".into(), 2, true, true));
        // One outside every listed folder is listed with its label.
        let outside = tempfile::tempdir()?;
        epubs(outside.path(), 1)?;
        let all = found(&sources(vec![root.join("Books")], outside.path())?);
        assert_eq!(all.len(), 3);
        assert_eq!((all[2].1, all[2].2, all[2].3), (1, true, true));
        Ok(())
    }

    #[test]
    fn counting_stops_at_the_cap() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        epubs(&dir.path().join("a"), 4)?;
        epubs(&dir.path().join("b"), 4)?;
        assert_eq!(count_epubs(dir.path(), 5, &|| false), 5);
        assert_eq!(count_epubs(dir.path(), 100, &|| false), 8);
        Ok(())
    }
}
