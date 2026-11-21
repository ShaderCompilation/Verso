//! Choosing a folder: browse subfolders from the user storage root and add
//! the one on screen. What adding means (library or dictionary folders) is
//! up to a [`FolderTarget`]; Settings lists and removes folders.
use std::path::{Path, PathBuf};

use crate::geom::Rect;
use crate::gfx::HAlign;
use crate::screens::{Action, EventCx, Screen};
use crate::store::{Store, with_folder};
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK2, Theme};
use crate::ui::widget::DrawCx;
use crate::ui::widgets::{Button, ButtonKind, List, ListEvent, Pager, PagerEvent, Row, TopBar};

/// Browsing never goes above this folder.
pub fn browse_root() -> PathBuf {
    #[cfg(feature = "kindle")]
    {
        PathBuf::from("/mnt/us")
    }
    #[cfg(feature = "desktop")]
    {
        PathBuf::from("/")
    }
}

/// Where browsing starts.
fn browse_start() -> PathBuf {
    #[cfg(feature = "kindle")]
    {
        browse_root()
    }
    #[cfg(feature = "desktop")]
    {
        std::env::var_os("HOME").map_or_else(browse_root, PathBuf::from)
    }
}

/// A folder as people know it: relative to the Kindle's storage
/// ("documents/Tolkien"), or with `~` for the desktop home folder.
pub fn display_folder(path: &Path) -> String {
    #[cfg(feature = "kindle")]
    let base: Option<(PathBuf, &str)> = Some((browse_root(), "Kindle storage"));
    #[cfg(feature = "desktop")]
    let base: Option<(PathBuf, &str)> = std::env::var_os("HOME").map(|home| (PathBuf::from(home), "~"));
    display_relative(path, base.as_ref().map(|(p, name)| (p.as_path(), *name)))
}

fn display_relative(path: &Path, base: Option<(&Path, &str)>) -> String {
    let Some((base, name)) = base else { return path.display().to_string() };
    match path.strip_prefix(base) {
        Ok(rest) if rest.as_os_str().is_empty() => name.to_owned(),
        Ok(rest) if name == "~" => format!("~/{}", rest.display()),
        Ok(rest) => rest.display().to_string(),
        Err(_) => path.display().to_string(),
    }
}

/// Visible subfolders, sorted by name. Hidden folders and symlinked
/// directories are left out, matching library and dictionary discovery.
fn subfolders(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut folders: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    folders.sort_by_cached_key(|p| p.file_name().unwrap_or_default().to_string_lossy().to_lowercase());
    Ok(folders)
}

/// What the picker does with the folder on screen.
pub trait FolderTarget {
    /// Whether `folder` can be added, and the note shown above the button.
    fn check(&self, folder: &Path) -> (bool, String);
    /// Saves `folder`. `Ok` carries the toast and closes the picker; `Err`
    /// carries the toast and keeps it open.
    fn add(&mut self, folder: &Path) -> Result<String, String>;
}

/// Library folders, saved in `settings.json`.
pub struct LibraryFolders {
    pub store: Store,
    pub folders: Vec<PathBuf>,
}
impl LibraryFolders {
    /// The folder list with `folder` added, if it isn't covered yet.
    fn addition(&self, folder: &Path) -> Option<Vec<PathBuf>> {
        if Store::library_folders_overridden() {
            return None;
        }
        with_folder(&self.folders, folder)
    }
}
impl FolderTarget for LibraryFolders {
    fn check(&self, folder: &Path) -> (bool, String) {
        if Store::library_folders_overridden() {
            (false, "Library folders are set by VERSO_BOOKS.".into())
        } else if self.addition(folder).is_some() {
            (true, "Books in this folder and its subfolders will appear in your library.".into())
        } else {
            (false, "This folder is already in your library.".into())
        }
    }
    fn add(&mut self, folder: &Path) -> Result<String, String> {
        let Some(folders) = self.addition(folder) else { return Err("This folder is already in your library.".into()) };
        match self.store.save_library_folders(folders) {
            Ok(()) => Ok(format!("Added {}", display_folder(folder))),
            Err(e) => {
                log::error!("saving library folders: {e:#}");
                Err("Couldn’t save library folders".into())
            }
        }
    }
}

