//! Settings (reading, library and dictionary folders, device) and the small system prompts
//! shared by the library and USB notification.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::clock::LocalDateTime;
use crate::document::TextShaping;
use crate::geom::Rect;
use crate::gfx::{HAlign, Icon};
use crate::platform::{RefreshMode, Waker, ssh, wifi};
use crate::reader::Zone;
use crate::screens::dictionaries::Dictionaries;
use crate::screens::folders::{FolderPicker, display_folder};
use crate::screens::koreader_import::{ImportSheet, Outcome};
use crate::screens::setup::SetupScreen;
use crate::screens::transfer::TransferScreen;
use crate::screens::update::{GoBackSheet, OfferEvent, OfferSheet, UpdateScreen};
use crate::screens::{Action, EventCx, RelayoutState, Screen};
use crate::store::{FolderPreview, Orientation, Settings, SleepScreen, Store, TapZones};
use crate::ui::gesture::Gesture;
use crate::ui::theme::{FILL, INK, INK2, LINE, PAPER, Theme};
use crate::ui::widget::{DrawCx, Press, PressEvent};
use crate::ui::widgets::{
    Button, ButtonKind, IconButton, List, ListEvent, Pager, PagerEvent, Row, Segmented, Sheet, Toggle, TopBar,
};
use crate::update::{self, Updates};
use verso_update::Version;
use verso_update::check::CHECK_EVERY;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Reading,
    Display,
    Library,
    Device,
    Advanced,
}
const TABS: [Tab; 5] = [Tab::Reading, Tab::Display, Tab::Library, Tab::Device, Tab::Advanced];

/// While SSH is on but Wi-Fi hasn't connected yet (it takes up to about a
/// minute after the Kindle wakes), the SSH row re-reads the status this
/// often…
const SSH_RECHECK: Duration = Duration::from_secs(2);
/// …for at most this long after Settings opens, a toggle or a wake.
const SSH_RECHECK_FOR: Duration = Duration::from_secs(90);

/// Full-flash intervals offered in Settings (0 = never).
const FLASH_CHOICES: [u32; 4] = [1, 6, 12, 0];

/// The offered choice closest to `every`, for hand-edited settings.
fn flash_choice(every: u32) -> usize {
    if every == 0 {
        return FLASH_CHOICES.len() - 1;
    }
    (0..FLASH_CHOICES.len() - 1).min_by_key(|&i| FLASH_CHOICES[i].abs_diff(every)).unwrap_or(1)
}

// The vertical rhythm, in millimetres: a section is a label, a control and
// a gray note, and sections are a gap apart.
/// Tabs, choices and buttons.
const CONTROL_MM: f32 = 8.0;
/// A section label's strip above its control.
const LABEL_MM: f32 = 5.5;
/// From a control to its note.
const NOTE_GAP_MM: f32 = 1.5;
/// Between sections.
const SECTION_GAP_MM: f32 = 5.0;

/// A column laid out top to bottom as it's drawn. Notes are measured, so
/// sections never overlap however their text wraps on a narrower screen.
struct Column {
    x: i32,
    w: i32,
    y: i32,
}

impl Column {
    fn new(area: Rect, y: i32) -> Self {
        Self { x: area.x, w: area.w, y }
    }

    /// The next `h` pixels of the column.
    fn take(&mut self, h: i32) -> Rect {
        let rect = Rect::new(self.x, self.y, self.w, h);
        self.y += h;
        rect
    }

    fn label(&mut self, cx: &mut DrawCx, text: &str) {
        let rect = self.take(cx.theme.mm(LABEL_MM));
        cx.section_label(text, rect);
    }

    fn control(&mut self, t: &Theme) -> Rect {
        self.take(t.mm(CONTROL_MM))
    }

    /// A gray note under a control, with room for the tallest of
    /// `variants` (the notes of the control's other choices), so that
    /// choosing doesn't move what follows.
    fn note(&mut self, cx: &mut DrawCx, text: &str, variants: &[&str]) {
        self.y += cx.theme.mm(NOTE_GAP_MM);
        let h = variants.iter().chain([&text]).map(|v| note_h(cx, v, self.w)).max().unwrap_or(0);
        let (rect, style) = (self.take(h), cx.theme.small.gray(INK2));
        cx.paragraph(text, style, rect, HAlign::Left);
    }

    fn gap(&mut self, t: &Theme) {
        self.y += t.mm(SECTION_GAP_MM);
    }
}

/// The height of a gray note `w` wide.
fn note_h(cx: &mut DrawCx, text: &str, w: i32) -> i32 {
    let style = cx.theme.small.gray(INK2);
    cx.text.measure(text, style, Some(w as f32)).1.ceil() as i32
}

/// A button wide enough for the longest of `labels`, at most half of `w`.
fn button_w(cx: &mut DrawCx, labels: &[&str], w: i32) -> i32 {
    let t = cx.theme;
    let text = labels.iter().map(|l| cx.text.width(l, t.button)).fold(0.0_f32, f32::max);
    (text.ceil() as i32 + t.mm(8.0)).min(w / 2)
}

/// A gray note with `button` (`button_w` wide) on its right, `button_dy`
/// lower, or below it across the column where the note would be too narrow
/// beside it.
fn note_beside(cx: &mut DrawCx, col: &mut Column, note: &str, button: &mut Button, button_w: i32, button_dy: i32) {
    let t = cx.theme;
    let style = t.small.gray(INK2);
    let note_w = col.w - button_w - t.mm(3.0);
    if note_w >= t.mm(40.0) {
        let h = note_h(cx, note, note_w);
        cx.paragraph(note, style, Rect::new(col.x, col.y, note_w, h), HAlign::Left);
        button.place(Rect::new(col.x + col.w - button_w, col.y + button_dy, button_w, t.mm(CONTROL_MM)), t);
        col.y += h.max(button_dy + t.mm(CONTROL_MM));
    } else {
        let h = note_h(cx, note, col.w);
        cx.paragraph(note, style, col.take(h), HAlign::Left);
        col.y += t.mm(2.0);
        button.place(col.control(t), t);
    }
    button.draw(cx);
}

