//! The update screens: the sheet that offers an
//! update (Home's notice and Settings open it), the screen that downloads
//! it and restarts Verso to install it, the question before going back to
//! the previous version, the sheet that says why the launcher went back,
//! and "What's new" after an update.
use std::sync::Arc;

use verso_update::Version;
use verso_update::check::Offer;
use verso_update::launcher::{EXIT_GO_BACK, EXIT_INSTALL, Reason, Rollback};

use crate::geom::Rect;
use crate::gfx::HAlign;
use crate::platform::Waker;
use crate::screens::paged_text::PagedText;
use crate::screens::{Action, EventCx, Screen, release_notes};
use crate::ui::gesture::Gesture;
use crate::ui::theme::{INK, INK2, LINE, Theme};
use crate::ui::widget::DrawCx;
use crate::ui::widgets::{Button, ButtonKind, Sheet, TopBar};
use crate::update::download::{DownloadError, Step};
use crate::update::whats_new::{self, WhatsNew};
use crate::update::{self, DownloadState, Status, Updates};

/// A sheet at the bottom in portrait, or at the right in landscape, where a
/// bottom sheet tall enough for its text would cover nearly everything.
pub(crate) fn sheet(title: impl Into<String>, height: i32, screen: Rect, t: &Theme) -> Sheet {
    if screen.w > screen.h {
        Sheet::side(title, screen.w * 11 / 20, true, screen, t)
    } else {
        Sheet::open(title, height.min(screen.h - t.top_bar), screen, t)
    }
}

/// \`2026-12-01\` → \`1 December 2026\`; anything else as it is.
pub fn release_date(date: &str) -> String {
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let mut parts = date.splitn(3, '-').map(|p| p.parse::<usize>().ok());
    match (parts.next().flatten(), parts.next().flatten(), parts.next().flatten()) {
        (Some(year), Some(month @ 1..=12), Some(day @ 1..=31)) => format!("{day} {} {year}", MONTHS[month - 1]),
        _ => date.to_owned(),
    }
}

/// What the person chose on the offer sheet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OfferEvent {
    Close,
    Update,
    Later,
    Skip,
}

/// "Verso 0.3.0": the download size and date, what's new, and Update /
/// Later / Skip this version. The notes (`release_notes`) fill the room
/// between the date and the buttons; longer ones turn in pages with a pager
/// above the buttons, or by swiping. Portrait gets a bottom sheet of a fixed
/// height, or the tallest one for notes that need pages; landscape a side
/// sheet.
pub struct OfferSheet {
    sheet: Sheet,
    screen: Rect,
    pub offer: Offer,
    meta: Rect,
    notes: PagedText,
    /// Why Update is off: Verso wasn't started by a launcher that installs.
    blocked: Option<&'static str>,
    update: Button,
    later: Button,
    skip: Button,
}

impl OfferSheet {
    pub fn new(t: &Theme, screen: Rect, offer: Offer) -> Self {
        let blocked = (!update::can_install()).then_some("Start Verso normally to install updates.");
        let mut update = Button::new("Update", ButtonKind::Primary);
        update.enabled = blocked.is_none();
        let mut this = Self {
            sheet: sheet(format!("Verso {}", offer.version), t.mm(100.0), screen, t),
            screen,
            notes: release_notes::paged(&offer.notes, Rect::default(), t),
            offer,
            meta: Rect::default(),
            blocked,
            update,
            later: Button::new("Later", ButtonKind::Secondary),
            skip: Button::new("Skip this version", ButtonKind::Secondary),
        };
        this.place(t);
        this
    }

    /// Lays the sheet out at its size: the buttons along its bottom, the
    /// notes above them.
    fn place(&mut self, t: &Theme) {
        let c = self.sheet.content(t);
        let bottom = c.bottom() - t.margin - t.button_h;
        let w = (c.w - t.mm(3.0)) / 2;
        self.later.place(Rect::new(c.x, bottom, w, t.button_h), t);
        self.skip.place(Rect::new(c.right() - w, bottom, w, t.button_h), t);
        self.update.place(Rect::new(c.x, bottom - t.mm(3.0) - t.button_h, c.w, t.button_h), t);
        // Room for two lines: the meta line splits on 800x600.
        self.meta = Rect { h: t.mm(7.0), ..c };
        let notes_top = c.y + t.mm(6.0) + t.mm(12.0);
        let notes_bottom = self.update.rect().y - t.mm(if self.blocked.is_some() { 12.0 } else { 4.0 });
        self.notes.place(Rect::new(c.x, notes_top, c.w, (notes_bottom - notes_top).max(1)), t);
    }

