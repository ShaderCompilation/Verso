//! Frozen cut selection and an ordered paste, suspended only for name clashes.
use super::*;
use crate::ui::widgets::Toggle;
use files::{ClashChoice, MoveItem, MoveOutcome};
use std::path::Path;

pub(super) struct Carry {
    items: Vec<MoveItem>,
    paste: Option<Paste>,
}
struct Paste {
    destination: PathBuf,
    next: usize,
    moved: usize,
    skipped: usize,
    choice: Option<ClashChoice>,
    changes: Vec<PathChange>,
}
#[derive(Debug, PartialEq, Eq)]
pub(super) enum PasteEnd {
    Done,
    /// The clash sheet was closed: the remaining items stay where they are.
    Cancelled,
    Failed(String),
}
pub(super) struct ClashSheet {
    sheet: Sheet,
    message: String,
    text: Rect,
    all: Toggle,
    all_label: Rect,
    skip: Button,
    keep: Button,
}
impl ClashSheet {
    fn new(t: &Theme, screen: Rect, item: &MoveItem, destination: &Path) -> Self {
        let sheet = Sheet::open("Name already taken", t.mm(65.0), screen, t);
        let c = sheet.content(t);
        let text = Rect::new(c.x, c.y, c.w, t.mm(17.0));
        let message = match item {
            MoveItem::Book(path) => format!("“{}” or its KOReader data already exists here.", collections::name(path)),
            // A plain file can take a folder's name too.
            MoveItem::Folder(path) if path.file_name().is_some_and(|n| !destination.join(n).is_dir()) => {
                format!("“{}” already exists here.", collections::name(path))
            }
            MoveItem::Folder(path) => format!("A folder named “{}” already exists here.", collections::name(path)),
        };
        let all_label = Rect::new(c.x, text.bottom() + t.mm(2.0), c.w - t.mm(15.0), t.target);
        let mut all = Toggle::new(false);
        all.place(c.right(), all_label.center().y, t);
        let y = all_label.bottom() + t.mm(4.0);
        let w = (c.w - t.mm(3.0)) / 2;
        let mut skip = Button::new("Skip", ButtonKind::Secondary);
        skip.place(Rect::new(c.x, y, w, t.button_h), t);
        let mut keep = Button::new("Keep both", ButtonKind::Primary);
        keep.place(Rect::new(c.right() - w, y, w, t.button_h), t);
        Self { sheet, message, text, all, all_label, skip, keep }
    }
    pub(super) fn draw(&self, cx: &mut DrawCx) {
        self.sheet.draw(cx);
        cx.paragraph(&self.message, cx.theme.body, self.text, HAlign::Left);
        cx.line("Apply to all", cx.theme.body, self.all_label, HAlign::Left);
        self.all.draw(cx);
        self.skip.draw(cx);
        self.keep.draw(cx);
    }
    pub(super) fn on_gesture(&mut self, g: &Gesture) -> Option<Option<(ClashChoice, bool)>> {
        if self.sheet.on_gesture(g) {
            return Some(None);
        }
        self.all.on_gesture(g);
        if self.skip.on_gesture(g) {
            return Some(Some((ClashChoice::Skip, self.all.on)));
        }
        if self.keep.on_gesture(g) {
            return Some(Some((ClashChoice::KeepBoth, self.all.on)));
        }
        None
    }
}
impl Books {
    pub(super) fn persist_paste(&mut self) {
        if let Some(carry) = &mut self.carry
            && let Some(paste) = &mut carry.paste
        {
            let parents: std::collections::BTreeSet<_> = carry.items.iter().filter_map(|i| i.path().parent()).collect();
            paste
                .changes
                .extend(parents.into_iter().filter(|p| files::is_empty(p)).map(|p| PathChange::AddEmpty(p.to_owned())));
            if !paste.changes.is_empty() {
                self.library.change_paths(std::mem::take(&mut paste.changes));
            }
        }
    }
    pub(super) fn carrying(&self) -> bool {
        self.carry.is_some()
    }
    pub(super) fn start_move(&mut self, targets: Vec<Target>, cx: &mut EventCx) {
        if targets.is_empty() || targets.iter().any(|t| matches!(t, Target::Folder(p) if self.folder_protected(p))) {
            return;
        }
        let items: Vec<_> = targets
            .iter()
            .filter_map(|target| match target {
                Target::Book(id) => self.book(id).map(|b| MoveItem::Book(b.path.clone())),
                Target::Folder(p) => Some(MoveItem::Folder(p.clone())),
            })
            .collect();
        if items.len() != targets.len() {
            cx.toast("The selection has changed", None);
            return;
        }
        // Snapshot paths before switching away from All books or search.
        self.start_selection(targets[0].clone(), cx);
        let Some(selection) = &mut self.selection else { return };
        selection.items = targets.into_iter().collect();
        selection.carrying = true;
        self.carry = Some(Carry { items, paste: None });
        self.search = None;
        self.home_active = false;
        self.navigation.tab = Tab::Collections;
        self.pager.page = self.navigation.page();
        self.refresh(cx.theme);
        self.save_navigation(cx);
        cx.refresh(self.screen, RefreshMode::Full);
    }
    pub(super) fn paste_reason(&self) -> Option<&'static str> {
        let carry = self.carry.as_ref()?;
        let Some(dest) = &self.navigation.folder else {
            return Some("Choose a destination folder");
        };
        if carry.items.iter().any(|item| item.path().parent() == Some(dest.as_path())) {
            return Some("Choose a different folder");
        }
        if carry.items.iter().any(|item| matches!(item, MoveItem::Folder(p) if dest.starts_with(p))) {
            return Some("Cannot paste inside a moving folder");
        }
        None
    }
    pub(super) fn start_paste(&mut self, cx: &mut EventCx) {
        if !self.carrying() || self.paste_reason().is_some() {
            return;
        }
        // Prevent a scan between a rename and its index edit, including while
        // the user is deciding a clash. Publications stay pending until done.
        self.library.pause(true);
        self.carry.as_mut().unwrap().paste = Some(Paste {
            destination: self.navigation.folder.clone().unwrap(),
            next: 0,
            moved: 0,
            skipped: 0,
            choice: None,
            changes: Vec::new(),
        });
        self.continue_paste(None, cx);
    }
    pub(super) fn continue_paste(&mut self, answer: Option<(ClashChoice, bool)>, cx: &mut EventCx) {
        let Some(carry) = &mut self.carry else { return };
        let Some(paste) = &mut carry.paste else { return };
        if let Some((choice, true)) = answer {
            paste.choice = Some(choice);
        }
        let mut once = answer.map(|(choice, _)| choice);
        while let Some(item) = carry.items.get(paste.next) {
            match item.move_to(&paste.destination, once.take().or(paste.choice)) {
                Ok(MoveOutcome::Clash) => {
                    self.clash = Some(ClashSheet::new(cx.theme, self.screen, item, &paste.destination));
                    return;
                }
                Ok(MoveOutcome::Skipped) => paste.skipped += 1,
                Ok(MoveOutcome::Moved(to)) => {
                    paste.changes.push(PathChange::Move(item.path().to_owned(), to));
                    paste.moved += 1;
                }
                Err(e) => {
                    let error = format!("{}: {e:#}", item.path().display());
                    self.finish_paste(PasteEnd::Failed(error), cx);
                    return;
                }
            }
            paste.next += 1;
        }
        self.finish_paste(PasteEnd::Done, cx);
    }
    /// A paste is underway, possibly waiting on a clash answer.
    pub(super) fn pasting(&self) -> bool {
        self.carry.as_ref().is_some_and(|c| c.paste.is_some())
    }
    /// The only way a paste ends: it saves the finished moves and resumes the
    /// library worker paused by `start_paste`.
    pub(super) fn finish_paste(&mut self, end: PasteEnd, cx: &mut EventCx) {
        self.clash = None;
        let Some(carry) = self.carry.take() else { return };
        let Some(mut paste) = carry.paste else { return };
        self.selection = None;
        // Recheck source parents once the entire batch has finished.
        let parents: std::collections::BTreeSet<_> = carry.items.iter().filter_map(|i| i.path().parent()).collect();
        paste
            .changes
            .extend(parents.into_iter().filter(|p| files::is_empty(p)).map(|p| PathChange::AddEmpty(p.to_owned())));
        if !paste.changes.is_empty() {
            let changes = self.library.change_paths(paste.changes);
            self.apply_changes(changes, cx);
        }
        self.library.pause(false);
        cx.refresh(self.screen, RefreshMode::Full);
        let total = carry.items.len();
        if let PasteEnd::Failed(error) = end {
            // Also reconcile the very rare case where a sidecar error and its
            // rollback both failed. The error names the surviving book path.
            self.library.rescan();
            let report = format!("Moved {} of {total} ({} skipped):\n\n{error}", paste.moved, paste.skipped);
            cx.toast(format!("Moved {} of {total}", paste.moved), None);
            self.details = Some(Details::from_text(cx.theme, self.screen, "Move incomplete", report));
        } else if end == PasteEnd::Cancelled {
            let skipped = if paste.skipped > 0 { format!(" · {} skipped", paste.skipped) } else { String::new() };
            cx.toast(format!("Moved {} of {total}{skipped} · rest left in place", paste.moved), None);
        } else {
            let suffix = if paste.skipped > 0 { format!(" · {} skipped", paste.skipped) } else { String::new() };
            cx.toast(format!("Moved {}{suffix}", counted(paste.moved, "item")), None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{tap, with_cx};
    use super::*;
    use crate::library::selection_tests::book;

    fn fixture(label: &str) -> anyhow::Result<(Books, Store, PathBuf, Theme, Rect)> {
        let root = std::env::temp_dir().join(format!(
            "verso-move-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let library = root.join("Library");
        for folder in ["Source/Tree/Child", "Destination", "Other"] {
            std::fs::create_dir_all(library.join(folder))?;
        }
        let store = Store::at(root.join("state"));
        let mut settings = store.settings();
        settings.library_folders = Some(vec![library.clone()]);
        store.save_settings(&settings)?;
        let mut entries = Vec::new();
        for (i, name) in
            ["Source/A.epub", "Source/B.epub", "Source/C.epub", "Source/Tree/Nested.epub"].into_iter().enumerate()
        {
            let mut b = book(i);
            b.path = library.join(name);
            std::fs::copy(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/library3.epub"), &b.path)?;
            entries.push(b);
        }
        std::fs::create_dir(library.join("Source/A.sdr"))?;
        std::fs::write(library.join("Source/A.sdr/metadata.epub.lua"), b"original metadata")?;
        store.save_library(&LibraryIndex {
            books: entries,
            empty_folders: vec![library.join("Destination"), library.join("Other"), library.join("Source/Tree/Child")],
            ..Default::default()
        })?;
        store.save_book(
            "0",
            &BookState {
                position: crate::document::Position("exact-anchor".into()),
                progress: 3700,
                last_opened: 1,
                ..Default::default()
            },
        )?;
        store.rebuild_home_summary(&store.library())?;
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let mut books = Books::with_store(&t, screen, Rc::new(Worker::stub()), store.clone());
        books.home_active = false;
        books.navigation.tab = Tab::Collections;
        books.navigation.folder = Some(library.join("Source"));
        books.refresh(&t);
        Ok((books, store, root, t, screen))
    }
    #[test]
    fn carrying_freezes_selection_but_allows_navigation_and_new_folder() -> anyhow::Result<()> {
        let (mut b, _, root, t, screen) = fixture("carrying")?;
        let library = root.join("Library");
        with_cx(&t, screen, |cx| {
            b.start_selection(Target::Book("0".into()), cx);
            let p = b.selection.as_ref().unwrap().move_items.rect().center();
            tap(&mut b, p, cx);
            assert!(b.carrying());
            assert_eq!(b.paste_reason(), Some("Choose a different folder"));
            let frozen = b.selection.as_ref().unwrap().items.clone();
            let book_cell = b.grid.cells().into_iter().find(|(i, _)| b.grid_book(*i).is_some()).unwrap().1.center();
            tap(&mut b, book_cell, cx);
            assert!(b.reader.is_none());
            assert_eq!(b.selection.as_ref().unwrap().items, frozen);
            let tab = Point::new(b.tabs.rect().right() - t.target, b.tabs.rect().center().y);
            tap(&mut b, tab, cx);
            assert_eq!(b.navigation.tab, Tab::Collections);
            let back = b.back.rect().center();
            tap(&mut b, back, cx);
            assert_eq!(b.navigation.folder, Some(library.clone()));
            assert_eq!(b.selection.as_ref().unwrap().items, frozen);
            let create = b.new_folder.rect().center();
            tap(&mut b, create, cx);
            b.on_text_input(&TextInput::Insert("New destination".into()), cx);
            b.on_text_input(&TextInput::Submit, cx);
            assert!(library.join("New destination").is_dir());
            assert!(b.carrying());
            b.navigate(None, cx);
            assert_eq!(b.paste_reason(), Some("Choose a destination folder"));
            b.navigate(Some(library.join("New destination")), cx);
            assert!(b.paste_reason().is_none());
            let cancel = b.selection.as_ref().unwrap().cancel.rect().center();
            tap(&mut b, cancel, cx);
            assert!(!b.carrying() && b.selection.is_none());
            assert!(library.join("Source/A.epub").exists());
            b.navigate(Some(library.join("Source")), cx);
            b.start_move(vec![Target::Folder(library.join("Source/Tree"))], cx);
            b.navigate(Some(library.join("Source/Tree/Child")), cx);
            assert_eq!(b.paste_reason(), Some("Cannot paste inside a moving folder"));
            b.start_paste(cx);
            assert!(library.join("Source/Tree").is_dir());
            b.end_selection(cx);
            b.navigation.tab = Tab::AllBooks;
            b.refresh(&t);
            b.start_move(vec![Target::Book("0".into())], cx);
            assert!(b.carrying() && b.navigation.tab == Tab::Collections);
            b.end_selection(cx);
            b.toggle_search(cx);
            b.on_text_input(&TextInput::Submit, cx);
            b.start_move(vec![Target::Book("0".into())], cx);
            assert!(b.search.is_none() && b.carrying() && b.navigation.tab == Tab::Collections);
        });
        drop(b);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn clash_choices_apply_to_all_and_next_free_names() -> anyhow::Result<()> {
        for (label, first, all) in [
            ("skip-one", ClashChoice::Skip, false),
            ("skip-all", ClashChoice::Skip, true),
            ("keep-one", ClashChoice::KeepBoth, false),
            ("keep-all", ClashChoice::KeepBoth, true),
        ] {
            let (mut b, store, root, t, screen) = fixture(label)?;
            let library = root.join("Library");
            let dest = library.join("Destination");
            for name in ["A.epub", "A (2).epub", "B.epub", "C.sdr"] {
                std::fs::write(dest.join(name), b"existing")?;
            }
            with_cx(&t, screen, |cx| {
                let original = b.index.clone();
                b.start_move(vec![Target::Book("0".into()), Target::Book("1".into()), Target::Book("2".into())], cx);
                b.navigate(Some(dest.clone()), cx);
                b.start_paste(cx);
                assert!(b.clash.is_some());
                let sheet = b.clash.as_ref().unwrap();
                if all {
                    let p = sheet.all.rect().center();
                    tap(&mut b, p, cx);
                }
                let sheet = b.clash.as_ref().unwrap();
                let p =
                    if first == ClashChoice::Skip { sheet.skip.rect().center() } else { sheet.keep.rect().center() };
                tap(&mut b, p, cx);
                if all {
                    assert!(b.clash.is_none() && !b.carrying());
                } else {
                    assert!(b.clash.is_some(), "the second clash asks again");
                    let p = b.clash.as_ref().unwrap().keep.rect().center();
                    tap(&mut b, p, cx);
                    assert!(b.clash.is_some(), "a sidecar-only clash also asks");
                    let p = b.clash.as_ref().unwrap().skip.rect().center();
                    tap(&mut b, p, cx);
                }
                assert!(!b.carrying() && b.selection.is_none());
                assert_eq!(std::fs::read(dest.join("A.epub")).unwrap(), b"existing");
                let moved_a = first == ClashChoice::KeepBoth;
                assert_eq!(dest.join("A (3).epub").exists(), moved_a);
                assert_eq!(dest.join("A (3).sdr/metadata.epub.lua").exists(), moved_a);
                let moved_b = !all || first == ClashChoice::KeepBoth;
                assert_eq!(dest.join("B (2).epub").exists(), moved_b);
                assert_eq!(dest.join("C (2).epub").exists(), all && first == ClashChoice::KeepBoth);
                assert_eq!(store.book("0").progress, 3700);
                assert!(
                    b.index.books.iter().find(|e| e.id == "0").unwrap().path.is_file(),
                    "a rewritten book path must open as a file, without a trailing slash"
                );
                assert_eq!(store.book("0").position.0, "exact-anchor");
                assert_eq!(
                    b.index.books.iter().find(|e| e.id == "0").unwrap().path,
                    if moved_a { dest.join("A (3).epub") } else { library.join("Source/A.epub") }
                );
                // Replaying an older in-flight publication cannot undo paths.
                b.pending = Some(Snapshot { index: original, ..Default::default() });
                b.apply_pending(cx);
                assert_eq!(
                    b.index.books.iter().find(|e| e.id == "0").unwrap().path,
                    if moved_a { dest.join("A (3).epub") } else { library.join("Source/A.epub") }
                );
            });
            drop(b);
            let saved = store.library();
            let summary = store.home_summary().unwrap();
            assert_eq!(summary.resume().unwrap().book.path, saved.books.iter().find(|e| e.id == "0").unwrap().path);
            std::fs::remove_dir_all(root)?;
        }
        Ok(())
    }
    #[test]
    fn paste_stops_after_failure_and_keeps_finished_moves_and_progress() -> anyhow::Result<()> {
        let (mut b, store, root, t, screen) = fixture("partial")?;
        let dest = root.join("Library/Destination");
        with_cx(&t, screen, |cx| {
            b.start_move(vec![Target::Book("0".into()), Target::Book("1".into()), Target::Book("2".into())], cx);
            std::fs::remove_file(root.join("Library/Source/B.epub")).unwrap();
            b.navigate(Some(dest.clone()), cx);
            b.start_paste(cx);
            assert!(!b.carrying());
            assert!(dest.join("A.epub").is_file() && dest.join("A.sdr").is_dir());
            assert!(root.join("Library/Source/C.epub").is_file());
            assert!(b.details.as_ref().unwrap().content.contains("Moved 1 of 3"));
            assert_eq!(b.index.books.iter().find(|e| e.id == "0").unwrap().path, dest.join("A.epub"));
            assert_eq!(store.book("0").progress, 3700);
            assert!(
                b.index.books.iter().find(|e| e.id == "0").unwrap().path.is_file(),
                "a rewritten book path must open as a file, without a trailing slash"
            );
            assert!(cx.forced.iter().any(|update| update.mode == RefreshMode::Full));
        });
        drop(b);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn closing_a_clash_or_ending_the_selection_keeps_finished_moves() -> anyhow::Result<()> {
        for end_selection in [false, true] {
            let (mut b, store, root, t, screen) = fixture(if end_selection { "end-mid-paste" } else { "close-clash" })?;
            let dest = root.join("Library/Destination");
            std::fs::create_dir(dest.join("Tree"))?;
            with_cx(&t, screen, |cx| {
                let tree = root.join("Library/Source/Tree");
                b.start_move(
                    vec![Target::Book("0".into()), Target::Folder(tree.clone()), Target::Book("2".into())],
                    cx,
                );
                b.navigate(Some(dest.clone()), cx);
                b.start_paste(cx);
                let clash = b.clash.as_ref().expect("the folder clashes");
                assert_eq!(clash.message, "A folder named “Tree” already exists here.");
                if end_selection {
                    b.end_selection(cx);
                } else {
                    tap(&mut b, Point::new(screen.x + 4, screen.y + 4), cx);
                }
                assert!(b.clash.is_none() && !b.carrying() && b.selection.is_none());
                assert!(b.details.is_none(), "cancelling is not reported as a failure");
                assert_eq!(cx.toast.as_ref().unwrap().text, "Moved 1 of 3 · rest left in place");
                assert!(tree.is_dir() && root.join("Library/Source/C.epub").is_file());
                assert_eq!(b.index.books.iter().find(|e| e.id == "0").unwrap().path, dest.join("A.epub"));
            });
            drop(b);
            assert_eq!(store.library().books.iter().find(|e| e.id == "0").unwrap().path, dest.join("A.epub"));
            std::fs::remove_dir_all(root)?;
        }
        Ok(())
    }
    #[test]
    fn a_plain_file_clash_and_earlier_skips_are_reported() -> anyhow::Result<()> {
        let (mut b, _store, root, t, screen) = fixture("file-clash")?;
        let dest = root.join("Library/Destination");
        std::fs::write(dest.join("B.epub"), b"existing")?;
        std::fs::write(dest.join("Tree"), b"a file, not a folder")?;
        with_cx(&t, screen, |cx| {
            let tree = root.join("Library/Source/Tree");
            b.start_move(
                vec![
                    Target::Book("0".into()),
                    Target::Book("1".into()),
                    Target::Folder(tree),
                    Target::Book("2".into()),
                ],
                cx,
            );
            b.navigate(Some(dest.clone()), cx);
            b.start_paste(cx);
            assert!(b.clash.is_some(), "B.epub clashes");
            b.continue_paste(Some((ClashChoice::Skip, false)), cx);
            assert_eq!(b.clash.as_ref().expect("Tree clashes").message, "“Tree” already exists here.");
            tap(&mut b, Point::new(screen.x + 4, screen.y + 4), cx);
            assert_eq!(cx.toast.as_ref().unwrap().text, "Moved 1 of 4 · 1 skipped · rest left in place");
        });
        drop(b);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn completed_moves_are_saved_if_quitting_during_a_later_clash() -> anyhow::Result<()> {
        let (mut b, store, root, t, screen) = fixture("quit-clash")?;
        let dest = root.join("Library/Destination");
        std::fs::write(dest.join("B.epub"), b"existing")?;
        with_cx(&t, screen, |cx| {
            b.start_move(vec![Target::Book("0".into()), Target::Book("1".into())], cx);
            b.navigate(Some(dest.clone()), cx);
            b.start_paste(cx);
            assert!(b.clash.is_some() && dest.join("A.epub").exists());
            b.on_quit().unwrap();
        });
        drop(b);
        assert_eq!(store.library().books.iter().find(|e| e.id == "0").unwrap().path, dest.join("A.epub"));
        assert_eq!(store.home_summary().unwrap().resume().unwrap().book.path, dest.join("A.epub"));
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