pub struct FolderPicker {
    top: TopBar,
    target: Box<dyn FolderTarget>,
    note_text: String,
    root: PathBuf,
    current: PathBuf,
    /// The list's first row goes up a level (unless at the root).
    up: bool,
    entries: Vec<PathBuf>,
    error: Option<String>,
    path: Rect,
    list: List,
    list_rect: Rect,
    pager: Pager,
    note: Rect,
    add: Button,
}

impl FolderPicker {
    /// Adds a library folder.
    pub fn new(t: &Theme, screen: Rect, store: Store, folders: Vec<PathBuf>) -> Self {
        Self::with_target(t, screen, Box::new(LibraryFolders { store, folders }))
    }

    /// Adds the chosen folder through `target`.
    pub fn with_target(t: &Theme, screen: Rect, target: Box<dyn FolderTarget>) -> Self {
        Self::at(t, screen, target, browse_root(), browse_start())
    }

    pub(crate) fn at(t: &Theme, screen: Rect, target: Box<dyn FolderTarget>, root: PathBuf, start: PathBuf) -> Self {
        let mut top = TopBar::new("Add folder", true);
        top.place(screen, t);
        let content = screen.split_top(t.top_bar).1.inset_xy(t.margin, 0);
        let path = Rect::new(content.x, content.y + t.mm(2.0), content.w, t.mm(8.0));
        let mut add = Button::new("Add this folder", ButtonKind::Primary);
        add.place(Rect::new(content.x, screen.bottom() - t.margin - t.button_h, content.w, t.button_h), t);
        let note = Rect::new(content.x, add.rect().y - t.mm(11.0), content.w, t.mm(9.0));
        let mut pager = Pager::new(1);
        pager.place(Rect::new(content.x, note.y - t.mm(12.0), content.w, t.mm(11.0)), t);
        // Rows span the screen; their text lines up with the margin.
        let list_rect = Rect::new(screen.x, path.bottom() + t.mm(1.0), screen.w, pager.rect().y - path.bottom());
        let start = if start.starts_with(&root) { start } else { root.clone() };
        let mut this = Self {
            top,
            target,
            note_text: String::new(),
            root,
            current: start,
            up: false,
            entries: Vec::new(),
            error: None,
            path,
            list: List::new(Vec::new()),
            list_rect,
            pager,
            note,
            add,
        };
        this.open(this.current.clone(), t);
        this
    }

    fn open(&mut self, dir: PathBuf, t: &Theme) {
        match subfolders(&dir) {
            Ok(entries) => {
                self.entries = entries;
                self.error = None;
            }
            Err(e) => {
                log::warn!("listing {}: {e}", dir.display());
                self.entries.clear();
                self.error = Some("Couldn’t open this folder.".into());
            }
        }
        self.current = dir;
        self.up = self.current != self.root;
        let mut rows = Vec::new();
        if self.up {
            let parent = self.current.parent().unwrap_or(&self.root);
            rows.push(Row::new(format!("Up to {}", display_folder(parent))));
        }
        rows.extend(
            self.entries.iter().map(|p| Row::new(p.file_name().unwrap_or_default().to_string_lossy()).chevron()),
        );
        self.list = List::new(rows);
        self.list.place(self.list_rect, t.row, t.margin);
        let pages = self.list.paging().pages();
        self.pager.set(0, pages);
        (self.add.enabled, self.note_text) = self.target.check(&self.current);
    }
}

impl Screen for FolderPicker {
    fn draw(&mut self, cx: &mut DrawCx) {
        let t = cx.theme;
        self.top.draw(cx);
        cx.line(&display_folder(&self.current), t.body, self.path, HAlign::Left);
        if let Some(error) = &self.error {
            let rect = Rect::new(self.path.x, self.list_rect.y + t.row, self.path.w, t.row);
            cx.line(error, t.body.gray(INK2), rect, HAlign::Left);
        } else if self.entries.is_empty() {
            let rect = Rect::new(self.path.x, self.list_rect.y + if self.up { t.row } else { 0 }, self.path.w, t.row);
            cx.line("No subfolders", t.body.gray(INK2), rect, HAlign::Left);
        }
        self.list.draw(cx);
        self.pager.draw(cx);
        cx.paragraph(&self.note_text, t.small.gray(INK2), self.note, HAlign::Left);
        self.add.draw(cx);
    }