    /// Lays the notes out. Notes that need pages get the tallest bottom
    /// sheet, for fewer of them.
    fn lay_out(&mut self, text: &mut crate::gfx::TextRenderer, t: &Theme) {
        if self.notes.laid_out() {
            return;
        }
        self.notes.lay_out(text);
        if self.screen.w <= self.screen.h && self.notes.pager.pages > 1 {
            self.sheet = sheet(self.sheet.title.clone(), self.screen.h, self.screen, t);
            self.place(t);
            self.notes.lay_out(text);
        }
    }

    /// One line, or two split at the dot where one doesn't fit (800x600).
    fn meta_line(&self, text: &mut crate::gfx::TextRenderer, t: &Theme) -> String {
        let (size, date) = (update::file_size(self.offer.download), release_date(&self.offer.released));
        let line = format!("A {size} download · released {date}");
        if text.width(&line, t.small) <= self.meta.w as f32 {
            line
        } else {
            format!("A {size} download\nReleased {date}")
        }
    }

    /// Where Update is off, the line that says why, above it.
    fn blocked_rect(&self, t: &Theme) -> Rect {
        let c = self.sheet.content(t);
        Rect::new(c.x, self.update.rect().y - t.mm(9.0), c.w, t.mm(7.0))
    }

    pub fn draw(&mut self, cx: &mut DrawCx) {
        let t = cx.theme;
        self.lay_out(cx.text, t);
        self.sheet.draw(cx);
        let meta = self.meta_line(cx.text, t);
        cx.paragraph(&meta, t.small.gray(INK2), self.meta, HAlign::Left);
        let notes = self.notes.body();
        cx.section_label("What’s new", Rect::new(notes.x, notes.y - t.mm(8.0), notes.w, t.mm(8.0)));
        self.notes.draw(cx);
        if let Some(why) = self.blocked {
            cx.paragraph(why, t.small.gray(INK2), self.blocked_rect(t), HAlign::Left);
        }
        self.update.draw(cx);
        self.later.draw(cx);
        self.skip.draw(cx);
    }

    pub fn visual_state(&self) -> ([bool; 4], (usize, usize, bool, bool, bool)) {
        let buttons =
            [self.sheet.is_pressed(), self.update.is_pressed(), self.later.is_pressed(), self.skip.is_pressed()];
        (buttons, self.notes.visual_state())
    }

    pub fn on_gesture(&mut self, g: &Gesture) -> Option<OfferEvent> {
        if self.sheet.on_gesture(g) {
            Some(OfferEvent::Close)
        } else if self.update.on_gesture(g) {
            Some(OfferEvent::Update)
        } else if self.later.on_gesture(g) {
            Some(OfferEvent::Later)
        } else if self.skip.on_gesture(g) {
            Some(OfferEvent::Skip)
        } else {
            self.notes.on_gesture(g);
            None
        }
    }

    /// Carries out Later or Skip.
    pub fn apply(&self, event: &OfferEvent, updates: &Updates) {
        match event {
            OfferEvent::Later => updates.postpone(),
            OfferEvent::Skip => updates.skip(&self.offer.version),
            OfferEvent::Close | OfferEvent::Update => {}
        }
    }
}

/// Downloads the update, then shows "Updating…" and exits for the launcher
/// to install it. Cancel stops the download; a failure says why and offers
/// to try again.
pub struct UpdateScreen {
    top: TopBar,
    updates: Arc<Updates>,
    waker: Waker,
    version: Version,
    status: Status,
    /// The download was started (it waits while a check runs).
    started: bool,
    battery: Option<crate::platform::Battery>,
    cancel: Button,
    retry: Button,
    close: Button,
    body: Rect,
    /// Exit for the launcher, or close, once this frame is on screen.
    finish: Option<Action>,
    /// The install exit was refused (a save failed): Try again asks once more.
    refused: bool,
}

