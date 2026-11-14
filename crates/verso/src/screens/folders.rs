//! Choosing a library folder: browse subfolders from the user storage root
//! and add the one on screen. Settings lists and removes folders.
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

/// Visible subfolders, sorted by name. Hidden folders are left out.
fn subfolders(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut folders: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    folders.sort_by_cached_key(|p| p.file_name().unwrap_or_default().to_string_lossy().to_lowercase());
    Ok(folders)
}

pub struct FolderPicker {
    top: TopBar,
    store: Store,
    folders: Vec<PathBuf>,
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
    pub fn new(t: &Theme, screen: Rect, store: Store, folders: Vec<PathBuf>) -> Self {
        Self::at(t, screen, store, folders, browse_root(), browse_start())
    }

    fn at(t: &Theme, screen: Rect, store: Store, folders: Vec<PathBuf>, root: PathBuf, start: PathBuf) -> Self {
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
            store,
            folders,
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
        self.add.enabled = self.addition().is_some();
    }

    /// The folder list with the current folder added, if it isn't covered yet.
    fn addition(&self) -> Option<Vec<PathBuf>> {
        if Store::library_folders_overridden() {
            return None;
        }
        with_folder(&self.folders, &self.current)
    }

    fn note(&self) -> &'static str {
        if Store::library_folders_overridden() {
            "Library folders are set by VERSO_BOOKS."
        } else if self.add.enabled {
            "Books in this folder and its subfolders will appear in your library."
        } else {
            "This folder is already in your library."
        }
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
        cx.paragraph(self.note(), t.small.gray(INK2), self.note, HAlign::Left);
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
        if self.add.on_gesture(&g)
            && let Some(folders) = self.addition()
        {
            return match self.store.save_library_folders(folders) {
                Ok(()) => {
                    cx.toast(format!("Added {}", display_folder(&self.current)), None);
                    Action::Pop
                }
                Err(e) => {
                    log::error!("saving library folders: {e:#}");
                    cx.toast("Couldn’t save library folders", None);
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
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let store = Store::at(root.join(".data"));
        let folders = vec![root.join("documents")];
        let mut picker = FolderPicker::at(&t, screen, store, folders, root.clone(), PathBuf::from("/elsewhere"));
        assert_eq!(picker.current, root, "a start outside the root falls back to it");
        assert!(!picker.up);
        let names: Vec<_> = picker.list.rows.iter().map(|r| r.label.clone()).collect();
        assert_eq!(names, ["books", "documents"], "sorted, folders only, no hidden ones");
        assert!(picker.add.enabled, "the root itself would absorb documents");
        picker.open(root.join("documents/Tolkien"), &t);
        assert!(picker.up && !picker.add.enabled, "inside a library folder");
        picker.open(root.join("books"), &t);
        let names: Vec<_> = picker.list.rows.iter().map(|r| r.label.clone()).collect();
        assert_eq!(names.len(), 3, "up row, then A and b: {names:?}");
        assert!(names[0].starts_with("Up to ") && names[1] == "A" && names[2] == "b");
        assert_eq!(picker.addition(), Some(vec![root.join("documents"), root.join("books")]));
        picker.open(root.join("missing"), &t);
        assert!(picker.error.is_some() && picker.entries.is_empty());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
