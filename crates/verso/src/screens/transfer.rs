//! Wi-Fi transfer: where to open the web
//! app, its code, and what has arrived. The server runs only while this
//! screen is open: Done, sleep, an exit (for USB too) or 10 minutes without
//! a request stop it.

use std::sync::Arc;
use std::time::Instant;

use crate::geom::Rect;
use crate::gfx::{Family, HAlign, Icon, TextStyle, Weight};
use crate::library::worker::{CoverPause, Worker as LibraryWorker};
use crate::platform::Waker;
use crate::screens::{Action, EventCx, Screen};
use crate::store::Store;
use crate::transfer::{Activity, Device, Failure, Phase, Session, Snapshot, Stop};
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK, INK2, LINE, PAPER, Theme};
use crate::ui::widget::DrawCx;
use crate::ui::widgets::{Button, ButtonKind, Sheet, TopBar};
use verso_update::launcher::{self, EXIT_RESTART};

pub struct TransferScreen {
    screen: Rect,
    top: TopBar,
    session: Session,
    /// Pauses library cover work while this screen is open; released on Done/Drop.
    cover_pause: Option<CoverPause>,
    /// Rescanned after Done when the Files tab changed a library folder.
    library: LibraryWorker,
    snapshot: Snapshot,
    revision: u64,
    /// A phase forced for a simulator capture: nothing runs.
    forced: bool,
    store: Store,
    /// Where to open the page, the QR code and the code; the left column in
    /// landscape.
    info: Rect,
    /// What arrived; below in portrait, the right column in landscape.
    list: Rect,
    done: Button,
    /// Try again, or Start again after a stop.
    again: Button,
    /// Shown when Done is tapped while an upload is still being received.
    confirm_stop: Option<ConfirmStop>,
    /// Shown on Done after Verso's live data was edited (developer mode).
    restart_sheet: Option<RestartSheet>,
    /// The launcher can start Verso again (protocol 2). Otherwise the
    /// Restart sheet offers Exit, and the person opens Verso again.
    can_restart: bool,
    /// Done was tapped: "Stopping…" is on screen, then the session closes.
    closing: bool,
    /// Restart (or Exit) was chosen on the Restart sheet: closing leaves
    /// Verso without saving.
    restarting: bool,
    /// The QR code and the link it holds.
    qr: Option<(String, qrcode::QrCode)>,
}

/// Confirmation sheet shown when tapping Done during an active upload.
const STOP_MESSAGE: &str = "A file is still being received. Stopping now will cancel it.";

struct ConfirmStop {
    sheet: Sheet,
    text: Rect,
    keep: Button,
    stop: Button,
}

impl ConfirmStop {
    fn new(t: &Theme, screen: Rect) -> Self {
        let sheet = Sheet::open("Stop Wi-Fi transfer?", t.mm(58.0).min(screen.h - t.top_bar), screen, t);
        let c = sheet.content(t);
        let buttons_y = c.bottom() - t.margin - t.button_h;
        let text = Rect { h: buttons_y - c.y - t.mm(3.0), ..c };
        let w = (c.w - t.mm(3.0)) / 2;
        let mut keep = Button::new("Keep waiting", ButtonKind::Secondary);
        keep.place(Rect::new(c.x, buttons_y, w, t.button_h), t);
        let mut stop = Button::new("Stop", ButtonKind::Primary);
        stop.place(Rect::new(c.right() - w, buttons_y, w, t.button_h), t);
        Self { sheet, text, keep, stop }
    }

    fn draw(&self, cx: &mut DrawCx) {
        self.sheet.draw(cx);
        let height = cx.text.measure(STOP_MESSAGE, cx.theme.body, Some(self.text.w as f32)).1.ceil() as i32;
        cx.paragraph(STOP_MESSAGE, cx.theme.body, Rect { h: height.min(self.text.h), ..self.text }, HAlign::Left);
        self.keep.draw(cx);
        self.stop.draw(cx);
    }

    /// `Some(true)` to stop and cancel the upload, `Some(false)` to keep waiting.
    fn on_gesture(&mut self, g: &Gesture) -> Option<bool> {
        if self.sheet.on_gesture(g) || self.keep.on_gesture(g) {
            return Some(false);
        }
        self.stop.on_gesture(g).then_some(true)
    }
}