impl UpdateScreen {
    pub fn new(t: &Theme, screen: Rect, updates: Arc<Updates>, waker: Waker, version: Version, cx: &EventCx) -> Self {
        let mut top = TopBar::new("Update", false);
        top.place(screen, t);
        let c = screen.split_top(t.top_bar).1.inset(t.margin);
        let w = c.w.min(t.mm(110.0));
        let body = Rect::new(c.x + (c.w - w) / 2, c.y + t.mm(8.0), w, c.h - t.mm(8.0));
        let bottom = c.bottom() - t.button_h;
        let mut cancel = Button::new("Cancel", ButtonKind::Secondary);
        cancel.place(Rect::new(body.x, bottom, body.w, t.button_h), t);
        let mut close = Button::new("Close", ButtonKind::Secondary);
        close.place(Rect::new(body.x, bottom, body.w, t.button_h), t);
        let mut retry = Button::new("Try again", ButtonKind::Primary);
        retry.place(Rect::new(body.x, bottom - t.mm(3.0) - t.button_h, body.w, t.button_h), t);
        let status = updates.status();
        let mut this = Self {
            top,
            updates,
            waker,
            version,
            status,
            started: false,
            battery: cx.battery,
            cancel,
            retry,
            close,
            body,
            finish: None,
            refused: false,
        };
        this.start();
        this
    }

    /// Starts (or restarts) the download; waits if a check is running.
    fn start(&mut self) {
        self.started = self.updates.download(&self.version, self.battery, &self.waker);
        self.status = self.updates.status();
    }

    fn failure(&self) -> Option<&DownloadError> {
        match &self.status.download {
            Some(DownloadState::Failed(e)) if self.started => Some(e),
            _ => None,
        }
    }

    fn working(&self) -> bool {
        !self.started || matches!(self.status.download, Some(DownloadState::Working(_)))
    }

    /// The heading, the line under it and how far along (0–1000), if known.
    fn progress(&self) -> (String, String, Option<u64>) {
        let version = &self.version;
        if !self.started {
            return (format!("Verso {version}"), "Waiting for the update check to finish…".into(), None);
        }
        if self.status.cancelling {
            return (format!("Verso {version}"), "Cancelling…".into(), None);
        }
        if self.refused {
            return ("Couldn't restart".into(), "Verso couldn't save your reading state, so it stayed open. The update is downloaded and installs when you try again.".into(), None);
        }
        match &self.status.download {
            Some(DownloadState::Working(Step::Preparing)) | None => {
                (format!("Verso {version}"), "Getting ready…".into(), Some(0))
            }
            Some(DownloadState::Working(Step::Downloading { done, total })) => (
                format!("Verso {version}"),
                format!("Downloading… {} of {}", megabytes_done(*done), update::file_size(*total)),
                Some(done * 1000 / (*total).max(1)),
            ),
            Some(DownloadState::Working(Step::Unpacking { .. } | Step::Finishing)) => {
                (format!("Verso {version}"), "Checking the downloaded files…".into(), Some(1000))
            }
            Some(DownloadState::Ready(_)) => ("Updating…".into(), "Verso restarts in a moment.".into(), None),
            Some(DownloadState::Failed(e)) => (e.title().into(), e.plain(), None),
        }
    }

    fn sync_buttons(&mut self) {
        self.cancel.enabled = !self.status.cancelling;
        // A newer release came out: Try again gets that one.
        if let Some(DownloadError::ReleaseChanged(newer)) = self.failure() {
            self.retry.label = format!("Update to {newer}");
        } else {
            self.retry.label = "Try again".into();
        }
    }

    fn refresh(&mut self) {
        self.status = self.updates.status();
        if !self.started && !self.updates.checking() {
            self.start();
        }
        self.sync_buttons();
        self.arm_finish();
    }

    /// Queues the exit (or close) the download's end calls for.
    fn arm_finish(&mut self) {
        match &self.status.download {
            Some(DownloadState::Ready(_)) if self.started && !self.refused => {
                self.finish = Some(Action::ExitWith(EXIT_INSTALL))
            }
            Some(DownloadState::Failed(DownloadError::Cancelled)) if self.started => self.finish = Some(Action::Pop),
            _ => {}
        }
    }
}

/// "2.1" of "6.1 MB": the part done, in megabytes.
fn megabytes_done(done: u64) -> String {
    format!("{:.1}", done as f64 / 1e6)
}

