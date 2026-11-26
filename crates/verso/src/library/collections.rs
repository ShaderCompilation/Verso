//! A filesystem-free view of the available index. Paths identify folders;
//! book identities and all reading/cover state remain in the library.
use super::LibraryIndex;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Navigation {
    pub folder: Option<PathBuf>,
    pub home_page: usize,
    pub folder_pages: BTreeMap<PathBuf, usize>,
}
impl Navigation {
    pub fn move_path(&mut self, from: &Path, to: &Path) {
        if let Some(folder) = &mut self.folder {
            super::rewrite_path(folder, from, to);
        }
        self.folder_pages = std::mem::take(&mut self.folder_pages)
            .into_iter()
            .map(|(mut path, page)| {
                super::rewrite_path(&mut path, from, to);
                (path, page)
            })
            .collect();
    }

    pub fn page(&self) -> usize {
        self.folder.as_ref().map_or(self.home_page, |p| self.folder_pages.get(p).copied().unwrap_or(0))
    }
    pub fn set_page(&mut self, page: usize) {
        match &self.folder {
            Some(path) => {
                self.folder_pages.insert(path.clone(), page);
            }
            None => self.home_page = page,
        }
    }
}

#[derive(Default)]
pub struct Folder {
    pub parent: Option<PathBuf>,
    pub children: BTreeSet<PathBuf>,
    pub books: HashSet<PathBuf>,
    /// Positions of the same direct books in the index this was built from.
    pub indices: Vec<usize>,
    pub count: usize,
}

/// Built off the UI thread by the library worker (see `worker::Snapshot`),
/// and shared with the screen without copying.
#[derive(Default)]
pub struct Collections {
    pub folders: HashMap<PathBuf, Folder>,
    pub roots: BTreeSet<PathBuf>,
}
impl Collections {
    pub fn from_index(index: &LibraryIndex, roots: &[PathBuf]) -> Self {
        let roots = distinct_roots(roots);
        let mut result = Self::default();
        for (i, book) in index.books.iter().enumerate().filter(|(_, b)| b.available) {
            let path = normalize_cow(&book.path);
            let Some(root) = roots.iter().find(|r| beneath(&path, r)) else { continue };
            let Some(folder) = path.parent() else { continue };
            // A misconfigured root may name the EPUB itself, rather than a
            // directory. Never walk above the selected root in cached data.
            if !beneath(folder, root) {
                continue;
            }
            result.link(folder, root);
            let node = result.folders.get_mut(folder).expect("linked above");
            // A path listed twice counts once.
            if node.books.insert(path.into_owned()) {
                node.indices.push(i);
                node.count += 1;
            }
        }
        for path in &index.empty_folders {
            let path = normalize_cow(path);
            let Some(root) = roots.iter().find(|r| path.starts_with(r)) else { continue };
            if path.strip_prefix(root).unwrap().components().any(|part| {
                let name = part.as_os_str().to_string_lossy();
                name.starts_with('.') || name.ends_with(".sdr")
            }) {
                continue;
            }
            result.link(&path, root);
        }
        // Each folder counts the books beneath it: add every folder's total to
        // its parent's, deepest folders first.
        let mut order: Vec<(usize, PathBuf)> =
            result.folders.keys().map(|p| (p.components().count(), p.clone())).collect();
        order.sort_unstable_by_key(|(depth, _)| std::cmp::Reverse(*depth));
        for (_, path) in order {
            let node = &result.folders[&path];
            if let Some(parent) = node.parent.clone() {
                let count = node.count;
                result.folders.get_mut(&parent).expect("parents are linked").count += count;
            }
        }
        result
    }

    /// Adds `folder` and any missing ancestors up to `root`. A folder already
    /// present is linked all the way up, so the walk stops at the first one.
    fn link(&mut self, folder: &Path, root: &Path) {
        let mut child: Option<PathBuf> = None;
        let mut folder = folder;
        loop {
            if let Some(node) = self.folders.get_mut(folder) {
                node.children.extend(child);
                return;
            }
            let mut node = Folder::default();
            node.children.extend(child.take());
            if folder == root {
                self.folders.insert(folder.to_owned(), node);
                self.roots.insert(root.to_owned());
                return;
            }
            let parent = folder.parent().expect("folder is beneath its absolute root");
            node.parent = Some(parent.to_owned());
            self.folders.insert(folder.to_owned(), node);
            child = Some(folder.to_owned());
            folder = parent;
        }
    }

