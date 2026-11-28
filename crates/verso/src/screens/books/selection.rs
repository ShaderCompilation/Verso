//! Selection belongs to one browsing list; only the page may change.
use super::*;
use crate::ui::theme::PAPER;

/// Circular selection badge (`52 × 52` at 300 dpi, `12 px` inset) anchored to
/// the top-left of `front` (book cover, folder front cover, or Icon tile).
pub(super) fn badge_rect(front: Rect, t: &Theme) -> Rect {
    let size = t.mm(4.4);
    let inset = t.mm(1.0);
    Rect::new(front.x + inset, front.y + inset, size, size)
}

pub(super) struct Selection {
    pub(super) items: HashSet<Target>,
    count: Rect,
    pub(super) all: Button,
    pub(super) delete: Button,
    pub(super) move_items: Button,
    pub(super) paste: Button,
    pub(super) carrying: bool,
    pub(super) cancel: Button,
}
impl Selection {
    #[cfg(test)]
    pub(super) fn new_for_test(t: &Theme, screen: Rect) -> Self {
        Self::new(t, screen, Target::Book("0".into()))
    }
    fn new(t: &Theme, screen: Rect, target: Target) -> Self {
        let narrow = screen.w < t.mm(100.0);
        let gap = t.mm(if narrow { 1.4 } else { 1.7 });
        let width = screen.w - 2 * t.margin;
        // Proportional button widths matching the Paperwhite mockup while keeping
        // every label untruncated on 6" panels alongside the selection count.
        let (all_w, move_w, del_w, cancel_w) = (t.mm(18.0), t.mm(13.0), t.mm(17.5), t.mm(16.0));
        let buttons_total = all_w + move_w + del_w + cancel_w + 3 * gap;
        let max_buttons = (width - t.mm(10.5)).max(1);
        let scale = |w: i32| if buttons_total > max_buttons { w * max_buttons / buttons_total } else { w };
        let (all_w, move_w, del_w, cancel_w) = (scale(all_w), scale(move_w), scale(del_w), scale(cancel_w));
        let button_h = t.mm(8.5).min(t.button_h);
        let y = (t.top_bar - button_h) / 2;
        let mut cancel = Button::new("Cancel", ButtonKind::Secondary);
        cancel.place(Rect::new(screen.right() - t.margin - cancel_w, y, cancel_w, button_h), t);
        let mut delete = Button::new("Delete…", ButtonKind::Secondary);
        delete.place(Rect::new(cancel.rect().x - gap - del_w, y, del_w, button_h), t);
        let mut move_items = Button::new("Move", ButtonKind::Secondary);
        move_items.place(Rect::new(delete.rect().x - gap - move_w, y, move_w, button_h), t);
        let mut paste = Button::new("Paste", ButtonKind::Primary);
        paste.place(Rect::new(cancel.rect().x - gap - cancel_w, y, cancel_w, button_h), t);
        let mut all = Button::new("Select all", ButtonKind::Secondary);
        all.place(Rect::new(move_items.rect().x - gap - all_w, y, all_w, button_h), t);
        Self {
            items: HashSet::from([target]),
            count: Rect::new(t.margin, 0, all.rect().x - t.margin - gap, t.top_bar),
            all,
            delete,
            move_items,
            paste,
            carrying: false,
            cancel,
        }
    }
    pub(super) fn draw(&mut self, cx: &mut DrawCx) {
        self.delete.enabled = !self.items.is_empty();
        self.move_items.enabled = !self.items.is_empty();
        let label = if self.carrying {
            format!("Moving {}", counted(self.items.len(), "item"))
        } else {
            format!("{} selected", self.items.len())
        };
        let count = if self.carrying {
            Rect { w: self.paste.rect().x - self.count.x - cx.theme.mm(2.0), ..self.count }
        } else {
            self.count
        };
        let style = if cx.text.width(&label, cx.theme.heading) <= count.w as f32 {
            cx.theme.heading
        } else if cx.text.width(&label, cx.theme.button) <= count.w as f32 {
            cx.theme.button
        } else {
            cx.theme.caption
        };
        if !self.carrying && cx.text.width(&label, style) > count.w as f32 {
            cx.paragraph(
                &format!("{}\nselected", self.items.len()),
                style,
                count.inset_xy(0, cx.theme.mm(2.0)),
                HAlign::Left,
            );
        } else {
            cx.line(&label, style, count, HAlign::Left);
        }
        if self.carrying {
            self.paste.draw(cx);
        } else {
            self.all.draw(cx);
            self.move_items.draw(cx);
            self.delete.draw(cx);
        }
        self.cancel.draw(cx);
        cx.hint(count, RefreshMode::Fast);
    }
}
impl Books {
    fn selectable(&self) -> Vec<Target> {
        if self.is_home() {
            return Vec::new();
        }
        // Folders holding a library folder can be neither deleted nor moved.
        self.folders
            .iter()
            .filter(|p| !self.folder_protected(p))
            .cloned()
            .map(Target::Folder)
            .chain(self.books.iter().map(|b| Target::Book(b.id.clone())))
            .collect()
    }
    pub(super) fn retain_selection(&mut self) {
        if self.selection.as_ref().is_none_or(|s| s.carrying) {
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
        self.update_header_rect(cx.theme);
        cx.refresh(self.screen, RefreshMode::Ui);
    }
    pub(super) fn end_selection(&mut self, cx: &mut EventCx) {
        // Never drop a paste midway: its finished moves must reach the index
        // and the paused library worker must resume.
        if self.pasting() {
            self.finish_paste(PasteEnd::Cancelled, cx);
            return;
        }
        self.carry = None;
        if self.selection.take().is_some() {
            self.update_header_rect(cx.theme);
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
        if let Some(target) = target.filter(|item| !matches!(item, Target::Folder(p) if self.folder_protected(p)))
            && let Some(selection) = &mut self.selection
            && !selection.items.remove(&target)
        {
            selection.items.insert(target);
        }
    }
    pub(super) fn draw_tick(&self, cx: &mut DrawCx, target: Target, cell: Rect, front: Rect) {
        let Some(selection) = &self.selection else { return };
        if matches!(&target, Target::Folder(p) if self.folder_protected(p)) {
            return;
        }
        if selection.carrying {
            if selection.items.contains(&target) {
                cx.canvas.fade_rect(cell);
            }
            return;
        }
        let t = cx.theme;
        let badge = badge_rect(front, t);
        let r = badge.w as f32 / 2.0;
        let (cx_f, cy_f) = (badge.x as f32 + r, badge.y as f32 + r);
        if selection.items.contains(&target) {
            cx.canvas.fill_circle(cx_f, cy_f, r, INK);
            cx.icon(Icon::Check, badge, t.mm(2.5), PAPER);
        } else {
            cx.canvas.fill_circle(cx_f, cy_f, r, PAPER);
            cx.canvas.stroke_circle(cx_f, cy_f, r, t.border as f32, INK);
        }
        // Only the badge: a cover that loads meanwhile keeps its grayscale refresh.
        cx.hint(badge, RefreshMode::Fast);
    }
    pub(super) fn selection_controls(&mut self, g: &Gesture, cx: &mut EventCx) -> bool {
        let paste_enabled = self.paste_reason().is_none();
        let Some(selection) = &mut self.selection else { return false };
        if selection.cancel.on_gesture(g) {
            self.end_selection(cx);
            return true;
        }
        if selection.carrying {
            selection.paste.enabled = paste_enabled;
            if selection.paste.on_gesture(g) {
                self.start_paste(cx);
            }
            return g.pos().y < cx.theme.top_bar;
        }
        selection.move_items.enabled = !selection.items.is_empty();
        if selection.move_items.on_gesture(g) {
            let targets = self.selected_targets();
            self.start_move(targets, cx);
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
        let has_folders = targets.iter().any(|t| matches!(t, Target::Folder(_)));
        match files::selection_contents(&self.delete_items(&targets)) {
            Ok(contents) => {
                let message = selection_delete_message(&contents, has_folders);
                self.confirm = Some(ConfirmDelete::new(
                    cx.theme,
                    self.screen,
                    ConfirmRequest::Selection(targets),
                    "Delete selection?",
                    message,
                ));
            }
            Err(e) => cx.toast(format!("{e:#}"), None),
        }
    }
    pub(super) fn delete_selection(&mut self, targets: Vec<Target>, cx: &mut EventCx) {
        // Selection already excludes these; roots can't change during a confirmation.
        if targets.iter().any(|t| matches!(t, Target::Folder(p) if self.folder_protected(p))) {
            return;
        }
        let started = std::time::Instant::now();
        let items = self.delete_items(&targets);
        let result = files::delete_selection(&items, &self.index, &self.store);
        log::info!(
            "selection delete: {} items, {} failed, {} sidecar errors in {:?}",
            items.len(),
            result.errors.len(),
            result.sidecar_errors.len(),
            started.elapsed()
        );
        self.selection = None;
        for id in &result.removed_books {
            self.remove_book_state(id);
            self.home.books.retain(|e| &e.book.id != id);
        }
        if !result.changes.is_empty() {
            let changes = self.library.change_paths(result.changes);
            self.apply_changes(changes, cx);
        } else {
            self.update_header_rect(cx.theme);
            cx.refresh(self.screen, RefreshMode::Clean);
        }
        let sidecars = result.sidecar_errors.len();
        if result.errors.is_empty() {
            cx.toast(
                match sidecars {
                    0 => format!("Deleted {}", counted(result.deleted, "item")),
                    n => format!(
                        "Deleted {}, but couldn't remove KOReader data for {}",
                        counted(result.deleted, "item"),
                        counted(n, "book")
                    ),
                },
                None,
            );
        } else {
            // Rescan also settles empty subfolders left by partial remove_dir_all.
            self.library.rescan();
            let mut report =
                format!("Deleted {} of {}:\n\n{}", result.deleted, items.len(), result.errors.join("\n\n"));
            if sidecars > 0 {
                report.push_str("\n\nDeleted, but their KOReader data stayed behind:\n\n");
                report.push_str(&result.sidecar_errors.join("\n\n"));
            }
            self.details = Some(Details::from_text(cx.theme, self.screen, "Deletion incomplete", report));
        }
    }
}

/// Names what a selection delete removes, counting what is inside its folders.
pub(super) fn selection_delete_message(contents: &files::Contents, has_folders: bool) -> String {
    let mut parts = Vec::new();
    if contents.books > 0 {
        let progress = if contents.books == 1 { "its" } else { "their" };
        parts.push(format!("{} and {progress} reading progress", counted(contents.books, "book")));
    }
    if contents.folders > 0 {
        parts.push(counted(contents.folders, "folder"));
    }
    if contents.other_files > 0 {
        parts.push(counted(contents.other_files, "other file"));
    }
    if has_folders {
        format!("The selection and everything in its folders will be removed from this device: {}.", parts.join(", "))
    } else {
        format!("{} will be removed from this device.", parts.join(", "))
    }
}