    fn on_gesture(&mut self, g: Gesture, cx: &mut EventCx) -> Action {
        cx.redraw();
        if self.top.on_gesture(&g) {
            return Action::Pop;
        }
        if let Gesture::Swipe { dir, from, .. } = g
            && self.list_rect.contains(from)
            && let Some(page) = self.pager.on_swipe(dir)
        {
            self.list.page = page;
            return Action::None;
        }
        if let Some(PagerEvent::Changed(page)) = self.pager.on_gesture(&g) {
            self.list.page = page;
            return Action::None;
        }
        if let Some(ListEvent::Tapped(i)) = self.list.on_gesture(&g) {
            let next = match (self.up, i) {
                (true, 0) => self.current.parent().map_or_else(|| self.root.clone(), Path::to_path_buf),
                (up, i) => self.entries[i - usize::from(up)].clone(),
            };
            self.open(next, cx.theme);
            return Action::None;
        }
        if self.add.on_gesture(&g) {
            return match self.target.add(&self.current) {
                Ok(toast) => {
                    cx.toast(toast, None);
                    Action::Pop
                }
                Err(toast) => {
                    cx.toast(toast, None);
                    // Show why, e.g. the folder was added meanwhile.
                    (self.add.enabled, self.note_text) = self.target.check(&self.current);
                    Action::None
                }
            };
        }
        Action::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_display_relative_to_storage_or_home() {
        let kindle = Some((Path::new("/mnt/us"), "Kindle storage"));
        assert_eq!(display_relative(Path::new("/mnt/us/documents/Tolkien"), kindle), "documents/Tolkien");
        assert_eq!(display_relative(Path::new("/mnt/us"), kindle), "Kindle storage");
        assert_eq!(display_relative(Path::new("/mnt/usb/books"), kindle), "/mnt/usb/books");
        let home = Some((Path::new("/home/reader"), "~"));
        assert_eq!(display_relative(Path::new("/home/reader/Books"), home), "~/Books");
        assert_eq!(display_relative(Path::new("/srv/books"), None), "/srv/books");
    }

    #[test]
    fn browsing_stays_inside_the_root_and_adds_new_folders_only() -> anyhow::Result<()> {
        let root = std::env::temp_dir().join(format!("verso-folders-{}", std::process::id()));
        for dir in ["documents/Tolkien", "books/b", "books/A", ".hidden"] {
            std::fs::create_dir_all(root.join(dir))?;
        }
        std::fs::write(root.join("file.epub"), b"")?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("books"), root.join("documents/linked"))?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let store = Store::at(root.join(".data"));
        let folders = vec![root.join("documents")];
        let target = LibraryFolders { store: store.clone(), folders: folders.clone() };
        let library = LibraryFolders { store, folders };
        let mut picker = FolderPicker::at(&t, screen, Box::new(target), root.clone(), PathBuf::from("/elsewhere"));
        assert_eq!(picker.current, root, "a start outside the root falls back to it");
        assert!(!picker.up);
        let names: Vec<_> = picker.list.rows.iter().map(|r| r.label.clone()).collect();
        assert_eq!(names, ["books", "documents"], "sorted, folders only, no hidden ones");
        assert!(picker.add.enabled, "the root itself would absorb documents");
        picker.open(root.join("documents"), &t);
        let doc_names: Vec<_> = picker.list.rows.iter().map(|r| r.label.clone()).collect();
        assert_eq!(doc_names.len(), 2, "up row and Tolkien only, no symlinked folder: {doc_names:?}");
        assert_eq!(doc_names[1], "Tolkien");
        picker.open(root.join("documents/Tolkien"), &t);
        assert!(picker.up && !picker.add.enabled, "inside a library folder");
        picker.open(root.join("books"), &t);
        let names: Vec<_> = picker.list.rows.iter().map(|r| r.label.clone()).collect();
        assert_eq!(names.len(), 3, "up row, then A and b: {names:?}");
        assert!(names[0].starts_with("Up to ") && names[1] == "A" && names[2] == "b");
        assert_eq!(library.addition(&picker.current), Some(vec![root.join("documents"), root.join("books")]));
        picker.open(root.join("missing"), &t);
        assert!(picker.error.is_some() && picker.entries.is_empty());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