impl Screen for UpdateScreen {
    fn draw(&mut self, cx: &mut DrawCx) {
        let t = cx.theme;
        self.top.draw(cx);
        let (heading, line, progress) = self.progress();
        let b = self.body;
        cx.line(&heading, t.title, Rect::new(b.x, b.y, b.w, t.mm(10.0)), HAlign::Left);
        // The line under the heading, then the bar (kept in place while one
        // line of progress changes), then the note.
        let one_line = cx.text.measure("M", t.body, None).1.ceil() as i32;
        let height = (cx.text.measure(&line, t.body, Some(b.w as f32)).1.ceil() as i32).clamp(one_line, t.mm(30.0));
        let line_rect = Rect::new(b.x, b.y + t.mm(13.0), b.w, height);
        cx.paragraph(&line, t.body, line_rect, HAlign::Left);
        let bar = Rect::new(b.x, line_rect.bottom() + t.mm(6.0), b.w, t.mm(1.0));
        if let Some(permille) = progress {
            cx.fill(bar, LINE);
            cx.fill(Rect { w: (bar.w as i64 * permille.min(1000) as i64 / 1000) as i32, ..bar }, INK);
        }
        let failed = self.failure().is_some() || self.refused;
        let note = if failed {
            format!("Nothing was changed: Verso {} is still installed.", self.status.current)
        } else if matches!(self.status.download, Some(DownloadState::Ready(_))) {
            "Your books, settings and reading positions stay as they are.".into()
        } else {
            "Verso restarts by itself once the download is complete. Your books, settings and reading positions stay as they are.".into()
        };
        let note_y = if progress.is_some() { bar.bottom() + t.mm(6.0) } else { line_rect.bottom() + t.mm(5.0) };
        cx.paragraph(&note, t.small.gray(INK2), Rect::new(b.x, note_y, b.w, t.mm(16.0)), HAlign::Left);
        if failed {
            if !matches!(self.failure(), Some(DownloadError::UpToDate)) {
                self.retry.draw(cx);
            }
            self.close.draw(cx);
        } else if self.working() {
            self.cancel.draw(cx);
        }
    }

    fn on_gesture(&mut self, g: Gesture, cx: &mut EventCx) -> Action {
        cx.redraw();
        if self.refused {
            if self.close.on_gesture(&g) {
                return Action::Pop;
            }
            if self.retry.on_gesture(&g) {
                self.refused = false;
                self.finish = Some(Action::ExitWith(EXIT_INSTALL));
            }
        } else if self.failure().is_some() {
            if self.close.on_gesture(&g) {
                return Action::Pop;
            }
            let up_to_date = matches!(self.failure(), Some(DownloadError::UpToDate));
            if !up_to_date && self.retry.on_gesture(&g) {
                if let Some(DownloadError::ReleaseChanged(newer)) = self.failure() {
                    self.version = newer.clone();
                }
                self.battery = cx.battery;
                self.start();
                self.sync_buttons();
            }
        } else if self.working() && self.cancel.on_gesture(&g) {
            if self.started {
                self.updates.cancel_download();
                self.status = self.updates.status();
                self.sync_buttons();
            } else {
                return Action::Pop;
            }
        }
        Action::None
    }

    fn on_background(&mut self, cx: &mut EventCx) {
        if self.updates.revision() != self.status.revision || !self.started {
            self.refresh();
            cx.redraw();
        }
    }

    /// Scripted screenshots wait for the download to end.
    fn busy(&self) -> bool {
        self.working()
    }

    fn exit_refused(&mut self) {
        self.finish = None;
        self.refused = true;
    }

    fn has_pending_work(&self) -> bool {
        self.finish.is_some()
    }

    fn run_pending_work(&mut self, _cx: &mut EventCx) -> Action {
        self.finish.take().unwrap_or(Action::None)
    }
}

/// "Go back to Verso 0.2.1?" in Settings → Device.
pub struct GoBackSheet {
    sheet: Sheet,
    message: String,
    text: Rect,
    cancel: Button,
    go_back: Button,
    /// Chosen: "Going back…" is on screen, then Verso exits.
    going: bool,
}