pub struct SystemSettings {
    top: TopBar,
    tabs: Segmented,
    tab: Tab,
    store: Store,
    /// For the dictionary-folder screen's catalog worker.
    waker: Waker,
    screen: Rect,
    content: Rect,
    /// The top of the area below the tabs.
    body: i32,
    /// `Store::settings_saves` when the folder list was last read.
    settings_seen: u64,
    /// Two columns side by side in landscape, which is too short for one;
    /// in portrait both are the content area. Every tab but Library places
    /// its controls as it draws, below the notes it measured.
    left: Rect,
    right: Rect,
    // Reading: tap zones, then the switches below the preview, or (landscape)
    // the zones note and the switches on the right.
    zones: Segmented,
    zones_preview: Rect,
    /// "Tap the top-right corner to bookmark".
    corner: Toggle,
    /// "Check KOReader progress".
    koreader: Toggle,
    // Display: one column, or (landscape) orientation and folder previews
    // side by side and the refresh choices across both below them.
    orientation: Segmented,
    flash: Segmented,
    previews: Segmented,
    // Library: folders on the left; the dictionaries and KOReader entries
    // below them side by side, or (landscape) one above the other on the right.
    library: Rect,
    dictionaries: Rect,
    dictionary_entry: List,
    koreader_area: Rect,
    /// "Import" progress and highlights from KOReader.
    koreader_entry: List,
    import_sheet: Option<ImportSheet>,
    /// The book open in the reader under Settings, which the import leaves out.
    open_book: Option<String>,
    folders: Vec<PathBuf>,
    removes: Vec<IconButton>,
    folder_row: i32,
    folder_pager: Pager,
    add_folder: Button,
    // Device: sleep screen and Wi-Fi, then (or on the right) updates.
    sleep: Segmented,
    /// The version in the Updates section. Tap it five times to show or
    /// hide the developer options (on the Advanced tab).
    version: Rect,
    version_press: Press,
    version_taps: (u8, Instant),
    developer: bool,
    wifi: Option<bool>,
    wifi_toggle: Toggle,
    wifi_pending: Option<bool>,
    /// Opens Wi-Fi transfer, beside the Wi-Fi note.
    transfer: Button,
    ssh: ssh::Status,
    ssh_toggle: Toggle,
    /// Start (true) or stop the SSH server once "Starting…" is on screen.
    ssh_pending: Option<bool>,
    /// The server was running when Settings opened. The developer section
    /// then stays for this visit, so turning the server off doesn't make the
    /// switch vanish.
    ssh_was_running: bool,
    /// When the SSH status was last read, and until when to keep re-reading
    /// it while it's incomplete (see SSH_RECHECK).
    ssh_read_at: Instant,
    ssh_recheck_until: Instant,
    /// The device slept: re-read the SSH status once it's awake.
    ssh_read_on_wake: bool,
    // Updates
    updates: Arc<Updates>,
    update_status: update::Status,
    /// Framed callout card at the top of Device while an update is available.
    update_card: Rect,
    update_card_press: Press,
    /// "Check for updates", or "Update" inside the callout card while an update is available.
    check_button: Button,
    auto_check: Toggle,
    /// The installed version and the previous one, while "Go back" can
    /// restore it.
    go_back: Option<(Version, Version)>,
    go_back_button: Button,
    offer: Option<OfferSheet>,
    go_back_sheet: Option<GoBackSheet>,
    // Advanced: text shaping, then (or on the right) help and the
    // developer section.
    setup_button: Button,
    report_button: Button,
    report_rx: Option<std::sync::mpsc::Receiver<anyhow::Result<String>>>,
    report_sheet: Option<ReportSheet>,
    shaping: Segmented,
}
impl SystemSettings {
    pub fn new(t: &Theme, screen: Rect, store: Store, waker: Waker) -> Self {
        let mut top = TopBar::new("Settings", true);
        top.show_battery = true;
        top.place(screen, t);
        let content = screen.split_top(t.top_bar).1.inset(t.margin);
        let settings = store.settings();
        let mut tabs = Segmented::texts(&["Reading", "Display", "Library", "Device", "Advanced"], 0);
        tabs.place(Rect::new(content.x, content.y, content.w, t.mm(CONTROL_MM)));
        let body = tabs.rect().bottom() + t.mm(4.0);
        let landscape = screen.w > screen.h;
        let (left, right) = if landscape {
            let gap = t.mm(10.0);
            let w = (content.w - gap) / 2;
            (Rect { w, ..content }, Rect { x: content.right() - w, w, ..content })
        } else {
            (content, content)
        };

        let selected = TapZones::ALL.iter().position(|&z| z == settings.tap_zones).unwrap_or(0);
        let zones = Segmented::texts(&["Standard", "Left-handed", "Halves"], selected);
        let orientation =
            Segmented::texts(&["Portrait", "Landscape"], usize::from(settings.orientation == Orientation::Landscape));
        let flash =
            Segmented::texts(&["Every page", "Every 6", "Every 12", "Never"], flash_choice(settings.page_flash));
        let selected = FolderPreview::OFFERED.iter().position(|&m| m == settings.folder_preview).unwrap_or(0);
        // Covers is disabled for now; see FolderPreview::OFFERED.
        let previews = Segmented::texts(&["Stack", "Latest", "Icon"], selected);

        let mut add_folder = Button::new("Add folder", ButtonKind::Secondary).with_icon(Icon::Folder);
        add_folder.place(Rect::new(content.x, 0, content.w, t.mm(CONTROL_MM)), t);
        let mut folder_pager = Pager::new(1);
        folder_pager.place(Rect::new(content.x, content.bottom() - t.mm(11.0), content.w, t.mm(11.0)), t);
        // The dictionary and KOReader entries: a section label and one row
        // each. Landscape is too short for them below the library folders,
        // and a 6" portrait screen has room for only one row of entries
        // there, so they stand side by side.
        let label_h = t.mm(LABEL_MM);
        let entry_h = label_h + t.row2;
        let (library, dictionaries, koreader_area) = if landscape {
            // The folder pager stays under the folders.
            folder_pager.place(Rect { w: left.w, ..folder_pager.rect() }, t);
            let dictionaries = Rect { y: body, h: entry_h, ..right };
            (left, dictionaries, Rect { y: dictionaries.bottom() + t.mm(SECTION_GAP_MM), ..dictionaries })
        } else {
            let y = folder_pager.rect().y - t.mm(2.0) - entry_h;
            let half = screen.x + screen.w / 2;
            let left = Rect::new(content.x, y, half - t.margin - content.x, entry_h);
            (content, left, Rect { x: half + t.margin, w: content.right() - half - t.margin, ..left })
        };
        let mut dictionary_entry = List::new(vec![Row::new("Dictionaries").sub("Sets, order and folders").chevron()]);
        let mut koreader_entry = List::new(vec![Row::new("Import").sub("Progress and highlights")]);
        if landscape {
            dictionary_entry.place(Rect { y: dictionaries.y + label_h, h: t.row2, ..dictionaries }, t.row2, 0);
            koreader_entry.place(Rect { y: koreader_area.y + label_h, h: t.row2, ..koreader_area }, t.row2, 0);
        } else {
            // Rows reach the screen edges, their text inset by the margin.
            let half = screen.x + screen.w / 2;
            let row = |x: i32, right: i32| Rect::new(x, dictionaries.y + label_h, right - x, t.row2);
            dictionary_entry.place(row(screen.x, half), t.row2, t.margin);
            koreader_entry.place(row(half, screen.right()), t.row2, t.margin);
        }

        let sleep =
            Segmented::texts(&["Book cover", "Amazon"], usize::from(settings.sleep_screen == SleepScreen::Amazon));
        let wifi = Self::read_wifi();
        let mut wifi_toggle = Toggle::new(wifi.unwrap_or(false));
        wifi_toggle.enabled = wifi.is_some();
        let updates = store.updates();
        let update_status = updates.status();
        let go_back = updates.previous();
        let label = go_back.as_ref().map_or_else(String::new, |(_, previous)| format!("Go back to {previous}"));
        let selected = TextShaping::ALL.iter().position(|&m| m == settings.text_shaping).unwrap_or(1);
        let shaping = Segmented::texts(&["Full", "Light", "FreeType"], selected);
        let ssh = ssh::status();
        let ssh_read_at = Instant::now();
        let ssh_toggle = Toggle::new(ssh.running);
        let mut this = Self {
            top,
            tabs,
            tab: Tab::Reading,
            settings_seen: store.settings_saves(),
            store,
            waker,
            screen,
            content,
            body,
            left,
            right,
            zones,
            zones_preview: Rect::default(),
            corner: Toggle::new(settings.bookmark_corner),
            koreader: Toggle::new(settings.koreader_progress),
            orientation,
            flash,
            previews,
            library,
            dictionaries,
            dictionary_entry,
            koreader_area,
            koreader_entry,
            import_sheet: None,
            open_book: None,
            folders: Vec::new(),
            removes: Vec::new(),
            folder_row: t.row,
            folder_pager,
            add_folder,
            sleep,
            version: Rect::default(),
            version_press: Press::default(),
            version_taps: (0, Instant::now()),
            developer: settings.developer,
            wifi,
            wifi_toggle,
            wifi_pending: None,
            transfer: Button::new("Wi-Fi transfer", ButtonKind::Secondary),
            ssh_was_running: ssh.running,
            ssh,
            ssh_toggle,
            ssh_pending: None,
            ssh_read_at,
            ssh_recheck_until: ssh_read_at + SSH_RECHECK_FOR,
            ssh_read_on_wake: false,
            updates,
            update_status,
            update_card: Rect::default(),
            update_card_press: Press::default(),
            check_button: Button::new("Check for updates", ButtonKind::Secondary),
            auto_check: Toggle::new(settings.check_updates),
            go_back,
            go_back_button: Button::new(label, ButtonKind::Secondary),
            offer: None,
            go_back_sheet: None,
            setup_button: Button::new("Run setup again", ButtonKind::Secondary),
            report_button: Button::new("Create problem report", ButtonKind::Secondary),
            report_rx: None,
            report_sheet: None,
            shaping,
        };
        this.load_folders(t);
        this.sync_check_button();
        this.sync_ssh_toggle();
        this.remember_hostname();
        this
    }

    /// Settings over an open book (from the top drawer): the KOReader import
    /// leaves that book out.
    pub fn with_open_book(mut self, id: Option<String>) -> Self {
        self.open_book = id;
        self
    }

    /// Settings reopened after turning the screen, where the choice was made.
    pub fn on_display_tab(mut self) -> Self {
        self.tab = Tab::Display;
        self.tabs.selected = TABS.iter().position(|&t| t == Tab::Display).unwrap_or(0);
        self
    }

    /// Settings opened to choose library folders.
    pub fn on_library_tab(mut self) -> Self {
        self.tab = Tab::Library;
        self.tabs.selected = TABS.iter().position(|&t| t == Tab::Library).unwrap_or(0);
        self
    }

    // ---- Reading ----

    fn save(&mut self, cx: &mut EventCx, what: &str, change: impl FnOnce(&mut Settings)) -> bool {
        let mut settings = self.store.settings();
        change(&mut settings);
        match self.store.save_settings(&settings) {
            Ok(()) => true,
            Err(e) => {
                log::error!("saving {what}: {e:#}");
                cx.toast(format!("Couldn't save {what}"), None);
                false
            }
        }
    }

    fn draw_reading(&mut self, cx: &mut DrawCx) {
        let t = cx.theme;
        let landscape = self.left != self.right;
        let mut col = Column::new(self.left, self.body);
        col.label(cx, "Tap zones");
        self.zones.place(col.control(t));
        self.zones.draw(cx);
        // A small page with the zones drawn on it, shaped like the screen,
        // leaving room in portrait for the two switch rows below it.
        let top = col.y + t.mm(3.0);
        let switch_rows = if landscape { 0 } else { 2 * t.mm(14.0) };
        let h = (self.content.bottom() - top - switch_rows).clamp(t.mm(20.0), t.mm(36.0));
        let w = (h * self.screen.w / self.screen.h.max(1)).min(col.w);
        let h = h.min(w * self.screen.h / self.screen.w.max(1));
        self.zones_preview = Rect::new(col.x, top, w, h);
        let zones = TapZones::ALL[self.zones.selected];
        // The corner square at the preview's scale.
        let corner =
            self.corner.on.then(|| t.mm(crate::reader::BOOKMARK_CORNER_MM) * self.zones_preview.w / self.screen.w);
        draw_zones(cx, self.zones_preview, zones, corner, None);
        let note = match zones {
            TapZones::Standard => "Tap the left side to go back and anywhere else to go forward.",
            TapZones::Mirrored => "Tap the right side to go back and anywhere else to go forward.",
            TapZones::Halves => "Tap the left half to go back and the right half to go forward.",
        };
        let gap = if landscape { "\n" } else { "\n\n" };
        let note = format!("{note}{gap}Tap the middle for the menu. Swiping left or right also turns pages.");
        let style = t.small.gray(INK2);
        // The note beside the preview and the switches below it, or
        // (landscape) the note heading the right column and the switches
        // following it.
        let mut y = if landscape {
            let h = note_h(cx, &note, self.right.w);
            cx.paragraph(&note, style, Rect { y: self.body, h, ..self.right }, HAlign::Left);
            self.body + h + t.mm(4.0)
        } else {
            let x = self.zones_preview.right() + t.mm(5.0);
            cx.paragraph(&note, style, Rect { x, w: self.content.right() - x, ..self.zones_preview }, HAlign::Left);
            self.zones_preview.bottom() + t.mm(SECTION_GAP_MM)
        };
        let (x, w) = (self.right.x, self.right.w);
        let corner = ("Tap the top-right corner to bookmark", "Tap it again to remove the bookmark.");
        y = switch_row(cx, &mut self.corner, corner, x, y, w);
        let koreader = ("Check KOReader progress", "When you open a book, offer KOReader’s newer position.");
        switch_row(cx, &mut self.koreader, koreader, x, y, w);
    }