    pub fn children(&self, folder: Option<&Path>) -> Vec<PathBuf> {
        let paths = match folder {
            Some(p) => self.folders.get(p).map(|f| &f.children),
            None => Some(&self.roots),
        };
        let mut paths: Vec<_> = paths.into_iter().flatten().cloned().collect();
        paths.sort_by_cached_key(|p| (super::sort::Natural::new(&name(p)), p.clone()));
        paths
    }

    /// Keep the closest surviving ancestor, or return to the collection roots.
    pub fn restore(&self, folder: Option<&Path>) -> Option<PathBuf> {
        folder?.ancestors().find(|p| self.folders.contains_key(*p)).map(Path::to_owned)
    }

    /// When only one root is active (one non-empty root, or a single configured
    /// root when empty), Library skips the root-chooser level and uses that root
    /// as its top level directly.
    pub fn single_root<'a>(&'a self, configured_roots: &'a [PathBuf]) -> Option<&'a Path> {
        if self.roots.len() == 1 {
            self.roots.iter().next().map(PathBuf::as_path)
        } else if self.roots.is_empty() && configured_roots.len() == 1 {
            Some(configured_roots[0].as_path())
        } else {
            None
        }
    }

    /// Keeps the closest surviving ancestor, falling back to the top level
    /// (the single active root when only one root is active, or `None`).
    pub fn restore_for(&self, folder: Option<&Path>, configured_roots: &[PathBuf]) -> Option<PathBuf> {
        let restored = self.restore(folder);
        match self.single_root(configured_roots) {
            Some(root) => Some(restored.unwrap_or_else(|| root.to_owned())),
            None => restored,
        }
    }

    /// Total books in `folder` and all its subfolders, or across all active
    /// roots when `folder` is `None`.
    pub fn book_count(&self, folder: Option<&Path>) -> usize {
        match folder {
            Some(path) => self.folders.get(path).map_or(0, |f| f.count),
            None => self.roots.iter().filter_map(|r| self.folders.get(r)).map(|f| f.count).sum(),
        }
    }

    /// Top-level Library counts for Home's Library card:
    /// `(root_count, folders, books)`, where `books` includes all subfolders.
    pub fn top_level_counts(&self, configured_roots: &[PathBuf]) -> (usize, usize, usize) {
        if let Some(root) = self.single_root(configured_roots) {
            let (folders, books) = self.folders.get(root).map_or((0, 0), |f| (f.children.len(), f.count));
            (1, folders, books)
        } else {
            let n = self.roots.len();
            (n, n, self.book_count(None))
        }
    }
}

/// `Path::starts_with` for normalized paths, comparing bytes instead of
/// parsing components.
fn beneath(path: &Path, root: &Path) -> bool {
    let (path, root) = (path.as_os_str().as_encoded_bytes(), root.as_os_str().as_encoded_bytes());
    path.starts_with(root) && (path.len() == root.len() || root.ends_with(b"/") || path[root.len()] == b'/')
}

pub fn name(path: &Path) -> String {
    path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned()
}

/// Lexical normalization needs no stat/read_dir and works for offline roots.
pub fn normalize(path: &Path) -> PathBuf {
    normalize_cow(path).into_owned()
}

/// [`normalize`], borrowing a path that is already normal (as indexed paths are).
pub fn normalize_cow(path: &Path) -> Cow<'_, Path> {
    // Normal: no `.` or `..` parts, no doubled or trailing separator.
    let bytes = path.as_os_str().as_encoded_bytes();
    let normal = bytes == b"/"
        || bytes.split(|&b| b == b'/').enumerate().all(|(i, part)| match part {
            b"" => i == 0,
            b"." | b".." => false,
            _ => true,
        });
    if normal { Cow::Borrowed(path) } else { Cow::Owned(rebuild(path)) }
}

fn rebuild(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other),
        }
    }
    result
}