impl GoBackSheet {
    pub fn new(t: &Theme, screen: Rect, current: &Version, previous: &Version) -> Self {
        let sheet =
            Sheet::open(format!("Go back to Verso {previous}?"), t.mm(62.0).min(screen.h - t.top_bar), screen, t);
        let c = sheet.content(t);
        let buttons_y = c.bottom() - t.margin - t.button_h;
        let w = (c.w - t.mm(3.0)) / 2;
        let mut cancel = Button::new("Cancel", ButtonKind::Secondary);
        cancel.place(Rect::new(c.x, buttons_y, w, t.button_h), t);
        let mut go_back = Button::new("Go back", ButtonKind::Primary);
        go_back.place(Rect::new(c.right() - w, buttons_y, w, t.button_h), t);
        let message = format!(
            "Verso restarts with version {previous} and won’t offer {current} again. Your books, settings and reading positions stay as they are."
        );
        Self { text: Rect { h: buttons_y - c.y - t.mm(3.0), ..c }, sheet, message, cancel, go_back, going: false }
    }

    pub fn draw(&self, cx: &mut DrawCx) {
        self.sheet.draw(cx);
        let message = if self.going { "Going back…" } else { &self.message };
        let height = cx.text.measure(message, cx.theme.body, Some(self.text.w as f32)).1.ceil() as i32;
        cx.paragraph(message, cx.theme.body, Rect { h: height.min(self.text.h), ..self.text }, HAlign::Left);
        self.cancel.draw(cx);
        self.go_back.draw(cx);
    }

    /// Some(true) to go back (shows "Going back…"), Some(false) to close.
    pub fn on_gesture(&mut self, g: &Gesture) -> Option<bool> {
        if self.going {
            return None;
        }
        if self.sheet.on_gesture(g) || self.cancel.on_gesture(g) {
            return Some(false);
        }
        if self.go_back.on_gesture(g) {
            self.going = true;
            self.cancel.enabled = false;
            self.go_back.enabled = false;
            return Some(true);
        }
        None
    }

    /// Once "Going back…" is on screen: the exit that asks the launcher.
    pub fn exit(&self) -> Option<Action> {
        self.going.then_some(Action::ExitWith(EXIT_GO_BACK))
    }
}

/// Why the launcher went back to this version: shown once, on the first
/// start after a rollback, over whatever screen is open.
pub struct RollbackSheet {
    updates: Arc<Updates>,
    sheet: Sheet,
    summary: String,
    reason: Option<String>,
    details: Vec<String>,
    text: Rect,
    ok: Button,
}

impl RollbackSheet {
    pub fn new(t: &Theme, screen: Rect, updates: Arc<Updates>, rollback: &Rollback) -> Self {
        let name =
            |v: &Option<Version>| v.as_ref().map_or_else(|| "the new version".to_owned(), |v| format!("Verso {v}"));
        let from = name(&rollback.from);
        let title = rollback
            .to
            .as_ref()
            .map_or_else(|| "Back to the previous version".to_owned(), |to| format!("Back to Verso {to}"));
        let back_to = rollback.to.as_ref().map_or_else(|| "the previous version".to_owned(), ToString::to_string);
        let again = rollback.from.as_ref().map(|v| format!(" It won’t offer {v} again.")).unwrap_or_default();
        let summary = match rollback.reason {
            Reason::Requested => format!("Verso went back to {back_to}, as you asked.{again}"),
            _ => format!("{from} didn’t start, so Verso went back to {back_to}.{again}"),
        };
        let reason = update::rollback_reason(&rollback.reason);
        let details = if reason.is_some() { update::rollback_details(&rollback.log, 8) } else { Vec::new() };
        let height = if details.is_empty() { t.mm(62.0) } else { t.mm(95.0) };
        let sheet = sheet(title, height, screen, t);
        let c = sheet.content(t);
        let mut ok = Button::new("OK", ButtonKind::Primary);
        ok.place(Rect::new(c.x, c.bottom() - t.margin - t.button_h, c.w, t.button_h), t);
        let text = Rect { h: ok.rect().y - c.y - t.mm(3.0), ..c };
        Self { updates, sheet, summary, reason, details, text, ok }
    }

