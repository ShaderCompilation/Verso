//! Settings → Dictionary folders: where Verso looks for StarDict
//! dictionaries. Verso's own folder is always searched; other folders can be
//! added (through the folder picker) and removed. Saves are immediate, and the
//! screen always shows what is on disk.
use std::path::{Path, PathBuf};

use crate::dictionary::worker::Worker;
use crate::dictionary::{Dictionary, Source, SourceStatus};
use crate::geom::Rect;
use crate::gfx::{HAlign, Icon};
use crate::library::collections::normalize;
use crate::platform::Waker;
use crate::screens::books::Details;
use crate::screens::folders::{FolderPicker, FolderTarget, display_folder};
use crate::screens::{Action, EventCx, Screen};
use crate::store::{
    KOREADER_DICTIONARY_FOLDER, MAX_DICTIONARY_FOLDERS, Store, find_dictionary_folder, with_dictionary_folder,
    without_dictionary_folder,
};
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK, INK2, LINE, PAPER, Theme};
use crate::ui::widget::{DrawCx, Press, PressEvent};
use crate::ui::widgets::{Button, ButtonKind, IconButton, Pager, PagerEvent, TopBar};

/// The folder offered as **Add KOReader folder** when it isn't configured.
fn koreader_preset() -> Option<PathBuf> {
    #[cfg(feature = "kindle")]
    {
        Some(PathBuf::from(KOREADER_DICTIONARY_FOLDER))
    }
    #[cfg(feature = "desktop")]
    {
        None
    }
}

/// A short name for a configured folder: **KOReader** for its standard
/// folder, otherwise the folder's own name (rows show the path below it).
fn folder_label(folder: &Path) -> String {
    if folder == Path::new(KOREADER_DICTIONARY_FOLDER) {
        "KOReader".into()
    } else {
        folder.file_name().map_or_else(|| display_folder(folder), |n| n.to_string_lossy().into_owned())
    }
}

/// Same folder by spelling, or by real directory when both exist.
fn same_folder(a: &Path, b: &Path) -> bool {
    normalize(a) == normalize(b)
        || std::fs::canonicalize(a).is_ok_and(|a| std::fs::canonicalize(b).is_ok_and(|b| a == b))
}

/// Adds a dictionary folder from the picker. Starts from the list the screen
/// shows and rereads the file before saving.
struct DictionaryTarget {
    store: Store,
    folders: Vec<PathBuf>,
}
impl DictionaryTarget {
    /// Why `folder` can't be added, if it can't.
    fn refusal(&self, folder: &Path) -> Option<String> {
        if same_folder(folder, &self.store.dictionary_dir()) {
            return Some("This is Verso’s dictionary folder, which is always included.".into());
        }
        if let Some(i) = find_dictionary_folder(&self.folders, folder) {
            return Some(format!("Already in your dictionary folders as “{}”.", folder_label(&self.folders[i])));
        }
        (self.folders.len() >= MAX_DICTIONARY_FOLDERS)
            .then(|| format!("Verso can search at most {MAX_DICTIONARY_FOLDERS} folders. Remove one first."))
    }
}
impl FolderTarget for DictionaryTarget {
    fn check(&self, folder: &Path) -> (bool, String) {
        match self.refusal(folder) {
            Some(why) => (false, why),
            None => (true, "Verso will search this folder and its subfolders for StarDict dictionaries.".into()),
        }
    }
    fn add(&mut self, folder: &Path) -> Result<String, String> {
        // Build on what is on disk, not on what the picker opened with.
        self.folders = self.store.dictionary_folders().folders;
        if let Some(why) = self.refusal(folder) {
            return Err(why);
        }
        let Ok(folders) = with_dictionary_folder(&self.folders, folder) else {
            return Err("Already in your dictionary folders.".into());
        };
        match self.store.save_dictionary_folders(&folders) {
            Ok(()) => {
                self.folders = folders;
                Ok(format!("Added {}", folder_label(folder)))
            }
            Err(e) => {
                log::error!("saving dictionary folders: {e:#}");
                Err("Couldn’t save dictionary folders".into())
            }
        }
    }
}

