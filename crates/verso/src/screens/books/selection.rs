//! Selection belongs to one browsing list; only the page and sort may change.
use super::*;
use crate::ui::theme::PAPER;

pub(super) struct Selection {
    pub(super) items: HashSet<Target>,
    count: Rect,
    pub(super) all: Button,
    pub(super) delete: Button,
    pub(super) cancel: Button,
}
impl Selection {
    fn new(t: &Theme, screen: Rect, target: Target) -> Self {
        let gap = t.mm(2.0);
        // Keep the same arrangement on smaller panels.
        let width = screen.w - 2 * t.margin;
        let button_w = ((width * 3 / 5 - 2 * gap) / 3).max(t.mm(18.0));
        let y = (t.top_bar - t.button_h) / 2;
        let mut cancel = Button::new("Cancel", ButtonKind::Secondary);
        cancel.place(Rect::new(screen.right() - t.margin - button_w, y, button_w, t.button_h), t);
        let mut delete = Button::new("Delete…", ButtonKind::Secondary);
        delete.place(Rect::new(cancel.rect().x - gap - button_w, y, button_w, t.button_h), t);
        let mut all = Button::new("Select all", ButtonKind::Secondary);
        all.place(Rect::new(delete.rect().x - gap - button_w, y, button_w, t.button_h), t);
        Self {
            items: HashSet::from([target]),
            count: Rect::new(t.margin, 0, all.rect().x - t.margin - gap, t.top_bar),
            all,
            delete,
            cancel,
        }
    }
    pub(super) fn draw(&mut self, cx: &mut DrawCx) {
        self.delete.enabled = !self.items.is_empty();
        let label = format!("{} selected", self.items.len());
        let style = if cx.text.width(&label, cx.theme.button) > self.count.w as f32 {
            cx.theme.caption
        } else {
            cx.theme.button
        };
        cx.line(&label, style, self.count, HAlign::Left);
        self.all.draw(cx);
        self.delete.draw(cx);
        self.cancel.draw(cx);
        cx.hint(self.count, RefreshMode::Fast);
    }
}
impl Books {
    fn selectable(&self) -> Vec<Target> {
        if self.is_home() {
            return Vec::new();
        }
        self.folders
            .iter()
            .filter(|p| !self.roots.contains(p))
            .cloned()
            .map(Target::Folder)
            .chain(self.books.iter().map(|b| Target::Book(b.id.clone())))
            .collect()
    }
    pub(super) fn retain_selection(&mut self) {
        if self.selection.is_none() {
            return;
        }
        let available: HashSet<_> = self.selectable().into_iter().collect();
        if let Some(selection) = &mut self.selection {
            selection.items.retain(|item| available.contains(item));
        }
    }
    pub(super) fn start_selection(&mut self, target: Target, cx: &mut EventCx) {
        if !self.selectable().contains(&target) {
            return;
        }
        if let Some(search) = &mut self.search {
            search.field.focused = false;
            search.keyboard.visible = false;
        }
        self.selection = Some(Selection::new(cx.theme, self.screen, target));
        cx.refresh(self.screen, RefreshMode::Ui);
    }
    pub(super) fn end_selection(&mut self, cx: &mut EventCx) {
        if self.selection.take().is_some() {
            cx.refresh(self.screen, RefreshMode::Ui);
        }
    }
    pub(super) fn toggle_selection(&mut self, index: usize) {
        let target = self
            .folders
            .get(index)
            .cloned()
            .map(Target::Folder)
            .or_else(|| self.grid_book(index).map(|b| Target::Book(b.id.clone())));
        if let Some(target) = target.filter(|item| !matches!(item, Target::Folder(p) if self.roots.contains(p)))
            && let Some(selection) = &mut self.selection
            && !selection.items.remove(&target)
        {
            selection.items.insert(target);
        }
    }
    pub(super) fn draw_tick(&self, cx: &mut DrawCx, target: Target, cell: Rect) {
        let Some(selection) = &self.selection else { return };
        if matches!(&target, Target::Folder(p) if self.roots.contains(p)) {
            return;
        }
        let t = cx.theme;
        let size = t.mm(6.0);
        let r = Rect::new(cell.right() - size, cell.y, size, size);
        cx.fill(r.inset(-t.mm(0.7)), PAPER);
        cx.canvas.stroke_rounded_rect(r, t.mm(0.7) as f32, t.border as f32, INK);
        if selection.items.contains(&target) {
            cx.icon(Icon::Check, r, size - t.mm(1.0), INK);
        }
        cx.hint(cell, RefreshMode::Fast);
    }
    pub(super) fn selection_controls(&mut self, g: &Gesture, cx: &mut EventCx) -> bool {
        let Some(selection) = &mut self.selection else { return false };
        if selection.cancel.on_gesture(g) {
            self.end_selection(cx);
            return true;
        }
        if selection.all.on_gesture(g) {
            self.selection.as_mut().unwrap().items = self.selectable().into_iter().collect();
            return true;
        }
        // Enabled state also follows input before the next draw (scripted input).
        selection.delete.enabled = !selection.items.is_empty();
        if selection.delete.on_gesture(g) {
            self.ask_delete_selection(cx);
            return true;
        }
        // Hidden ordinary header buttons must never receive this touch.
        g.pos().y < cx.theme.top_bar
    }
    fn selected_targets(&self) -> Vec<Target> {
        // Use list order, not hash order, for reproducible deletion and reports.
        self.selectable()
            .into_iter()
            .filter(|item| self.selection.as_ref().is_some_and(|s| s.items.contains(item)))
            .collect()
    }
    fn delete_items(&self, targets: &[Target]) -> Vec<files::DeleteItem> {
        targets
            .iter()
            .filter_map(|target| match target {
                Target::Book(id) => self.book(id).map(files::DeleteItem::book),
                Target::Folder(path) => Some(files::DeleteItem::Folder(path.clone())),
            })
            .collect()
    }
    fn ask_delete_selection(&mut self, cx: &mut EventCx) {
        let targets = self.selected_targets();
        if targets.is_empty() {
            return;
        }
        if targets.iter().any(|t| matches!(t, Target::Folder(p) if self.folder_protected(p))) {
            cx.toast("A selected folder contains a library folder and cannot be deleted", None);
            return;
        }
        match files::selection_contents(&self.delete_items(&targets)) {
            Ok(contents) => {
                let mut parts = Vec::new();
                if contents.books > 0 {
                    parts.push(counted(contents.books, "book"));
                }
                if contents.folders > 0 {
                    parts.push(counted(contents.folders, "folder"));
                }
                if contents.other_files > 0 {
                    parts.push(counted(contents.other_files, "other file"));
                }
                let progress = if contents.books > 0 { ", including the books’ reading progress" } else { "" };
                let message = format!(
                    "{} will be removed from this device{progress}. Folders and everything in them will be deleted.",
                    parts.join(", ")
                );
                self.confirm = Some(ConfirmDelete::new(
                    cx.theme,
                    self.screen,
                    DeleteRequest::Selection(targets),
                    "Delete selection?",
                    message,
                ));
            }
            Err(e) => cx.toast(format!("{e:#}"), None),
        }
    }
    pub(super) fn delete_selection(&mut self, targets: Vec<Target>, cx: &mut EventCx) {
        if targets.iter().any(|t| matches!(t, Target::Folder(p) if self.folder_protected(p))) {
            return;
        }
        let items = self.delete_items(&targets);
        let result = files::delete_selection(&items, &self.index, &self.store);
        self.selection = None;
        for id in &result.removed_books {
            self.states.remove(id);
            self.home.books.retain(|e| &e.book.id != id);
        }
        if !result.changes.is_empty() {
            let changes = self.library.change_paths(result.changes);
            self.apply_changes(changes, cx);
        } else {
            cx.refresh(self.screen, RefreshMode::Full);
        }
        if result.errors.is_empty() {
            cx.toast(format!("Deleted {}", counted(result.deleted, "item")), None);
        } else {
            // Rescan also settles empty subfolders left by partial remove_dir_all.
            self.library.rescan();
            let report = format!("Deleted {} of {}:\n\n{}", result.deleted, items.len(), result.errors.join("\n\n"));
            self.details = Some(Details::from_text(cx.theme, self.screen, "Deletion incomplete", report));
        }
    }
}