    pub fn draw(&self, cx: &mut DrawCx) {
        let t = cx.theme;
        self.sheet.draw(cx);
        let mut y = self.text.y;
        let block = |cx: &mut DrawCx, text: &str, style: crate::gfx::TextStyle, y: &mut i32| {
            let h = cx.text.measure(text, style, Some(self.text.w as f32)).1.ceil() as i32;
            let h = h.min(self.text.bottom() - *y).max(0);
            cx.paragraph(text, style, Rect { y: *y, h, ..self.text }, HAlign::Left);
            *y += h + t.mm(3.0);
        };
        block(cx, &self.summary, t.body, &mut y);
        if let Some(reason) = &self.reason {
            block(cx, reason, t.body.gray(INK2), &mut y);
        }
        if !self.details.is_empty() && y + t.mm(12.0) < self.text.bottom() {
            y += t.mm(2.0);
            cx.section_label("Details", Rect { y, h: t.mm(8.0), ..self.text });
            y += t.mm(8.0);
            block(cx, &self.details.join("\n"), t.caption.gray(INK2), &mut y);
        }
        self.ok.draw(cx);
    }

    /// True when OK was tapped (or the sheet closed): it isn't shown again.
    pub fn on_gesture(&mut self, g: &Gesture) -> bool {
        if self.sheet.on_gesture(g) || self.ok.on_gesture(g) {
            self.updates.dismiss_rollback();
            return true;
        }
        false
    }
}

/// "Updated to Verso X": once, on the first start after an update, over
/// whatever screen is open (an app `Notice`), with the notes of every
/// version since the one before and "From Verso Y" where that is known.
/// Settings opens it for this version alone (`for_version`). The notes
/// fill the sheet above OK; notes that need pages get a pager and, in
/// portrait, the tallest sheet, as on the offer sheet.
pub struct WhatsNewSheet {
    sheet: Sheet,
    screen: Rect,
    /// "From Verso Y", small, above the notes.
    from: Option<String>,
    from_rect: Rect,
    notes: PagedText,
    ok: Button,
    /// After an update: told on close, so the sheet isn't shown again.
    updates: Option<Arc<Updates>>,
}

impl WhatsNewSheet {
    /// After an update; closing it remembers this version as seen.
    pub fn after_update(t: &Theme, screen: Rect, updates: Arc<Updates>, news: &WhatsNew) -> Self {
        let from = news.from.as_ref().map(|v| format!("From Verso {v}"));
        Self::new(t, screen, format!("Updated to Verso {}", news.version), from, &news.text(), Some(updates))
    }

    /// The built-in notes of one version (Settings: this version), or a
    /// line saying there are none.
    pub fn for_version(t: &Theme, screen: Rect, version: &Version) -> Self {
        let notes = whats_new::notes_for(version).unwrap_or_default();
        Self::new(t, screen, format!("What’s new in Verso {version}"), None, notes, None)
    }

    fn new(
        t: &Theme,
        screen: Rect,
        title: String,
        from: Option<String>,
        notes: &str,
        updates: Option<Arc<Updates>>,
    ) -> Self {
        let mut this = Self {
            sheet: sheet(title, t.mm(100.0), screen, t),
            screen,
            from,
            from_rect: Rect::default(),
            notes: release_notes::paged(notes, Rect::default(), t),
            ok: Button::new("OK", ButtonKind::Primary),
            updates,
        };
        this.place(t);
        this
    }

    /// OK along the bottom, "From" at the top, the notes between.
    fn place(&mut self, t: &Theme) {
        let c = self.sheet.content(t);
        self.ok.place(Rect::new(c.x, c.bottom() - t.margin - t.button_h, c.w, t.button_h), t);
        self.from_rect = Rect { h: if self.from.is_some() { t.mm(7.0) } else { 0 }, ..c };
        let top = self.from_rect.bottom() + if self.from.is_some() { t.mm(3.0) } else { 0 };
        let bottom = self.ok.rect().y - t.mm(4.0);
        self.notes.place(Rect::new(c.x, top, c.w, (bottom - top).max(1)), t);
    }

    /// Lays the notes out; notes that need pages get the tallest bottom sheet.
    fn lay_out(&mut self, text: &mut crate::gfx::TextRenderer, t: &Theme) {
        if self.notes.laid_out() {
            return;
        }
        self.notes.lay_out(text);
        if self.screen.w <= self.screen.h && self.notes.pager.pages > 1 {
            self.sheet = sheet(self.sheet.title.clone(), self.screen.h, self.screen, t);
            self.place(t);
            self.notes.lay_out(text);
        }
    }

