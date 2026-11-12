//! Small system controls shared by the library and USB notification.
use std::time::{Duration, Instant};

use crate::geom::Rect;
use crate::gfx::HAlign;
use crate::platform::ssh;
use crate::screens::{Action, EventCx, Screen};
use crate::store::{SleepScreen, Store};
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK2, INK3, Theme};
use crate::ui::widget::{DrawCx, Press, PressEvent};
use crate::ui::widgets::{Button, ButtonKind, Segmented, Sheet, Toggle, TopBar};

pub struct SystemSettings {
    top: TopBar,
    sleep: Segmented,
    store: Store,
    content: Rect,
    /// Tap it five times to show or hide the developer options.
    version: Rect,
    version_press: Press,
    version_taps: (u8, Instant),
    developer: bool,
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
        let mut sleep = Segmented::texts(
            &["Book cover", "Amazon"],
            usize::from(store.settings().sleep_screen == SleepScreen::Amazon),
        );
        sleep.place(Rect::new(content.x, content.y + t.mm(10.0), content.w, t.button_h));
        let dev_top = sleep.rect().bottom() + t.mm(30.0);
        let ssh = ssh::status();
        let mut ssh_toggle = Toggle::new(ssh.running);
        ssh_toggle.place(content.right(), dev_top + t.mm(12.0), t);
        let version = Rect::new(content.x, screen.bottom() - t.mm(14.0), content.w, t.target);
        let developer = store.settings().developer;
        let mut this = Self {
            top,
            sleep,
            store,
            content,
            version,
            version_press: Press::default(),
            version_taps: (0, Instant::now()),
            developer,
            ssh_was_running: ssh.running,
            ssh,
            ssh_toggle,
            ssh_pending: None,
            dev_top,
        };
        this.sync_ssh_toggle();
        this
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
        let mut settings = self.store.settings();
        settings.developer = !settings.developer;
        match self.store.save_settings(&settings) {
            Ok(()) => {
                self.developer = settings.developer;
                let text = if self.developer { "Developer options shown" } else { "Developer options hidden" };
                cx.toast(text, None);
            }
            Err(e) => {
                log::error!("saving developer options: {e:#}");
                cx.toast("Couldn't save settings", None);
            }
        }
    }
}
impl Screen for SystemSettings {
    fn draw(&mut self, cx: &mut DrawCx) {
        self.top.draw(cx);
        let t = cx.theme;
        cx.section_label("Sleep screen", Rect { h: t.mm(8.0), ..self.content });
        self.sleep.draw(cx);
        cx.paragraph(
            "Show the current book’s cover while asleep. When no cover is available, use Amazon’s sleep screen.",
            t.body,
            Rect::new(self.content.x, self.sleep.rect().bottom() + t.mm(6.0), self.content.w, t.mm(20.0)),
            HAlign::Left,
        );
        if self.show_developer() {
            let c = self.content;
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
    fn on_gesture(&mut self, g: Gesture, cx: &mut EventCx) -> Action {
        cx.redraw();
        if self.top.on_gesture(&g) {
            return Action::Pop;
        }
        if let Some(selected) = self.sleep.on_gesture(&g) {
            let mut settings = self.store.settings();
            settings.sleep_screen = if selected == 0 { SleepScreen::Cover } else { SleepScreen::Amazon };
            if let Err(e) = self.store.save_settings(&settings) {
                log::error!("saving sleep screen: {e:#}");
                self.sleep.selected = usize::from(self.store.settings().sleep_screen == SleepScreen::Amazon);
                cx.toast("Couldn't save sleep screen", None);
            }
        }
        if self.show_developer()
            && self.ssh_pending.is_none()
            && let Some(on) = self.ssh_toggle.on_gesture(&g)
        {
            self.ssh_pending = Some(on);
        }
        if let Some(PressEvent::Tap) = self.version_press.on_gesture(&g, self.version) {
            self.tap_version(cx);
        }
        Action::None
    }
    fn has_pending_work(&self) -> bool {
        self.ssh_pending.is_some()
    }
    fn run_pending_work(&mut self, cx: &mut EventCx) -> Action {
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