    fn reading_gesture(&mut self, g: &Gesture, cx: &mut EventCx) {
        if let Some(on) = self.corner.on_gesture(g)
            && !self.save(cx, "the corner tap", |s| s.bookmark_corner = on)
        {
            self.corner.on = !on;
        }
        if let Some(on) = self.koreader.on_gesture(g)
            && !self.save(cx, "KOReader progress", |s| s.koreader_progress = on)
        {
            self.koreader.on = !on;
        }
        if let Some(i) = self.zones.on_gesture(g)
            && !self.save(cx, "tap zones", |s| s.tap_zones = TapZones::ALL[i])
        {
            let current = self.store.settings().tap_zones;
            self.zones.selected = TapZones::ALL.iter().position(|&z| z == current).unwrap_or(0);
        }
    }

    // ---- Display ----

    /// Portrait: one column. Landscape: orientation and folder previews side
    /// by side, and the refresh choices across both below them, where all
    /// four labels fit.
    fn draw_display(&mut self, cx: &mut DrawCx) {
        let t = cx.theme;
        let mut col = Column::new(self.left, self.body);
        col.label(cx, "Orientation");
        self.orientation.place(col.control(t));
        self.orientation.draw(cx);
        col.note(cx, "For landscape, turn the Kindle clockwise so its top edge is on the right.", &[]);
        col.gap(t);
        if self.left == self.right {
            self.draw_refresh(cx, &mut col);
            col.gap(t);
            self.draw_previews(cx, &mut col);
        } else {
            let mut right = Column::new(self.right, self.body);
            self.draw_previews(cx, &mut right);
            right.gap(t);
            let mut across = Column::new(self.content, col.y.max(right.y));
            self.draw_refresh(cx, &mut across);
        }
    }

    fn draw_refresh(&mut self, cx: &mut DrawCx, col: &mut Column) {
        col.label(cx, "Full page refresh");
        self.flash.place(col.control(cx.theme));
        self.flash.draw(cx);
        col.note(cx, "A brief flash clears ghosting. Chapter starts and pictures always refresh fully.", &[]);
    }

    fn draw_previews(&mut self, cx: &mut DrawCx, col: &mut Column) {
        col.label(cx, "Folder previews");
        self.previews.place(col.control(cx.theme));
        self.previews.draw(cx);
        let note = |preview| match preview {
            FolderPreview::Covers => "Folders in Collections show four covers, the books read most recently first.",
            FolderPreview::Stack => {
                "Folders in Collections show three covers stacked, the book read most recently in front."
            }
            FolderPreview::Latest => "Folders in Collections show the cover of the book read most recently in them.",
            FolderPreview::Icon => "Folders in Collections show a folder icon and how many books they hold.",
        };
        let variants: Vec<_> = FolderPreview::OFFERED.iter().map(|&p| note(p)).collect();
        col.note(cx, note(FolderPreview::OFFERED[self.previews.selected]), &variants);
    }

    fn display_gesture(&mut self, g: &Gesture, cx: &mut EventCx) -> Action {
        // Collections shows the new previews when Settings closes.
        if let Some(i) = self.previews.on_gesture(g)
            && !self.save(cx, "folder previews", |s| s.folder_preview = FolderPreview::OFFERED[i])
        {
            let current = self.store.settings().folder_preview;
            self.previews.selected = FolderPreview::OFFERED.iter().position(|&m| m == current).unwrap_or(0);
        }
        if let Some(i) = self.flash.on_gesture(g)
            && !self.save(cx, "page refresh", |s| s.page_flash = FLASH_CHOICES[i])
        {
            self.flash.selected = flash_choice(self.store.settings().page_flash);
        }
        if let Some(i) = self.orientation.on_gesture(g) {
            let orientation = if i == 0 { Orientation::Portrait } else { Orientation::Landscape };
            let previous = self.store.settings().orientation;
            if self.save(cx, "orientation", |s| s.orientation = orientation) {
                if orientation != previous {
                    return Action::Orientation(orientation);
                }
            } else {
                self.orientation.selected = usize::from(self.store.settings().orientation == Orientation::Landscape);
            }
        }
        Action::None
    }

    // ---- Library ----

    fn load_folders(&mut self, t: &Theme) {
        self.folders = self.store.library_folders();
        let editable = !Store::library_folders_overridden();
        self.removes = self
            .folders
            .iter()
            .map(|_| {
                let mut b = IconButton::new(Icon::Close);
                b.size = Some(t.icon_small);
                b.enabled = editable && self.folders.len() > 1;
                b
            })
            .collect();
        self.add_folder.enabled = editable;
        let page = self.folder_pager.page;
        self.folder_pager.set(page, self.folders.len().div_ceil(self.folders_per_page(t)));
        self.place_folders(t);
    }

    fn folders_top(&self, t: &Theme) -> i32 {
        self.body + t.mm(LABEL_MM)
    }

    /// Rows that fit above the add button, note and pager (and, in portrait,
    /// the dictionary entry).
    fn folders_per_page(&self, t: &Theme) -> usize {
        let bottom = if self.library.w == self.content.w { self.dictionaries.y } else { self.folder_pager.rect().y };
        let space = bottom - t.mm(24.0) - self.folders_top(t);
        (space / self.folder_row).max(1) as usize
    }

    fn visible_folders(&self, t: &Theme) -> std::ops::Range<usize> {
        let per = self.folders_per_page(t);
        let start = (self.folder_pager.page * per).min(self.folders.len());
        start..(start + per).min(self.folders.len())
    }

    fn place_folders(&mut self, t: &Theme) {
        let top = self.folders_top(t);
        let range = self.visible_folders(t);
        for (k, i) in range.clone().enumerate() {
            let row = Rect::new(self.library.x, top + k as i32 * self.folder_row, self.library.w, self.folder_row);
            self.removes[i].place(Rect::new(row.right() - t.target + t.mm(2.0), row.y, t.target, row.h));
        }
        let y = top + range.len() as i32 * self.folder_row + t.mm(3.0);
        self.add_folder.place(Rect::new(self.library.x, y, self.library.w, t.mm(CONTROL_MM)), t);
    }

    fn draw_library(&mut self, cx: &mut DrawCx) {
        let t = cx.theme;
        let c = self.library;
        cx.section_label("Library folders", Rect::new(c.x, self.body, c.w, t.mm(LABEL_MM)));
        let top = self.folders_top(t);
        for (k, i) in self.visible_folders(t).enumerate() {
            let row = Rect::new(c.x, top + k as i32 * self.folder_row, c.w, self.folder_row);
            let label = Rect { w: row.w - t.target, ..row };
            cx.line(&display_folder(&self.folders[i]), t.body, label, HAlign::Left);
            self.removes[i].draw(cx);
            cx.hline(row.bottom() - t.hairline / 2 - 1, row.x, row.right(), t.hairline, LINE);
        }
        self.add_folder.draw(cx);
        let note = if Store::library_folders_overridden() {
            "Set by VERSO_BOOKS for this session."
        } else if self.folders.len() == 1 {
            "Verso shows EPUB books from these folders and their subfolders. Add another folder before removing this one."
        } else {
            "Verso shows EPUB books from these folders and their subfolders. Removing a folder keeps its books and reading progress on the Kindle."
        };
        let y = self.add_folder.rect().bottom() + t.mm(2.0);
        cx.paragraph(note, t.small.gray(INK2), Rect::new(c.x, y, c.w, t.mm(14.0)), HAlign::Left);
        self.folder_pager.draw(cx);
        let d = self.dictionaries;
        cx.section_label("Dictionaries", Rect::new(d.x, d.y, d.w, t.mm(LABEL_MM)));
        self.dictionary_entry.draw(cx);
        let k = self.koreader_area;
        cx.section_label("KOReader", Rect::new(k.x, k.y, k.w, t.mm(LABEL_MM)));
        self.koreader_entry.draw(cx);
    }