    pub fn draw(&mut self, cx: &mut DrawCx) {
        let t = cx.theme;
        self.lay_out(cx.text, t);
        self.sheet.draw(cx);
        if let Some(from) = &self.from {
            cx.paragraph(from, t.small.gray(INK2), self.from_rect, HAlign::Left);
        }
        self.notes.draw(cx);
        self.ok.draw(cx);
    }

    /// True when OK was tapped (or the sheet closed). Swipes turn the pages.
    pub fn on_gesture(&mut self, g: &Gesture) -> bool {
        if self.sheet.on_gesture(g) || self.ok.on_gesture(g) {
            if let Some(updates) = &self.updates {
                updates.saw_whats_new();
            }
            return true;
        }
        self.notes.on_gesture(g);
        false
    }
}

#[cfg(test)]
fn sample_offer(notes: &str) -> Offer {
    Offer {
        version: "10.12.3".parse().unwrap(),
        released: "2026-12-01".into(),
        notes: notes.into(),
        components: vec!["app".into()],
        download: 12_300_000,
        unpacked: 15_000_000,
    }
}

/// Release notes too long for one page of the offer sheet on any screen.
#[cfg(test)]
fn long_notes() -> String {
    let item = "- A change that takes a line or two to describe on a small screen.\n";
    format!("The tenth beta.\n\n## Reading\n{}\n## Library\n{}", item.repeat(12), item.repeat(12))
}