pub struct DictionaryFolders {
    top: TopBar,
    store: Store,
    screen: Rect,
    content: Rect,
    worker: Worker,
    request: u64,
    pending: bool,
    /// External folders as saved on disk.
    folders: Vec<PathBuf>,
    config_error: Option<String>,
    /// `Store::dictionary_folder_saves` when `folders` was read.
    saves_seen: u64,
    preset: Option<PathBuf>,
    /// From the last catalog, until the next one arrives.
    sources: Vec<Source>,
    dictionaries: Vec<Dictionary>,
    // Row 0 is Verso's folder; row i + 1 is `folders[i]`.
    removes: Vec<IconButton>,
    press: Press,
    pressed: Option<usize>,
    row_h: i32,
    list: Rect,
    pager: Pager,
    note: Rect,
    add: Button,
    add_preset: Button,
    rescan: Button,
    details: Option<Details>,
}

impl DictionaryFolders {
    pub fn new(t: &Theme, screen: Rect, store: Store, waker: Waker) -> Self {
        let mut top = TopBar::new("Dictionary folders", true);
        top.place(screen, t);
        let content = screen.split_top(t.top_bar).1.inset(t.margin);
        let worker = Worker::new(store.clone(), waker);
        let mut this = Self {
            top,
            store,
            screen,
            content,
            worker,
            request: 0,
            pending: false,
            folders: Vec::new(),
            config_error: None,
            saves_seen: 0,
            preset: koreader_preset(),
            sources: Vec::new(),
            dictionaries: Vec::new(),
            removes: Vec::new(),
            press: Press::default(),
            pressed: None,
            row_h: t.row2,
            list: Rect::default(),
            pager: Pager::new(1),
            note: Rect::default(),
            add: Button::new("Add folder", ButtonKind::Primary).with_icon(Icon::Folder),
            add_preset: Button::new("Add KOReader folder", ButtonKind::Secondary),
            rescan: Button::new("Rescan", ButtonKind::Secondary),
            details: None,
        };
        this.reload(t);
        // Lists dictionaries without a book or a lookup.
        this.request = this.worker.catalog(false);
        this.pending = true;
        this
    }

    /// Rereads the folder list from disk and lays out the rows.
    fn reload(&mut self, t: &Theme) {
        self.saves_seen = Store::dictionary_folder_saves();
        let config = self.store.dictionary_folders();
        self.folders = config.folders;
        self.config_error = config.error;
        self.removes = self
            .folders
            .iter()
            .map(|_| {
                let mut b = IconButton::new(Icon::Close);
                b.size = Some(t.icon_small);
                b
            })
            .collect();
        self.layout(t);
    }

    fn preset_offered(&self) -> bool {
        self.preset.as_ref().is_some_and(|p| find_dictionary_folder(&self.folders, p).is_none())
            && self.folders.len() < MAX_DICTIONARY_FOLDERS
    }

    fn layout(&mut self, t: &Theme) {
        let c = self.content;
        let gap = t.mm(3.0);
        let buttons = c.bottom() - t.button_h;
        let half = (c.w - gap) / 2;
        self.add.place(Rect::new(c.x, buttons, half, t.button_h), t);
        self.rescan.place(Rect::new(c.right() - half, buttons, half, t.button_h), t);
        let mut above = buttons;
        if self.preset_offered() {
            above -= gap + t.button_h;
            self.add_preset.place(Rect::new(c.x, above, c.w, t.button_h), t);
        }
        let note_h = if self.config_error.is_some() { t.mm(24.0) } else { t.mm(14.0) };
        self.note = Rect::new(c.x, above - gap - note_h, c.w, note_h);
        self.pager.place(Rect::new(c.x, self.note.y - t.mm(12.0), c.w, t.mm(11.0)), t);
        self.list = Rect::new(c.x, c.y, c.w, self.pager.rect().y - c.y);
        let per = self.per_page();
        let page = self.pager.page;
        self.pager.set(page, self.rows().div_ceil(per));
        self.place_removes(t);
    }