    fn library_gesture(&mut self, g: &Gesture, cx: &mut EventCx) -> Action {
        if let Some(PagerEvent::Changed(_)) = self.folder_pager.on_gesture(g) {
            self.place_folders(cx.theme);
            return Action::None;
        }
        if let Gesture::Swipe { dir, .. } = *g
            && self.folder_pager.on_swipe(dir).is_some()
        {
            self.place_folders(cx.theme);
            return Action::None;
        }
        if let Some(ListEvent::Tapped(_)) = self.koreader_entry.on_gesture(g) {
            let (store, waker) = (self.store.clone(), self.waker.clone());
            self.import_sheet = Some(ImportSheet::open(cx.theme, self.screen, store, waker, self.open_book.clone()));
            return Action::None;
        }
        if let Some(ListEvent::Tapped(_)) = self.dictionary_entry.on_gesture(g) {
            return Action::Push(Box::new(Dictionaries::new(
                cx.theme,
                self.screen,
                self.store.clone(),
                self.waker.clone(),
                None,
            )));
        }
        if self.add_folder.on_gesture(g) {
            return Action::Push(Box::new(FolderPicker::new(
                cx.theme,
                self.screen,
                self.store.clone(),
                self.folders.clone(),
            )));
        }
        let range = self.visible_folders(cx.theme);
        if let Some(i) = range.clone().find(|&i| self.removes[i].on_gesture(g)) {
            let mut folders = self.folders.clone();
            let removed = folders.remove(i);
            match self.store.save_library_folders(folders) {
                Ok(()) => cx.toast(format!("Removed {}", display_folder(&removed)), None),
                Err(e) => {
                    log::error!("saving library folders: {e:#}");
                    cx.toast("Couldn't save library folders", None);
                }
            }
            self.load_folders(cx.theme);
        }
        Action::None
    }

    // ---- Device ----

    fn read_wifi() -> Option<bool> {
        match wifi::enabled() {
            Ok(on) => Some(on),
            Err(e) => {
                log::error!("Wi-Fi state: {e:#}");
                None
            }
        }
    }

    fn wifi_note(&self) -> &'static str {
        match (self.wifi_pending, self.wifi) {
            (Some(true), _) => "Turning on…",
            (Some(false), _) => "Turning off…",
            (None, Some(true)) if self.ssh.running => "On. Turning Wi-Fi off also stops SSH.",
            (None, Some(true)) => "On. Uses networks saved in Kindle settings.",
            (None, Some(false)) => "Off. Turn on to use a saved Wi-Fi network.",
            (None, None) => "Couldn't read Wi-Fi state. Reopen Settings to retry.",
        }
    }

    /// The developer section also shows while the server runs, so it can
    /// always be turned off.
    fn show_developer(&self) -> bool {
        self.developer || self.ssh.running || self.ssh_was_running
    }

    fn read_ssh(&mut self) {
        self.ssh = ssh::status();
        self.ssh_read_at = Instant::now();
        self.sync_ssh_toggle();
        self.remember_hostname();
    }

    /// The server runs and Wi-Fi is on, but there's no address yet, or the
    /// name isn't claimed yet: Wi-Fi is still connecting (after a wake, or
    /// a reconnect) and the row should catch up by itself.
    fn ssh_incomplete(&self) -> bool {
        self.ssh.running
            && self.ssh_pending.is_none()
            && self.wifi == Some(true)
            && (self.ssh.address.is_none() || self.ssh.hostname.is_none())
    }

    /// Still re-reading an incomplete status (see SSH_RECHECK).
    fn ssh_rechecking(&self) -> bool {
        self.ssh_incomplete() && self.ssh_read_at < self.ssh_recheck_until
    }

    /// Keeps the name the responder won (`verso-2` if another device had
    /// `verso`), so the device is found by the same name next time.
    fn remember_hostname(&mut self) {
        let Some(name) = &self.ssh.hostname else { return };
        let mut settings = self.store.settings();
        if &settings.hostname == name {
            return;
        }
        settings.hostname = name.clone();
        match self.store.save_settings(&settings) {
            Ok(()) => self.settings_seen = self.store.settings_saves(),
            Err(e) => log::error!("saving the device name: {e:#}"),
        }
    }

    fn sync_ssh_toggle(&mut self) {
        self.ssh_toggle.on = self.ssh.running;
        self.ssh_toggle.enabled = self.ssh.running || self.ssh.unavailable.is_none();
    }

    fn ssh_note(&self) -> String {
        ssh_note(self.ssh_pending, &self.ssh, self.wifi, self.ssh_rechecking())
    }

    fn tap_version(&mut self, cx: &mut EventCx) {
        let (count, last) = self.version_taps;
        let count = if cx.now.duration_since(last) < Duration::from_millis(1500) { count + 1 } else { 1 };
        self.version_taps = (count, cx.now);
        if count < 5 {
            return;
        }
        self.version_taps.0 = 0;
        let developer = !self.store.settings().developer;
        if self.save(cx, "settings", |s| s.developer = developer) {
            self.developer = developer;
            let text = if self.developer { "Developer options shown in Advanced" } else { "Developer options hidden" };
            cx.toast(text, None);
        }
    }

    fn draw_device(&mut self, cx: &mut DrawCx) {
        let t = cx.theme;
        let has_update = self.update_status.update().is_some();
        if !has_update {
            self.update_card = Rect::default();
        }
        let portrait = self.left == self.right;
        let gap = if portrait && has_update { t.mm(4.0) } else { t.mm(SECTION_GAP_MM) };
        let mut col = Column::new(self.left, self.body);
        if portrait && has_update {
            self.draw_updates(cx, &mut col);
            col.y += gap;
        }
        col.label(cx, "Sleep screen");
        self.sleep.place(col.control(t));
        self.sleep.draw(cx);
        let note = "Show the current book’s cover while asleep. When no cover is available, use Amazon’s sleep screen.";
        col.note(cx, note, &[]);
        col.y += gap;
        // The Wi-Fi switch, then its note with Wi-Fi transfer beside it.
        let row = col.take(t.mm(6.0));
        cx.line("Wi-Fi", t.body, Rect { w: row.w - Toggle::size(t).0 - t.mm(3.0), ..row }, HAlign::Left);
        self.wifi_toggle.place(row.right(), row.center().y, t);
        self.wifi_toggle.draw(cx);
        let transfer_w = button_w(cx, &[&self.transfer.label], col.w);
        // The button clear of the switch above it, and of its touch area.
        note_beside(cx, &mut col, self.wifi_note(), &mut self.transfer, transfer_w, t.mm(2.5));
        // Updates below in portrait (when up to date), or heading the right column in landscape.
        if !portrait {
            let mut right = Column::new(self.right, self.body);
            self.draw_updates(cx, &mut right);
        } else if !has_update {
            col.gap(t);
            self.draw_updates(cx, &mut col);
        }
    }

    // ---- Updates (on the Device tab) ----

    fn refresh_update_status(&mut self) {
        self.update_status = self.updates.status();
        self.sync_check_button();
    }

    /// The download checks again anyway, so with an update available the
    /// button opens the update sheet instead of checking, and the Device tab
    /// carries the update dot.
    fn sync_check_button(&mut self) {
        self.check_button.enabled = !self.update_status.checking;
        let available = self.update_status.update().is_some();
        self.check_button.label = if available { "Update" } else { "Check for updates" }.into();
        self.check_button.kind = if available { ButtonKind::Primary } else { ButtonKind::Secondary };
        self.tabs.dot = available.then(|| TABS.iter().position(|&t| t == Tab::Device)).flatten();
    }

    /// Framed callout at the top of Updates while an update is available.
    fn draw_update_card(&mut self, cx: &mut DrawCx, col: &mut Column, offer: &verso_update::check::Offer) {
        let t = cx.theme;
        let pad_x = t.mm(3.0);
        let pad_y = t.mm(2.2);
        let gap_x = t.mm(3.0);
        let btn_h = t.mm(CONTROL_MM);
        let btn_w = button_w(cx, &["Update"], col.w - 2 * pad_x);
        let text_w = (col.w - 2 * pad_x - btn_w - gap_x).max(0);
        let title = format!("Verso {} is available", offer.version);
        let now = LocalDateTime::at(update::unix_now());
        let checked = self.update_status.state.checked.map(|at| match (LocalDateTime::at(at), now) {
            (Some(at), Some(now)) => at.since(now),
            _ => "earlier".into(),
        });
        let mut sub = format!("{} download", update::file_size(offer.download));
        if self.update_status.state.is_skipped(&offer.version) {
            sub.push_str(" · Skipped");
        } else if let Some(when) = checked {
            sub.push_str(&format!(" · Checked {when}"));
        }
        let title_h = cx.text.measure(&title, t.button, Some(text_w as f32)).1.ceil() as i32;
        let sub_h = note_h(cx, &sub, text_w);
        let text_gap = t.mm(0.6);
        let text_h = title_h + text_gap + sub_h;
        let card = col.take(text_h.max(btn_h) + 2 * pad_y);
        self.update_card = card;
        if self.update_card_press.is_down() {
            cx.canvas.fill_rounded_rect(card, t.radius, FILL);
            cx.hint(card, RefreshMode::Fast);
        }
        cx.canvas.stroke_rounded_rect(card, t.radius, t.border as f32, INK);
        let text_x = card.x + pad_x;
        let text_y = card.y + (card.h - text_h) / 2;
        cx.paragraph(&title, t.button.gray(INK), Rect::new(text_x, text_y, text_w, title_h), HAlign::Left);
        cx.paragraph(
            &sub,
            t.small.gray(INK2),
            Rect::new(text_x, text_y + title_h + text_gap, text_w, sub_h),
            HAlign::Left,
        );
        let btn_rect = Rect::new(card.right() - pad_x - btn_w, card.y + (card.h - btn_h) / 2, btn_w, btn_h);
        self.check_button.place(btn_rect, t);
        self.check_button.draw(cx);
    }

    /// The version and the check button, the check's note (with "Go back
    /// to …" beside it, under the check button), and the automatic check.
    /// When an update is available, the framed callout card heads the section.
    fn draw_updates(&mut self, cx: &mut DrawCx, col: &mut Column) {
        let t = cx.theme;
        if let Some(offer) = self.update_status.update().cloned() {
            self.draw_update_card(cx, col, &offer);
            col.y += t.mm(2.0);
            if self.go_back.is_some() {
                let go_w = button_w(cx, &[&self.go_back_button.label], col.w);
                let (go_rect, version) = col.control(t).split_right(go_w);
                self.version = Rect { w: version.w - t.mm(3.0), ..version };
                cx.line(
                    &format!("Installed: Verso {}", self.update_status.current),
                    t.body,
                    self.version,
                    HAlign::Left,
                );
                self.go_back_button.place(go_rect, t);
                self.go_back_button.draw(cx);
            } else {
                self.version = col.take(t.mm(5.5));
                cx.line(
                    &format!("Installed: Verso {}", self.update_status.current),
                    t.body,
                    self.version,
                    HAlign::Left,
                );
            }
            col.y += t.mm(2.0);
        } else {
            col.label(cx, "Updates");
            let check_w = button_w(cx, &["Check for updates", &self.go_back_button.label], col.w);
            let (check, version) = col.control(t).split_right(check_w);
            // The installed release, which a data-only release moves past the app's own version.
            self.version = Rect { w: version.w - t.mm(3.0), ..version };
            cx.line(&format!("Verso {}", self.update_status.current), t.body, self.version, HAlign::Left);
            self.check_button.place(check, t);
            self.check_button.draw(cx);
            let now = LocalDateTime::at(update::unix_now());
            let note = self.update_status.note(|at| match (LocalDateTime::at(at), now) {
                (Some(at), Some(now)) => at.since(now),
                _ => "earlier".into(),
            });
            if self.go_back.is_some() {
                col.y += t.mm(NOTE_GAP_MM);
                note_beside(cx, col, &note, &mut self.go_back_button, check_w, 0);
            } else {
                col.note(cx, &note, &[]);
            }
            col.y += t.mm(3.0);
        }
        let every = format!("Every {} hours, at start or wake with Wi-Fi on.", CHECK_EVERY / 3600);
        let auto = if !self.auto_check.on {
            "Off. Verso checks only when you ask."
        } else if self.update_status.dev {
            "Off for development installs."
        } else {
            &every
        };
        col.y = switch_row(cx, &mut self.auto_check, ("Check automatically", auto), col.x, col.y, col.w);
    }

    fn updates_gesture(&mut self, g: &Gesture, cx: &mut EventCx) {
        let card_body = if self.update_card.w > 0 {
            Rect { w: (self.check_button.rect().x - self.update_card.x).max(0), ..self.update_card }
        } else {
            Rect::default()
        };
        let button_tapped = self.check_button.on_gesture(g);
        let card_tapped = self.update_card_press.on_gesture(g, card_body) == Some(PressEvent::Tap);
        if button_tapped || card_tapped {
            if let Some(offer) = self.update_status.update() {
                self.offer = Some(OfferSheet::new(cx.theme, self.screen, offer.clone()));
            } else {
                self.updates.check_now(&self.waker);
                self.refresh_update_status();
            }
        }
        if let Some((current, previous)) = &self.go_back
            && self.go_back_button.on_gesture(g)
        {
            self.go_back_sheet = Some(GoBackSheet::new(cx.theme, self.screen, current, previous));
        }
        if let Some(on) = self.auto_check.on_gesture(g)
            && !self.save(cx, "update checks", |s| s.check_updates = on)
        {
            self.auto_check.on = self.store.settings().check_updates;
        }
    }

    fn device_gesture(&mut self, g: &Gesture, cx: &mut EventCx) -> Action {
        if self.wifi_pending.is_none() && self.ssh_pending.is_none() && self.transfer.on_gesture(g) {
            let screen = TransferScreen::new(cx.theme, self.screen, self.store.clone(), self.waker.clone());
            return Action::Push(Box::new(screen));
        }
        if let Some(selected) = self.sleep.on_gesture(g) {
            let screen = if selected == 0 { SleepScreen::Cover } else { SleepScreen::Amazon };
            if !self.save(cx, "sleep screen", |s| s.sleep_screen = screen) {
                self.sleep.selected = usize::from(self.store.settings().sleep_screen == SleepScreen::Amazon);
            }
        }
        if self.wifi_pending.is_none()
            && self.ssh_pending.is_none()
            && let Some(on) = self.wifi_toggle.on_gesture(g)
        {
            self.wifi_pending = Some(on);
        }
        self.updates_gesture(g, cx);
        if let Some(PressEvent::Tap) = self.version_press.on_gesture(g, self.version) {
            self.tap_version(cx);
        }
        Action::None
    }

    fn start_report(&mut self) {
        self.report_button.enabled = false;
        self.report_button.label = "Creating report…".into();
        let (tx, rx) = std::sync::mpsc::channel();
        let data = self.store.root();
        let waker = self.waker.clone();
        self.report_rx = Some(rx);
        std::thread::Builder::new()
            .name("verso-report".into())
            .spawn(move || {
                let res = crate::report::create_report(&data);
                let _ = tx.send(res);
                waker.wake();
            })
            .ok();
    }
}

