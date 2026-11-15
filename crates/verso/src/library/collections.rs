//! A filesystem-free view of the available index. Paths identify folders;
//! book identities and all reading/cover state remain in the library.
use super::{LibraryIndex, Sort};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tab {
    #[default]
    AllBooks,
    Collections,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Navigation {
    pub tab: Tab,
    pub folder: Option<PathBuf>,
    pub all_page: usize,
    pub all_sort: Option<Sort>,
    pub collections_sort: Option<Sort>,
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

    pub fn sort(&self, fallback: Sort) -> Sort {
        match self.tab {
            Tab::AllBooks => self.all_sort,
            Tab::Collections => self.collections_sort,
        }
        .unwrap_or(fallback)
    }
    pub fn set_sort(&mut self, sort: Sort) {
        match self.tab {
            Tab::AllBooks => self.all_sort = Some(sort),
            Tab::Collections => self.collections_sort = Some(sort),
        }
    }
    pub fn page(&self) -> usize {
        match self.tab {
            Tab::AllBooks => self.all_page,
            Tab::Collections => {
                self.folder.as_ref().map_or(self.home_page, |p| self.folder_pages.get(p).copied().unwrap_or(0))
            }
        }
    }
    pub fn set_page(&mut self, page: usize) {
        match self.tab {
            Tab::AllBooks => self.all_page = page,
            Tab::Collections => match &self.folder {
                Some(path) => {
                    self.folder_pages.insert(path.clone(), page);
                }
                None => self.home_page = page,
            },
        }
    }
}

#[derive(Default)]
pub struct Folder {
    pub parent: Option<PathBuf>,
    pub children: BTreeSet<PathBuf>,
    pub books: BTreeSet<PathBuf>,
    pub count: usize,
}

#[derive(Default)]
pub struct Collections {
    pub folders: BTreeMap<PathBuf, Folder>,
    pub roots: BTreeSet<PathBuf>,
}
impl Collections {
    pub fn from_index(index: &LibraryIndex, roots: &[PathBuf]) -> Self {
        let roots = distinct_roots(roots);
        let mut result = Self::default();
        let mut seen = BTreeSet::new();
        for book in index.books.iter().filter(|b| b.available) {
            let path = normalize(&book.path);
            let Some(root) = roots.iter().find(|r| path.starts_with(r)) else { continue };
            if !seen.insert(path.clone()) {
                continue;
            }
            let Some(mut folder) = path.parent() else { continue };
            // A misconfigured root may name the EPUB itself, rather than a
            // directory. Never walk above the selected root in cached data.
            if !folder.starts_with(root) {
                continue;
            }
            result.folders.entry(folder.to_owned()).or_default().books.insert(path.clone());
            loop {
                let node = result.folders.entry(folder.to_owned()).or_default();
                node.count += 1;
                if folder == root {
                    result.roots.insert(root.clone());
                    break;
                }
                let parent = folder.parent().expect("folder is beneath its absolute root");
                node.parent = Some(parent.to_owned());
                result.folders.entry(parent.to_owned()).or_default().children.insert(folder.to_owned());
                folder = parent;
            }
        }
        for path in &index.empty_folders {
            let path = normalize(path);
            let Some(root) = roots.iter().find(|r| path.starts_with(r)) else { continue };
            if path.strip_prefix(root).unwrap().components().any(|part| {
                let name = part.as_os_str().to_string_lossy();
                name.starts_with('.') || name.ends_with(".sdr")
            }) {
                continue;
            }
            let mut folder = path.as_path();
            loop {
                let node = result.folders.entry(folder.to_owned()).or_default();
                if folder == root {
                    result.roots.insert(root.clone());
                    break;
                }
                let parent = folder.parent().expect("folder beneath root");
                node.parent = Some(parent.to_owned());
                result.folders.entry(parent.to_owned()).or_default().children.insert(folder.to_owned());
                folder = parent;
            }
        }
        result
    }

    pub fn children(&self, folder: Option<&Path>) -> Vec<PathBuf> {
        let paths = match folder {
            Some(p) => self.folders.get(p).map(|f| &f.children),
            None => Some(&self.roots),
        };
        let mut paths: Vec<_> = paths.into_iter().flatten().cloned().collect();
        paths.sort_by_cached_key(|p| (name(p).to_lowercase(), p.clone()));
        paths
    }

    /// Keep the closest surviving ancestor, or return to the collection roots.
    pub fn restore(&self, folder: Option<&Path>) -> Option<PathBuf> {
        folder?.ancestors().find(|p| self.folders.contains_key(*p)).map(Path::to_owned)
    }
}

pub fn name(path: &Path) -> String {
    path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned()
}

/// Lexical normalization needs no stat/read_dir and works for offline roots.
pub fn normalize(path: &Path) -> PathBuf {
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