/// "Restart Verso to apply changes": Done after a developer wrote into
/// Verso's live data, which Verso holds in memory and saves over on exit.
struct RestartSheet {
    sheet: Sheet,
    text: Rect,
    later: Button,
    restart: Button,
    can_restart: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RestartChoice {
    /// The sheet was closed: back to the screen, still serving.
    Dismiss,
    Later,
    Restart,
}

impl RestartSheet {
    fn new(t: &Theme, screen: Rect, can_restart: bool) -> Self {
        let sheet = Sheet::open("Restart Verso to apply changes", t.mm(52.0).min(screen.h - t.top_bar), screen, t);
        let c = sheet.content(t);
        let buttons_y = c.bottom() - t.margin - t.button_h;
        let text = Rect { h: buttons_y - c.y - t.mm(3.0), ..c };
        let w = (c.w - t.mm(3.0)) / 2;
        let mut later = Button::new("Later", ButtonKind::Secondary);
        later.place(Rect::new(c.x, buttons_y, w, t.button_h), t);
        let mut restart = Button::new(if can_restart { "Restart" } else { "Exit" }, ButtonKind::Primary);
        restart.place(Rect::new(c.right() - w, buttons_y, w, t.button_h), t);
        Self { sheet, text, later, restart, can_restart }
    }

    fn message(&self) -> &'static str {
        if self.can_restart {
            "You changed Verso’s data over Wi-Fi. Verso reads it when it starts, and until then it may save over \
             your changes. Restart now to apply them."
        } else {
            "You changed Verso’s data over Wi-Fi. Exit Verso and open it again to apply your changes; until then \
             it may save over them. Exit doesn’t save."
        }
    }

    fn draw(&self, cx: &mut DrawCx) {
        self.sheet.draw(cx);
        let message = self.message();
        let height = cx.text.measure(message, cx.theme.body, Some(self.text.w as f32)).1.ceil() as i32;
        cx.paragraph(message, cx.theme.body, Rect { h: height.min(self.text.h), ..self.text }, HAlign::Left);
        self.later.draw(cx);
        self.restart.draw(cx);
    }

    fn on_gesture(&mut self, g: &Gesture) -> Option<RestartChoice> {
        if self.sheet.on_gesture(g) {
            return Some(RestartChoice::Dismiss);
        }
        if self.later.on_gesture(g) {
            return Some(RestartChoice::Later);
        }
        self.restart.on_gesture(g).then_some(RestartChoice::Restart)
    }
}

impl TransferScreen {
    pub fn new(t: &Theme, screen: Rect, store: Store, waker: Waker) -> Self {
        let device = Device::current(store.root());
        Self::with_device(t, screen, store, device, waker)
    }

    pub(crate) fn with_device(t: &Theme, screen: Rect, store: Store, device: Device, waker: Waker) -> Self {
        let mut top = TopBar::new("Wi-Fi transfer", true);
        top.place(screen, t);
        let content = screen.split_top(t.top_bar).1.inset(t.margin);
        let bottom = content.bottom() - t.button_h;
        let (info, list, done_rect) = if screen.w > screen.h {
            let gap = t.mm(10.0);
            // The address and code take a little more than half.
            let info_w = (content.w - gap) * 58 / 100;
            let w = content.w - gap - info_w;
            let right = content.right() - w;
            (
                Rect::new(content.x, content.y, info_w, content.h),
                Rect::new(right, content.y + t.mm(4.0), w, bottom - t.mm(4.0) - content.y - t.mm(4.0)),
                Rect::new(right, bottom, w, t.button_h),
            )
        } else {
            let w = content.w.min(t.mm(110.0));
            let x = content.x + (content.w - w) / 2;
            let info_h = t.mm(70.0).min((bottom - content.y) * 2 / 3);
            let list_y = content.y + info_h;
            (
                Rect::new(x, content.y, w, info_h),
                Rect::new(x, list_y, w, bottom - t.mm(4.0) - list_y),
                Rect::new(x, bottom, w, t.button_h),
            )
        };
        let mut done = Button::new("Done", ButtonKind::Secondary);
        done.place(done_rect, t);
        let mut again = Button::new("Try again", ButtonKind::Primary);
        let again_w = t.mm(45.0).min(info.w);
        again.place(Rect::new(info.x + (info.w - again_w) / 2, info.y + t.mm(52.0), again_w, t.button_h), t);
        let settings = store.settings();
        let forced = device.forced.is_some();
        if device.edited {
            store.note_live_data_edit();
        }
        let library = store.library_worker(waker.clone());
        let cover_pause = Some(library.pause_covers());
        let session = Session::start(
            Arc::new(device),
            store.clone(),
            library.clone(),
            waker,
            settings.hostname,
            settings.developer,
        );
        let snapshot = session.snapshot();
        let revision = session.revision();
        let mut this = Self {
            screen,
            top,
            session,
            cover_pause,
            library,
            snapshot,
            revision,
            forced,
            store,
            info,
            list,
            done,
            again,
            confirm_stop: None,
            restart_sheet: None,
            can_restart: launcher::protocol() >= 2,
            closing: false,
            restarting: false,
            qr: None,
        };
        this.sync();
        this
    }