impl SystemSettings {
    // ---- Advanced ----

    fn draw_advanced(&mut self, cx: &mut DrawCx) {
        let t = cx.theme;
        let mut col = Column::new(self.left, self.body);
        col.label(cx, "Text shaping");
        self.shaping.place(col.control(t));
        self.shaping.draw(cx);
        let notes = TextShaping::ALL.map(|shaping| match shaping {
            TextShaping::Full => {
                "HarfBuzz with ligatures such as “fi” and “Th”, and full support for scripts like Arabic or Hindi. The slowest to lay out a book."
            }
            TextShaping::Light => {
                "HarfBuzz with kerning but no ligatures. Lines break as with Full, and books lay out 10–30% faster. The default."
            }
            TextShaping::FreeType => {
                "The simplest shaping. Books lay out about three times faster, but without ligatures, and spacing and hyphenation can look rougher."
            }
        });
        col.note(cx, notes[self.shaping.selected], &notes);
        col.note(cx, "Applies from the next book you open. Each book is laid out again once.", &[]);
        // Help and the developer section below, or on the right in landscape.
        let mut col = if self.left == self.right {
            col.gap(t);
            col
        } else {
            Column::new(self.right, self.body)
        };
        self.draw_help(cx, &mut col);
        if self.show_developer() {
            col.gap(t);
            col.label(cx, "Developer");
            let note = self.ssh_note();
            switch_row(cx, &mut self.ssh_toggle, ("SSH server", &note), col.x, col.y, col.w);
        }
    }

    /// Run setup again and the problem report, side by side where both
    /// labels fit, or one above the other.
    fn draw_help(&mut self, cx: &mut DrawCx, col: &mut Column) {
        let t = cx.theme;
        col.label(cx, "Help");
        let half = (col.w - t.mm(3.0)) / 2;
        let labels = [self.setup_button.label.as_str(), "Create problem report", "Creating report…"];
        let text = labels.iter().map(|l| cx.text.width(l, t.button)).fold(0.0_f32, f32::max);
        if text.ceil() as i32 + t.mm(4.0) <= half {
            let row = col.control(t);
            self.setup_button.place(Rect { w: half, ..row }, t);
            self.report_button.place(Rect { x: row.right() - half, w: half, ..row }, t);
        } else {
            self.setup_button.place(col.control(t), t);
            col.y += t.mm(2.0);
            self.report_button.place(col.control(t), t);
        }
        self.setup_button.draw(cx);
        self.report_button.draw(cx);
    }

    fn advanced_gesture(&mut self, g: &Gesture, cx: &mut EventCx) -> Action {
        if self.show_developer()
            && self.wifi_pending.is_none()
            && self.ssh_pending.is_none()
            && let Some(on) = self.ssh_toggle.on_gesture(g)
        {
            self.ssh_pending = Some(on);
        }
        if let Some(i) = self.shaping.on_gesture(g)
            && !self.save(cx, "text shaping", |s| s.text_shaping = TextShaping::ALL[i])
        {
            let current = self.store.settings().text_shaping;
            self.shaping.selected = TextShaping::ALL.iter().position(|&m| m == current).unwrap_or(1);
        }
        if self.setup_button.on_gesture(g) {
            let setup = SetupScreen::new(cx.theme, self.screen, self.store.clone(), self.waker.clone(), None);
            return Action::Push(Box::new(setup.with_open_book(self.open_book.clone())));
        }
        if self.report_rx.is_none() && self.report_button.on_gesture(g) {
            self.start_report();
        }
        Action::None
    }
}