pub fn distinct_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    let roots: Vec<_> = roots.iter().map(|r| normalize(r)).collect();
    let mut result: Vec<PathBuf> = Vec::new();
    for root in &roots {
        if !roots.iter().any(|r| root != r && root.starts_with(r)) && !result.contains(root) {
            result.push(root.clone());
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::selection_tests::book;
    #[test]
    fn hierarchy_counts_loose_books_duplicates_overlaps_and_offline_entries() {
        let paths = [
            "/a/loose.epub",
            "/a/Fiction/Same/one.epub",
            "/a/Other/Same/two.epub",
            "/b/Same/three.epub",
            "/a/Fiction/Same/one.epub",
            "/a/empty/gone.epub",
            "/outside/four.epub",
        ];
        let mut index = LibraryIndex {
            books: paths
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let mut b = book(i);
                    b.path = p.into();
                    b
                })
                .collect(),
            ..Default::default()
        };
        index.books[5].available = false;
        let roots = vec!["/a/./Fiction/..".into(), "/b".into(), "/a/Fiction".into()];
        let tree = Collections::from_index(&index, &roots);
        assert_eq!(tree.roots.len(), 2);
        assert_eq!(tree.folders[Path::new("/a")].count, 3);
        assert_eq!(tree.folders[Path::new("/a")].books.len(), 1);
        assert_eq!(tree.children(Some(Path::new("/a"))), vec![PathBuf::from("/a/Fiction"), "/a/Other".into()]);
        for p in ["/a/Fiction/Same", "/a/Other/Same", "/b/Same"] {
            assert_eq!(tree.folders[Path::new(p)].count, 1);
        }
        assert!(!tree.folders.contains_key(Path::new("/a/empty")));
        assert!(Collections::from_index(&index, &["/a/loose.epub".into()]).folders.is_empty());
        assert_eq!(tree.restore(Some(Path::new("/a/Fiction/removed"))), Some("/a/Fiction".into()));
        assert_eq!(tree.restore(Some(Path::new("/removed"))), None);
        // No filesystem reads: retained available entries in an inaccessible root still count.
        index.books[1].path = "/b/Moved/one.epub".into();
        index.books[4].available = false;
        let moved = Collections::from_index(&index, &roots);
        assert_eq!(moved.folders[Path::new("/b")].count, 2);
        assert!(!moved.folders.contains_key(Path::new("/a/Fiction")));
        // Folder names compare naturally, ignoring case.
        let numbered = LibraryIndex {
            books: ["/n/Vol 10/a.epub", "/n/vol 2/b.epub", "/n/Vol 1/c.epub"]
                .iter()
                .enumerate()
                .map(|(i, p)| crate::library::Book { path: p.into(), ..book(i) })
                .collect(),
            ..Default::default()
        };
        let tree = Collections::from_index(&numbered, &["/n".into()]);
        assert_eq!(tree.children(Some(Path::new("/n"))), ["/n/Vol 1", "/n/vol 2", "/n/Vol 10"].map(PathBuf::from));
    }

    #[test]
    fn single_root_skipping_and_top_level_counts() {
        let mut a = book(0);
        a.path = "/my-lib/Sub/one.epub".into();
        let mut loose = book(1);
        loose.path = "/my-lib/loose.epub".into();
        let index = LibraryIndex { books: vec![a, loose], ..Default::default() };

        // Two configured roots, but only one has EPUBs -> skips root chooser.
        let roots = vec![PathBuf::from("/documents"), PathBuf::from("/my-lib")];
        let tree = Collections::from_index(&index, &roots);
        assert_eq!(tree.single_root(&roots), Some(Path::new("/my-lib")));
        assert_eq!(tree.top_level_counts(&roots), (1, 1, 2));
        assert_eq!(tree.book_count(Some(Path::new("/my-lib"))), 2);
        assert_eq!(tree.book_count(Some(Path::new("/my-lib/Sub"))), 1);
        assert_eq!(tree.restore_for(None, &roots), Some(PathBuf::from("/my-lib")));
        assert_eq!(tree.restore_for(Some(Path::new("/my-lib/Sub")), &roots), Some(PathBuf::from("/my-lib/Sub")));
        assert_eq!(tree.restore_for(Some(Path::new("/removed")), &roots), Some(PathBuf::from("/my-lib")));

        // Both roots active -> multi-root top level ("Library").
        let mut second = book(2);
        second.path = "/documents/other.epub".into();
        let multi_index = LibraryIndex { books: vec![index.books[0].clone(), second], ..Default::default() };
        let multi = Collections::from_index(&multi_index, &roots);
        assert_eq!(multi.single_root(&roots), None);
        assert_eq!(multi.top_level_counts(&roots), (2, 2, 2));
        assert_eq!(multi.book_count(None), 2);
        assert_eq!(multi.restore_for(None, &roots), None);
        assert_eq!(multi.restore_for(Some(Path::new("/removed")), &roots), None);

        // Single configured empty root -> still skips root chooser.
        let single_cfg = vec![PathBuf::from("/my-lib")];
        let empty = Collections::from_index(&LibraryIndex::default(), &single_cfg);
        assert_eq!(empty.single_root(&single_cfg), Some(Path::new("/my-lib")));
        assert_eq!(empty.top_level_counts(&single_cfg), (1, 0, 0));
        assert_eq!(empty.restore_for(None, &single_cfg), Some(PathBuf::from("/my-lib")));
    }
}

