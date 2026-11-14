//! Settings (reading, library folders, device) and the small system prompts
//! shared by the library and USB notification.
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::geom::Rect;
use crate::gfx::{HAlign, Icon};
use crate::platform::{ssh, wifi};
use crate::screens::folders::{FolderPicker, display_folder};
use crate::screens::{Action, EventCx, Screen};
use crate::store::{Settings, SleepScreen, Store, TapZones};
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK, INK2, INK3, LINE, PAPER, Theme};
use crate::ui::widget::{DrawCx, Press, PressEvent};
use crate::ui::widgets::{Button, ButtonKind, IconButton, Pager, PagerEvent, Segmented, Sheet, Toggle, TopBar};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Reading,
    Library,
    Device,
}

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
    screen: Rect,
    content: Rect,
    /// The top of the area below the tabs.
    body: i32,
    /// `Store::settings_saves` when the folder list was last read.
    settings_seen: u64,
    // Reading
    flash: Segmented,
    zones: Segmented,
    zones_preview: Rect,
    // Library
    folders: Vec<PathBuf>,
    removes: Vec<IconButton>,
    folder_row: i32,
    folder_pager: Pager,
    add_folder: Button,
    // Device
    sleep: Segmented,
    /// Tap it five times to show or hide the developer options.
    version: Rect,
    version_press: Press,
    version_taps: (u8, Instant),
    developer: bool,
    wifi: Option<bool>,
    wifi_toggle: Toggle,
    wifi_pending: Option<bool>,
    wifi_top: i32,
    ssh: ssh::Status,
    ssh_toggle: Toggle,
    /// Start (true) or stop the SSH server once "Starting…" is on screen.
    ssh_pending: Option<bool>,
    /// The server was running when Settings opened. The developer section
    /// then stays for this visit, so turning the server off doesn't make the
    /// switch vanish.
    ssh_was_running: bool,
    dev_top: i32,
}
impl SystemSettings {
    pub fn new(t: &Theme, screen: Rect, store: Store) -> Self {
        let mut top = TopBar::new("Settings", true);
        top.show_battery = true;
        top.place(screen, t);
        let content = screen.split_top(t.top_bar).1.inset(t.margin);
        let settings = store.settings();
        let mut tabs = Segmented::texts(&["Reading", "Library", "Device"], 0);
        tabs.place(Rect::new(content.x, content.y, content.w, t.button_h));
        let body = tabs.rect().bottom() + t.mm(6.0);

        let mut flash =
            Segmented::texts(&["Every page", "Every 6", "Every 12", "Never"], flash_choice(settings.page_flash));
        flash.place(Rect::new(content.x, body + t.mm(8.0), content.w, t.button_h));
        let zones_top = flash.rect().bottom() + t.mm(16.0);
        let selected = TapZones::ALL.iter().position(|&z| z == settings.tap_zones).unwrap_or(0);
        let mut zones = Segmented::texts(&["Standard", "Left-handed", "Halves"], selected);
        zones.place(Rect::new(content.x, zones_top + t.mm(8.0), content.w, t.button_h));
        // A small page with the zones drawn on it, portrait like the screen.
        let preview_top = zones.rect().bottom() + t.mm(5.0);
        let preview_h = (content.bottom() - preview_top).clamp(t.mm(20.0), t.mm(36.0));
        let preview_w = preview_h * screen.w / screen.h.max(1);
        let zones_preview = Rect::new(content.x, preview_top, preview_w, preview_h);

        let mut add_folder = Button::new("Add folder", ButtonKind::Secondary).with_icon(Icon::Folder);
        add_folder.place(Rect::new(content.x, 0, content.w, t.button_h), t);
        let mut folder_pager = Pager::new(1);
        folder_pager.place(Rect::new(content.x, content.bottom() - t.mm(11.0), content.w, t.mm(11.0)), t);

        let mut sleep =
            Segmented::texts(&["Book cover", "Amazon"], usize::from(settings.sleep_screen == SleepScreen::Amazon));
        sleep.place(Rect::new(content.x, body + t.mm(8.0), content.w, t.button_h));
        let wifi_top = sleep.rect().bottom() + t.mm(20.0);
        let wifi = Self::read_wifi();
        let mut wifi_toggle = Toggle::new(wifi.unwrap_or(false));
        wifi_toggle.enabled = wifi.is_some();
        wifi_toggle.place(content.right(), wifi_top + t.mm(4.0), t);
        let dev_top = wifi_top + t.mm(22.0);
        let ssh = ssh::status();
        let mut ssh_toggle = Toggle::new(ssh.running);
        ssh_toggle.place(content.right(), dev_top + t.mm(12.0), t);
        let version = Rect::new(content.x, screen.bottom() - t.mm(14.0), content.w, t.target);
        let mut this = Self {
            top,
            tabs,
            tab: Tab::Reading,
            settings_seen: store.settings_saves(),
            store,
            screen,
            content,
            body,
            flash,
            zones,
            zones_preview,
            folders: Vec::new(),
            removes: Vec::new(),
            folder_row: t.row,
            folder_pager,
            add_folder,
            sleep,
            version,
            version_press: Press::default(),
            version_taps: (0, Instant::now()),
            developer: settings.developer,
            wifi,
            wifi_toggle,
            wifi_pending: None,
            wifi_top,
            ssh_was_running: ssh.running,
            ssh,
            ssh_toggle,
            ssh_pending: None,
            dev_top,
        };
        this.load_folders(t);
        this.sync_ssh_toggle();
        this
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
        cx.section_label("Full page refresh", Rect::new(c.x, self.body, c.w, t.mm(8.0)));
        self.flash.draw(cx);
        cx.paragraph(
            "A brief flash clears ghosting. Chapter starts and pictures always refresh fully.",
            t.small.gray(INK2),
            Rect::new(c.x, self.flash.rect().bottom() + t.mm(2.0), c.w, t.mm(11.0)),
            HAlign::Left,
        );
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
        if let Some(i) = self.flash.on_gesture(g)
            && !self.save(cx, "page refresh", |s| s.page_flash = FLASH_CHOICES[i])
        {
            self.flash.selected = flash_choice(self.store.settings().page_flash);
        }
        if let Some(i) = self.zones.on_gesture(g)
            && !self.save(cx, "tap zones", |s| s.tap_zones = TapZones::ALL[i])
        {
            let current = self.store.settings().tap_zones;
            self.zones.selected = TapZones::ALL.iter().position(|&z| z == current).unwrap_or(0);
        }
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

    /// Rows that fit above the add button, note and pager.
    fn folders_per_page(&self, t: &Theme) -> usize {
        let space = self.folder_pager.rect().y - t.mm(26.0) - self.folders_top(t);
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
            let row = Rect::new(self.content.x, top + k as i32 * self.folder_row, self.content.w, self.folder_row);
            self.removes[i].place(Rect::new(row.right() - t.target + t.mm(2.0), row.y, t.target, row.h));
        }
        let y = top + range.len() as i32 * self.folder_row + t.mm(4.0);
        self.add_folder.place(Rect::new(self.content.x, y, self.content.w, t.button_h), t);
    }