    /// Stops the session, resumes cover work, and rescans what the web app
    /// changed: the library after Files tab changes in a library folder, the
    /// dictionary catalog after dictionary changes. Not when restarting:
    /// the rescans would save, and the next start scans anyway.
    fn finish(&mut self) {
        self.session.close();
        self.cover_pause = None;
        if self.restarting {
            return;
        }
        if self.session.library_changed() {
            log::info!("Wi-Fi transfer: library files changed, rescanning");
            self.library.rescan();
        }
        if self.session.dictionaries_changed()
            && let Some(service) = self.store.dictionary_service.get()
        {
            service.rescan();
        }
    }

    fn begin_closing(&mut self) {
        self.closing = true;
        self.done.label = if self.restarting { "Restarting…" } else { "Stopping…" }.into();
        self.done.enabled = false;
    }

    /// Done, once no upload is in the way: closes, or first asks to restart
    /// when Verso's live data was edited.
    fn leave(&mut self, cx: &mut EventCx) {
        if self.store.live_data_edited() {
            self.restart_sheet = Some(RestartSheet::new(cx.theme, self.screen, self.can_restart));
        } else {
            self.begin_closing();
        }
    }

    /// Reads the session again; true if anything visible changed.
    fn refresh(&mut self) -> bool {
        let revision = self.session.revision();
        if revision == self.revision {
            return false;
        }
        self.revision = revision;
        self.snapshot = self.session.snapshot();
        self.sync();
        self.remember_hostname();
        true
    }

    fn sync(&mut self) {
        self.again.label = match self.snapshot.phase {
            Phase::Stopped(_) => "Start again",
            _ => "Try again",
        }
        .into();
        let url = self.snapshot.qr_url();
        if self.qr.as_ref().map(|(u, _)| u) != url.as_ref() {
            self.qr = url.and_then(|url| {
                let code = qrcode::QrCode::with_error_correction_level(url.as_bytes(), qrcode::EcLevel::M).ok()?;
                Some((url, code))
            });
        }
    }

    /// Keeps the name the responder won (`verso-2` after a conflict), as
    /// the SSH row does, so the Kindle keeps its name next time.
    fn remember_hostname(&mut self) {
        let Some(name) = &self.snapshot.hostname else { return };
        if self.store.live_data_edited() {
            return; // It would save over a hand-edited settings.json.
        }
        if &self.store.settings().hostname == name {
            return;
        }
        if let Err(e) = self.store.update_settings(|settings| settings.hostname = name.clone()) {
            log::error!("saving the device name: {e:#}");
        }
    }

    fn has_again(&self) -> bool {
        matches!(self.snapshot.phase, Phase::NoNetwork | Phase::Stopped(_) | Phase::Failed(_))
    }

    /// The heading and note shown instead of the address, if not ready.
    fn message(&self) -> Option<(&'static str, &'static str)> {
        Some(match self.snapshot.phase {
            Phase::Ready => return None,
            Phase::Starting => ("Turning on Wi-Fi…", "Verso uses the networks saved in Kindle settings."),
            Phase::Waiting => ("Waiting for Wi-Fi to connect…", "Verso uses the networks saved in Kindle settings."),
            Phase::NoNetwork => ("Wi-Fi hasn’t connected", "Check the network in Kindle settings, then try again."),
            Phase::Stopped(Stop::Idle) => {
                ("Wi-Fi transfer stopped", "It stops after 10 minutes without activity, to save battery.")
            }
            Phase::Stopped(Stop::Slept) => ("Wi-Fi transfer stopped", "It stops while the Kindle sleeps."),
            Phase::Failed(Failure::Wifi) => {
                ("Couldn’t turn on Wi-Fi", "Try again, or turn Wi-Fi on in Kindle settings.")
            }
            Phase::Failed(Failure::PortInUse) => ("Port 80 is in use", "Another program on the Kindle is using it."),
            Phase::Failed(Failure::Server) => {
                ("Couldn’t start Wi-Fi transfer", "Try again. verso.log has the details.")
            }
        })
    }