/// A switch row in a column `w` wide from `x`: its label beside the toggle
/// (on two lines when it needs them) and a note below. Places the toggle;
/// returns the next row's top.
pub(super) fn switch_row(
    cx: &mut DrawCx,
    toggle: &mut Toggle,
    (label, note): (&str, &str),
    x: i32,
    y: i32,
    w: i32,
) -> i32 {
    let t = cx.theme;
    let label_w = w - Toggle::size(t).0 - t.mm(3.0);
    // The note starts just below the label, as under a one-line label.
    let top = if cx.text.width(label, t.body) <= label_w as f32 {
        cx.line(label, t.body, Rect::new(x, y, label_w, t.mm(6.0)), HAlign::Left);
        y + t.mm(6.0)
    } else {
        let h = cx.text.measure(label, t.body, Some(label_w as f32)).1.ceil() as i32;
        cx.paragraph(label, t.body, Rect::new(x, y + t.mm(1.0), label_w, h), HAlign::Left);
        y + t.mm(1.5) + h
    };
    toggle.place(x + w, y + t.mm(3.0), t);
    toggle.draw(cx);
    let style = t.small.gray(INK2);
    let note_h = cx.text.measure(note, style, Some(w as f32)).1.ceil() as i32;
    cx.paragraph(note, style, Rect::new(x, top, w, note_h), HAlign::Left);
    top + note_h + t.mm(SECTION_GAP_MM)
}

/// A miniature page: the back zone shaded, the menu box outlined, and the
/// bookmark corner (`corner` pixels a side, when on) outlined with a ribbon.
/// `tried`, the zone last tapped on it (setup), is drawn black.
pub(super) fn draw_zones(cx: &mut DrawCx, page: Rect, zones: TapZones, corner: Option<i32>, tried: Option<Zone>) {
    let t = cx.theme;
    // Zones inside the outline, so the shading keeps its rounded corners.
    let inner = page.inset(t.border);
    let back = zones.back(inner);
    let menu = TapZones::menu(page);
    let ink = |zone: Zone| if tried == Some(zone) { PAPER } else { INK };
    let page_fill = if tried == Some(Zone::Forward) { INK } else { PAPER };
    let back_fill = if tried == Some(Zone::Back) { INK } else { FILL_ZONE };
    cx.canvas.fill_rounded_rect(page, t.radius, page_fill);
    cx.canvas.fill_rounded_rect(back, t.radius - t.border as f32, back_fill);
    // Square off the shading's inner edge.
    let half = back.w / 2;
    cx.fill(if back.x == inner.x { back.split_right(half).0 } else { back.split_left(half).0 }, back_fill);
    let menu_fill = if tried == Some(Zone::Menu) { INK } else { PAPER };
    cx.canvas.fill_rounded_rect(menu, t.radius, menu_fill);
    cx.canvas.stroke_rounded_rect(menu, t.radius, t.hairline as f32, INK2);
    if let Some(size) = corner {
        let square = crate::reader::bookmark_corner(inner, size);
        let fill = if tried == Some(Zone::Bookmark) { INK } else { PAPER };
        cx.canvas.fill_rounded_rect(square, t.radius, fill);
        cx.canvas.stroke_rounded_rect(square, t.radius, t.hairline as f32, INK2);
        cx.icon(Icon::BookmarkFilled, square, size * 3 / 5, ink(Zone::Bookmark));
    }
    cx.canvas.stroke_rounded_rect(page, t.radius, t.border as f32, INK);
    // Labels sit below the menu box so they never overlap it.
    let band = Rect::new(page.x, menu.bottom(), page.w, page.bottom() - menu.bottom());
    let forward = if back.x == inner.x { inner.split_left(back.w).1 } else { inner.split_right(back.w).1 };
    cx.line("Back", t.caption.gray(ink(Zone::Back)), Rect { x: back.x, w: back.w, ..band }, HAlign::Center);
    cx.line("Next", t.caption.gray(ink(Zone::Forward)), Rect { x: forward.x, w: forward.w, ..band }, HAlign::Center);
    let menu_ink = if tried == Some(Zone::Menu) { PAPER } else { INK2 };
    cx.line("Menu", t.caption.gray(menu_ink), menu, HAlign::Center);
}
/// The shaded back zone in the tap-zone preview (an exact panel gray).
const FILL_ZONE: u8 = 0xdd;

/// The note under the SSH switch. `rechecking`: Wi-Fi is on but still
/// connecting, and the row is waiting for it (see SSH_RECHECK).
fn ssh_note(pending: Option<bool>, s: &ssh::Status, wifi: Option<bool>, rechecking: bool) -> String {
    match pending {
        Some(true) => return "Starting…".into(),
        Some(false) => return "Stopping…".into(),
        None => {}
    }
    if !s.running {
        return match s.unavailable {
            Some(reason) => reason.into(),
            None => format!("Port {}, key login only", ssh::PORT),
        };
    }
    match (&s.hostname, &s.address) {
        (Some(host), Some(ip)) => format!(
            "ssh -p {} root@{host}.local\nor root@{ip}. Keeps running after Verso closes. Turn it off to save battery.",
            ssh::PORT
        ),
        (None, Some(ip)) => {
            format!("ssh -p {} root@{ip}\nKeeps running after Verso closes. Turn it off to save battery.", ssh::PORT)
        }
        (_, None) if wifi == Some(true) && rechecking => "On. Waiting for Wi-Fi to connect…".into(),
        (_, None) if wifi == Some(true) => {
            "On, but Wi-Fi hasn’t connected. Check the network in Kindle settings.".into()
        }
        (_, None) => "On, but there’s no network. Turn on Wi-Fi to connect.".into(),
    }
}

impl SystemSettings {
    fn import_closed(&mut self, outcome: Outcome, cx: &mut EventCx) {
        self.import_sheet = None;
        if let Outcome::Imported(message) = outcome {
            cx.toast(message, None);
        }
        cx.redraw();
    }
}

impl Screen for SystemSettings {
    fn relayout_state(&self, state: &mut RelayoutState) {
        state.in_settings = true;
    }
    fn draw(&mut self, cx: &mut DrawCx) {
        self.top.draw(cx);
        self.tabs.draw(cx);
        match self.tab {
            Tab::Reading => self.draw_reading(cx),
            Tab::Display => self.draw_display(cx),
            Tab::Library => self.draw_library(cx),
            Tab::Device => self.draw_device(cx),
            Tab::Advanced => self.draw_advanced(cx),
        }
        if let Some(offer) = &self.offer {
            offer.draw(cx);
        }
        if let Some(sheet) = &self.go_back_sheet {
            sheet.draw(cx);
        }
        if let Some(sheet) = &self.report_sheet {
            sheet.draw(cx);
        }
        if let Some(sheet) = &self.import_sheet {
            sheet.draw(cx);
        }
    }
    fn on_gesture(&mut self, g: Gesture, cx: &mut EventCx) -> Action {
        cx.redraw();
        if let Some(sheet) = &mut self.import_sheet {
            if let Some(outcome) = sheet.on_gesture(&g, cx.theme) {
                self.import_closed(outcome, cx);
            }
            return Action::None;
        }
        if let Some(sheet) = &mut self.report_sheet {
            if sheet.on_gesture(&g) {
                self.report_sheet = None;
            }
            return Action::None;
        }
        if let Some(sheet) = &mut self.go_back_sheet {
            if sheet.on_gesture(&g) == Some(false) {
                self.go_back_sheet = None;
            }
            return Action::None;
        }
        if let Some(offer) = &mut self.offer {
            if let Some(event) = offer.on_gesture(&g) {
                let offer = self.offer.take().unwrap();
                offer.apply(&event, &self.updates);
                self.refresh_update_status();
                if event == OfferEvent::Update {
                    let (updates, waker) = (self.updates.clone(), self.waker.clone());
                    let version = offer.offer.version.clone();
                    return Action::Push(Box::new(UpdateScreen::new(
                        cx.theme,
                        self.screen,
                        updates,
                        waker,
                        version,
                        cx,
                    )));
                }
            }
            return Action::None;
        }
        if self.top.on_gesture(&g) {
            return Action::Pop;
        }
        if let Some(i) = self.tabs.on_gesture(&g) {
            self.tab = TABS[i];
            return Action::None;
        }
        match self.tab {
            Tab::Reading => self.reading_gesture(&g, cx),
            Tab::Display => return self.display_gesture(&g, cx),
            Tab::Library => return self.library_gesture(&g, cx),
            Tab::Device => return self.device_gesture(&g, cx),
            Tab::Advanced => return self.advanced_gesture(&g, cx),
        }
        Action::None
    }
    /// Called when the folder picker closes (show its addition) and when
    /// the update check finishes.
    fn on_background(&mut self, cx: &mut EventCx) {
        if let Some(sheet) = &mut self.import_sheet {
            let mut changed = false;
            if let Some(outcome) = sheet.on_background(cx.theme, &mut changed) {
                self.import_closed(outcome, cx);
            } else if changed {
                cx.redraw();
            }
        }
        if let Some(rx) = &self.report_rx {
            match rx.try_recv() {
                Ok(res) => {
                    self.report_rx = None;
                    self.report_button.enabled = true;
                    self.report_button.label = "Create problem report".into();
                    match res {
                        Ok(filename) => {
                            self.report_sheet = Some(ReportSheet::new(cx.theme, self.screen, filename));
                        }
                        Err(e) => {
                            log::error!("creating problem report: {e:#}");
                            cx.toast("Couldn't create problem report", None);
                        }
                    }
                    cx.redraw();
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.report_rx = None;
                    self.report_button.enabled = true;
                    self.report_button.label = "Create problem report".into();
                    cx.toast("Couldn't create problem report", None);
                    cx.redraw();
                }
            }
        }
        if self.updates.revision() != self.update_status.revision {
            let had_dot = self.tabs.dot;
            self.refresh_update_status();
            if self.tab == Tab::Device || self.tabs.dot != had_dot {
                cx.redraw();
            }
        }
        let saves = self.store.settings_saves();
        if saves != self.settings_seen {
            self.settings_seen = saves;
            let folders = self.store.library_folders();
            if folders != self.folders {
                self.load_folders(cx.theme);
                cx.redraw();
            }
        }
    }
    /// Re-reads the SSH status while Wi-Fi is still connecting, and once
    /// after a wake. Nothing otherwise.
    fn deadline(&self) -> Option<Instant> {
        if self.ssh_read_on_wake {
            return Some(self.ssh_read_at);
        }
        let visible = self.tab == Tab::Advanced && self.show_developer();
        (visible && self.ssh_rechecking()).then(|| (self.ssh_read_at + SSH_RECHECK).min(self.ssh_recheck_until))
    }
    fn on_tick(&mut self, cx: &mut EventCx) -> Action {
        if std::mem::take(&mut self.ssh_read_on_wake) {
            // Wi-Fi reconnects after a wake, which can take about a minute.
            self.ssh_recheck_until = cx.now + SSH_RECHECK_FOR;
        }
        let before = (self.ssh.clone(), self.ssh_rechecking());
        self.read_ssh();
        if (self.ssh.clone(), self.ssh_rechecking()) != before {
            cx.redraw();
        }
        Action::None
    }
    fn on_sleep(&mut self) {
        self.ssh_read_on_wake = true;
    }
    fn exit_refused(&mut self) {
        self.go_back_sheet = None;
    }
    fn has_pending_work(&self) -> bool {
        self.wifi_pending.is_some()
            || self.ssh_pending.is_some()
            || self.go_back_sheet.as_ref().is_some_and(|s| s.exit().is_some())
    }
    /// Scripted screenshots wait for a running update check or report build.
    fn busy(&self) -> bool {
        self.update_status.checking
            || self.report_rx.is_some()
            || self.import_sheet.as_ref().is_some_and(ImportSheet::busy)
    }
    fn run_pending_work(&mut self, cx: &mut EventCx) -> Action {
        // "Going back…" is on screen: the launcher does the rest.
        if let Some(exit) = self.go_back_sheet.as_ref().and_then(GoBackSheet::exit) {
            return exit;
        }
        if let Some(on) = self.wifi_pending.take() {
            // Stop SSH before losing the network. If stopping fails, leave
            // Wi-Fi alone so the server can still be reached and stopped.
            let stop_ssh = if on { Ok(()) } else { ssh::stop() };
            if let Err(e) = stop_ssh {
                log::error!("stopping SSH before Wi-Fi off: {e:#}");
                cx.toast("Couldn't stop SSH. Wi-Fi left on.", None);
            } else if let Err(e) = wifi::set_enabled(on) {
                log::error!("Wi-Fi: {e:#}");
                cx.toast(if on { "Couldn't turn Wi-Fi on" } else { "Couldn't turn Wi-Fi off" }, None);
            }
            self.wifi = Self::read_wifi();
            self.wifi_toggle.on = self.wifi.unwrap_or(false);
            self.wifi_toggle.enabled = self.wifi.is_some();
            self.read_ssh();
            self.ssh_recheck_until = self.ssh_read_at + SSH_RECHECK_FOR;
            cx.redraw();
        }
        if let Some(on) = self.ssh_pending.take() {
            let result = if on { ssh::start(&self.store.settings().hostname) } else { ssh::stop() };
            if let Err(e) = result {
                log::error!("SSH server: {e:#}");
                cx.toast(if on { "Couldn't start SSH" } else { "Couldn't stop SSH" }, None);
            }
            self.read_ssh();
            self.ssh_recheck_until = self.ssh_read_at + SSH_RECHECK_FOR;
            cx.redraw();
        }
        Action::None
    }
}