/// The offer (with one page of notes, and with several) and go-back
/// sheets, for `screens/fit_tests.rs`.
#[cfg(test)]
pub(super) fn fit_samples(t: &Theme, screen: Rect, text: &mut crate::gfx::TextRenderer) -> Vec<crate::ui::fit::Parts> {
    use crate::ui::fit::{Parts, text_h};
    let mut samples = Vec::new();
    for (name, notes) in
        [("OfferSheet", "Faster page turns.\n\n- Dictionary fixes.".into()), ("OfferSheet pages", long_notes())]
    {
        let mut o = OfferSheet::new(t, screen, sample_offer(&notes));
        o.lay_out(text, t);
        let mut buttons = vec![o.update.rect(), o.later.rect(), o.skip.rect()];
        if o.notes.pager.visible() {
            buttons.push(o.notes.pager.rect());
        }
        let needed = o.notes.one_page_height().unwrap_or(t.body.line_height().ceil() as i32);
        let mut sheet = Parts::new(name, o.sheet.rect(), buttons).body(o.notes.body(), needed);
        let meta = o.meta_line(text, t);
        sheet = sheet.body(o.meta, text_h(text, &meta, t.small, o.meta.w));
        if let Some(why) = o.blocked {
            let r = o.blocked_rect(t);
            sheet = sheet.body(r, text_h(text, why, t.small, r.w));
        }
        samples.push(sheet);
    }
    let several = |n: usize| WhatsNew {
        version: "10.12.3".parse().unwrap(),
        from: Some("10.11.0".parse().unwrap()),
        notes: vec![("10.12.3".parse().unwrap(), "Faster page turns.\n\n- Dictionary fixes.\n"); n],
    };
    let updates = Arc::new(Updates::new(update::Paths { root: "/nonexistent".into() }, update::Source::configured()));
    for (name, news) in [("WhatsNewSheet", several(1)), ("WhatsNewSheet pages", several(5))] {
        let mut w = WhatsNewSheet::after_update(t, screen, updates.clone(), &news);
        w.lay_out(text, t);
        let mut buttons = vec![w.ok.rect()];
        if w.notes.pager.visible() {
            buttons.push(w.notes.pager.rect());
        }
        let needed = w.notes.one_page_height().unwrap_or(t.body.line_height().ceil() as i32);
        let from = w.from.clone().unwrap_or_default();
        samples.push(
            Parts::new(name, w.sheet.rect(), buttons)
                .body(w.notes.body(), needed)
                .body(w.from_rect, text_h(text, &from, t.small, w.from_rect.w)),
        );
    }
    let back = GoBackSheet::new(t, screen, &"10.12.3".parse().unwrap(), &"10.11.0".parse().unwrap());
    samples.push(
        Parts::new("GoBackSheet", back.sheet.rect(), [back.cancel.rect(), back.go_back.rect()])
            .body(back.text, text_h(text, &back.message, t.body, back.text.w)),
    );
    samples
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_dates_read_as_words() {
        assert_eq!(release_date("2026-12-01"), "1 December 2026");
        assert_eq!(release_date("2027-01-31"), "31 January 2027");
        assert_eq!(release_date("soon"), "soon");
        assert_eq!(release_date("2026-13-01"), "2026-13-01");
    }

    #[test]
    fn long_notes_turn_in_pages_and_short_ones_keep_the_room() {
        use crate::gfx::{Canvas, IconRenderer, TextRenderer};
        use crate::ui::gesture::Direction;
        let (mut text, mut icons) = (TextRenderer::new(), IconRenderer::default());
        for (w, h, dpi) in [(1272, 1696, 300), (800, 600, 167)] {
            let (t, screen) = (Theme::for_dpi(dpi), Rect::new(0, 0, w, h));
            let mut canvas = Canvas::new(w as u32, h as u32);
            let mut draw =
                |o: &mut OfferSheet| o.draw(&mut DrawCx::new(&mut canvas, &mut text, &mut icons, &t, &mut vec![]));
            let mut short = OfferSheet::new(&t, screen, sample_offer("Faster page turns."));
            let before = short.notes.body();
            draw(&mut short);
            assert!(!short.notes.pager.visible(), "{w}x{h}");
            assert!(short.notes.body().h > before.h, "{w}x{h}: one page takes the pager's room too");
            let mut long = OfferSheet::new(&t, screen, sample_offer(&long_notes()));
            draw(&mut long);
            assert!(long.notes.pager.visible() && long.notes.pager.pages > 1, "{w}x{h}");
            let (body, pager) = (long.notes.body(), long.notes.pager.rect());
            assert!(body.bottom() < pager.y && pager.bottom() < long.update.rect().y, "{w}x{h}: pager between");
            let at = body.center();
            assert_eq!(long.on_gesture(&Gesture::Swipe { dir: Direction::Left, from: at, to: at }), None);
            assert_eq!(long.notes.pager.page, 1, "{w}x{h}: a swipe turns the page");
            draw(&mut long);
        }
    }

    #[test]
    fn a_refused_install_exit_offers_try_again_and_close() {
        use crate::geom::Point;
        use crate::update::Source;
        use std::time::Instant;

        let root = tempfile::tempdir().unwrap();
        let updates = Arc::new(Updates::new(update::Paths { root: root.path().to_owned() }, Source::configured()));
        let t = Theme::for_dpi(300);
        let screen = Rect::new(0, 0, 1272, 1696);
        let version: Version = "0.3.0".parse().unwrap();
        let mut cx = EventCx {
            theme: &t,
            screen,
            now: Instant::now(),
            toast: &mut None,
            forced: &mut vec![],
            needs_redraw: &mut false,
            battery: None,
        };
        let mut s = UpdateScreen::new(&t, screen, updates, Waker::new(|| {}), version.clone(), &cx);
        let tap = |s: &mut UpdateScreen, r: Rect, cx: &mut EventCx| {
            let p = Point::new(r.x + r.w / 2, r.y + r.h / 2);
            s.on_gesture(Gesture::Down(p), cx);
            s.on_gesture(Gesture::Tap(p), cx)
        };

        // The download finished: the screen asks to exit, once.
        (s.started, s.status.download) = (true, Some(DownloadState::Ready(version)));
        s.arm_finish();
        assert!(matches!(s.run_pending_work(&mut cx), Action::ExitWith(EXIT_INSTALL)));
        assert!(!s.has_pending_work());

        // The exit is refused: nothing re-asks, and the way out is on screen.
        s.exit_refused();
        s.arm_finish();
        assert!(!s.has_pending_work());
        assert!(s.progress().0.contains("restart"));

        // Try again asks for the exit once more.
        let retry = s.retry.rect();
        tap(&mut s, retry, &mut cx);
        assert!(matches!(s.run_pending_work(&mut cx), Action::ExitWith(EXIT_INSTALL)));

        // Refused again: Close leaves the screen.
        s.exit_refused();
        let close = s.close.rect();
        assert!(matches!(tap(&mut s, close, &mut cx), Action::Pop));
    }
}