    fn draw_ready(&self, cx: &mut DrawCx) {
        let t = cx.theme;
        let b = self.info;
        let mut y = b.y + t.mm(3.0);
        let line = |cx: &mut DrawCx, text: &str, style: TextStyle, y: i32, h: i32| {
            cx.line(text, style, Rect::new(b.x, y, b.w, h), HAlign::Center);
        };
        line(cx, "On your phone or computer, open", t.body.gray(INK2), y, t.mm(7.0));
        y += t.mm(8.0);
        let (first, second) = match (self.snapshot.name_url(), self.snapshot.ip_url()) {
            (Some(name), ip) => (name, ip.map(|ip| format!("or {ip}"))),
            (None, Some(ip)) => (ip, None),
            (None, None) => return,
        };
        let large = TextStyle::new(t.pt(16.0), Weight::SemiBold, Family::Sans);
        line(cx, &first, large, y, t.mm(11.0));
        y += t.mm(12.0);
        if let Some(second) = second {
            line(cx, &second, t.body.gray(INK2), y, t.mm(7.0));
        }
        y += t.mm(10.0);

        // The QR code, with the code beside it.
        // The code needs about 36 mm; the QR code shrinks first.
        let gap = t.mm(5.0);
        let code_w = t.mm(42.0).min(b.w - gap - t.mm(24.0)).max(t.mm(36.0));
        let side = t.mm(34.0).min(b.w - gap - code_w);
        let x = b.x + (b.w - side - gap - code_w) / 2;
        if let Some((_, qr)) = &self.qr {
            draw_qr(cx, Rect::new(x, y, side, side), qr);
        }
        let c = Rect::new(x + side + gap, y, code_w, side);
        cx.section_label("Code", Rect::new(c.x, c.y + t.mm(2.0), c.w, t.mm(8.0)));
        let digits = TextStyle::new(t.pt(28.0), Weight::SemiBold, Family::Sans).tracking(0.25);
        cx.line(&self.snapshot.code, digits, Rect::new(c.x, c.y + t.mm(10.0), c.w, t.mm(14.0)), HAlign::Left);
        cx.paragraph(
            "Same Wi-Fi network only. The QR code includes the code.",
            t.small.gray(INK2),
            Rect::new(c.x, c.y + t.mm(25.0), c.w, t.mm(12.0)),
            HAlign::Left,
        );
    }

    fn draw_message(&self, cx: &mut DrawCx, heading: &str, note: &str) {
        let t = cx.theme;
        let b = self.info;
        cx.line(heading, t.heading, Rect::new(b.x, b.y + t.mm(22.0), b.w, t.mm(10.0)), HAlign::Center);
        cx.paragraph(note, t.body.gray(INK2), Rect::new(b.x, b.y + t.mm(33.0), b.w, t.mm(16.0)), HAlign::Center);
        if self.has_again() {
            self.again.draw(cx);
        }
    }

    fn draw_activity(&self, cx: &mut DrawCx) {
        let t = cx.theme;
        let l = self.list;
        cx.hline(l.y, l.x, l.right(), t.hairline, LINE);
        cx.section_label("Received", Rect::new(l.x, l.y + t.mm(3.0), l.w, t.mm(8.0)));
        let top = l.y + t.mm(10.0);
        if self.snapshot.activity.is_empty() {
            cx.paragraph(
                "Books and dictionaries you send show up here.",
                t.small.gray(INK2),
                Rect::new(l.x, top, l.w, t.mm(12.0)),
                HAlign::Left,
            );
            return;
        }
        let row_h = t.mm(8.0);
        let rows = ((l.bottom() - top) / row_h).max(0) as usize;
        for (i, activity) in self.snapshot.activity.iter().take(rows).enumerate() {
            let row = Rect::new(l.x, top + i as i32 * row_h, l.w, row_h);
            let (icon, label, value) = match activity {
                Activity::Receiving { name } => (None, name.clone(), "receiving…".to_owned()),
                Activity::Received { name, folder } => (Some(Icon::Check), name.clone(), folder.clone()),
                Activity::Deleted { name } => (None, format!("Deleted: {name}"), String::new()),
                Activity::Dictionary { name } => (Some(Icon::Check), format!("Dictionary: {name}"), String::new()),
                Activity::Setting { label } => (Some(Icon::Check), label.clone(), String::new()),
            };
            let icon_w = t.icon_small + t.mm(2.0);
            if let Some(icon) = icon {
                cx.icon(icon, Rect { w: t.icon_small, ..row }, t.icon_small, INK);
            }
            let value_w = if value.is_empty() { 0 } else { (row.w / 3).min(t.mm(32.0)) };
            let label_rect = Rect::new(row.x + icon_w, row.y, row.w - icon_w - value_w - t.mm(2.0), row.h);
            cx.line(&label, t.body, label_rect, HAlign::Left);
            if !value.is_empty() {
                let value_rect = Rect::new(row.right() - value_w, row.y, value_w, row.h);
                cx.line(&value, t.small.gray(INK2), value_rect, HAlign::Right);
            }
        }
    }
}

/// The QR code's modules, as large as fit in `rect`, on whole pixels.
/// Dark mode draws a tight 4-module white (canvas `INK`) quiet-zone plate
/// around dark (`PAPER`) modules so phones scan it on a black page.
fn draw_qr(cx: &mut DrawCx, rect: Rect, qr: &qrcode::QrCode) {
    let n = qr.width() as i32;
    let dark_mode = cx.canvas.is_dark();
    let pad = if dark_mode { 8 } else { 2 };
    let module = (rect.w.min(rect.h) / (n + pad)).max(1);
    let x0 = rect.x + (rect.w - module * n) / 2;
    let y0 = rect.y + (rect.h - module * n) / 2;
    let module_gray = if dark_mode {
        if let Some(plate) =
            Rect::new(x0 - 4 * module, y0 - 4 * module, (n + 8) * module, (n + 8) * module).intersect(&rect)
        {
            cx.fill(plate, INK);
        }
        PAPER
    } else {
        INK
    };
    let colors = qr.to_colors();
    for y in 0..n {
        let mut x = 0;
        while x < n {
            let dark = |x: i32| colors[(y * n + x) as usize] == qrcode::Color::Dark;
            if !dark(x) {
                x += 1;
                continue;
            }
            let start = x;
            while x < n && dark(x) {
                x += 1;
            }
            cx.fill(Rect::new(x0 + start * module, y0 + y * module, (x - start) * module, module), module_gray);
        }
    }
}