/// Confirmation sheet showing where a manually created problem report was saved.
struct ReportSheet {
    sheet: Sheet,
    filename: String,
    body: Rect,
    ok: Button,
}

impl ReportSheet {
    fn new(t: &Theme, screen: Rect, filename: String) -> Self {
        let sheet = Sheet::open("Problem report saved", t.mm(64.0).min(screen.h), screen, t);
        let c = sheet.content(t);
        let mut ok = Button::new("OK", ButtonKind::Primary);
        ok.place(Rect::new(c.x, c.bottom() - t.margin - t.button_h, c.w, t.button_h), t);
        let body = Rect { h: (ok.rect().y - c.y - t.mm(3.0)).max(0), ..c };
        Self { sheet, filename, body, ok }
    }

    fn message(&self) -> String {
        format!(
            "Saved to:\nverso/reports/{}\n\nCopy it with Wi-Fi transfer → Files → Reports (or over USB) and attach it to a bug report. Book names have been removed.",
            self.filename,
        )
    }

    fn draw(&self, cx: &mut DrawCx) {
        self.sheet.draw(cx);
        cx.paragraph(&self.message(), cx.theme.small.gray(INK2), self.body, HAlign::Left);
        self.ok.draw(cx);
    }

    fn on_gesture(&mut self, g: &Gesture) -> bool {
        self.sheet.on_gesture(g) || self.ok.on_gesture(g)
    }
}

#[derive(Clone, Copy)]
pub enum ExitChoice {
    Cancel,
    KeepSsh,
    StopSsh,
}

/// App-wide confirmation: no touches reach the screen below it.
pub struct ExitPrompt {
    sheet: Sheet,
    stop: Button,
    exit: Button,
    text: Rect,
    stopping: bool,
}
impl ExitPrompt {
    pub fn new(t: &Theme, screen: Rect) -> Self {
        let sheet = Sheet::open("SSH is still running", t.mm(63.0).min(screen.h), screen, t);
        let content = sheet.content(t);
        let mut stop = Button::new("Stop and exit", ButtonKind::Primary);
        stop.place(Rect::new(content.x, screen.bottom() - t.mm(24.0), content.w, t.button_h), t);
        let mut exit = Button::new("Exit", ButtonKind::Secondary);
        exit.place(Rect::new(content.x, screen.bottom() - t.mm(12.0), content.w, t.button_h), t);
        Self { text: Rect { h: t.mm(23.0), ..content }, sheet, stop, exit, stopping: false }
    }

    pub fn set_stopping(&mut self, stopping: bool) {
        self.stopping = stopping;
        self.stop.enabled = !stopping;
        self.exit.enabled = !stopping;
    }

    fn message(&self) -> &'static str {
        if self.stopping {
            "Stopping SSH…"
        } else {
            "SSH will keep running after Verso closes. Stop it to close remote connections, or exit and leave it running."
        }
    }

    pub fn draw(&self, cx: &mut DrawCx) {
        self.sheet.draw(cx);
        cx.paragraph(self.message(), cx.theme.small, self.text, HAlign::Left);
        self.stop.draw(cx);
        self.exit.draw(cx);
    }

    pub fn on_gesture(&mut self, g: &Gesture) -> Option<ExitChoice> {
        if self.stopping {
            return None;
        }
        if self.sheet.on_gesture(g) {
            Some(ExitChoice::Cancel)
        } else if self.stop.on_gesture(g) {
            Some(ExitChoice::StopSsh)
        } else if self.exit.on_gesture(g) {
            Some(ExitChoice::KeepSsh)
        } else {
            None
        }
    }
}

const USB_MESSAGE: &str = "Verso keeps your books available while charging. Exit to transfer books from your computer.";

pub struct UsbPrompt {
    sheet: Sheet,
    exit: Button,
    stay: Button,
    text: Rect,
}
impl UsbPrompt {
    pub fn new(t: &Theme, screen: Rect) -> Self {
        let sheet = Sheet::open("USB connected", t.mm(57.0).min(screen.h), screen, t);
        let content = sheet.content(t);
        let mut exit = Button::new("Exit to connect to computer", ButtonKind::Primary);
        exit.place(Rect::new(content.x, screen.bottom() - t.mm(24.0), content.w, t.button_h), t);
        let mut stay = Button::new("Keep reading", ButtonKind::Secondary);
        stay.place(Rect::new(content.x, screen.bottom() - t.mm(12.0), content.w, t.button_h), t);
        Self { text: Rect { h: t.mm(15.0), ..content }, sheet, exit, stay }
    }
}
impl Screen for UsbPrompt {
    fn draw(&mut self, cx: &mut DrawCx) {
        self.sheet.draw(cx);
        cx.paragraph(USB_MESSAGE, cx.theme.small, self.text, HAlign::Left);
        self.exit.draw(cx);
        self.stay.draw(cx);
    }
    fn on_gesture(&mut self, g: Gesture, cx: &mut EventCx) -> Action {
        cx.redraw();
        if self.exit.on_gesture(&g) {
            Action::Exit
        } else if self.stay.on_gesture(&g) || self.sheet.on_gesture(&g) {
            Action::Pop
        } else {
            Action::None
        }
    }
}