    fn rows(&self) -> usize {
        self.folders.len() + 1
    }

    fn per_page(&self) -> usize {
        (self.list.h / self.row_h).max(1) as usize
    }

    fn visible(&self) -> std::ops::Range<usize> {
        let per = self.per_page();
        let start = (self.pager.page * per).min(self.rows());
        start..(start + per).min(self.rows())
    }

    fn row_rect(&self, row: usize) -> Rect {
        let k = (row - self.visible().start) as i32;
        Rect::new(self.list.x, self.list.y + k * self.row_h, self.list.w, self.row_h)
    }

    fn place_removes(&mut self, t: &Theme) {
        for row in self.visible().filter(|&r| r > 0) {
            let r = self.row_rect(row);
            self.removes[row - 1].place(Rect::new(r.right() - t.target + t.mm(2.0), r.y, t.target, r.h));
        }
    }

    /// The configured path of a row.
    fn path(&self, row: usize) -> PathBuf {
        if row == 0 { self.store.dictionary_dir() } else { self.folders[row - 1].clone() }
    }

    fn label(&self, row: usize) -> String {
        if row == 0 { "Verso".into() } else { folder_label(&self.folders[row - 1]) }
    }

    /// The catalog's source for a row, once a catalog for this list arrived.
    fn source(&self, row: usize) -> Option<(usize, &Source)> {
        if row == 0 {
            return self.sources.iter().enumerate().find(|(_, s)| s.native);
        }
        let folder = normalize(&self.folders[row - 1]);
        self.sources.iter().enumerate().find(|(_, s)| !s.native && normalize(&s.root) == folder)
    }

    /// Dictionaries in a row's folder, also those an overlapping folder found.
    fn found(&self, row: usize) -> Vec<&Dictionary> {
        let Some((index, source)) = self.source(row) else { return Vec::new() };
        self.dictionaries
            .iter()
            .filter(|d| {
                if d.source == index || d.path.starts_with(&source.root) {
                    return true;
                }
                let Some(real_root) = &source.real_root else { return false };
                if d.path.starts_with(real_root) {
                    return true;
                }
                // Native dictionaries can retain a symlinked configured path
                // even though the covered source was compared by real path.
                self.sources.get(d.source).is_some_and(|origin| {
                    origin.real_root.as_ref().is_some_and(|origin_real| {
                        d.path
                            .strip_prefix(&origin.root)
                            .is_ok_and(|relative| origin_real.join(relative).starts_with(real_root))
                    })
                })
            })
            .collect()
    }

    /// One line under the label: the state, then the path of added folders.
    fn status(&self, row: usize) -> String {
        if row == 0 {
            self.state(row)
        } else {
            format!("{} · {}", self.state(row), display_folder(&self.folders[row - 1]))
        }
    }

    /// Availability and the number of dictionaries.
    fn state(&self, row: usize) -> String {
        let status = match self.source(row) {
            None => "Checking…".into(),
            Some((_, source)) => {
                let count = match self.found(row).len() {
                    0 => "No dictionaries".into(),
                    1 => "1 dictionary".into(),
                    n => format!("{n} dictionaries"),
                };
                match &source.status {
                    SourceStatus::Available => count,
                    SourceStatus::Absent if source.native => "No dictionaries".into(),
                    SourceStatus::Absent if self.preset.as_deref() == Some(source.root.as_path()) => {
                        "Not installed".into()
                    }
                    SourceStatus::Absent => "Not found".into(),
                    SourceStatus::Unreadable(_) => format!("Couldn’t read everything · {count}"),
                    SourceStatus::LimitReached => format!("Too many files · {count}"),
                }
            }
        };
        if row == 0 { format!("Always included · {status}") } else { status }
    }