    fn draw_library(&mut self, cx: &mut DrawCx) {
        let t = cx.theme;
        let c = self.content;
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

    fn sync_ssh_toggle(&mut self) {
        self.ssh_toggle.on = self.ssh.running;
        self.ssh_toggle.enabled = self.ssh.running || self.ssh.unavailable.is_none();
    }

    fn ssh_note(&self) -> String {
        match (self.ssh_pending, &self.ssh) {
            (Some(true), _) => "Starting…".into(),
            (Some(false), _) => "Stopping…".into(),
            (None, s) if s.running => match &s.address {
                Some(ip) => format!(
                    "ssh -p {} root@{ip}\nKeeps running after Verso closes. Turn it off to save battery.",
                    ssh::PORT
                ),
                None => "On, but there’s no network. Turn on Wi-Fi to connect.".into(),
            },
            (None, s) => match s.unavailable {
                Some(reason) => reason.into(),
                None => format!("Port {}, key login only", ssh::PORT),
            },
        }
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
            let text = if self.developer { "Developer options shown" } else { "Developer options hidden" };
            cx.toast(text, None);
        }
    }

    fn draw_device(&mut self, cx: &mut DrawCx) {
        let t = cx.theme;
        let c = self.content;
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
        cx.paragraph(
            self.wifi_note(),
            t.small.gray(INK2),
            Rect::new(label.x, label.bottom() - t.mm(1.0), label.w, t.mm(14.0)),
            HAlign::Left,
        );
        if self.show_developer() {
            cx.section_label("Developer", Rect::new(c.x, self.dev_top, c.w, t.mm(8.0)));
            // A label line centred on the toggle, then the note under it.
            let label = Rect::new(c.x, self.dev_top + t.mm(8.0), c.w - t.mm(12.0), t.mm(8.0));
            cx.line("SSH server", t.body, label, HAlign::Left);
            self.ssh_toggle.draw(cx);
            let note = self.ssh_note();
            let note_rect = Rect::new(c.x, label.bottom() - t.mm(1.0), c.w - t.mm(12.0), t.mm(14.0));
            cx.paragraph(&note, t.small.gray(INK2), note_rect, HAlign::Left);
        }
        let version = format!("Verso {}", env!("CARGO_PKG_VERSION"));
        cx.line(&version, t.caption.gray(INK3), self.version, HAlign::Center);
    }

