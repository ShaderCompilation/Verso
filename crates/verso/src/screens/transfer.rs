//! Wi-Fi transfer (docs/web-app.md, "Kindle screen"): where to open the web
//! app, its code, and what has arrived. The server runs only while this
//! screen is open: Done, sleep, an exit (for USB too) or 10 minutes without
//! a request stop it.

use std::sync::Arc;
use std::time::Instant;

use crate::geom::Rect;
use crate::gfx::{Family, HAlign, Icon, TextStyle, Weight};
use crate::platform::Waker;
use crate::screens::{Action, EventCx, Screen};
use crate::store::Store;
use crate::transfer::{Activity, Device, Failure, Phase, Session, Snapshot, Stop};
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK, INK2, LINE, Theme};
use crate::ui::widget::DrawCx;
use crate::ui::widgets::{Button, ButtonKind, TopBar};

pub struct TransferScreen {
    top: TopBar,
    session: Session,
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
    /// Done was tapped: "Stopping…" is on screen, then the session closes.
    closing: bool,
    /// The QR code and the link it holds.
    qr: Option<(String, qrcode::QrCode)>,
}

impl TransferScreen {
    pub fn new(t: &Theme, screen: Rect, store: Store, waker: Waker) -> Self {
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
        let device = Device::current(store.root());
        let forced = device.forced.is_some();
        let session = Session::start(Arc::new(device), waker, settings.hostname, settings.developer);
        let snapshot = session.snapshot();
        let revision = session.revision();
        let mut this =
            Self { top, session, snapshot, revision, forced, store, info, list, done, again, closing: false, qr: None };
        this.sync();
        this
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
        let mut settings = self.store.settings();
        if &settings.hostname == name {
            return;
        }
        settings.hostname = name.clone();
        if let Err(e) = self.store.save_settings(&settings) {
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
fn draw_qr(cx: &mut DrawCx, rect: Rect, qr: &qrcode::QrCode) {
    let n = qr.width() as i32;
    // The page around it is blank, so one module of margin is enough here.
    let module = (rect.w.min(rect.h) / (n + 2)).max(1);
    let x0 = rect.x + (rect.w - module * n) / 2;
    let y0 = rect.y + (rect.h - module * n) / 2;
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
            cx.fill(Rect::new(x0 + start * module, y0 + y * module, (x - start) * module, module), INK);
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
    }

    fn on_gesture(&mut self, g: Gesture, cx: &mut EventCx) -> Action {
        if self.closing {
            return Action::None;
        }
        cx.redraw();
        if self.top.on_gesture(&g) || self.done.on_gesture(&g) {
            self.closing = true;
            self.done.label = "Stopping…".into();
            self.done.enabled = false;
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
            self.session.stop(Stop::Idle);
            self.refresh();
            cx.redraw();
        }
        Action::None
    }

    fn on_sleep(&mut self) {
        self.session.stop(Stop::Slept);
        self.refresh();
    }

    fn on_wake(&mut self, _now: Instant) {
        self.refresh();
    }

    /// Exiting (also to connect USB) stops the server and puts Wi-Fi back.
    fn on_quit(&mut self) -> anyhow::Result<()> {
        self.session.close();
        Ok(())
    }

    /// Scripted screenshots wait for the start.
    fn busy(&self) -> bool {
        self.snapshot.phase == Phase::Starting && !self.forced
    }

    fn has_pending_work(&self) -> bool {
        self.closing
    }

    /// "Stopping…" is on screen: stop, then leave.
    fn run_pending_work(&mut self, _cx: &mut EventCx) -> Action {
        self.session.close();
        Action::Pop
    }
}