    /// The full path, status and dictionaries, for the details sheet.
    fn details_text(&self, row: usize) -> String {
        let mut text = format!("{}\n\n{}", self.path(row).display(), self.state(row));
        match self.source(row).map(|(_, s)| &s.status) {
            Some(SourceStatus::Unreadable(e)) => text += &format!("\n\n{e}"),
            Some(SourceStatus::LimitReached) => {
                text += "\n\nVerso stopped scanning here, so some dictionaries may be missing."
            }
            _ => {}
        }
        if row == 0 {
            text += "\n\nVerso always searches its own folder. Copy StarDict dictionaries here, in any subfolder.";
        }
        let found = self.found(row);
        if !found.is_empty() {
            text += "\n\nDictionaries:";
            for d in found {
                text += &format!("\n• {} ({})", d.name, d.relative.display());
            }
        }
        text
    }

    fn note_text(&self) -> String {
        match &self.config_error {
            Some(e) => format!(
                "Dictionary folder settings couldn’t be read, so only Verso’s folder is searched. Adding a folder replaces them. {e}"
            ),
            None => "Verso searches these folders and their subfolders. Removing a folder doesn’t delete anything. Tap a folder for details.".into(),
        }
    }

    /// Saves `folders`, then shows what is on disk and refreshes the catalog.
    fn save(&mut self, folders: Vec<PathBuf>, done: String, cx: &mut EventCx) {
        match self.store.save_dictionary_folders(&folders) {
            Ok(()) => cx.toast(done, None),
            Err(e) => {
                log::error!("saving dictionary folders: {e:#}");
                cx.toast("Couldn’t save dictionary folders", None);
            }
        }
        self.changed(cx.theme);
    }

    /// The folder list may have changed on disk: reread it and, if it did,
    /// rediscover with the new configuration.
    fn changed(&mut self, t: &Theme) {
        let before = (self.folders.clone(), self.config_error.clone());
        self.reload(t);
        if (self.folders.clone(), self.config_error.clone()) != before {
            self.request = self.worker.catalog(true);
            self.pending = true;
        }
    }

    fn remove(&mut self, index: usize, cx: &mut EventCx) {
        let folder = self.folders[index].clone();
        let folders = without_dictionary_folder(&self.folders, &folder);
        self.save(folders, format!("Removed {}", folder_label(&folder)), cx);
    }

    fn restore_preset(&mut self, cx: &mut EventCx) {
        let Some(preset) = self.preset.clone() else { return };
        if let Ok(folders) = with_dictionary_folder(&self.folders, &preset) {
            self.save(folders, "Added KOReader".into(), cx);
        }
    }

    fn picker(&self, t: &Theme) -> FolderPicker {
        FolderPicker::with_target(t, self.screen, self.target())
    }

    fn target(&self) -> Box<dyn FolderTarget> {
        Box::new(DictionaryTarget { store: self.store.clone(), folders: self.folders.clone() })
    }

    fn rescan(&mut self) {
        self.request = self.worker.catalog(true);
        self.pending = true;
    }

    fn receive(&mut self) -> bool {
        let Some(reply) = self.worker.receive() else { return false };
        if reply.id != self.request {
            return false;
        }
        self.pending = false;
        self.sources = reply.sources;
        self.dictionaries = reply.dictionaries;
        true
    }
}

impl Screen for DictionaryFolders {
    fn draw(&mut self, cx: &mut DrawCx) {
        let t = cx.theme;
        self.top.draw(cx);
        for row in self.visible() {
            let r = self.row_rect(row);
            let pressed = self.pressed == Some(row) && self.press.is_down();
            let (fg, fg2) = if pressed { (PAPER, PAPER) } else { (INK, INK2) };
            if pressed {
                cx.fill(r, INK);
            }
            let text = Rect { w: r.w - t.target, ..r };
            let (top, bottom) = text.split_top(r.h / 2 + t.mm(0.4));
            cx.line(&self.label(row), t.body.gray(fg), top.moved(0, t.mm(1.2)), HAlign::Left);
            cx.line(&self.status(row), t.small.gray(fg2), bottom.moved(0, -t.mm(1.0)), HAlign::Left);
            if row > 0 && !pressed {
                self.removes[row - 1].draw(cx);
            }
            cx.hline(r.bottom() - t.hairline / 2 - 1, r.x, r.right(), t.hairline, LINE);
        }
        self.pager.draw(cx);
        cx.paragraph(&self.note_text(), t.small.gray(INK2), self.note, HAlign::Left);
        if self.preset_offered() {
            self.add_preset.draw(cx);
        }
        self.add.draw(cx);
        self.rescan.enabled = !self.pending;
        self.rescan.draw(cx);
        if let Some(details) = &mut self.details {
            details.draw(cx);
        }
    }