impl Screen for TransferScreen {
    fn draw(&mut self, cx: &mut DrawCx) {
        self.top.draw(cx);
        match self.message() {
            None => self.draw_ready(cx),
            Some((heading, note)) => self.draw_message(cx, heading, note),
        }
        self.draw_activity(cx);
        self.done.draw(cx);
        if let Some(sheet) = &self.confirm_stop {
            sheet.draw(cx);
        }
        if let Some(sheet) = &self.restart_sheet {
            sheet.draw(cx);
        }
    }

    fn on_gesture(&mut self, g: Gesture, cx: &mut EventCx) -> Action {
        if self.closing {
            return Action::None;
        }
        cx.redraw();
        if let Some(sheet) = &mut self.restart_sheet {
            match sheet.on_gesture(&g) {
                Some(RestartChoice::Dismiss) => self.restart_sheet = None,
                Some(RestartChoice::Later) => {
                    self.restart_sheet = None;
                    cx.toast("Verso may save over your changes until it starts again.", None);
                    self.begin_closing();
                }
                Some(RestartChoice::Restart) => {
                    self.restart_sheet = None;
                    self.restarting = true;
                    self.begin_closing();
                }
                None => {}
            }
            return Action::None;
        }
        if let Some(sheet) = &mut self.confirm_stop {
            match sheet.on_gesture(&g) {
                Some(true) => {
                    self.confirm_stop = None;
                    self.leave(cx);
                }
                Some(false) => self.confirm_stop = None,
                None => {}
            }
            return Action::None;
        }
        if self.top.on_gesture(&g) || self.done.on_gesture(&g) {
            if self.session.receiving() {
                self.confirm_stop = Some(ConfirmStop::new(cx.theme, self.screen));
            } else {
                self.leave(cx);
            }
            return Action::None;
        }
        if self.has_again() && self.again.on_gesture(&g) {
            match self.snapshot.phase {
                Phase::NoNetwork => self.session.retry(),
                _ => self.session.restart(),
            }
            self.refresh();
        }
        Action::None
    }

    fn on_background(&mut self, cx: &mut EventCx) {
        self.session.set_battery(cx.battery);
        if self.refresh() {
            cx.redraw();
        }
    }

    /// The idle stop, IDLE after the last request.
    fn deadline(&self) -> Option<Instant> {
        if self.closing { None } else { self.session.idle_deadline() }
    }

    fn on_tick(&mut self, cx: &mut EventCx) -> Action {
        if self.session.idle_deadline().is_some_and(|due| cx.now >= due) {
            self.confirm_stop = None;
            self.restart_sheet = None;
            self.session.stop(Stop::Idle);
            self.refresh();
            cx.redraw();
        }
        Action::None
    }

    fn on_sleep(&mut self) {
        self.confirm_stop = None;
        self.restart_sheet = None;
        self.session.stop(Stop::Slept);
        self.refresh();
    }

    fn on_wake(&mut self, _now: Instant) {
        self.refresh();
    }

    /// Exiting (also to connect USB) stops the server and puts Wi-Fi back.
    fn on_quit(&mut self) -> anyhow::Result<()> {
        self.finish();
        Ok(())
    }

    /// Scripted screenshots wait for the start.
    fn busy(&self) -> bool {
        self.snapshot.phase == Phase::Starting && !self.forced
    }

    fn has_pending_work(&self) -> bool {
        self.closing
    }

    /// The server is already stopped; the screen shows that and offers Done again.
    fn exit_refused(&mut self) {
        self.closing = false;
        self.restarting = false;
        self.done.label = "Done".into();
        self.done.enabled = true;
    }

    /// "Stopping…" is on screen: stop, then leave. Restart stops the server,
    /// closes the port and puts Wi-Fi back like Done, then leaves Verso
    /// with nothing more saved (`Store::freeze`), so it reads the edits when
    /// the launcher starts it again.
    fn run_pending_work(&mut self, _cx: &mut EventCx) -> Action {
        self.finish();
        if !self.restarting {
            return Action::Pop;
        }
        self.store.freeze();
        let code = if self.can_restart { EXIT_RESTART } else { 0 };
        log::info!("Wi-Fi transfer: leaving to apply edits to Verso's data (exit code {code})");
        Action::ExitWith(code)
    }
}