/// The report, exit and USB sheets, for `screens/fit_tests.rs`.
#[cfg(test)]
pub(super) fn fit_samples(t: &Theme, screen: Rect, text: &mut crate::gfx::TextRenderer) -> Vec<crate::ui::fit::Parts> {
    use crate::ui::fit::{Parts, text_h};
    let report = ReportSheet::new(t, screen, "report-20261122-123000.txt".into());
    let exit = ExitPrompt::new(t, screen);
    let usb = UsbPrompt::new(t, screen);
    vec![
        Parts::new("ReportSheet", report.sheet.rect(), [report.ok.rect()])
            .body(report.body, text_h(text, &report.message(), t.small, report.body.w)),
        Parts::new("ExitPrompt", exit.sheet.rect(), [exit.stop.rect(), exit.exit.rect()])
            .body(exit.text, text_h(text, exit.message(), t.small, exit.text.w)),
        Parts::new("UsbPrompt", usb.sheet.rect(), [usb.exit.rect(), usb.stay.rect()])
            .body(usb.text, text_h(text, USB_MESSAGE, t.small, usb.text.w)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(address: Option<&str>, hostname: Option<&str>) -> ssh::Status {
        ssh::Status {
            running: true,
            unavailable: None,
            address: address.map(str::to_owned),
            hostname: hostname.map(str::to_owned),
        }
    }

    fn draw(s: &mut SystemSettings, t: &Theme, w: i32, h: i32) -> crate::gfx::Canvas {
        use crate::gfx::{Canvas, IconRenderer, TextRenderer};
        let (mut canvas, mut text, mut icons) =
            (Canvas::new(w as u32, h as u32), TextRenderer::new(), IconRenderer::default());
        let mut hints = vec![];
        s.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, t, &mut hints));
        canvas
    }

    fn tap(s: &mut SystemSettings, t: &Theme, screen: Rect, rect: Rect) -> Option<String> {
        let p = crate::geom::Point::new(rect.center().x, rect.center().y);
        let mut toast = None;
        let mut cx = EventCx {
            theme: t,
            screen,
            now: Instant::now(),
            toast: &mut toast,
            forced: &mut vec![],
            needs_redraw: &mut false,
            battery: None,
        };
        s.on_gesture(Gesture::Down(p), &mut cx);
        s.on_gesture(Gesture::Tap(p), &mut cx);
        toast.map(|t| t.text)
    }

    /// Waits for the import sheet's job, as the main loop would.
    fn settle(s: &mut SystemSettings, t: &Theme, screen: Rect) -> Option<String> {
        let mut toast = None;
        for _ in 0..1000 {
            let mut cx = EventCx {
                theme: t,
                screen,
                now: Instant::now(),
                toast: &mut toast,
                forced: &mut vec![],
                needs_redraw: &mut false,
                battery: None,
            };
            s.on_background(&mut cx);
            if !s.busy() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        toast.map(|t| t.text)
    }

    #[test]
    fn settings_rows_fit_every_screen_save_and_import() -> anyhow::Result<()> {
        use crate::geom::Point;
        let dir = tempfile::tempdir()?;
        let store = Store::at(dir.path().join("data"));
        for (w, h, dpi) in crate::ui::fit::SCREENS {
            let t = Theme::for_dpi(dpi);
            let screen = Rect::new(0, 0, w, h);
            let mut s = SystemSettings::new(&t, screen, store.clone(), Waker::new(|| {}));
            // Every tab, developer section shown, ends above the bottom margin.
            s.developer = true;
            for tab in TABS {
                s.tab = tab;
                let canvas = draw(&mut s, &t, w, h);
                let margin = &canvas.gray_data()[(s.content.bottom() * w) as usize..];
                assert!(margin.iter().all(|&g| g == PAPER), "{w}x{h}: {tab:?} reaches the bottom margin");
            }
            // Device tab with an available update card and Go back button also fits.
            s.update_status.state.available = Some(verso_update::check::Offer {
                version: "99.0.0".parse()?,
                released: "2026-12-01".into(),
                notes: "Faster page turns.".into(),
                components: vec!["app".into()],
                download: 6_100_000,
                unpacked: 7_000_000,
            });
            s.go_back = Some(("0.2.0".parse()?, "0.1.0".parse()?));
            s.go_back_button.label = "Go back to 0.1.0".into();
            s.sync_check_button();
            assert_eq!(s.tabs.dot, Some(3), "Device tab carries the update dot");
            s.tab = Tab::Device;
            let canvas = draw(&mut s, &t, w, h);
            let margin = &canvas.gray_data()[(s.content.bottom() * w) as usize..];
            assert!(margin.iter().all(|&g| g == PAPER), "{w}x{h}: Device with update card reaches the bottom margin");
            assert!(s.update_card.y == s.body, "{w}x{h}: update card sits at the top of Device");
            // Tapping the card body opens the offer sheet.
            let card_left = Rect::new(s.update_card.x, s.update_card.y, t.mm(10.0), s.update_card.h);
            tap(&mut s, &t, screen, card_left);
            assert!(s.offer.is_some(), "{w}x{h}: tapping update card opens offer sheet");
            s.offer = None;
            s.update_status.state.available = None;
            s.go_back = None;
            s.sync_check_button();
            assert_eq!(s.tabs.dot, None);
            s.tab = Tab::Reading;
            draw(&mut s, &t, w, h);
            let (corner, koreader) = (s.corner.rect(), s.koreader.rect());
            assert!(corner.bottom() < koreader.y, "{w}x{h}: one row after the other");
            assert!(koreader.bottom() <= s.content.bottom(), "{w}x{h}: {koreader:?}");
            assert!(corner.y > s.zones.rect().bottom() && corner.x > s.zones_preview.right(), "{w}x{h}");
            s.tab = Tab::Library;
            draw(&mut s, &t, w, h);
            let (d, k) = (s.dictionaries, s.koreader_area);
            assert!(d.bottom() <= k.y || d.right() <= k.x, "{w}x{h}: {d:?} {k:?}");
            assert!(k.bottom() <= s.content.bottom() && k.right() <= s.content.right(), "{w}x{h}: {k:?}");
            assert!(k.y > s.add_folder.rect().bottom() || k.x >= s.library.right(), "{w}x{h}: below the folders");
            if w > h {
                assert_eq!((d.x, k.x), (s.library.right() + t.mm(10.0), d.x), "{w}x{h}: right column");
            } else {
                assert_eq!(d.y, k.y, "{w}x{h}: side by side");
            }
        }
        let (t, screen) = (Theme::for_dpi(300), Rect::new(0, 0, 1272, 1696));

        // The KOReader progress switch: off, saved; on again.
        let mut s = SystemSettings::new(&t, screen, store.clone(), Waker::new(|| {}));
        draw(&mut s, &t, 1272, 1696);
        for on in [false, true] {
            let p = Point::new(s.koreader.rect().center().x, s.koreader.rect().center().y);
            let mut toast = None;
            let mut cx = EventCx {
                theme: &t,
                screen,
                now: Instant::now(),
                toast: &mut toast,
                forced: &mut vec![],
                needs_redraw: &mut false,
                battery: None,
            };
            s.on_gesture(Gesture::Down(p), &mut cx);
            s.on_gesture(Gesture::Tap(p), &mut cx);
            assert_eq!(store.settings().koreader_progress, on);
        }

        // A book with KOReader progress, and one open under Settings.
        let lib = dir.path().join("lib");
        let books: Vec<_> = (0..2)
            .map(|i| crate::library::Book {
                path: lib.join(format!("{i}.epub")),
                ..crate::library::selection_tests::book(i)
            })
            .collect();
        for book in &books {
            let path = crate::koreader::metadata_path(&book.path).unwrap();
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(&path, "return { last_xpointer = '/k', percent_finished = 0.25 }")?;
        }
        store.save_library(&crate::library::LibraryIndex { books, ..Default::default() })?;
        let mut s = SystemSettings::new(&t, screen, store.clone(), Waker::new(|| {})).with_open_book(Some("1".into()));
        s.tab = Tab::Library;
        let row = Rect { y: s.koreader_area.y + t.mm(LABEL_MM), h: t.row2, ..s.koreader_area };
        // Cancel: nothing written.
        tap(&mut s, &t, screen, row);
        assert!(s.import_sheet.is_some());
        settle(&mut s, &t, screen);
        draw(&mut s, &t, 1272, 1696);
        let cancel = s.import_sheet.as_ref().unwrap().button("Cancel").unwrap();
        tap(&mut s, &t, screen, cancel);
        assert!(s.import_sheet.is_none());
        assert!(store.book("0").position.0.is_empty());
        // Import: the open book is left out.
        tap(&mut s, &t, screen, row);
        settle(&mut s, &t, screen);
        let import = s.import_sheet.as_ref().unwrap().button("Import").unwrap();
        assert_eq!(tap(&mut s, &t, screen, import), None);
        let toast = settle(&mut s, &t, screen);
        assert_eq!(toast.as_deref(), Some("Imported KOReader progress for 1 book"));
        assert!(s.import_sheet.is_none());
        assert_eq!((store.book("0").position.0.as_str(), store.book("1").position.0.as_str()), ("/k", ""));
        Ok(())
    }

    #[test]
    fn ssh_note_waits_for_wifi_to_connect() {
        let status = on(None, None);
        assert_eq!(ssh_note(None, &status, Some(true), true), "On. Waiting for Wi-Fi to connect…");
        assert!(ssh_note(None, &status, Some(true), false).contains("Wi-Fi hasn’t connected"));
        assert!(ssh_note(None, &status, Some(false), true).contains("Turn on Wi-Fi"));
        assert!(ssh_note(None, &status, None, true).contains("Turn on Wi-Fi"));
    }

    #[test]
    fn ssh_note_shows_how_to_connect() {
        let named = ssh_note(None, &on(Some("192.168.1.42"), Some("verso")), Some(true), false);
        assert!(named.starts_with("ssh -p 2222 root@verso.local\nor root@192.168.1.42."));
        let unnamed = ssh_note(None, &on(Some("192.168.1.42"), None), Some(true), true);
        assert!(unnamed.starts_with("ssh -p 2222 root@192.168.1.42\n"));
        assert_eq!(ssh_note(Some(true), &on(None, None), Some(true), true), "Starting…");
    }
}