    fn on_gesture(&mut self, g: Gesture, cx: &mut EventCx) -> Action {
        cx.redraw();
        if let Some(details) = &mut self.details {
            if details.on_gesture(&g, cx) {
                self.details = None;
            }
            return Action::None;
        }
        if self.top.on_gesture(&g) {
            return Action::Pop;
        }
        if let Gesture::Swipe { dir, from, .. } = g
            && self.list.contains(from)
            && self.pager.on_swipe(dir).is_some()
        {
            self.place_removes(cx.theme);
            return Action::None;
        }
        if let Some(PagerEvent::Changed(_)) = self.pager.on_gesture(&g) {
            self.place_removes(cx.theme);
            return Action::None;
        }
        if let Some(i) = self.visible().filter(|&r| r > 0).map(|r| r - 1).find(|&i| self.removes[i].on_gesture(&g)) {
            self.remove(i, cx);
            return Action::None;
        }
        // Rows open their details; the remove button's area belongs to it.
        if let Gesture::Down(p) = g {
            self.pressed = self.visible().find(|&row| {
                let r = self.row_rect(row);
                r.contains(p) && (row == 0 || !self.removes[row - 1].rect().contains(p))
            });
        }
        if let Some(row) = self.pressed {
            let event = self.press.on_gesture(&g, self.row_rect(row));
            if !self.press.is_down() {
                self.pressed = None;
            }
            if let Some(PressEvent::Tap) = event {
                let text = self.details_text(row);
                self.details = Some(Details::from_text(cx.theme, self.screen, &self.label(row), text));
            }
            return Action::None;
        }
        if self.preset_offered() && self.add_preset.on_gesture(&g) {
            self.restore_preset(cx);
            return Action::None;
        }
        if self.add.on_gesture(&g) {
            return Action::Push(Box::new(self.picker(cx.theme)));
        }
        if !self.pending && self.rescan.on_gesture(&g) {
            self.rescan();
        }
        Action::None
    }

    fn busy(&self) -> bool {
        self.pending
    }