/// The stop and restart sheets, for `screens/fit_tests.rs`.
#[cfg(test)]
pub(super) fn fit_samples(t: &Theme, screen: Rect, text: &mut crate::gfx::TextRenderer) -> Vec<crate::ui::fit::Parts> {
    use crate::ui::fit::{Parts, text_h};
    let stop = ConfirmStop::new(t, screen);
    let mut samples = vec![
        Parts::new("ConfirmStop", stop.sheet.rect(), [stop.keep.rect(), stop.stop.rect()])
            .body(stop.text, text_h(text, STOP_MESSAGE, t.body, stop.text.w)),
    ];
    for can_restart in [true, false] {
        let restart = RestartSheet::new(t, screen, can_restart);
        let needed = text_h(text, restart.message(), t.body, restart.text.w);
        samples.push(
            Parts::new("RestartSheet", restart.sheet.rect(), [restart.later.rect(), restart.restart.rect()])
                .body(restart.text, needed),
        );
    }
    samples
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Point;
    use crate::gfx::Canvas;
    use crate::transfer::fake::{Net, device as fake_device, free_port};
    use std::io::{Read, Write};
    use std::net::{Ipv4Addr, SocketAddr, TcpStream};
    use std::sync::Mutex;
    use std::time::Duration;

    fn tap(screen: &mut TransferScreen, point: Point, cx: &mut EventCx) -> Action {
        screen.on_gesture(Gesture::Down(point), cx);
        screen.on_gesture(Gesture::Tap(point), cx)
    }

    #[test]
    fn done_during_upload_asks_then_cancels_and_deletes_part_file() {
        let dir = tempfile::tempdir().unwrap();
        let books_dir = dir.path().join("books");
        std::fs::create_dir_all(&books_dir).unwrap();
        let store = Store::at(dir.path().join("data"));
        store.save_library_folders(vec![books_dir.clone()]).unwrap();
        let net = Arc::new(Mutex::new(Net { wifi: true, address: Some(Ipv4Addr::LOCALHOST), ..Net::default() }));
        let port = free_port();
        let mut dev = fake_device(&net, dir.path().join("markers"));
        dev.listen = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let theme = Theme::for_dpi(300);
        let bounds = Rect::new(0, 0, 1272, 1696);
        let mut screen = TransferScreen::with_device(&theme, bounds, store, dev, Waker::new(|| {}));

        let started = Instant::now();
        while screen.session.snapshot().phase != Phase::Ready {
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(10));
        }
        let code = screen.session.snapshot().code;

        // Sign in and start an upload that stalls mid-body.
        let mut login = TcpStream::connect(("127.0.0.1", port)).unwrap();
        login.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let body = format!("{{\"code\":\"{code}\"}}");
        let req = format!(
            "POST /api/login HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-Verso-Request: 1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        login.write_all(req.as_bytes()).unwrap();
        let mut resp = String::new();
        login.read_to_string(&mut resp).unwrap();
        let cookie = resp.lines().find_map(|l| l.strip_prefix("Set-Cookie: ")).unwrap().split(';').next().unwrap();

        let encoded_folder: String = books_dir
            .to_str()
            .unwrap()
            .bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.') {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect();
        let mut upload = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let head = format!(
            "PUT /api/books?folder={encoded_folder}&name=Stalled.epub HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nCookie: {cookie}\r\nX-Verso-Request: 1\r\nContent-Length: 500000\r\n\r\n"
        );
        upload.write_all(head.as_bytes()).unwrap();
        let mut chunk = Vec::from(*b"PK\x03\x04");
        chunk.extend(vec![0u8; 70_000]);
        upload.write_all(&chunk).unwrap();

        let part = books_dir.join(".verso-upload.part");
        let started = Instant::now();
        while !part.exists() || !screen.session.receiving() {
            assert!(started.elapsed() < Duration::from_secs(5), "upload didn't enter receiving state");
            std::thread::sleep(Duration::from_millis(10));
        }

        let mut toast = None;
        let mut forced = Vec::new();
        let mut needs_redraw = false;
        let mut cx = EventCx {
            theme: &theme,
            screen: bounds,
            now: Instant::now(),
            battery: None,
            toast: &mut toast,
            forced: &mut forced,
            needs_redraw: &mut needs_redraw,
            dark: false,
        };

        // Tapping Done during an upload opens the confirmation sheet instead of closing.
        let done_pt = screen.done.rect().center();
        tap(&mut screen, done_pt, &mut cx);
        assert!(screen.confirm_stop.is_some());
        assert!(!screen.closing);
        assert!(part.exists());

        // Keep waiting dismisses the sheet and leaves the upload running.
        let keep_pt = screen.confirm_stop.as_ref().unwrap().keep.rect().center();
        tap(&mut screen, keep_pt, &mut cx);
        assert!(screen.confirm_stop.is_none());
        assert!(!screen.closing);
        assert!(screen.session.receiving());

        // Done → Stop cancels the upload, deletes its part file, and pops.
        tap(&mut screen, done_pt, &mut cx);
        let stop_pt = screen.confirm_stop.as_ref().unwrap().stop.rect().center();
        tap(&mut screen, stop_pt, &mut cx);
        assert!(screen.closing);
        assert!(matches!(screen.run_pending_work(&mut cx), Action::Pop));
        assert!(!part.exists(), "confirmed stop deletes the part file");
        assert!(!books_dir.join("Stalled.epub").exists());
        assert!(!screen.session.receiving());
    }

    /// A screen whose session shows a phase without serving, over its own
    /// data folder.
    fn idle_screen(edited: bool, can_restart: bool) -> (tempfile::TempDir, Store, TransferScreen) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().join("data"));
        let net = Arc::new(Mutex::new(Net { wifi: true, address: Some(Ipv4Addr::LOCALHOST), ..Net::default() }));
        let mut dev = fake_device(&net, dir.path().join("markers"));
        dev.forced = Some(Phase::Stopped(Stop::Idle));
        dev.edited = edited;
        let theme = Theme::for_dpi(300);
        let mut screen =
            TransferScreen::with_device(&theme, Rect::new(0, 0, 1272, 1696), store.clone(), dev, Waker::new(|| {}));
        screen.can_restart = can_restart;
        (dir, store, screen)
    }

    #[test]
    fn done_after_live_data_edits_offers_restart_and_leaves_without_saving() {
        let theme = Theme::for_dpi(300);
        let bounds = Rect::new(0, 0, 1272, 1696);
        let mut toast = None;
        let mut forced = Vec::new();
        let mut needs_redraw = false;
        let mut cx = EventCx {
            theme: &theme,
            screen: bounds,
            now: Instant::now(),
            battery: None,
            toast: &mut toast,
            forced: &mut forced,
            needs_redraw: &mut needs_redraw,
            dark: false,
        };

        // Without edits, Done just closes.
        let (_dir, _store, mut plain) = idle_screen(false, true);
        let done = plain.done.rect().center();
        tap(&mut plain, done, &mut cx);
        assert!(plain.restart_sheet.is_none() && plain.closing);
        assert!(matches!(plain.run_pending_work(&mut cx), Action::Pop));

        // With edits: the sheet; closing it goes back to the screen.
        let (dir, store, mut screen) = idle_screen(true, true);
        assert!(store.live_data_edited());
        tap(&mut screen, done, &mut cx);
        let sheet = screen.restart_sheet.as_ref().expect("the Restart sheet");
        assert_eq!(sheet.restart.label, "Restart");
        tap(&mut screen, Point::new(bounds.w / 2, 40), &mut cx);
        assert!(screen.restart_sheet.is_none() && !screen.closing, "dismissed");

        // Restart: nothing more is saved, and the launcher starts Verso again.
        tap(&mut screen, done, &mut cx);
        let restart = screen.restart_sheet.as_ref().unwrap().restart.rect().center();
        tap(&mut screen, restart, &mut cx);
        assert!(screen.closing && screen.restarting);
        assert_eq!(screen.done.label, "Restarting…");
        assert!(matches!(screen.run_pending_work(&mut cx), Action::ExitWith(EXIT_RESTART)));
        let settings = dir.path().join("data/settings.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(&settings, "{\"developer\":true}").unwrap();
        store.save_settings(&store.settings()).unwrap();
        store.save_book("abc", &crate::store::BookState::default()).unwrap();
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), "{\"developer\":true}", "the edit stays");
        assert!(!dir.path().join("data/books/abc.json").exists());
        screen.on_quit().unwrap();
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), "{\"developer\":true}");

        // Under an older launcher: Exit, with code 0.
        let (_dir, _store, mut old) = idle_screen(true, false);
        tap(&mut old, done, &mut cx);
        assert_eq!(old.restart_sheet.as_ref().unwrap().restart.label, "Exit");
        let exit = old.restart_sheet.as_ref().unwrap().restart.rect().center();
        tap(&mut old, exit, &mut cx);
        assert!(matches!(old.run_pending_work(&mut cx), Action::ExitWith(0)));

        // Later: closes as usual, with a warning, and saves still happen.
        let (dir, store, mut later) = idle_screen(true, true);
        tap(&mut later, done, &mut cx);
        let button = later.restart_sheet.as_ref().unwrap().later.rect().center();
        tap(&mut later, button, &mut cx);
        assert!(later.closing && !later.restarting);
        assert!(matches!(later.run_pending_work(&mut cx), Action::Pop));
        assert!(cx.toast.is_some(), "Later warns");
        store.save_settings(&store.settings()).unwrap();
        assert!(dir.path().join("data/settings.json").exists());
    }

    #[test]
    fn done_rescans_the_library_after_files_tab_changes() {
        let dir = tempfile::tempdir().unwrap();
        // The fake device roots the Files tab in its marker folder.
        let files_root = dir.path().join("markers");
        let books_dir = files_root.join("documents");
        std::fs::create_dir_all(&books_dir).unwrap();
        let store = Store::at(dir.path().join("data"));
        store.save_library_folders(vec![books_dir.clone()]).unwrap();
        let net = Arc::new(Mutex::new(Net { wifi: true, address: Some(Ipv4Addr::LOCALHOST), ..Net::default() }));
        let port = free_port();
        let mut dev = fake_device(&net, files_root.clone());
        dev.listen = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let theme = Theme::for_dpi(300);
        let bounds = Rect::new(0, 0, 1272, 1696);
        let mut screen = TransferScreen::with_device(&theme, bounds, store, dev, Waker::new(|| {}));
        let started = Instant::now();
        while screen.session.snapshot().phase != Phase::Ready {
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(10));
        }
        // Let the opening scan of the empty library finish.
        let started = Instant::now();
        while screen.library.take_status().is_none_or(|s| s.working) {
            assert!(started.elapsed() < Duration::from_secs(10), "initial scan timed out");
            std::thread::sleep(Duration::from_millis(10));
        }

        let send = |head: String, body: &[u8]| {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let head = format!(
                "{head}Host: 127.0.0.1:{port}\r\nX-Verso-Request: 1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(body).unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            response
        };
        let code = screen.session.snapshot().code;
        let login = send("POST /api/login HTTP/1.1\r\n".into(), format!("{{\"code\":\"{code}\"}}").as_bytes());
        let cookie = login.lines().find_map(|l| l.strip_prefix("Set-Cookie: ")).unwrap().split(';').next().unwrap();
        let book = std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/library3.epub"))
            .unwrap();
        let put = send(format!("PUT /api/files?path=documents/New.epub HTTP/1.1\r\nCookie: {cookie}\r\n"), &book);
        assert!(put.starts_with("HTTP/1.1 201"), "{put}");
        assert!(screen.session.library_changed());
        assert!(screen.library.current().books.is_empty(), "the Files tab doesn't touch the index itself");

        screen.begin_closing();
        let mut toast = None;
        let mut forced = Vec::new();
        let mut needs_redraw = false;
        let mut cx = EventCx {
            theme: &theme,
            screen: bounds,
            now: Instant::now(),
            battery: None,
            toast: &mut toast,
            forced: &mut forced,
            needs_redraw: &mut needs_redraw,
            dark: false,
        };
        assert!(matches!(screen.run_pending_work(&mut cx), Action::Pop));
        let started = Instant::now();
        while !screen.library.current().books.iter().any(|b| b.path == books_dir.join("New.epub")) {
            assert!(started.elapsed() < Duration::from_secs(10), "Done didn't rescan the library");
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    #[test]
    fn dark_qr_draws_a_tight_four_module_quiet_zone_plate() {
        let qr =
            qrcode::QrCode::with_error_correction_level(b"http://10.0.0.2/?code=4827", qrcode::EcLevel::M).unwrap();
        let n = qr.width() as i32;
        let rect = Rect::new(40, 40, 340, 340);
        let theme = Theme::for_dpi(300);
        let (mut text, mut icons, mut hints) =
            (crate::gfx::TextRenderer::new(), crate::gfx::IconRenderer::default(), vec![]);

        let mut light = Canvas::new(420, 420);
        light.fill(PAPER);
        draw_qr(&mut DrawCx::new(&mut light, &mut text, &mut icons, &theme, &mut hints), rect, &qr);
        // Light mode draws no plate: quiet zone outside modules remains PAPER.
        assert_eq!(light.gray_data()[40 * 420 + 40], PAPER);

        let mut dark = Canvas::new(420, 420);
        dark.set_dark(true);
        dark.fill(PAPER);
        draw_qr(&mut DrawCx::new(&mut dark, &mut text, &mut icons, &theme, &mut hints), rect, &qr);

        let module = (rect.w.min(rect.h) / (n + 8)).max(1);
        let x0 = rect.x + (rect.w - module * n) / 2;
        let y0 = rect.y + (rect.h - module * n) / 2;
        let plate = Rect::new(x0 - 4 * module, y0 - 4 * module, (n + 8) * module, (n + 8) * module);
        let mut out = vec![0u8; 420 * 420];
        dark.copy_gray(dark.bounds(), &mut out, 420, true);
        let at = |x: i32, y: i32| out[y as usize * 420 + x as usize];
        // Outside plate is black background; inside 4-module border is white; (0, 0) finder corner is black.
        assert_eq!(at(plate.x - 1, plate.y), 0, "plate is tight to (n+8)*module");
        assert_eq!(at(plate.x, plate.y), 255, "quiet-zone corner is white on dark panel");
        assert_eq!(at(x0 - 1, y0 - 1), 255, "quiet-zone inner margin is white");
        assert_eq!(at(x0, y0), 0, "finder top-left dark module is black on dark panel");
    }
}