    fn device_gesture(&mut self, g: &Gesture, cx: &mut EventCx) {
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
        if self.show_developer()
            && self.wifi_pending.is_none()
            && self.ssh_pending.is_none()
            && let Some(on) = self.ssh_toggle.on_gesture(g)
        {
            self.ssh_pending = Some(on);
        }
        if let Some(PressEvent::Tap) = self.version_press.on_gesture(g, self.version) {
            self.tap_version(cx);
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

impl Screen for SystemSettings {
    fn draw(&mut self, cx: &mut DrawCx) {
        self.top.draw(cx);
        self.tabs.draw(cx);
        match self.tab {
            Tab::Reading => self.draw_reading(cx),
            Tab::Library => self.draw_library(cx),
            Tab::Device => self.draw_device(cx),
        }
    }
    fn on_gesture(&mut self, g: Gesture, cx: &mut EventCx) -> Action {
        cx.redraw();
        if self.top.on_gesture(&g) {
            return Action::Pop;
        }
        if let Some(i) = self.tabs.on_gesture(&g) {
            self.tab = [Tab::Reading, Tab::Library, Tab::Device][i];
            return Action::None;
        }
        match self.tab {
            Tab::Reading => self.reading_gesture(&g, cx),
            Tab::Library => return self.library_gesture(&g, cx),
            Tab::Device => self.device_gesture(&g, cx),
        }
        Action::None
    }
    /// Called when the folder picker closes: show its addition.
    fn on_background(&mut self, cx: &mut EventCx) {
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
    fn has_pending_work(&self) -> bool {
        self.wifi_pending.is_some() || self.ssh_pending.is_some()
    }
    fn run_pending_work(&mut self, cx: &mut EventCx) -> Action {
        if let Some(on) = self.wifi_pending.take() {
            // Stop SSH before losing the network. If stopping fails, leave
            // Wi-Fi alone so the server can still be reached and stopped.
            let stop_ssh = if on { Ok(()) } else { ssh::set_running(false) };
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
            self.ssh = ssh::status();
            self.sync_ssh_toggle();
            cx.redraw();
        }
        if let Some(on) = self.ssh_pending.take() {
            if let Err(e) = ssh::set_running(on) {
                log::error!("SSH server: {e:#}");
                cx.toast(if on { "Couldn't start SSH" } else { "Couldn't stop SSH" }, None);
            }
            self.ssh = ssh::status();
            self.sync_ssh_toggle();
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