    /// The picker closed (it may have saved) or the catalog arrived.
    fn on_background(&mut self, cx: &mut EventCx) {
        if Store::dictionary_folder_saves() != self.saves_seen {
            self.changed(cx.theme);
            cx.redraw();
        }
        if self.receive() {
            cx.redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        root: PathBuf,
        store: Store,
        theme: Theme,
        screen: Rect,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "verso-dictionary-folders-screen-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&root).unwrap();
            let root = std::fs::canonicalize(root).unwrap();
            let store = Store::at(root.join("data"));
            Self { root, store, theme: Theme::for_dpi(300), screen: Rect::new(0, 0, 1272, 1696) }
        }
        fn screen(&self) -> DictionaryFolders {
            let mut s = DictionaryFolders::new(&self.theme, self.screen, self.store.clone(), Waker::new(|| {}));
            s.preset = Some(PathBuf::from(KOREADER_DICTIONARY_FOLDER));
            s.layout(&self.theme);
            s
        }
        fn on_disk(&self) -> Vec<PathBuf> {
            self.store.dictionary_folders().folders
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    fn with_cx<R>(theme: &Theme, f: impl FnOnce(&mut EventCx) -> R) -> (R, Option<String>) {
        let mut toast = None;
        let mut cx = EventCx {
            theme,
            screen: Rect::new(0, 0, 1272, 1696),
            now: Instant::now(),
            toast: &mut toast,
            forced: &mut Vec::new(),
            needs_redraw: &mut false,
            battery: None,
        };
        let result = f(&mut cx);
        (result, toast.map(|t| t.text))
    }
    fn wait(s: &mut DictionaryFolders) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !s.receive() || s.pending {
            assert!(Instant::now() < deadline, "no catalog");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn verso_row_is_fixed_and_folders_are_added_removed_and_listed() {
        let f = Fixture::new();
        f.store.save_dictionary_folders(&[]).unwrap();
        let parent = f.root.join("dicts");
        let child = parent.join("en");
        std::fs::create_dir_all(&child).unwrap();
        let mut s = f.screen();
        assert_eq!(s.rows(), 1);
        assert_eq!(s.label(0), "Verso");
        assert!(s.removes.is_empty(), "no Remove action on Verso's folder");
        wait(&mut s);
        assert_eq!(s.status(0), "Always included · No dictionaries");
        assert!(s.details_text(0).contains(&f.store.dictionary_dir().display().to_string()), "full path");

        // Add through the picker's target, then the screen notices on return.
        let mut target = s.target();
        assert!(target.check(&parent).0);
        assert!(target.add(&parent).is_ok());
        assert_eq!(f.on_disk(), vec![parent.clone()]);
        let mut target = s.target();
        assert!(target.add(&child).is_ok());
        with_cx(&f.theme, |cx| s.on_background(cx));
        assert_eq!(s.folders, vec![parent.clone(), child.clone()]);
        // A fast catalog reply can clear `pending` within that same
        // on_background call, so the flag may already be down. Either the
        // refresh is still running (the new row is unchecked) or its reply
        // has arrived; both prove the change triggered a refresh.
        if s.pending {
            assert!(s.state(2).starts_with("Checking"), "a refresh in flight leaves the new folder unchecked");
            wait(&mut s);
        }
        assert!(s.status(1).starts_with("No dictionaries · "));
        assert_eq!(s.label(2), "en");
        assert_eq!(s.details_text(2).lines().next(), Some(child.display().to_string().as_str()), "full path");

        // Removing the parent keeps the explicitly added child.
        let (_, toast) = with_cx(&f.theme, |cx| s.remove(0, cx));
        assert!(toast.unwrap().starts_with("Removed"));
        assert_eq!(f.on_disk(), vec![child.clone()]);
        assert_eq!(s.folders, vec![child]);
    }

    #[test]
    fn duplicates_point_to_the_existing_row_and_cancel_changes_nothing() {
        let f = Fixture::new();
        let real = f.root.join("real");
        std::fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, f.root.join("alias")).unwrap();
        f.store.save_dictionary_folders(std::slice::from_ref(&real)).unwrap();
        let s = f.screen();
        let mut target = s.target();
        for same in [real.clone(), f.root.join("real/./"), f.root.join("alias")] {
            let (ok, note) = target.check(&same);
            assert!(!ok && note.contains("Already in your dictionary folders"), "{note}");
            assert!(target.add(&same).is_err());
        }
        let (ok, note) = target.check(&f.store.dictionary_dir());
        assert!(!ok && note.contains("always included"), "{note}");
        assert_eq!(f.on_disk(), vec![real.clone()]);

        // Cancel: the picker closes without saving.
        let mut picker = FolderPicker::at(&f.theme, f.screen, s.target(), f.root.clone(), f.root.clone());
        let t = &f.theme;
        let back = crate::geom::Point::new(t.margin + t.target / 2 - t.mm(2.0), t.top_bar / 2);
        let (action, _) = with_cx(&f.theme, |cx| {
            picker.on_gesture(Gesture::Down(back), cx);
            picker.on_gesture(Gesture::Tap(back), cx)
        });
        assert!(matches!(action, Action::Pop));
        assert_eq!(f.on_disk(), vec![real]);
    }

    #[test]
    fn symlinked_child_covered_by_parent_reports_its_dictionaries() {
        let f = Fixture::new();
        let real = f.root.join("real");
        let child = real.join("child");
        std::fs::create_dir_all(&child).unwrap();
        let ifo = child.join("words.ifo");
        std::fs::write(&ifo, "StarDict's dict ifo file\nversion=3.0.0\nbookname=Words\nwordcount=1\nidxfilesize=13\nsametypesequence=m\n").unwrap();
        let mut idx = b"word\0".to_vec();
        idx.extend_from_slice(&0u32.to_be_bytes());
        idx.extend_from_slice(&4u32.to_be_bytes());
        std::fs::write(child.join("words.idx"), idx).unwrap();
        std::fs::write(child.join("words.dict"), b"text").unwrap();
        let sibling = real.join("sibling");
        std::fs::create_dir_all(&sibling).unwrap();
        for name in ["words.ifo", "words.idx", "words.dict"] {
            std::fs::copy(child.join(name), sibling.join(name)).unwrap();
        }
        std::os::unix::fs::symlink(&real, f.root.join("alias")).unwrap();
        let alias_child = f.root.join("alias/child");
        f.store.save_dictionary_folders(&[real, alias_child]).unwrap();
        let mut s = f.screen();
        wait(&mut s);
        assert_eq!(s.sources[2].status, SourceStatus::Available);
        assert_eq!(s.found(1).len(), 2);
        assert_eq!(s.found(2).len(), 1);
        assert!(s.state(2).starts_with("1 dictionary"));
        assert!(s.details_text(2).contains("Words"));
    }

    #[test]
    fn koreader_preset_is_offered_until_configured_even_when_absent() {
        let f = Fixture::new();
        f.store.save_dictionary_folders(&[]).unwrap();
        let mut s = f.screen();
        assert!(s.preset_offered());
        let (_, toast) = with_cx(&f.theme, |cx| s.restore_preset(cx));
        assert_eq!(toast.as_deref(), Some("Added KOReader"));
        assert_eq!(f.on_disk(), vec![PathBuf::from(KOREADER_DICTIONARY_FOLDER)]);
        assert!(!s.preset_offered());
        assert_eq!(s.label(1), "KOReader");
        wait(&mut s);
        if !Path::new(KOREADER_DICTIONARY_FOLDER).exists() {
            assert!(s.status(1).starts_with("Not installed · "));
        }
        with_cx(&f.theme, |cx| s.remove(0, cx));
        assert!(f.on_disk().is_empty(), "removed KOReader stays removed");
        assert!(s.preset_offered());
    }

    #[test]
    fn failed_saves_report_and_keep_what_is_on_disk() {
        let f = Fixture::new();
        let kept = f.root.join("kept");
        f.store.save_dictionary_folders(std::slice::from_ref(&kept)).unwrap();
        let mut s = f.screen();
        // A directory where the temporary file goes makes every save fail.
        std::fs::create_dir_all(f.root.join("data/dictionary-folders.json.tmp")).unwrap();
        let (_, toast) = with_cx(&f.theme, |cx| s.remove(0, cx));
        assert_eq!(toast.as_deref(), Some("Couldn’t save dictionary folders"));
        assert_eq!(s.folders, vec![kept.clone()], "still shows what is on disk");
        assert_eq!(f.on_disk(), vec![kept.clone()]);
        let (_, toast) = with_cx(&f.theme, |cx| s.restore_preset(cx));
        assert_eq!(toast.as_deref(), Some("Couldn’t save dictionary folders"));
        assert_eq!(s.folders, vec![kept.clone()]);
        let other = f.root.join("other");
        std::fs::create_dir_all(&other).unwrap();
        let mut target = s.target();
        assert_eq!(target.add(&other), Err("Couldn’t save dictionary folders".into()));
        assert_eq!(f.on_disk(), vec![kept]);
    }

    #[test]
    fn a_damaged_file_is_shown_and_only_replaced_by_an_add() {
        let f = Fixture::new();
        std::fs::create_dir_all(f.root.join("data")).unwrap();
        std::fs::write(f.root.join("data/dictionary-folders.json"), "{broken").unwrap();
        let s = f.screen();
        assert!(s.folders.is_empty());
        assert!(s.note_text().contains("couldn’t be read"));
        assert_eq!(std::fs::read_to_string(f.root.join("data/dictionary-folders.json")).unwrap(), "{broken");
    }
}