#[cfg(test)]
mod normalize_tests {
    use super::*;
    #[test]
    fn borrowed_paths_are_exactly_what_normalizing_would_build() {
        for p in [
            "",
            "/",
            "/a/b.epub",
            "a/b",
            "/a//b",
            "/a/b/",
            "/a/./b",
            "./a",
            "/a/../b",
            "..",
            "/a/.hidden/b.epub",
            "/a/b..c",
        ] {
            let path = Path::new(p);
            let built = rebuild(path);
            let cow = normalize_cow(path);
            assert_eq!(cow.as_os_str(), built.as_os_str(), "{p}");
            assert_eq!(matches!(cow, Cow::Borrowed(_)), built.as_os_str() == path.as_os_str(), "{p}");
        }
        for (path, root) in
            [("/a/b", "/a"), ("/a", "/a"), ("/ab", "/a"), ("/a/b", "/"), ("/", "/"), ("/a", "/a/b"), ("a/b", "a")]
        {
            assert_eq!(
                beneath(Path::new(path), Path::new(root)),
                Path::new(path).starts_with(root),
                "{path} in {root}"
            );
        }
    }
}

#[cfg(test)]
mod folder_action_tests {
    use super::*;
    use crate::library::selection_tests::book;
    #[test]
    fn empty_ancestors_and_component_rewrites_keep_identities_and_pages() {
        let mut b = book(42);
        b.path = "/books/Tolkien/a.epub".into();
        let mut index = LibraryIndex {
            books: vec![b],
            empty_folders: vec!["/books/Tolkien/nested/empty".into(), "/books/PDF".into()],
            ..Default::default()
        };
        let tree = Collections::from_index(&index, &["/books".into()]);
        assert_eq!(tree.folders[Path::new("/books/Tolkien/nested")].count, 0);
        assert_eq!(tree.folders[Path::new("/books")].count, 1);
        let mut nav = Navigation {
            folder: Some("/books/Tolkien/nested".into()),
            folder_pages: BTreeMap::from([("/books/Tolkien/nested".into(), 3), ("/books/Tolkien2".into(), 2)]),
            ..Default::default()
        };
        index.move_path(Path::new("/books/Tolkien"), Path::new("/books/Renamed"));
        nav.move_path(Path::new("/books/Tolkien"), Path::new("/books/Renamed"));
        assert_eq!(index.books[0].id, "42");
        assert_eq!(index.books[0].path, Path::new("/books/Renamed/a.epub"));
        assert_eq!(nav.folder, Some("/books/Renamed/nested".into()));
        assert_eq!(nav.folder_pages[Path::new("/books/Renamed/nested")], 3);
        assert_eq!(nav.folder_pages[Path::new("/books/Tolkien2")], 2);
        index.remove_path(Path::new("/books/Renamed"));
        assert!(index.books.is_empty());
        assert_eq!(index.empty_folders, [PathBuf::from("/books/PDF")]);
        assert_eq!(
            serde_json::from_str::<LibraryIndex>(r#"{"version":1,"books":[]}"#).unwrap(),
            LibraryIndex::default()
        );
    }
}
