//! Settings (reading, library and dictionary folders, device) and the small system prompts
//! shared by the library and USB notification.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::clock::LocalDateTime;
use crate::document::TextShaping;
use crate::geom::Rect;
use crate::gfx::{HAlign, Icon};
use crate::platform::{Waker, ssh, wifi};
use crate::screens::dictionaries::Dictionaries;
use crate::screens::folders::{FolderPicker, display_folder};
use crate::screens::transfer::TransferScreen;
use crate::screens::update::{GoBackSheet, OfferEvent, OfferSheet, UpdateScreen};
use crate::screens::{Action, EventCx, RelayoutState, Screen};
use crate::store::{FolderPreview, Orientation, Settings, SleepScreen, Store, TapZones};
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK, INK2, LINE, PAPER, Theme};
use crate::ui::widget::{DrawCx, Press, PressEvent};
use crate::ui::widgets::{
    Button, ButtonKind, IconButton, List, ListEvent, Pager, PagerEvent, Row, Segmented, Sheet, Toggle, TopBar,
};
use crate::update::{self, Updates};
use verso_update::Version;

#[derive(Clone, Copy, PartialEq, Eq)]
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
    // Reading
    zones: Segmented,
    zones_preview: Rect,
    // Display: one column, or (landscape) folder previews on the right.
    orientation: Segmented,
    flash: Segmented,
    previews: Segmented,
    // Library: folders on the left, dictionaries below or (landscape) right.
    library: Rect,
    dictionaries: Rect,
    dictionary_entry: List,
    folders: Vec<PathBuf>,
    removes: Vec<IconButton>,
    folder_row: i32,
    folder_pager: Pager,
    add_folder: Button,
    // Device (and Advanced): one column, or two side by side in landscape.
    device_left: Rect,
    device_right: Rect,
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
    wifi_top: i32,
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
    update_top: i32,
    /// Room for the check's note: two lines, three in a landscape column.
    update_note_h: i32,
    /// "Check for updates", or "Update" while an update is available.
    check_button: Button,
    auto_check: Toggle,
    /// The installed version and the previous one, while "Go back" can
    /// restore it.
    go_back: Option<(Version, Version)>,
    go_back_button: Button,
    offer: Option<OfferSheet>,
    go_back_sheet: Option<GoBackSheet>,
    // Advanced: text shaping, then (below, or on the right in landscape)
    // the developer section.
    dev_top: i32,
    shaping: Segmented,
    /// Height of each text-shaping note: taller in a landscape column.
    shaping_note_h: i32,
}
impl SystemSettings {
    pub fn new(t: &Theme, screen: Rect, store: Store, waker: Waker) -> Self {
        let mut top = TopBar::new("Settings", true);
        top.show_battery = true;
        top.place(screen, t);
        let content = screen.split_top(t.top_bar).1.inset(t.margin);
        let settings = store.settings();
        let mut tabs = Segmented::texts(&["Reading", "Display", "Library", "Device", "Advanced"], 0);
        tabs.place(Rect::new(content.x, content.y, content.w, t.button_h));
        let body = tabs.rect().bottom() + t.mm(6.0);

        let selected = TapZones::ALL.iter().position(|&z| z == settings.tap_zones).unwrap_or(0);
        let mut zones = Segmented::texts(&["Standard", "Left-handed", "Halves"], selected);
        zones.place(Rect::new(content.x, body + t.mm(8.0), content.w, t.button_h));
        // A small page with the zones drawn on it, shaped like the screen.
        let preview_top = zones.rect().bottom() + t.mm(5.0);
        let preview_h = (content.bottom() - preview_top).clamp(t.mm(20.0), t.mm(36.0));
        let preview_w = preview_h * screen.w / screen.h.max(1);
        let zones_preview = Rect::new(content.x, preview_top, preview_w, preview_h);

        // Landscape is too short for everything in one column.
        let (device_left, device_right) = if screen.w > screen.h {
            let gap = t.mm(10.0);
            let w = (content.w - gap) / 2;
            (Rect { w, ..content }, Rect { x: content.right() - w, w, ..content })
        } else {
            (content, content)
        };

        let mut orientation =
            Segmented::texts(&["Portrait", "Landscape"], usize::from(settings.orientation == Orientation::Landscape));
        orientation.place(Rect::new(device_left.x, body + t.mm(8.0), device_left.w, t.button_h));
        let mut flash =
            Segmented::texts(&["Every page", "Every 6", "Every 12", "Never"], flash_choice(settings.page_flash));
        flash.place(Rect::new(device_left.x, orientation.rect().bottom() + t.mm(24.0), device_left.w, t.button_h));
        let selected = FolderPreview::OFFERED.iter().position(|&m| m == settings.folder_preview).unwrap_or(0);
        // Covers is disabled for now; see FolderPreview::OFFERED.
        let mut previews = Segmented::texts(&["Stack", "Latest", "Icon"], selected);
        // In one column, below the refresh note: its two lines and a label.
        let previews_y =
            if device_right == device_left { flash.rect().bottom() + t.mm(20.0) } else { body + t.mm(8.0) };
        previews.place(Rect::new(device_right.x, previews_y, device_right.w, t.button_h));

        let mut add_folder = Button::new("Add folder", ButtonKind::Secondary).with_icon(Icon::Folder);
        add_folder.place(Rect::new(content.x, 0, content.w, t.button_h), t);
        let mut folder_pager = Pager::new(1);
        folder_pager.place(Rect::new(content.x, content.bottom() - t.mm(11.0), content.w, t.mm(11.0)), t);
        // The dictionary entry: a section label and one row. Landscape is too
        // short for it below the library folders.
        let entry_h = t.mm(8.0) + t.row2;
        let (library, dictionaries) = if screen.w > screen.h {
            let gap = t.mm(10.0);
            let w = (content.w - gap) / 2;
            let right = Rect { x: content.right() - w, w, ..content };
            // The folder pager stays under the folders.
            folder_pager.place(Rect { w, ..folder_pager.rect() }, t);
            (Rect { w, ..content }, Rect { y: body, h: entry_h, ..right })
        } else {
            let y = folder_pager.rect().y - t.mm(2.0) - entry_h;
            (content, Rect { y, h: entry_h, ..content })
        };
        let mut dictionary_entry = List::new(vec![Row::new("Dictionaries").sub("Sets, order and folders").chevron()]);
        let entry_row = Rect::new(screen.x, dictionaries.y + t.mm(8.0), screen.w, t.row2);
        let entry_row =
            if screen.w > screen.h { Rect { x: dictionaries.x, w: dictionaries.w, ..entry_row } } else { entry_row };
        let pad = if screen.w > screen.h { 0 } else { t.margin };
        dictionary_entry.place(entry_row, t.row2, pad);

        // Landscape is too short for the developer section below the rest.
        let mut sleep =
            Segmented::texts(&["Book cover", "Amazon"], usize::from(settings.sleep_screen == SleepScreen::Amazon));
        sleep.place(Rect::new(device_left.x, body + t.mm(8.0), device_left.w, t.button_h));
        let wifi_top = sleep.rect().bottom() + t.mm(20.0);
        let wifi = Self::read_wifi();
        let mut wifi_toggle = Toggle::new(wifi.unwrap_or(false));
        wifi_toggle.enabled = wifi.is_some();
        wifi_toggle.place(device_left.right(), wifi_top + t.mm(4.0), t);
        let mut transfer = Button::new("Wi-Fi transfer", ButtonKind::Secondary);
        let transfer_w = t.mm(40.0).min(device_left.w / 2);
        transfer.place(Rect::new(device_left.right() - transfer_w, wifi_top + t.mm(9.0), transfer_w, t.button_h), t);
        // Updates: below Wi-Fi, or on the right in landscape.
        let landscape = device_right != device_left;
        let update_top = if landscape { body } else { wifi_top + t.mm(23.0) };
        let u = device_right;
        let mut check_button = Button::new("Check for updates", ButtonKind::Secondary);
        let check_w = t.mm(40.0).min(u.w / 2);
        check_button.place(Rect::new(u.right() - check_w, update_top + t.mm(8.0), check_w, t.button_h), t);
        let version = Rect::new(u.x, update_top + t.mm(8.0), u.w - check_w, t.button_h);
        let update_note_h = if landscape { t.mm(16.0) } else { t.mm(11.0) };
        let mut auto_check = Toggle::new(settings.check_updates);
        auto_check.place(u.right(), Self::auto_check_top(update_top, update_note_h, t) + t.mm(4.0), t);
        let updates = store.updates();
        let update_status = updates.status();
        check_button.enabled = !update_status.checking;
        // "Go back to …": under the check button in portrait (the note
        // beside it narrows), or a row of its own in the landscape column.
        let go_back = updates.previous();
        let label = go_back.as_ref().map_or_else(String::new, |(_, previous)| format!("Go back to {previous}"));
        let mut go_back_button = Button::new(label, ButtonKind::Secondary);
        let go_back_rect = if landscape {
            let y = Self::auto_check_top(update_top, update_note_h, t) + t.mm(17.0);
            Rect::new(u.x, y, u.w, t.button_h)
        } else {
            Rect { y: check_button.rect().bottom() + t.mm(2.0), ..check_button.rect() }
        };
        go_back_button.place(go_back_rect, t);
        // Advanced: text shaping on the left, the developer section below
        // it or on the right.
        let selected = TextShaping::ALL.iter().position(|&m| m == settings.text_shaping).unwrap_or(1);
        let mut shaping = Segmented::texts(&["Full", "Light", "FreeType"], selected);
        shaping.place(Rect::new(device_left.x, body + t.mm(8.0), device_left.w, t.button_h));
        let shaping_note_h = if landscape { t.mm(16.0) } else { t.mm(11.0) };
        let dev_top = if landscape { body } else { shaping.rect().bottom() + 2 * shaping_note_h + t.mm(12.0) };
        let ssh = ssh::status();
        let ssh_read_at = Instant::now();
        let mut ssh_toggle = Toggle::new(ssh.running);
        ssh_toggle.place(device_right.right(), dev_top + t.mm(12.0), t);
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
            zones,
            zones_preview,
            orientation,
            flash,
            previews,
            library,
            dictionaries,
            dictionary_entry,
            folders: Vec::new(),
            removes: Vec::new(),
            folder_row: t.row,
            folder_pager,
            add_folder,
            device_left,
            device_right,
            sleep,
            version,
            version_press: Press::default(),
            version_taps: (0, Instant::now()),
            developer: settings.developer,
            wifi,
            wifi_toggle,
            wifi_pending: None,
            wifi_top,
            transfer,
            ssh_was_running: ssh.running,
            ssh,
            ssh_toggle,
            ssh_pending: None,
            ssh_read_at,
            ssh_recheck_until: ssh_read_at + SSH_RECHECK_FOR,
            ssh_read_on_wake: false,
            updates,
            update_status,
            update_top,
            update_note_h,
            check_button,
            auto_check,
            go_back,
            go_back_button,
            offer: None,
            go_back_sheet: None,
            dev_top,
            shaping,
            shaping_note_h,
        };
        this.load_folders(t);
        this.sync_check_button();
        this.sync_ssh_toggle();
        this.remember_hostname();
        this
    }

    /// Settings reopened after turning the screen, where the choice was made.
    pub fn on_display_tab(mut self) -> Self {
        self.tab = Tab::Display;
        self.tabs.selected = TABS.iter().position(|&t| t == Tab::Display).unwrap_or(0);
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
        let c = self.content;
        let zones_label = self.zones.rect().y - t.mm(8.0);
        cx.section_label("Tap zones", Rect::new(c.x, zones_label, c.w, t.mm(8.0)));
        self.zones.draw(cx);
        let zones = TapZones::ALL[self.zones.selected];
        draw_zones(cx, self.zones_preview, zones);
        let note = match zones {
            TapZones::Standard => "Tap the left side to go back and anywhere else to go forward.",
            TapZones::Mirrored => "Tap the right side to go back and anywhere else to go forward.",
            TapZones::Halves => "Tap the left half to go back and the right half to go forward.",
        };
        let p = self.zones_preview;
        let text = Rect::new(p.right() + t.mm(5.0), p.y, c.right() - p.right() - t.mm(5.0), p.h);
        cx.paragraph(
            &format!("{note}\n\nTap the middle for the menu. Swiping left or right also turns pages."),
            t.small.gray(INK2),
            text,
            HAlign::Left,
        );
    }

    fn reading_gesture(&mut self, g: &Gesture, cx: &mut EventCx) {
        if let Some(i) = self.zones.on_gesture(g)
            && !self.save(cx, "tap zones", |s| s.tap_zones = TapZones::ALL[i])
        {
            let current = self.store.settings().tap_zones;
            self.zones.selected = TapZones::ALL.iter().position(|&z| z == current).unwrap_or(0);
        }
    }

    // ---- Display ----

    fn draw_display(&mut self, cx: &mut DrawCx) {
        let t = cx.theme;
        let c = self.device_left;
        cx.section_label("Orientation", Rect::new(c.x, self.orientation.rect().y - t.mm(8.0), c.w, t.mm(8.0)));
        self.orientation.draw(cx);
        cx.paragraph(
            "For landscape, turn the Kindle clockwise so its top edge is on the right.",
            t.small.gray(INK2),
            Rect::new(c.x, self.orientation.rect().bottom() + t.mm(2.0), c.w, t.mm(11.0)),
            HAlign::Left,
        );
        cx.section_label("Full page refresh", Rect::new(c.x, self.flash.rect().y - t.mm(8.0), c.w, t.mm(8.0)));
        self.flash.draw(cx);
        cx.paragraph(
            "A brief flash clears ghosting. Chapter starts and pictures always refresh fully.",
            t.small.gray(INK2),
            Rect::new(c.x, self.flash.rect().bottom() + t.mm(2.0), c.w, t.mm(9.0)),
            HAlign::Left,
        );
        let c = self.device_right;
        cx.section_label("Folder previews", Rect::new(c.x, self.previews.rect().y - t.mm(8.0), c.w, t.mm(8.0)));
        self.previews.draw(cx);
        let note = match FolderPreview::OFFERED[self.previews.selected] {
            FolderPreview::Covers => "Folders in Collections show four covers, the books read most recently first.",
            FolderPreview::Stack => {
                "Folders in Collections show three covers stacked, the book read most recently in front."
            }
            FolderPreview::Latest => "Folders in Collections show the cover of the book read most recently in them.",
            FolderPreview::Icon => "Folders in Collections show a folder icon and how many books they hold.",
        };
        // As many lines as fit above the bottom edge on small screens.
        let y = self.previews.rect().bottom() + t.mm(2.0);
        let h = (self.content.bottom() - y).clamp(0, t.mm(11.0));
        cx.paragraph(note, t.small.gray(INK2), Rect::new(c.x, y, c.w, h), HAlign::Left);
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
        self.body + t.mm(8.0)
    }

    /// Rows that fit above the add button, note and pager (and, in portrait,
    /// the dictionary entry).
    fn folders_per_page(&self, t: &Theme) -> usize {
        let bottom = if self.library.w == self.content.w { self.dictionaries.y } else { self.folder_pager.rect().y };
        let space = bottom - t.mm(26.0) - self.folders_top(t);
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
        let y = top + range.len() as i32 * self.folder_row + t.mm(4.0);
        self.add_folder.place(Rect::new(self.library.x, y, self.library.w, t.button_h), t);
    }

    fn draw_library(&mut self, cx: &mut DrawCx) {
        let t = cx.theme;
        let c = self.library;
        cx.section_label("Library folders", Rect::new(c.x, self.body, c.w, t.mm(8.0)));
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
        let y = self.add_folder.rect().bottom() + t.mm(3.0);
        cx.paragraph(note, t.small.gray(INK2), Rect::new(c.x, y, c.w, t.mm(14.0)), HAlign::Left);
        self.folder_pager.draw(cx);
        let d = self.dictionaries;
        cx.section_label("Dictionaries", Rect::new(d.x, d.y, d.w, t.mm(8.0)));
        self.dictionary_entry.draw(cx);
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
        let c = self.device_left;
        cx.section_label("Sleep screen", Rect::new(c.x, self.body, c.w, t.mm(8.0)));
        self.sleep.draw(cx);
        cx.paragraph(
            "Show the current book’s cover while asleep. When no cover is available, use Amazon’s sleep screen.",
            t.small.gray(INK2),
            Rect::new(c.x, self.sleep.rect().bottom() + t.mm(2.0), c.w, t.mm(12.0)),
            HAlign::Left,
        );
        let label = Rect::new(c.x, self.wifi_top, c.w - t.mm(12.0), t.mm(8.0));
        cx.line("Wi-Fi", t.body, label, HAlign::Left);
        self.wifi_toggle.draw(cx);
        let note_w = self.transfer.rect().x - t.mm(3.0) - label.x;
        cx.paragraph(
            self.wifi_note(),
            t.small.gray(INK2),
            Rect::new(label.x, label.bottom() - t.mm(1.0), note_w, t.mm(14.0)),
            HAlign::Left,
        );
        self.transfer.draw(cx);
        self.draw_updates(cx);
    }

    // ---- Updates (on the Device tab) ----

    /// The "Check automatically" row: below the section label, the version
    /// row and the check's note.
    fn auto_check_top(update_top: i32, note_h: i32, t: &Theme) -> i32 {
        update_top + t.mm(8.0) + t.button_h + note_h + t.mm(2.0)
    }

    fn refresh_update_status(&mut self) {
        self.update_status = self.updates.status();
        self.sync_check_button();
    }

    /// The download checks again anyway, so with an update available the
    /// button opens the update sheet instead of checking.
    fn sync_check_button(&mut self) {
        self.check_button.enabled = !self.update_status.checking;
        let available = self.update_status.update().is_some();
        self.check_button.label = if available { "Update" } else { "Check for updates" }.into();
        self.check_button.kind = if available { ButtonKind::Primary } else { ButtonKind::Secondary };
    }

    /// Portrait with "Go back": the note leaves room for the button.
    fn update_note_w(&self, t: &Theme) -> i32 {
        let c = self.device_right;
        if self.go_back.is_some() && self.device_left == self.device_right {
            self.check_button.rect().x - t.mm(3.0) - c.x
        } else {
            c.w
        }
    }

    fn draw_updates(&mut self, cx: &mut DrawCx) {
        let t = cx.theme;
        let c = self.device_right;
        cx.section_label("Updates", Rect::new(c.x, self.update_top, c.w, t.mm(8.0)));
        // The installed release, which a data-only release moves past the app's own version.
        cx.line(&format!("Verso {}", self.update_status.current), t.body, self.version, HAlign::Left);
        self.check_button.draw(cx);
        let now = LocalDateTime::at(update::unix_now());
        let note = self.update_status.note(|at| match (LocalDateTime::at(at), now) {
            (Some(at), Some(now)) => at.since(now),
            _ => "earlier".into(),
        });
        let note_rect = Rect::new(c.x, self.version.bottom() + t.mm(1.0), self.update_note_w(t), self.update_note_h);
        cx.paragraph(&note, t.small.gray(INK2), note_rect, HAlign::Left);
        if self.go_back.is_some() {
            self.go_back_button.draw(cx);
        }
        let top = Self::auto_check_top(self.update_top, self.update_note_h, t);
        let label = Rect::new(c.x, top, c.w - t.mm(12.0), t.mm(8.0));
        cx.line("Check automatically", t.body, label, HAlign::Left);
        self.auto_check.draw(cx);
        let note = if !self.auto_check.on {
            "Off. Verso checks only when you ask."
        } else if self.update_status.dev {
            "Off for development installs."
        } else {
            "Once a day, when Verso starts with Wi-Fi on."
        };
        let note_rect = Rect::new(label.x, label.bottom() - t.mm(1.0), label.w, t.mm(6.0));
        cx.paragraph(note, t.small.gray(INK2), note_rect, HAlign::Left);
    }

    fn updates_gesture(&mut self, g: &Gesture, cx: &mut EventCx) {
        if self.check_button.on_gesture(g) {
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
}

impl SystemSettings {
    // ---- Advanced ----

    fn draw_advanced(&mut self, cx: &mut DrawCx) {
        self.draw_shaping(cx);
        if self.show_developer() {
            self.draw_developer(cx);
        }
    }

    fn draw_developer(&mut self, cx: &mut DrawCx) {
        let t = cx.theme;
        let c = self.device_right;
        cx.section_label("Developer", Rect::new(c.x, self.dev_top, c.w, t.mm(8.0)));
        // A label line centred on the toggle, then the note under it.
        let label = Rect::new(c.x, self.dev_top + t.mm(8.0), c.w - t.mm(12.0), t.mm(8.0));
        cx.line("SSH server", t.body, label, HAlign::Left);
        self.ssh_toggle.draw(cx);
        let note = self.ssh_note();
        let note_rect = Rect::new(c.x, label.bottom() - t.mm(1.0), c.w - t.mm(12.0), t.mm(14.0));
        cx.paragraph(&note, t.small.gray(INK2), note_rect, HAlign::Left);
    }

    fn draw_shaping(&mut self, cx: &mut DrawCx) {
        let t = cx.theme;
        let c = self.device_left;
        cx.section_label("Text shaping", Rect::new(c.x, self.shaping.rect().y - t.mm(8.0), c.w, t.mm(8.0)));
        self.shaping.draw(cx);
        let note = match TextShaping::ALL[self.shaping.selected] {
            TextShaping::Full => {
                "HarfBuzz with ligatures such as “fi” and “Th”, and full support for scripts like Arabic or Hindi. The slowest to lay out a book."
            }
            TextShaping::Light => {
                "HarfBuzz with kerning but no ligatures. Lines break as with Full, and books lay out 10–30% faster. The default."
            }
            TextShaping::FreeType => {
                "The simplest shaping. Books lay out about three times faster, but without ligatures, and spacing and hyphenation can look rougher."
            }
        };
        let note_rect = Rect::new(c.x, self.shaping.rect().bottom() + t.mm(2.0), c.w, self.shaping_note_h);
        cx.paragraph(note, t.small.gray(INK2), note_rect, HAlign::Left);
        cx.paragraph(
            "Applies from the next book you open. Each book is laid out again once.",
            t.small.gray(INK2),
            Rect { y: note_rect.bottom() + t.mm(2.0), ..note_rect },
            HAlign::Left,
        );
    }

    fn advanced_gesture(&mut self, g: &Gesture, cx: &mut EventCx) {
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
    }
}

/// A miniature page: the back zone shaded, the menu box outlined.
fn draw_zones(cx: &mut DrawCx, page: Rect, zones: TapZones) {
    let t = cx.theme;
    // Zones inside the outline, so the shading keeps its rounded corners.
    let inner = page.inset(t.border);
    let back = zones.back(inner);
    let menu = TapZones::menu(page);
    cx.canvas.fill_rounded_rect(page, t.radius, PAPER);
    cx.canvas.fill_rounded_rect(back, t.radius - t.border as f32, FILL_ZONE);
    // Square off the shading's inner edge.
    let half = back.w / 2;
    cx.fill(if back.x == inner.x { back.split_right(half).0 } else { back.split_left(half).0 }, FILL_ZONE);
    cx.canvas.fill_rounded_rect(menu, t.radius, PAPER);
    cx.canvas.stroke_rounded_rect(menu, t.radius, t.hairline as f32, INK2);
    cx.canvas.stroke_rounded_rect(page, t.radius, t.border as f32, INK);
    // Labels sit below the menu box so they never overlap it.
    let band = Rect::new(page.x, menu.bottom(), page.w, page.bottom() - menu.bottom());
    let forward = if back.x == inner.x { inner.split_left(back.w).1 } else { inner.split_right(back.w).1 };
    let style = t.caption.gray(INK);
    cx.line("Back", style, Rect { x: back.x, w: back.w, ..band }, HAlign::Center);
    cx.line("Next", style, Rect { x: forward.x, w: forward.w, ..band }, HAlign::Center);
    cx.line("Menu", t.caption.gray(INK2), menu, HAlign::Center);
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
    }
    fn on_gesture(&mut self, g: Gesture, cx: &mut EventCx) -> Action {
        cx.redraw();
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
            Tab::Advanced => self.advanced_gesture(&g, cx),
        }
        Action::None
    }
    /// Called when the folder picker closes (show its addition) and when
    /// the update check finishes.
    fn on_background(&mut self, cx: &mut EventCx) {
        if self.updates.revision() != self.update_status.revision {
            self.refresh_update_status();
            if self.tab == Tab::Device {
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
    /// Scripted screenshots wait for a running update check.
    fn busy(&self) -> bool {
        self.update_status.checking
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

    pub fn draw(&self, cx: &mut DrawCx) {
        self.sheet.draw(cx);
        cx.paragraph(
            if self.stopping {
                "Stopping SSH…"
            } else {
                "SSH will keep running after Verso closes. Stop it to close remote connections, or exit and leave it running."
            },
            cx.theme.small,
            self.text,
            HAlign::Left,
        );
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
        cx.paragraph(
            "Verso keeps your books available while charging. Exit to transfer books from your computer.",
            cx.theme.small,
            self.text,
            HAlign::Left,
        );
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
