//! The app: owns the canvas and the screen stack, turns gestures and timers
//! into redraws, and works out which parts of the display to refresh.
//!
//! App-wide overlays draw in fixed z-order over the active screen: `TopDrawer`,
//! queued startup `Notice`s (rollback, what's new, untested model), `UsbPrompt`,
//! `ExitPrompt` and `Toast`. `fast_debt` queues areas last refreshed with
//! `RefreshMode::Fast` (1-bit DU) for an automatic `Ui` pass once the finger
//! lifts. Live orientation changes save screens via `prepare_orientation`, drop
//! the old stack so workers stop cleanly, and rebuild from `RelayoutState`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::geom::Rect;
use crate::gfx::{Canvas, IconRenderer, TextRenderer};
use crate::platform::{
    Battery, DarkRefresh, DeviceInfo, Frontlight, KeyAction, LightRange, Orientation, RefreshMode, TextInput, Update,
    Waker,
};
use crate::screens::system::{ExitChoice, ExitPrompt};
use crate::screens::top_drawer::{self, DrawerEvent, DrawerWorker, NetOps, TopDrawer};
use crate::screens::untested::UntestedSheet;
use crate::screens::update::{RollbackSheet, WhatsNewSheet};
use crate::screens::{Action, EventCx, RelayoutState, Screen, TopDrawerBookAction};
use crate::store::Store;
use crate::ui::damage::{self, FrameDiff};
use crate::ui::gesture::{Direction, Gesture};
use crate::ui::theme::{PAPER, Theme};
use crate::ui::widget::DrawCx;
use crate::ui::widgets::Toast;

const DARK_SWITCH_WAIT: Duration = Duration::from_millis(500);

/// What the Top Drawer needs from the rest of Verso: settings, screens to
/// open, and a way to wake the UI when Wi-Fi or SSH finishes switching.
pub struct Services {
    pub store: Store,
    pub waker: Waker,
    pub net: Arc<dyn NetOps>,
}

impl Services {
    pub fn device(store: Store, waker: Waker) -> Self {
        Self { store, waker, net: Arc::new(top_drawer::DeviceNet) }
    }
}

/// A sheet shown once over whatever screen is open, until OK.
pub enum Notice {
    /// Why the launcher went back to this version.
    Rollback(RollbackSheet),
    /// What changed since the version before an update.
    WhatsNew(Box<WhatsNewSheet>),
    /// This Kindle model hasn't been tested yet.
    Untested(UntestedSheet),
}

impl Notice {
    fn draw(&mut self, cx: &mut DrawCx) {
        match self {
            Self::Rollback(sheet) => sheet.draw(cx),
            Self::WhatsNew(sheet) => sheet.draw(cx),
            Self::Untested(sheet) => sheet.draw(cx),
        }
    }

    /// True when dismissed.
    fn on_gesture(&mut self, g: &Gesture) -> bool {
        match self {
            Self::Rollback(sheet) => sheet.on_gesture(g),
            Self::WhatsNew(sheet) => sheet.on_gesture(g),
            Self::Untested(sheet) => sheet.on_gesture(g),
        }
    }
}

pub struct App {
    canvas: Canvas,
    text: TextRenderer,
    icons: IconRenderer,
    theme: Theme,
    diff: FrameDiff,
    stack: Vec<Box<dyn Screen>>,
    toast: Option<Toast>,
    /// Where the toast was drawn last.
    toast_rect: Rect,
    /// The current touch started on the toast.
    toast_touch: bool,
    /// Refreshes asked for regardless of what changed.
    forced: Vec<Update>,
    /// Areas last refreshed with `Fast` (black and white only). They get a
    /// clean `Ui` refresh once the finger lifts, even if nothing changed.
    fast_debt: Vec<Rect>,
    touch_down: bool,
    needs_redraw: bool,
    asleep: bool,
    exit: bool,
    exit_prompt: Option<ExitPrompt>,
    /// The exit code a screen asked for (`Action::ExitWith`).
    exit_code: Option<i32>,
    /// Startup notices, one at a time, over any screen.
    notices: std::collections::VecDeque<Notice>,
    stop_ssh_on_exit: bool,
    pub frontlight: Option<Frontlight>,
    light_request: Option<Frontlight>,
    /// The light sliders' top step on this model.
    light_range: LightRange,
    pub battery: Option<Battery>,
    usb_prompt: Option<Box<dyn Screen>>,
    usb_connected: bool,
    wake_cleanup: Option<Instant>,
    dark_pending: Option<(bool, Instant)>,
    dark_refresh_request: Option<DarkRefresh>,
    orientation: Orientation,
    /// A screen asked to turn the display; see `prepare_orientation`.
    orientation_request: Option<Orientation>,
    services: Option<Services>,
    /// The quick-settings drawer while it's open, over the top screen and
    /// under the prompts and toasts.
    drawer: Option<TopDrawer>,
    /// Switches Wi-Fi and SSH for the drawer; made when it first opens and
    /// kept, so a switch finishes (and its result is kept) after it closes.
    net: Option<DrawerWorker>,
    /// `NetState::finished` when the claimed SSH name was last checked.
    net_seen: u64,
}

impl App {
    pub fn new(info: &DeviceInfo, theme: Theme, screen: Box<dyn Screen>) -> Self {
        let canvas = Canvas::with_mode(info.width, info.height, info.color);
        Self {
            diff: FrameDiff::new(&canvas),
            canvas,
            text: TextRenderer::new(),
            icons: IconRenderer::default(),
            theme,
            stack: vec![screen],
            toast: None,
            toast_rect: Rect::default(),
            toast_touch: false,
            forced: Vec::new(),
            fast_debt: Vec::new(),
            touch_down: false,
            needs_redraw: true,
            asleep: false,
            exit: false,
            exit_prompt: None,
            exit_code: None,
            notices: Default::default(),
            stop_ssh_on_exit: false,
            frontlight: None,
            light_request: None,
            light_range: LightRange::default(),
            battery: None,
            usb_prompt: None,
            usb_connected: false,
            wake_cleanup: None,
            dark_pending: None,
            dark_refresh_request: None,
            orientation: Orientation::Portrait,
            orientation_request: None,
            services: None,
            drawer: None,
            net: None,
            net_seen: 0,
        }
    }

    /// Gives the app what the Top Drawer needs. Without it the drawer stays closed.
    pub fn set_services(&mut self, services: Services) {
        self.services = Some(services);
    }

    /// The orientation the canvas is drawn in (set at startup or by `relayout`).
    pub fn set_orientation(&mut self, orientation: Orientation) {
        self.orientation = orientation;
    }

    /// Sets whether the canvas copies out inverted for dark mode.
    pub fn set_dark(&mut self, dark: bool) {
        self.dark_pending = None;
        self.canvas.set_dark(dark);
    }

    fn dark_mode(&self) -> bool {
        self.dark_pending.map_or(self.canvas.is_dark(), |(dark, _)| dark)
    }

    fn commit_dark_switch(&mut self, dark: bool) {
        self.dark_pending = None;
        self.canvas.set_dark(dark);
        self.refresh_all(RefreshMode::Full);
    }

    fn finish_dark_wait_if_ready(&mut self) {
        if let Some((dark, _)) = self.dark_pending
            && !self.stack.last().is_some_and(|s| s.waiting_for_dark())
        {
            self.commit_dark_switch(dark);
        }
    }

    /// Saves the screens before handing the pending orientation to the
    /// platform, with what they showed so `relayout` can rebuild the same.
    /// A failed save leaves the request pending so a later event can retry it.
    pub fn prepare_orientation(&mut self) -> Option<(Orientation, RelayoutState)> {
        let orientation = self.orientation_request?;
        let state = self.relayout_state();
        if !self.prepare_relayout() {
            return None;
        }
        self.orientation_request = None;
        Some((orientation, state))
    }

    /// What the screens show, bottom to top.
    pub fn relayout_state(&self) -> RelayoutState {
        let mut state = RelayoutState::default();
        for s in &self.stack {
            s.relayout_state(&mut state);
        }
        state
    }

    /// Saves every screen like a normal exit, before a relayout. On failure
    /// the error is shown and the screens stay as they are.
    fn prepare_relayout(&mut self) -> bool {
        if let Some((dark, _)) = self.dark_pending.take() {
            self.commit_dark_switch(dark);
        }
        self.on_gesture(Gesture::Cancel(crate::geom::Point::default()));
        self.close_drawer();
        for s in &mut self.stack {
            if let Err(e) = s.on_quit() {
                log::error!("before turning the screen: {e:#}");
                self.toast = Some(Toast::new(e.to_string(), None, Instant::now()));
                self.needs_redraw = true;
                return false;
            }
        }
        true
    }

    /// Replaces the screens with ones built for a new screen size (after
    /// `prepare_orientation`, whose `RelayoutState` says what to rebuild).
    /// The old screens are dropped first, so their workers stop before the
    /// new ones start.
    pub fn relayout(
        &mut self,
        info: &DeviceInfo,
        orientation: Orientation,
        build: impl FnOnce(&Theme, Rect) -> Vec<Box<dyn Screen>>,
    ) {
        self.stack.clear();
        self.drawer = None;
        self.orientation = orientation;
        let dark = self.dark_mode();
        self.dark_pending = None;
        self.canvas = Canvas::with_mode(info.width, info.height, info.color);
        self.canvas.set_dark(dark);
        self.diff = FrameDiff::new(&self.canvas);
        self.stack = build(&self.theme, self.canvas.bounds());
        assert!(!self.stack.is_empty(), "relayout needs at least one screen");
        if self.usb_prompt.is_some() {
            self.usb_prompt = Some(Box::new(crate::screens::system::UsbPrompt::new(&self.theme, self.canvas.bounds())));
        }
        self.toast_touch = false;
        self.touch_down = false;
        self.refresh_all(RefreshMode::Full);
    }

    /// What the last frame showed on top, for the log.
    pub fn screen_name(&self) -> &'static str {
        if self.exit_prompt.is_some() {
            "ExitPrompt"
        } else if let Some(prompt) = &self.usb_prompt {
            prompt.name()
        } else if let Some(notice) = self.notices.front() {
            match notice {
                Notice::Rollback(_) => "RollbackNotice",
                Notice::WhatsNew(_) => "WhatsNewNotice",
                Notice::Untested(_) => "UntestedNotice",
            }
        } else if self.drawer.is_some() {
            "TopDrawer"
        } else {
            self.stack.last().map_or("", |s| s.name())
        }
    }

    pub fn canvas(&self) -> &Canvas {
        &self.canvas
    }

    pub fn wants_exit(&self) -> bool {
        self.exit
    }

    /// The process exit code: 0, or what a screen asked the launcher for.
    pub fn exit_code(&self) -> i32 {
        self.exit_code.unwrap_or(0)
    }

    /// Opens a screen over the current one (first-time setup at startup).
    pub fn push_screen(&mut self, screen: Box<dyn Screen>) {
        self.apply(Action::Push(screen));
    }

    /// Shows why the launcher went back to this version, over whatever
    /// screen is open, until OK.
    pub fn show_rollback(&mut self, sheet: RollbackSheet) {
        self.show_notice(Notice::Rollback(sheet));
    }

    /// Queues a sheet shown once over whatever screen is open, after the
    /// ones already queued.
    pub fn show_notice(&mut self, notice: Notice) {
        self.notices.push_back(notice);
        self.needs_redraw = true;
    }

    /// The model's light range, for the Top Drawer's sliders.
    pub fn set_light_range(&mut self, range: LightRange) {
        self.light_range = range;
    }

    /// User-requested exits ask what to do with a running SSH server. System
    /// shutdown/recovery and headless capture completion bypass this prompt.
    pub fn request_exit(&mut self) {
        if self.exit_prompt.is_some() {
            return;
        }
        if crate::platform::ssh::status().running {
            self.show_exit_prompt();
        } else {
            self.exit = true;
        }
    }

    fn show_exit_prompt(&mut self) {
        self.on_gesture(Gesture::Cancel(crate::geom::Point::default()));
        self.toast = None;
        self.toast_touch = false;
        self.exit_prompt = Some(ExitPrompt::new(&self.theme, self.canvas.bounds()));
        self.needs_redraw = true;
    }

    /// Refreshes the whole display in the next frame (first frame, wake).
    pub fn refresh_all(&mut self, mode: RefreshMode) {
        if mode == RefreshMode::Full {
            // A full refresh also satisfies queued layout refreshes and press cleanup.
            self.forced.clear();
            self.fast_debt.clear();
        }
        self.forced.push(Update { rect: self.canvas.bounds(), mode });
        self.needs_redraw = true;
    }

    /// Draws a new frame if anything changed, and returns the display updates.
    pub fn frame(&mut self) -> Vec<Update> {
        if self.asleep || self.dark_pending.is_some() || !self.needs_redraw {
            return Vec::new();
        }
        self.needs_redraw = false;

        let mut forced = std::mem::take(&mut self.forced);
        if !self.touch_down {
            forced.extend(self.fast_debt.drain(..).map(|rect| Update { rect, mode: RefreshMode::Ui }));
        }
        let bounds = self.canvas.bounds();
        // Nothing draws over a kept canvas, so diffing it would find nothing:
        // only forced refreshes (a pressed cover's cleanup) go out.
        if self.stack.last().is_some_and(|s| s.keeps_canvas())
            && self.drawer.is_none()
            && self.notices.is_empty()
            && self.usb_prompt.is_none()
            && self.exit_prompt.is_none()
            && self.toast.is_none()
        {
            let updates = damage::plan(&damage::Tiles::clean(bounds), bounds, &[], RefreshMode::Ui, &forced);
            self.remember_fast_debt(&updates);
            log::trace!("frame: canvas kept, {} update(s)", updates.len());
            return updates;
        }
        // Check before drawing whether forced refreshes already guarantee a
        // full-screen bypass. DrawCx::hint only emits Fast or Ui, so any
        // full-screen forced refresh >= Ui with no stronger local forced rect
        // skips diffing and does not need diff.prev materialized beforehand.
        let bypass_mode = forced
            .iter()
            .filter(|u| u.rect == bounds)
            .map(|u| u.mode)
            .max()
            .filter(|&mode| mode >= RefreshMode::Ui && forced.iter().all(|u| u.mode <= mode));
        let sync_time = if bypass_mode.is_none() {
            let sync_started = Instant::now();
            self.diff.sync_baseline(&self.canvas);
            sync_started.elapsed()
        } else {
            Duration::ZERO
        };

        let draw_started = Instant::now();
        if !self.stack.last().is_some_and(|s| s.fills_canvas()) {
            self.canvas.fill(PAPER);
        }
        let mut hints = Vec::new();
        {
            let mut cx = DrawCx::new(&mut self.canvas, &mut self.text, &mut self.icons, &self.theme, &mut hints);
            cx.battery = self.battery;
            if let Some(screen) = self.stack.last_mut() {
                screen.draw(&mut cx);
            }
            if let Some(drawer) = &mut self.drawer {
                drawer.draw(&mut cx);
            }
            if let Some(notice) = self.notices.front_mut() {
                notice.draw(&mut cx);
            }
            if let Some(prompt) = &mut self.usb_prompt {
                prompt.draw(&mut cx);
            }
            if let Some(prompt) = &self.exit_prompt {
                prompt.draw(&mut cx);
            }
            if let Some(toast) = &self.toast {
                self.toast_rect = toast.draw(&mut cx);
            }
        }
        let drawn = draw_started.elapsed();

        let diff_started = Instant::now();
        // A known full-screen refresh already covers all changed pixels. Keep
        // its strongest mode (including wake/periodic flashes) and defer the
        // baseline copy until a partial frame actually needs to diff.
        let updates = if let Some(mode) = bypass_mode {
            let strongest = hints.iter().map(|u| u.mode).max().unwrap_or(mode).max(mode);
            self.diff.mark_canvas_as_baseline();
            vec![Update { rect: bounds, mode: strongest }]
        } else {
            // Pixels covered by a forced flash (Clean or Full) will be sent to the panel
            // with the strongest mode anyway: tile diffing neither reads them
            // (most of a grid-only library frame's diff time) nor emits a
            // redundant Ui update along unaligned edges of the forced rect.
            let flashed: Vec<Rect> = forced.iter().filter(|u| u.mode >= RefreshMode::Clean).map(|u| u.rect).collect();
            let mut tiles = self.diff.changed_outside(&self.canvas, &flashed);
            self.diff.binarize_fast(&mut self.canvas, &mut tiles, &hints, RefreshMode::Ui);
            let updates = damage::plan(&tiles, bounds, &hints, RefreshMode::Ui, &forced);
            self.diff.commit(&self.canvas, &tiles);
            updates
        };
        let diffed = sync_time + diff_started.elapsed();
        self.remember_fast_debt(&updates);
        log::trace!("frame: drawn in {drawn:?}, diffed in {diffed:?}, {} update(s)", updates.len());
        updates
    }

    /// `Fast` updates leave debt that gets a `Ui` pass once the finger lifts.
    fn remember_fast_debt(&mut self, updates: &[Update]) {
        for u in updates.iter().filter(|u| u.mode == RefreshMode::Fast) {
            if !self.fast_debt.iter().any(|d| d.union(&u.rect) == *d) {
                self.fast_debt.push(u.rect);
            }
        }
    }

    pub fn begin_batch(&mut self) {
        if !self.asleep
            && let Some(screen) = self.stack.last_mut()
        {
            screen.begin_batch();
        }
    }

    pub fn finish_batch(&mut self) {
        if self.asleep {
            return;
        }
        self.with_screen(|screen, cx| {
            screen.finish_batch(cx);
            Action::None
        });
    }

    pub fn on_gesture(&mut self, g: Gesture) {
        if self.asleep {
            return;
        }
        if matches!(g, Gesture::Down(_)) {
            self.touch_down = true;
            self.toast_touch = self.toast.is_some() && self.toast_rect.contains(g.pos());
        } else if g.ends_touch() {
            self.touch_down = false;
            // Fast pressed/drag feedback needs a grayscale cleanup even when
            // the release itself changes no pixels.
            self.needs_redraw |= !self.fast_debt.is_empty();
        }
        if let Some(mut prompt) = self.exit_prompt.take() {
            let choice = prompt.on_gesture(&g);
            self.needs_redraw = true;
            match choice {
                Some(ExitChoice::Cancel) => {}
                Some(ExitChoice::KeepSsh) => self.exit = true,
                Some(ExitChoice::StopSsh) => {
                    self.exit = true;
                    self.stop_ssh_on_exit = true;
                    prompt.set_stopping(true);
                    self.exit_prompt = Some(prompt);
                }
                None => self.exit_prompt = Some(prompt),
            }
            return;
        }
        if let Some(mut prompt) = self.usb_prompt.take() {
            let mut action = Action::None;
            self.with_screen(|_, cx| {
                action = prompt.on_gesture(g, cx);
                Action::None
            });
            match action {
                Action::Exit => self.request_exit(),
                Action::Pop => self.needs_redraw = true,
                _ => self.usb_prompt = Some(prompt),
            }
            return;
        }
        if let Some(notice) = self.notices.front_mut() {
            if g.ends_touch() || matches!(g, Gesture::Down(_)) {
                self.needs_redraw = true;
            }
            if notice.on_gesture(&g) {
                self.notices.pop_front();
                self.on_background();
            }
            return;
        }
        if self.toast_touch {
            // Touches on the toast only dismiss it.
            if g.ends_touch() {
                self.toast_touch = false;
                if matches!(g, Gesture::Tap(_)) {
                    self.toast = None;
                    self.needs_redraw = true;
                }
            }
            return;
        }
        if self.drawer.is_some() {
            self.drawer_gesture(g);
            return;
        }
        if let Gesture::Swipe { dir: Direction::Down, from, .. } = g
            && from.y < self.theme.top_bar.max(self.canvas.bounds().h / 10)
            && self.stack.last().is_some_and(|s| s.allows_top_drawer())
        {
            // The screen saw this touch start: end it there without acting on it.
            self.with_screen(|screen, cx| screen.on_gesture(Gesture::Cancel(g.pos()), cx));
            self.open_drawer();
            return;
        }
        self.with_screen(|screen, cx| screen.on_gesture(g, cx));
    }

    /// Opens the Top Drawer over the top screen, unless a prompt is up.
    fn open_drawer(&mut self) {
        if self.drawer.is_some()
            || self.asleep
            || self.exit_prompt.is_some()
            || self.usb_prompt.is_some()
            || !self.notices.is_empty()
        {
            return;
        }
        let Some(services) = &self.services else {
            log::warn!("Top Drawer: no services");
            return;
        };
        let net = self.net.get_or_insert_with(|| DrawerWorker::new(services.net.clone(), services.waker.clone()));
        // SSH is read here (no subprocess); Wi-Fi in the background.
        net.read_ssh();
        net.probe();
        let net_state = net.state();
        let book = self.stack.last().and_then(|s| s.top_drawer_book());
        if let Some(screen) = self.stack.last_mut() {
            screen.top_drawer_opened();
        }
        let now = Instant::now();
        self.drawer = Some(TopDrawer::new(
            &self.theme,
            self.canvas.bounds(),
            self.orientation,
            self.dark_mode(),
            self.frontlight,
            self.light_range,
            net_state,
            book,
            now,
        ));
        self.needs_redraw = true;
    }

    fn close_drawer(&mut self) {
        if self.drawer.take().is_some() {
            self.needs_redraw = true;
        }
    }

    fn drawer_gesture(&mut self, g: Gesture) {
        let Some(drawer) = &mut self.drawer else { return };
        let before = drawer.visual_state();
        let event = drawer.on_gesture(&g);
        if event.is_some() || drawer.visual_state() != before {
            self.needs_redraw = true;
        }
        let Some(event) = event else { return };
        let bounds = self.canvas.bounds();
        match event {
            DrawerEvent::Close => self.close_drawer(),
            DrawerEvent::CloseBook => {
                self.close_drawer();
                self.with_screen(|screen, cx| screen.on_top_drawer_book(TopDrawerBookAction::CloseBook, cx));
            }
            DrawerEvent::Search => {
                self.close_drawer();
                self.with_screen(|screen, cx| screen.on_top_drawer_book(TopDrawerBookAction::Search, cx));
            }
            DrawerEvent::Light(light) => {
                self.frontlight = Some(light);
                self.light_request = Some(light);
            }
            DrawerEvent::Wifi(on) => {
                if let Some(net) = &mut self.net {
                    net.set_wifi(on);
                }
                self.sync_drawer();
            }
            DrawerEvent::Probe => {
                if let Some(net) = &mut self.net {
                    net.probe();
                }
                self.sync_drawer();
            }
            DrawerEvent::Ssh(on) => {
                if let (Some(net), Some(services)) = (&mut self.net, &self.services) {
                    net.set_ssh(on, services.store.settings().hostname);
                }
                self.sync_drawer();
            }
            DrawerEvent::DarkMode(dark) => {
                self.close_drawer();
                let Some(services) = &self.services else { return };
                if let Err(e) = services.store.update_settings(|s| s.dark_mode = dark) {
                    log::error!("saving dark mode: {e:#}");
                    self.toast = Some(Toast::new("Couldn't save dark mode", None, Instant::now()));
                    self.needs_redraw = true;
                    return;
                }
                let mut cx = EventCx {
                    theme: &self.theme,
                    screen: bounds,
                    now: Instant::now(),
                    battery: self.battery,
                    toast: &mut self.toast,
                    forced: &mut self.forced,
                    needs_redraw: &mut self.needs_redraw,
                    dark,
                };
                for screen in &mut self.stack {
                    screen.set_dark(dark, &mut cx);
                }
                if self.stack.last().is_some_and(|s| s.waiting_for_dark()) {
                    self.dark_pending = Some((dark, Instant::now() + DARK_SWITCH_WAIT));
                } else {
                    self.commit_dark_switch(dark);
                }
            }
            DrawerEvent::Orientation(orientation) => {
                self.close_drawer();
                let Some(services) = &self.services else { return };
                let mut settings = services.store.settings();
                settings.orientation = orientation;
                match services.store.save_settings(&settings) {
                    Ok(()) => self.orientation_request = Some(orientation),
                    Err(e) => {
                        log::error!("saving orientation: {e:#}");
                        self.toast = Some(Toast::new("Couldn't save orientation", None, Instant::now()));
                    }
                }
            }
            DrawerEvent::Refresh => {
                self.close_drawer();
                self.refresh_all(RefreshMode::Full);
            }
            DrawerEvent::Transfer | DrawerEvent::Settings => {
                self.close_drawer();
                let Some(services) = &self.services else { return };
                let (store, waker) = (services.store.clone(), services.waker.clone());
                let screen: Box<dyn Screen> = if event == DrawerEvent::Settings {
                    // The KOReader import leaves out a book open under Settings.
                    let open = self.relayout_state().open_book;
                    Box::new(
                        crate::screens::system::SystemSettings::new(&self.theme, bounds, store, waker)
                            .with_open_book(open),
                    )
                } else {
                    Box::new(crate::screens::transfer::TransferScreen::new(&self.theme, bounds, store, waker))
                };
                self.stack.push(screen);
            }
        }
    }

    /// Takes the worker's latest Wi-Fi/SSH state: into the open drawer,
    /// failures as a toast, and a newly claimed SSH name into the settings.
    fn sync_drawer(&mut self) {
        let Some(net) = &self.net else { return };
        let now = Instant::now();
        if let Some(message) = net.take_errors().pop() {
            self.toast = Some(Toast::new(message, None, now));
            self.needs_redraw = true;
        }
        let state = net.state();
        if state.finished != self.net_seen {
            self.net_seen = state.finished;
            if let Some(services) = &self.services {
                top_drawer::remember_hostname(&services.store, &state.ssh);
            }
        }
        if let Some(drawer) = &mut self.drawer
            && drawer.net() != &state
        {
            drawer.set_net(&self.theme, state, now);
            self.needs_redraw = true;
        }
    }

    /// A page-turn button (`button`: the device's own, not a remote's).
    /// Prompts, notices and the Top Drawer ignore it; the screen below them
    /// decides what it means.
    pub fn on_key(&mut self, key: KeyAction, button: bool) {
        if self.asleep
            || self.exit_prompt.is_some()
            || self.usb_prompt.is_some()
            || !self.notices.is_empty()
            || self.drawer.is_some()
            || self.touch_down
        {
            return;
        }
        let swap = button && self.services.as_ref().is_some_and(|s| s.store.settings().swap_page_buttons);
        let key = if swap { key.reversed() } else { key };
        self.with_screen(|screen, cx| screen.on_key(key, cx));
    }

    pub fn on_text_input(&mut self, input: &TextInput) -> bool {
        if self.asleep {
            return false;
        }
        if self.exit_prompt.is_some() {
            if matches!(input, TextInput::Dismiss) {
                self.exit_prompt = None;
                self.needs_redraw = true;
            }
            return true;
        }
        if self.usb_prompt.is_some() {
            if matches!(input, TextInput::Dismiss) {
                self.usb_prompt = None;
                self.needs_redraw = true;
            }
            return true;
        }
        if self.drawer.is_some() {
            if matches!(input, TextInput::Dismiss) {
                self.close_drawer();
            }
            return true;
        }
        let mut consumed = false;
        self.with_screen(|screen, cx| {
            consumed = screen.on_text_input(input, cx);
            Action::None
        });
        self.needs_redraw |= consumed;
        consumed
    }

    /// The next time `on_tick` should run, if any.
    pub fn deadline(&self) -> Option<Instant> {
        if self.asleep {
            return None;
        }
        let screen = self.stack.last().and_then(|s| s.deadline());
        let idle = self.stack.last().filter(|s| s.has_idle_work()).map(|_| Instant::now());
        let drawer = self.drawer.as_ref().and_then(TopDrawer::deadline);
        let dark = self.dark_pending.map(|(_, due)| due);
        [screen, idle, drawer, self.toast.as_ref().map(|t| t.until), self.wake_cleanup, dark]
            .into_iter()
            .flatten()
            .min()
    }

    pub fn on_tick(&mut self, now: Instant) {
        if self.asleep {
            return;
        }
        // A pending redraw (usually input) is presented before more layout.
        if !self.needs_redraw
            && let Some(screen) = self.stack.last_mut().filter(|s| s.has_idle_work())
        {
            self.needs_redraw |= screen.on_idle(&mut self.text);
        }
        if self.wake_cleanup.is_some_and(|due| now >= due) {
            self.wake_cleanup = None;
            let bounds = self.canvas.bounds();
            // The landscape canvas is turned onto the portrait panel: its
            // left edge maps to the panel's bottom edge.
            let rect = match self.orientation {
                Orientation::Portrait => {
                    let height = bounds.h.div_euclid(10) + 1;
                    Rect::new(0, bounds.bottom() - height, bounds.w, height)
                }
                Orientation::Landscape => Rect::new(0, 0, bounds.w.div_euclid(10) + 1, bounds.h),
            };
            self.forced.push(Update { rect, mode: RefreshMode::Ui });
            self.needs_redraw = true;
        }
        if self.toast.as_ref().is_some_and(|t| now >= t.until) {
            self.toast = None;
            self.needs_redraw = true;
        }
        if let (Some(drawer), Some(net)) = (&mut self.drawer, &self.net)
            && drawer.deadline().is_some_and(|d| now >= d)
        {
            if drawer.on_tick(now, net) {
                self.needs_redraw = true;
            }
            if let Some(services) = &self.services {
                top_drawer::remember_hostname(&services.store, &drawer.net().ssh);
            }
        }
        if let Some((dark, due)) = self.dark_pending
            && (now >= due || !self.stack.last().is_some_and(|s| s.waiting_for_dark()))
        {
            self.commit_dark_switch(dark);
        }
        if self.stack.last().and_then(|s| s.deadline()).is_some_and(|d| now >= d) {
            self.with_screen(|screen, cx| screen.on_tick(cx));
        }
    }

    pub fn busy(&self) -> bool {
        self.dark_pending.is_some()
            || self.stack.last().is_some_and(|s| s.busy())
            || self.net.as_ref().is_some_and(DrawerWorker::busy)
    }

    pub fn has_pending_work(&self) -> bool {
        !self.asleep && self.stack.last().is_some_and(|s| s.has_pending_work())
    }

    pub fn run_pending_work(&mut self) {
        self.needs_redraw = true;
        self.with_screen(|screen, cx| screen.run_pending_work(cx));
    }

    pub fn on_background(&mut self) {
        if self.asleep {
            return;
        }
        self.sync_drawer();
        self.with_screen(|screen, cx| {
            screen.on_background(cx);
            Action::None
        });
        self.finish_dark_wait_if_ready();
    }
    /// True once after a frame that showed a page turn.
    pub fn take_page_turn(&mut self) -> bool {
        self.stack.last_mut().is_some_and(|s| s.take_page_turn())
    }

    pub fn after_present(&mut self) {
        if let Some(s) = self.stack.last_mut() {
            s.after_present();
        }
    }
    /// Refuse a normal exit if durable saves failed, so the error remains
    /// visible and the reader can retry rather than silently losing its place.
    pub fn on_quit(&mut self) -> bool {
        for s in &mut self.stack {
            if let Err(e) = s.on_quit() {
                log::error!("exit: {e:#}");
                self.toast = Some(Toast::new(e.to_string(), None, Instant::now()));
                self.exit = false;
                self.exit_code = None;
                self.stop_ssh_on_exit = false;
                // Screens that were waiting to exit stop waiting, so input is read again.
                self.stack.iter_mut().for_each(|s| s.exit_refused());
                if let Some(prompt) = &mut self.exit_prompt {
                    prompt.set_stopping(false);
                }
                self.needs_redraw = true;
                return false;
            }
        }
        // Save first: a failed save must keep both the app and SSH available.
        if self.stop_ssh_on_exit {
            self.stop_ssh_on_exit = false;
            if let Err(e) = crate::platform::ssh::stop() {
                log::error!("stopping SSH on exit: {e:#}");
                self.exit = false;
                if let Some(prompt) = &mut self.exit_prompt {
                    prompt.set_stopping(false);
                }
                self.toast = Some(Toast::new("Couldn't stop SSH. Try again or choose Exit.", None, Instant::now()));
                self.needs_redraw = true;
                return false;
            }
        }
        true
    }
    pub fn light_request(&mut self) -> Option<Frontlight> {
        self.light_request.take()
    }

    pub fn dark_refresh_request(&mut self) -> Option<DarkRefresh> {
        self.dark_refresh_request.take()
    }

    pub fn on_sleep(&mut self) {
        // Cancel captures before dropping raw touch state, including overlays.
        self.on_gesture(Gesture::Cancel(crate::geom::Point::default()));
        self.close_drawer();
        if let Some((dark, _)) = self.dark_pending.take() {
            self.canvas.set_dark(dark);
        }
        for s in &mut self.stack {
            s.on_sleep();
        }
        self.asleep = true;
        self.touch_down = false;
        self.wake_cleanup = None;
    }

    pub fn draw_sleep_cover(&mut self, preference: crate::store::SleepScreen) -> bool {
        if preference == crate::store::SleepScreen::Amazon {
            return false;
        }
        // The cover stays portrait, like Amazon's sleep screen: in landscape
        // it is drawn turned, so the display's rotation stands it upright.
        let (w, h) = (self.canvas.width(), self.canvas.height());
        let (cover_w, cover_h) = match self.orientation {
            Orientation::Portrait => (w, h),
            Orientation::Landscape => (h, w),
        };
        let cover = self.stack.iter().rev().find_map(|s| s.sleep_cover(cover_w, cover_h));
        let Some(cover) = cover else { return false };
        let cover = match self.orientation {
            Orientation::Portrait => cover,
            // Panel pixel (y, w - 1 - x) shows canvas pixel (x, y).
            Orientation::Landscape => cover.rotate90(),
        };
        self.canvas.fill(PAPER);
        let bounds = self.canvas.bounds();
        cover.blit(&mut self.canvas, bounds);
        self.diff.mark_canvas_as_baseline();
        true
    }

    pub fn set_battery(&mut self, battery: Option<Battery>) {
        if self.battery != battery {
            self.battery = battery;
            self.needs_redraw = true;
        }
    }

    /// Delivers an asynchronous startup frontlight reading without overwriting
    /// a value already set by the user.
    pub fn set_initial_frontlight(&mut self, light: Option<Frontlight>) {
        if self.frontlight.is_none()
            && let Some(light) = light
        {
            self.frontlight = Some(light);
            if let Some(drawer) = &mut self.drawer
                && drawer.set_frontlight(&self.theme, light)
            {
                self.needs_redraw = true;
            }
            self.on_background();
        }
    }

    /// Returns true when connection state changes, so the caller can discard
    /// the raw gesture that may have started before this modal appeared/closed.
    pub fn on_usb(&mut self, connected: bool) -> bool {
        if connected == self.usb_connected {
            return false;
        }
        self.usb_connected = connected;
        if !self.asleep {
            self.on_gesture(Gesture::Cancel(crate::geom::Point::default()));
        }
        self.usb_prompt = connected.then(|| {
            Box::new(crate::screens::system::UsbPrompt::new(&self.theme, self.canvas.bounds())) as Box<dyn Screen>
        });
        self.needs_redraw = true;
        true
    }

    pub fn schedule_wake_cleanup(&mut self) {
        if !self.asleep {
            self.wake_cleanup = Some(Instant::now() + Duration::from_millis(1500));
        }
    }

    pub fn on_wake(&mut self) {
        self.asleep = false;
        let now = Instant::now();
        for s in &mut self.stack {
            s.on_wake(now);
        }
        self.on_background();
        self.refresh_all(RefreshMode::Full);
        // Use a bounded cleanup on every wake; firmware hibernation detection is
        // inconsistent on PW6, and this avoids reading Amazon's private state.
        self.schedule_wake_cleanup();
    }

    fn with_screen(&mut self, f: impl FnOnce(&mut dyn Screen, &mut EventCx) -> Action) {
        let dark = self.dark_mode();
        let Some(screen) = self.stack.last_mut() else { return };
        let mut cx = EventCx {
            theme: &self.theme,
            screen: self.canvas.bounds(),
            now: Instant::now(),
            battery: self.battery,
            toast: &mut self.toast,
            forced: &mut self.forced,
            needs_redraw: &mut self.needs_redraw,
            dark,
        };
        let action = f(screen.as_mut(), &mut cx);
        self.apply(action);
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::None => {}
            Action::Push(screen) => {
                self.stack.push(screen);
                self.needs_redraw = true;
            }
            Action::Pop => {
                self.needs_redraw = true;
                if self.stack.len() == 1 {
                    self.request_exit();
                } else {
                    self.stack.pop();
                    self.with_screen(|screen, cx| {
                        screen.on_return(cx);
                        Action::None
                    });
                    self.on_background();
                }
            }
            Action::Exit => self.request_exit(),
            Action::ExitWith(code) => {
                self.exit = true;
                self.exit_code = Some(code);
            }
            Action::Orientation(o) => self.orientation_request = Some(o),
            Action::DarkRefresh(dark) => {
                self.dark_refresh_request = Some(dark);
                if self.dark_mode() {
                    self.refresh_all(RefreshMode::Full);
                }
            }
            Action::OpenTopDrawer => self.open_drawer(),
        }
        self.finish_dark_wait_if_ready();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};
    #[test]
    fn failed_exit_keeps_the_app_and_error_visible_for_retry() {
        struct FailsOnce(bool);
        impl Screen for FailsOnce {
            fn draw(&mut self, _: &mut DrawCx) {}
            fn on_gesture(&mut self, _: Gesture, _: &mut EventCx) -> Action {
                Action::None
            }
            fn on_quit(&mut self) -> anyhow::Result<()> {
                anyhow::ensure!(std::mem::replace(&mut self.0, true), "Couldn't save your place");
                Ok(())
            }
        }
        let info =
            DeviceInfo { name: "test".into(), width: 600, height: 800, dpi: 150, color: crate::gfx::ColorMode::Gray8 };
        let mut app = App::new(&info, Theme::for_dpi(150), Box::new(FailsOnce(false)));
        app.frame();
        app.exit = true;
        assert!(!app.on_quit());
        assert!(!app.wants_exit());
        assert!(app.toast.is_some());
        assert!(!app.frame().is_empty());
        assert!(app.on_quit());
    }
    #[test]
    fn refused_exit_from_pending_work_does_not_loop_without_input() {
        struct Leaving(bool);
        impl Screen for Leaving {
            fn draw(&mut self, _: &mut DrawCx) {}
            fn on_gesture(&mut self, _: Gesture, _: &mut EventCx) -> Action {
                Action::None
            }
            fn has_pending_work(&self) -> bool {
                self.0
            }
            fn run_pending_work(&mut self, _: &mut EventCx) -> Action {
                Action::ExitWith(86)
            }
            fn exit_refused(&mut self) {
                self.0 = false;
            }
            fn on_quit(&mut self) -> anyhow::Result<()> {
                anyhow::bail!("Couldn't save your place")
            }
        }
        let info =
            DeviceInfo { name: "test".into(), width: 600, height: 800, dpi: 150, color: crate::gfx::ColorMode::Gray8 };
        let mut app = App::new(&info, Theme::for_dpi(150), Box::new(Leaving(true)));
        app.frame();
        assert!(app.has_pending_work());
        app.run_pending_work();
        assert!(app.wants_exit());
        assert!(!app.on_quit());
        assert!(!app.wants_exit() && !app.has_pending_work(), "the main loop reaches next_event again");
        assert_eq!(app.exit_code(), 0, "no stale launcher code");
    }
    struct BookScreen {
        touches: Rc<Cell<usize>>,
        backgrounds: Rc<Cell<usize>>,
        saves: Rc<Cell<usize>>,
        cover: bool,
    }
    impl Screen for BookScreen {
        fn draw(&mut self, cx: &mut DrawCx) {
            cx.canvas.fill(51);
        }
        fn on_gesture(&mut self, g: Gesture, _: &mut EventCx) -> Action {
            if matches!(g, Gesture::Tap(_)) {
                self.touches.set(self.touches.get() + 1);
            }
            Action::None
        }
        fn on_background(&mut self, _: &mut EventCx) {
            self.backgrounds.set(self.backgrounds.get() + 1);
        }
        fn after_present(&mut self) {
            self.saves.set(self.saves.get() + 1);
        }
        fn sleep_cover(&self, width: u32, height: u32) -> Option<crate::library::covers::CoverImage> {
            self.cover.then(|| {
                crate::library::covers::CoverImage::Gray(image::GrayImage::from_pixel(
                    width,
                    height,
                    image::Luma([119]),
                ))
            })
        }
    }
    fn book_app(cover: bool) -> App {
        let info =
            DeviceInfo { name: "test".into(), width: 600, height: 800, dpi: 150, color: crate::gfx::ColorMode::Gray8 };
        App::new(
            &info,
            Theme::for_dpi(150),
            Box::new(BookScreen {
                touches: Rc::new(Cell::new(0)),
                backgrounds: Rc::new(Cell::new(0)),
                saves: Rc::new(Cell::new(0)),
                cover,
            }),
        )
    }
    fn tap(app: &mut App, p: crate::geom::Point) {
        app.on_gesture(Gesture::Down(p));
        app.on_gesture(Gesture::Tap(p));
    }

    #[test]
    fn ssh_exit_dialog_captures_input_and_dismissal_cancels_exit() {
        let mut app = book_app(false);
        app.frame();
        app.show_exit_prompt();
        assert!(!app.wants_exit());
        assert!(!app.frame().is_empty());
        // Background work and persistence keep running under the modal.
        app.on_background();
        app.after_present();
        assert!(app.on_text_input(&TextInput::Insert("q".into())));
        assert!(!app.wants_exit());
        // Close button, outside tap, and Escape each cancel the exit.
        let close = crate::geom::Point::new(
            600 - app.theme.margin - app.theme.mm(2.0),
            800 - app.theme.mm(63.0) + app.theme.mm(6.5),
        );
        for p in [close, crate::geom::Point::new(300, 100)] {
            tap(&mut app, p);
            assert!(app.exit_prompt.is_none());
            assert!(!app.wants_exit());
            app.show_exit_prompt();
        }
        app.on_sleep();
        app.on_wake();
        assert!(app.exit_prompt.is_some());
        assert!(app.on_text_input(&TextInput::Dismiss));
        assert!(app.exit_prompt.is_none());
        assert!(!app.wants_exit());
        app.show_exit_prompt();
        let exit = crate::geom::Point::new(300, 800 - app.theme.mm(12.0) + app.theme.button_h / 2);
        tap(&mut app, exit);
        assert!(app.wants_exit());
        assert!(!app.stop_ssh_on_exit);
        assert!(app.on_quit());
    }

    #[test]
    fn stop_ssh_exit_saves_first_and_keeps_the_dialog_on_save_failure() {
        struct SavesOnce(bool);
        impl Screen for SavesOnce {
            fn draw(&mut self, _: &mut DrawCx) {}
            fn on_gesture(&mut self, _: Gesture, _: &mut EventCx) -> Action {
                Action::None
            }
            fn on_quit(&mut self) -> anyhow::Result<()> {
                anyhow::ensure!(std::mem::replace(&mut self.0, true), "Couldn't save your place");
                Ok(())
            }
        }
        let mut app = book_app(false);
        app.stack = vec![Box::new(SavesOnce(false))];
        app.show_exit_prompt();
        let stop = crate::geom::Point::new(300, 800 - app.theme.mm(24.0) + app.theme.button_h / 2);
        tap(&mut app, stop);
        assert!(app.wants_exit() && app.stop_ssh_on_exit);
        assert!(!app.frame().is_empty(), "show Stopping SSH before the save and stop");
        assert!(!app.on_quit());
        assert!(!app.wants_exit() && !app.stop_ssh_on_exit);
        assert!(app.exit_prompt.is_some());
        // Retrying the same choice is enabled after a failed save.
        tap(&mut app, stop);
        assert!(app.wants_exit() && app.stop_ssh_on_exit);
        assert!(app.on_quit());
        assert!(!app.stop_ssh_on_exit);
    }

    #[test]
    fn full_updates_keep_flash_strength_and_do_not_swallow_stronger_local_updates() {
        let mut app = book_app(false);
        let bounds = app.canvas.bounds();
        app.refresh_all(RefreshMode::Page);
        app.forced.push(Update { rect: bounds, mode: RefreshMode::Full });
        app.forced.push(Update { rect: Rect::new(0, 0, 16, 16), mode: RefreshMode::Ui });
        assert_eq!(app.frame(), vec![Update { rect: bounds, mode: RefreshMode::Full }]);
        assert!(app.diff.is_baseline_deferred());
        app.refresh_all(RefreshMode::Page);
        let local = Update { rect: Rect::new(0, 0, 16, 16), mode: RefreshMode::Full };
        app.forced.push(local);
        assert_eq!(app.frame(), vec![Update { rect: bounds, mode: RefreshMode::Page }, local]);
        assert!(!app.diff.is_baseline_deferred());
        assert!(!app.diff.changed(&app.canvas).any());
    }

    #[test]
    fn back_to_back_full_screen_frames_defer_baseline_until_partial_frame() {
        struct PageScreen {
            bg: Rc<Cell<u8>>,
            dot: Rc<Cell<bool>>,
            kept: Rc<Cell<bool>>,
        }
        impl Screen for PageScreen {
            fn fills_canvas(&self) -> bool {
                true
            }
            fn keeps_canvas(&self) -> bool {
                self.kept.get()
            }
            fn draw(&mut self, cx: &mut DrawCx) {
                cx.canvas.fill(self.bg.get());
                if self.dot.get() {
                    cx.fill(Rect::new(20, 20, 4, 4), 0);
                }
            }
            fn on_gesture(&mut self, _: Gesture, _: &mut EventCx) -> Action {
                Action::None
            }
        }
        let bg = Rc::new(Cell::new(240u8));
        let dot = Rc::new(Cell::new(false));
        let kept = Rc::new(Cell::new(false));
        let info =
            DeviceInfo { name: "test".into(), width: 160, height: 96, dpi: 150, color: crate::gfx::ColorMode::Gray8 };
        let screen = PageScreen { bg: bg.clone(), dot: dot.clone(), kept: kept.clone() };
        let mut app = App::new(&info, Theme::for_dpi(150), Box::new(screen));
        let initial_copies = app.diff.full_copies();
        // Multiple full-screen Page/Full frames copy `prev` 0 times.
        for (value, mode) in [(220, RefreshMode::Page), (200, RefreshMode::Page), (180, RefreshMode::Full)] {
            bg.set(value);
            app.refresh_all(mode);
            assert_eq!(app.frame(), vec![Update { rect: app.canvas.bounds(), mode }]);
            assert!(app.diff.is_baseline_deferred());
        }
        assert_eq!(app.diff.full_copies(), initial_copies, "back-to-back full frames never copy prev");
        // A screen keeping the canvas (a book opening) is neither drawn nor diffed.
        kept.set(true);
        bg.set(0);
        app.needs_redraw = true;
        assert!(app.frame().is_empty());
        let cleanup = Update { rect: Rect::new(0, 0, 16, 16), mode: RefreshMode::Ui };
        app.forced.push(cleanup);
        app.needs_redraw = true;
        assert_eq!(app.frame(), vec![cleanup], "forced refreshes still go out");
        assert_eq!((app.canvas.raw()[0], app.diff.full_copies()), (180, initial_copies));
        kept.set(false);
        bg.set(180);
        // A partial frame right after syncs once and diffs accurately against bg = 180.
        dot.set(true);
        app.needs_redraw = true;
        assert_eq!(app.frame(), vec![Update { rect: Rect::new(16, 16, 16, 16), mode: RefreshMode::Ui }]);
        assert!(!app.diff.is_baseline_deferred());
        assert_eq!(app.diff.full_copies(), initial_copies + 1);
        // Removing the dot diffs accurately back to bg = 180.
        dot.set(false);
        app.needs_redraw = true;
        assert_eq!(app.frame(), vec![Update { rect: Rect::new(16, 16, 16, 16), mode: RefreshMode::Ui }]);
        assert_eq!(app.diff.full_copies(), initial_copies + 1);
    }

    #[test]
    fn non_visual_on_tick_does_not_schedule_a_redraw() {
        let ticks = Rc::new(Cell::new(0));
        let info =
            DeviceInfo { name: "test".into(), width: 600, height: 800, dpi: 150, color: crate::gfx::ColorMode::Gray8 };
        let screen = TimedScreen { ticks: ticks.clone(), backgrounds: Rc::new(Cell::new(0)), due: Instant::now() };
        let mut app = App::new(&info, Theme::for_dpi(150), Box::new(screen));
        app.frame();
        assert!(!app.needs_redraw);
        app.on_tick(Instant::now());
        assert_eq!(ticks.get(), 1);
        assert!(!app.needs_redraw, "non-visual tick must not redraw");
        assert!(app.frame().is_empty());
    }

    #[test]
    fn drawer_ignores_noop_gestures_but_keeps_press_and_cancel_feedback() {
        use top_drawer::Tile;
        let mut d = drawer_app(true, true);
        tap(&mut d.app, Point::new(300, 600));
        d.app.frame();
        // Down outside the drawer changes nothing visually.
        d.app.on_gesture(Gesture::Down(Point::new(300, 780)));
        assert!(!d.app.needs_redraw);
        assert!(d.app.frame().is_empty());
        // Down on a tile shows immediate Fast pressed feedback; Cancel clears it.
        let p = tile(&d.app, Tile::Refresh);
        d.app.on_gesture(Gesture::Down(p));
        assert!(d.app.needs_redraw);
        assert!(d.app.frame().iter().any(|u| u.mode == RefreshMode::Fast));
        d.app.on_gesture(Gesture::Cancel(p));
        assert!(d.app.needs_redraw);
        assert!(d.app.frame().iter().any(|u| u.mode == RefreshMode::Ui));
    }

    #[test]
    fn partial_full_refresh_drops_inner_fast_debt_and_orders_outer_cleanup_first() {
        let mut app = book_app(false);
        app.frame();
        let grid = Rect::new(20, 30, 200, 150);
        let inner = Rect::new(40, 50, 32, 32);
        let outer = Rect::new(20, 220, 48, 32);
        app.fast_debt.push(inner);
        app.fast_debt.push(outer);
        app.forced.push(Update { rect: grid, mode: RefreshMode::Full });
        app.needs_redraw = true;
        let updates = app.frame();
        assert_eq!(
            updates,
            vec![Update { rect: outer, mode: RefreshMode::Ui }, Update { rect: grid, mode: RefreshMode::Full }]
        );
        assert!(!app.diff.changed(&app.canvas).any());
    }

    #[test]
    fn unchanged_gestures_and_background_wakeups_do_not_draw() {
        let mut app = book_app(false);
        app.frame();
        let p = crate::geom::Point::new(500, 400);
        for g in [Gesture::Down(p), Gesture::Move(p), Gesture::Cancel(p), Gesture::Tap(p)] {
            app.on_gesture(g);
            assert!(!app.needs_redraw);
            assert!(app.frame().is_empty());
        }
        app.on_background();
        assert!(!app.needs_redraw);
        app.with_screen(|_, cx| {
            cx.redraw();
            Action::None
        });
        assert!(app.needs_redraw);
        app.frame();
        app.with_screen(|_, cx| {
            cx.refresh(Rect::new(0, 0, 20, 20), RefreshMode::Ui);
            Action::None
        });
        assert_eq!(app.frame(), vec![Update { rect: Rect::new(0, 0, 20, 20), mode: RefreshMode::Ui }]);
    }
    #[test]
    fn touch_release_cleans_fast_debt_without_a_screen_change() {
        let mut app = book_app(false);
        app.frame();
        let p = crate::geom::Point::new(300, 400);
        let rect = Rect::new(0, 0, 20, 20);
        app.on_gesture(Gesture::Down(p));
        app.fast_debt.push(rect);
        app.on_gesture(Gesture::Move(p));
        assert!(app.frame().is_empty());
        app.on_gesture(Gesture::End(p));
        assert_eq!(app.frame(), vec![Update { rect, mode: RefreshMode::Ui }]);
        assert!(app.fast_debt.is_empty());
    }
    #[test]
    fn toasts_invalidate_on_show_dismiss_and_expiry() {
        let mut app = book_app(false);
        app.frame();
        let show = |app: &mut App| {
            app.with_screen(|_, cx| {
                cx.toast("Saved", None);
                Action::None
            });
            assert!(!app.frame().is_empty());
        };
        show(&mut app);
        let p = app.toast_rect.center();
        app.on_gesture(Gesture::Down(p));
        assert!(!app.needs_redraw);
        app.on_gesture(Gesture::Tap(p));
        assert!(app.toast.is_none());
        assert!(!app.frame().is_empty());
        show(&mut app);
        app.on_tick(app.toast.as_ref().unwrap().until);
        assert!(app.toast.is_none());
        assert!(!app.frame().is_empty());
    }
    #[test]
    fn usb_controls_keep_immediate_press_and_cancel_feedback() {
        let mut app = book_app(false);
        app.frame();
        app.on_usb(true);
        app.frame();
        let p = crate::geom::Point::new(300, 800 - app.theme.mm(12.0) + app.theme.button_h / 2);
        app.on_gesture(Gesture::Down(p));
        assert!(app.frame().iter().any(|u| u.mode == RefreshMode::Fast));
        app.on_gesture(Gesture::Cancel(p));
        assert!(app.frame().iter().any(|u| u.mode == RefreshMode::Ui));
        assert!(app.usb_prompt.is_some());
        app.on_gesture(Gesture::Down(p));
        app.frame();
        app.on_gesture(Gesture::Tap(p));
        assert!(!app.frame().is_empty());
        assert!(app.usb_prompt.is_none());
    }
    #[test]
    fn stack_changes_invalidate_without_a_screen_request() {
        let mut app = book_app(false);
        app.frame();
        app.with_screen(|_, _| {
            Action::Push(Box::new(TimedScreen {
                ticks: Rc::new(Cell::new(0)),
                backgrounds: Rc::new(Cell::new(0)),
                due: Instant::now() + Duration::from_secs(60),
            }))
        });
        assert!(app.needs_redraw);
        assert!(!app.frame().is_empty());
        app.with_screen(|_, _| Action::Pop);
        assert!(app.needs_redraw);
        assert!(!app.frame().is_empty());
    }
    #[test]
    fn covers_do_not_replace_the_awake_frame_and_native_is_a_safe_fallback() {
        let mut app = book_app(true);
        app.frame();
        app.on_sleep();
        assert!(app.draw_sleep_cover(crate::store::SleepScreen::Cover));
        assert_eq!(app.canvas.gray_data()[0], 119);
        assert!(app.frame().is_empty());
        app.on_wake();
        assert_eq!(app.frame(), vec![Update { rect: app.canvas.bounds(), mode: RefreshMode::Full }]);
        assert_eq!(app.canvas.gray_data()[0], 51);
        assert!(!app.draw_sleep_cover(crate::store::SleepScreen::Amazon));
        assert!(!book_app(false).draw_sleep_cover(crate::store::SleepScreen::Cover));
    }
    #[test]
    fn landscape_sleep_cover_is_drawn_turned_so_it_stands_upright() {
        struct Marked;
        impl Screen for Marked {
            fn draw(&mut self, _: &mut DrawCx) {}
            fn on_gesture(&mut self, _: Gesture, _: &mut EventCx) -> Action {
                Action::None
            }
            fn sleep_cover(&self, width: u32, height: u32) -> Option<crate::library::covers::CoverImage> {
                assert_eq!((width, height), (600, 800), "covers are made for the portrait panel");
                let mut cover = image::GrayImage::from_pixel(width, height, image::Luma([119]));
                cover.put_pixel(0, 0, image::Luma([0]));
                Some(crate::library::covers::CoverImage::Gray(cover))
            }
        }
        let info =
            DeviceInfo { name: "test".into(), width: 800, height: 600, dpi: 150, color: crate::gfx::ColorMode::Gray8 };
        let mut app = App::new(&info, Theme::for_dpi(150), Box::new(Marked));
        app.set_orientation(Orientation::Landscape);
        assert!(app.draw_sleep_cover(crate::store::SleepScreen::Cover));
        // The panel's top-left pixel shows the canvas's top-right one.
        let mut panel = vec![255u8; 600 * 800];
        app.canvas.copy_gray_rotated(
            app.canvas.bounds(),
            &mut panel,
            600,
            &mut crate::gfx::RotateScratch::default(),
            false,
        );
        assert_eq!(panel[0], 0);
        assert_eq!(panel[1], 119);

        // Dark mode keeps sleep covers identical after panel inversion, with black margins intact.
        for orientation in [Orientation::Portrait, Orientation::Landscape] {
            for mode in [crate::gfx::ColorMode::Gray8, crate::gfx::ColorMode::Color32] {
                let (w, h) = match orientation {
                    Orientation::Portrait => (600, 800),
                    Orientation::Landscape => (800, 600),
                };
                struct ColorMarked;
                impl Screen for ColorMarked {
                    fn draw(&mut self, _: &mut DrawCx) {}
                    fn on_gesture(&mut self, _: Gesture, _: &mut EventCx) -> Action {
                        Action::None
                    }
                    fn sleep_cover(&self, width: u32, height: u32) -> Option<crate::library::covers::CoverImage> {
                        let mut img = image::RgbImage::from_pixel(width, height, image::Rgb([0, 0, 0]));
                        img.put_pixel(10, 20, image::Rgb([40, 130, 210]));
                        Some(crate::library::covers::CoverImage::Rgb(img))
                    }
                }
                let info = DeviceInfo { name: "test".into(), width: w, height: h, dpi: 150, color: mode };
                let screen_light: Box<dyn Screen> =
                    if mode == crate::gfx::ColorMode::Gray8 { Box::new(Marked) } else { Box::new(ColorMarked) };
                let screen_dark: Box<dyn Screen> =
                    if mode == crate::gfx::ColorMode::Gray8 { Box::new(Marked) } else { Box::new(ColorMarked) };
                let mut light_app = App::new(&info, Theme::for_dpi(150), screen_light);
                let mut dark_app = App::new(&info, Theme::for_dpi(150), screen_dark);
                light_app.set_orientation(orientation);
                dark_app.set_orientation(orientation);
                dark_app.set_dark(true);
                assert!(light_app.draw_sleep_cover(crate::store::SleepScreen::Cover));
                assert!(dark_app.draw_sleep_cover(crate::store::SleepScreen::Cover));

                let bpp = light_app.canvas.bytes_per_pixel();
                let read_panel = |app: &App, invert: bool| {
                    let mut out = vec![9u8; 600 * 800 * bpp];
                    let mut scratch = crate::gfx::RotateScratch::default();
                    match (orientation, mode) {
                        (Orientation::Portrait, crate::gfx::ColorMode::Gray8) => {
                            app.canvas.copy_gray(app.canvas.bounds(), &mut out, 600, invert);
                        }
                        (Orientation::Landscape, crate::gfx::ColorMode::Gray8) => {
                            app.canvas.copy_gray_rotated(app.canvas.bounds(), &mut out, 600, &mut scratch, invert);
                        }
                        (Orientation::Portrait, crate::gfx::ColorMode::Color32) => {
                            app.canvas.copy_rgba(app.canvas.bounds(), &mut out, 600, invert);
                        }
                        (Orientation::Landscape, crate::gfx::ColorMode::Color32) => {
                            app.canvas.copy_rgba_rotated(app.canvas.bounds(), &mut out, 600, &mut scratch);
                            if invert {
                                for px in out.as_chunks_mut::<4>().0 {
                                    *px = [px[0] ^ 0xff, px[1] ^ 0xff, px[2] ^ 0xff, 0xff];
                                }
                            }
                        }
                    }
                    out
                };
                let fb_l = read_panel(&light_app, false);
                let fb_d = read_panel(&dark_app, true);
                assert_eq!(fb_d, fb_l, "sleep cover {orientation:?} {mode:?}");
                assert_eq!(fb_d[0], 0, "black margin stays black ({orientation:?} {mode:?})");
            }
        }
    }

    #[test]
    fn turning_the_screen_saves_and_rebuilds_the_screens_at_the_new_size() {
        struct Turns(Rc<Cell<usize>>);
        impl Screen for Turns {
            fn draw(&mut self, _: &mut DrawCx) {}
            fn on_gesture(&mut self, g: Gesture, _: &mut EventCx) -> Action {
                if matches!(g, Gesture::Tap(_)) { Action::Orientation(Orientation::Landscape) } else { Action::None }
            }
            fn on_quit(&mut self) -> anyhow::Result<()> {
                self.0.set(self.0.get() + 1);
                Ok(())
            }
        }
        let saves = Rc::new(Cell::new(0));
        let info =
            DeviceInfo { name: "test".into(), width: 600, height: 800, dpi: 150, color: crate::gfx::ColorMode::Gray8 };
        let mut app = App::new(&info, Theme::for_dpi(150), Box::new(Turns(saves.clone())));
        app.frame();
        tap(&mut app, crate::geom::Point::new(300, 400));
        assert_eq!(app.prepare_orientation(), Some((Orientation::Landscape, RelayoutState::default())));
        assert!(app.prepare_orientation().is_none());
        assert_eq!(saves.get(), 1, "positions are saved before the screens are rebuilt");
        let turned = DeviceInfo { width: 800, height: 600, ..info };
        app.relayout(&turned, Orientation::Landscape, |_, rect| {
            assert_eq!(rect, Rect::new(0, 0, 800, 600));
            vec![Box::new(Turns(saves.clone())), Box::new(Turns(saves.clone()))]
        });
        assert_eq!(app.stack.len(), 2);
        assert_eq!(app.canvas.bounds(), Rect::new(0, 0, 800, 600));
        assert_eq!(app.frame(), vec![Update { rect: Rect::new(0, 0, 800, 600), mode: RefreshMode::Full }]);
    }

    #[test]
    fn a_color_device_draws_on_a_color_canvas_through_relayout() {
        struct Square;
        impl Screen for Square {
            fn draw(&mut self, cx: &mut DrawCx) {
                cx.canvas.fill_rect(Rect::new(0, 0, 40, 40), 0);
            }
            fn on_gesture(&mut self, _: Gesture, _: &mut EventCx) -> Action {
                Action::None
            }
        }
        use crate::gfx::ColorMode::Color32;
        let info = DeviceInfo { name: "test".into(), width: 600, height: 800, dpi: 150, color: Color32 };
        let mut app = App::new(&info, Theme::for_dpi(150), Box::new(Square));
        app.set_dark(true);
        assert_eq!(app.canvas.mode(), Color32);
        assert!(app.canvas.is_dark());
        assert!(!app.frame().is_empty());
        assert_eq!(app.canvas.raw()[..4], [0, 0, 0, 255]);
        app.relayout(&DeviceInfo { width: 800, height: 600, ..info }, Orientation::Landscape, |_, _| {
            vec![Box::new(Square)]
        });
        assert_eq!((app.canvas.mode(), app.canvas.raw().len()), (Color32, 800 * 600 * 4));
        assert!(app.canvas.is_dark(), "dark polarity carries across relayout");
        assert_eq!(app.frame(), vec![Update { rect: Rect::new(0, 0, 800, 600), mode: RefreshMode::Full }]);
    }

    #[test]
    fn failed_relayout_save_keeps_orientation_pending_until_retry_succeeds() {
        struct FailsOnce(bool);
        impl Screen for FailsOnce {
            fn draw(&mut self, _: &mut DrawCx) {}
            fn on_gesture(&mut self, _: Gesture, _: &mut EventCx) -> Action {
                Action::None
            }
            fn on_quit(&mut self) -> anyhow::Result<()> {
                anyhow::ensure!(std::mem::replace(&mut self.0, true), "Couldn't save your place");
                Ok(())
            }
        }
        let info =
            DeviceInfo { name: "test".into(), width: 600, height: 800, dpi: 150, color: crate::gfx::ColorMode::Gray8 };
        let mut app = App::new(&info, Theme::for_dpi(150), Box::new(FailsOnce(false)));
        app.orientation_request = Some(Orientation::Landscape);
        app.frame();

        assert!(app.prepare_orientation().is_none());
        assert_eq!(app.orientation_request, Some(Orientation::Landscape));
        assert_eq!(app.orientation, Orientation::Portrait);
        assert_eq!(app.canvas.bounds(), Rect::new(0, 0, 600, 800));
        assert!(app.toast.is_some());
        assert!(!app.frame().is_empty(), "the save error must be visible");
        assert_eq!(app.prepare_orientation().map(|(o, _)| o), Some(Orientation::Landscape));
        assert!(app.prepare_orientation().is_none(), "a successful preparation consumes the request once");
    }

    #[test]
    fn late_wake_cleanup_rewrites_unchanged_pixels_and_cancels_on_resleep() {
        let mut app = book_app(false);
        app.frame();
        app.on_sleep();
        app.on_wake();
        app.frame();
        let due = app.wake_cleanup.unwrap();
        app.on_tick(due);
        assert_eq!(app.frame(), vec![Update { rect: Rect::new(0, 719, 600, 81), mode: RefreshMode::Ui }]);
        assert!(app.wake_cleanup.is_none());
        app.on_sleep();
        app.on_wake();
        app.on_sleep();
        app.schedule_wake_cleanup();
        assert!(app.deadline().is_none());
        assert!(app.wake_cleanup.is_none());
    }
    #[test]
    fn landscape_wake_cleanup_targets_the_panel_bottom() {
        let info =
            DeviceInfo { name: "test".into(), width: 800, height: 600, dpi: 150, color: crate::gfx::ColorMode::Gray8 };
        let mut app = App::new(
            &info,
            Theme::for_dpi(150),
            Box::new(BookScreen {
                touches: Rc::new(Cell::new(0)),
                backgrounds: Rc::new(Cell::new(0)),
                saves: Rc::new(Cell::new(0)),
                cover: false,
            }),
        );
        app.set_orientation(Orientation::Landscape);
        app.frame();
        app.on_sleep();
        app.on_wake();
        app.frame();
        app.on_tick(app.wake_cleanup.unwrap());
        assert_eq!(app.frame(), vec![Update { rect: Rect::new(0, 0, 81, 600), mode: RefreshMode::Ui }]);
    }
    #[test]
    fn usb_is_modal_but_keeps_worker_results_and_saves_running() {
        let touches = Rc::new(Cell::new(0));
        let backgrounds = Rc::new(Cell::new(0));
        let saves = Rc::new(Cell::new(0));
        let info =
            DeviceInfo { name: "test".into(), width: 600, height: 800, dpi: 150, color: crate::gfx::ColorMode::Gray8 };
        let mut app = App::new(
            &info,
            Theme::for_dpi(150),
            Box::new(BookScreen {
                touches: touches.clone(),
                backgrounds: backgrounds.clone(),
                saves: saves.clone(),
                cover: false,
            }),
        );
        app.on_usb(true);
        app.on_background();
        app.frame();
        app.after_present();
        assert_eq!(backgrounds.get(), 1);
        assert_eq!(saves.get(), 1);
        let p = crate::geom::Point::new(300, 400);
        app.on_gesture(Gesture::Down(p));
        app.on_gesture(Gesture::Tap(p)); // outside the sheet: dismiss, no page turn
        assert_eq!(touches.get(), 0);
        assert!(app.usb_prompt.is_none());
        app.on_usb(true); // duplicates must not reopen a dismissed prompt
        assert!(app.usb_prompt.is_none());
        app.on_usb(false);
        app.on_sleep();
        app.on_usb(true);
        assert!(app.frame().is_empty());
        app.on_wake();
        assert!(app.usb_prompt.is_some());
        app.on_usb(false);
        assert!(app.usb_prompt.is_none());
        app.on_usb(true);
        let exit = crate::geom::Point::new(300, 800 - app.theme.mm(24.0) + app.theme.button_h / 2);
        app.on_gesture(Gesture::Down(exit));
        app.on_gesture(Gesture::Tap(exit));
        assert!(app.wants_exit());
        assert_eq!(touches.get(), 0);
    }
    #[test]
    fn idle_layout_runs_one_step_per_turn_and_redraws_only_visible_progress() {
        struct LayoutScreen {
            remaining: Rc<Cell<usize>>,
        }
        impl Screen for LayoutScreen {
            fn draw(&mut self, cx: &mut DrawCx) {
                cx.fill(Rect::new(0, 0, 20, 20), crate::ui::theme::INK);
            }
            fn on_gesture(&mut self, _: Gesture, _: &mut EventCx) -> Action {
                Action::None
            }
            fn has_idle_work(&self) -> bool {
                self.remaining.get() > 0
            }
            fn on_idle(&mut self, _: &mut TextRenderer) -> bool {
                self.remaining.set(self.remaining.get() - 1);
                self.remaining.get() == 0
            }
        }
        let remaining = Rc::new(Cell::new(3));
        let info =
            DeviceInfo { name: "test".into(), width: 600, height: 800, dpi: 150, color: crate::gfx::ColorMode::Gray8 };
        let mut app = App::new(&info, Theme::for_dpi(150), Box::new(LayoutScreen { remaining: remaining.clone() }));
        assert!(!app.frame().is_empty());
        assert_eq!(remaining.get(), 3, "present the initial frame before more layout");
        assert!(app.deadline().is_some());
        app.on_tick(Instant::now());
        assert_eq!(remaining.get(), 2);
        assert!(app.frame().is_empty(), "cache-only progress needs no drawing");
        app.needs_redraw = true;
        app.on_tick(Instant::now());
        assert_eq!(remaining.get(), 2, "a pending redraw (e.g. a page turn) is drawn before more layout");
        app.frame();
        assert!(!app.needs_redraw);
        app.on_sleep();
        assert!(app.deadline().is_none());
        app.on_tick(Instant::now());
        assert_eq!(remaining.get(), 2);
        app.on_wake();
        app.frame();
        app.on_tick(Instant::now());
        assert_eq!(remaining.get(), 1);
        app.frame();
        app.on_tick(Instant::now());
        assert_eq!(remaining.get(), 0);
        assert!(app.needs_redraw, "completion changes the page counter");
        // Wake's separate delayed repaint may still have a deadline.
        app.on_tick(Instant::now() + Duration::from_secs(2));
        assert!(app.deadline().is_none(), "finished layout stops waking the event loop");
    }

    struct TimedScreen {
        ticks: Rc<Cell<usize>>,
        backgrounds: Rc<Cell<usize>>,
        due: Instant,
    }
    impl Screen for TimedScreen {
        fn draw(&mut self, _: &mut DrawCx) {}
        fn on_gesture(&mut self, _: Gesture, _: &mut EventCx) -> Action {
            Action::None
        }
        fn deadline(&self) -> Option<Instant> {
            Some(self.due)
        }
        fn on_tick(&mut self, _: &mut EventCx) -> Action {
            self.ticks.set(self.ticks.get() + 1);
            Action::None
        }
        fn on_background(&mut self, cx: &mut EventCx) {
            self.backgrounds.set(self.backgrounds.get() + 1);
            cx.refresh(cx.screen, RefreshMode::Full);
        }
    }
    #[test]
    fn sleep_suspends_timers_and_worker_results_until_wake() {
        let ticks = Rc::new(Cell::new(0));
        let backgrounds = Rc::new(Cell::new(0));
        let info =
            DeviceInfo { name: "test".into(), width: 600, height: 800, dpi: 150, color: crate::gfx::ColorMode::Gray8 };
        let screen = TimedScreen { ticks: ticks.clone(), backgrounds: backgrounds.clone(), due: Instant::now() };
        let mut app = App::new(&info, Theme::for_dpi(150), Box::new(screen));
        app.on_sleep();
        assert!(app.deadline().is_none());
        app.on_tick(Instant::now());
        app.on_background();
        assert_eq!(ticks.get(), 0);
        assert_eq!(backgrounds.get(), 0);
        app.on_wake();
        app.on_tick(Instant::now());
        assert_eq!(ticks.get(), 1);
        assert_eq!(backgrounds.get(), 1);
        let updates = app.frame();
        assert_eq!(updates.len(), 1, "wake and layout completion share one full refresh");
        assert_eq!(updates[0].mode, RefreshMode::Full);
    }

    #[test]
    fn initial_frontlight_notifies_screen_once_without_overwriting_user_choice() {
        let backgrounds = Rc::new(Cell::new(0));
        let info =
            DeviceInfo { name: "test".into(), width: 600, height: 800, dpi: 150, color: crate::gfx::ColorMode::Gray8 };
        let screen = BookScreen {
            touches: Rc::new(Cell::new(0)),
            backgrounds: backgrounds.clone(),
            saves: Rc::new(Cell::new(0)),
            cover: false,
        };
        let mut app = App::new(&info, Theme::for_dpi(150), Box::new(screen));
        assert_eq!(app.frontlight, None);
        app.set_initial_frontlight(None);
        assert_eq!(app.frontlight, None);
        assert_eq!(backgrounds.get(), 0);
        let hw = Frontlight { brightness: 9, warmth: Some(4) };
        app.set_initial_frontlight(Some(hw));
        assert_eq!(app.frontlight, Some(hw));
        assert_eq!(backgrounds.get(), 1);
        // A later delivery must never overwrite an existing value.
        app.set_initial_frontlight(Some(Frontlight { brightness: 1, warmth: Some(0) }));
        assert_eq!(app.frontlight, Some(hw));
        assert_eq!(backgrounds.get(), 1);
    }

    #[test]
    fn swapping_turns_the_devices_own_buttons_but_not_remotes() -> anyhow::Result<()> {
        use KeyAction::{PageNext, PagePrev};
        struct Keys(Rc<std::cell::RefCell<Vec<KeyAction>>>);
        impl Screen for Keys {
            fn draw(&mut self, _: &mut DrawCx) {}
            fn on_gesture(&mut self, _: Gesture, _: &mut EventCx) -> Action {
                Action::None
            }
            fn on_key(&mut self, key: KeyAction, _: &mut EventCx) -> Action {
                self.0.borrow_mut().push(key);
                Action::None
            }
        }
        let mut d = drawer_app(false, false);
        let keys = Rc::new(std::cell::RefCell::new(Vec::new()));
        d.app.push_screen(Box::new(Keys(keys.clone())));
        for swap in [false, true] {
            d.store.save_settings(&crate::store::Settings { swap_page_buttons: swap, ..d.store.settings() })?;
            d.app.on_key(PageNext, true);
            d.app.on_key(PageNext, false);
        }
        assert_eq!(*keys.borrow(), [PageNext, PageNext, PagePrev, PageNext]);
        Ok(())
    }

    /// A screen that counts what reaches it and can ask for the drawer.
    struct Under {
        allows: bool,
        taps: Rc<Cell<usize>>,
        cancels: Rc<Cell<usize>>,
        swipes: Rc<Cell<usize>>,
        open_on_tap: bool,
    }
    impl Screen for Under {
        fn draw(&mut self, cx: &mut DrawCx) {
            cx.canvas.fill(0xcc);
        }
        fn on_gesture(&mut self, g: Gesture, _: &mut EventCx) -> Action {
            match g {
                Gesture::Tap(_) if self.open_on_tap => return Action::OpenTopDrawer,
                Gesture::Tap(_) => self.taps.set(self.taps.get() + 1),
                Gesture::Cancel(_) => self.cancels.set(self.cancels.get() + 1),
                Gesture::Swipe { .. } => self.swipes.set(self.swipes.get() + 1),
                _ => {}
            }
            Action::None
        }
        fn allows_top_drawer(&self) -> bool {
            self.allows
        }
    }
    struct DrawerApp {
        app: App,
        taps: Rc<Cell<usize>>,
        cancels: Rc<Cell<usize>>,
        swipes: Rc<Cell<usize>>,
        ops: Arc<top_drawer::tests::FakeNet>,
        store: Store,
        _dir: tempfile::TempDir,
    }
    fn drawer_app(allows: bool, open_on_tap: bool) -> DrawerApp {
        let (taps, cancels, swipes) = (Rc::new(Cell::new(0)), Rc::new(Cell::new(0)), Rc::new(Cell::new(0)));
        let info =
            DeviceInfo { name: "test".into(), width: 600, height: 800, dpi: 150, color: crate::gfx::ColorMode::Gray8 };
        let under = Under { allows, taps: taps.clone(), cancels: cancels.clone(), swipes: swipes.clone(), open_on_tap };
        let mut app = App::new(&info, Theme::for_dpi(150), Box::new(under));
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().to_path_buf());
        let ops = Arc::new(top_drawer::tests::FakeNet::new(false, false));
        app.set_services(Services { store: store.clone(), waker: Waker::new(|| {}), net: ops.clone() });
        app.frame();
        DrawerApp { app, taps, cancels, swipes, ops, store, _dir: dir }
    }
    fn swipe(app: &mut App, from: crate::geom::Point, to: crate::geom::Point) {
        app.on_gesture(Gesture::Down(from));
        app.on_gesture(Gesture::Move(to));
        let dir = if to.y > from.y { Direction::Down } else { Direction::Up };
        app.on_gesture(Gesture::Swipe { dir, from, to });
    }
    fn tile(app: &App, kind: top_drawer::Tile) -> crate::geom::Point {
        let drawer = app.drawer.as_ref().expect("drawer open");
        let i = drawer.tile_kinds().iter().position(|&k| k == kind).unwrap();
        drawer.layout().tiles[i].center()
    }
    fn settle(app: &mut App) {
        let started = Instant::now();
        while app.net.as_ref().is_some_and(DrawerWorker::busy) {
            assert!(started.elapsed() < Duration::from_secs(10), "worker finishes");
            std::thread::sleep(Duration::from_millis(5));
        }
        app.on_background();
    }
    use crate::geom::Point;

    #[test]
    fn top_edge_swipe_down_opens_the_drawer_and_cancels_the_touch_below() {
        let mut d = drawer_app(true, false);
        // The zone is the larger of the top bar and a tenth of the screen.
        let zone = d.app.theme.top_bar.max(80);
        swipe(&mut d.app, Point::new(300, zone + 5), Point::new(300, 500));
        assert!(d.app.drawer.is_none(), "below the top edge it's an ordinary swipe");
        assert_eq!(d.swipes.get(), 1);
        swipe(&mut d.app, Point::new(300, zone - 5), Point::new(300, 500));
        assert!(d.app.drawer.is_some());
        assert_eq!((d.cancels.get(), d.swipes.get()), (1, 1), "the screen sees its touch cancelled");
        let updates = d.app.frame();
        let panel = d.app.drawer.as_ref().unwrap().rect();
        assert!(!updates.is_empty() && updates.iter().all(|u| u.mode == RefreshMode::Ui), "{updates:?}");
        assert!(updates.iter().all(|u| u.rect.y < panel.bottom() + 32), "only the drawer area changes");
        // Touches on the drawer stay there; a tap below closes it without reaching the screen.
        let status = d.app.drawer.as_ref().unwrap().layout().status.center().y;
        tap(&mut d.app, Point::new(300, status));
        assert!(d.app.drawer.is_some());
        tap(&mut d.app, Point::new(300, 780));
        assert!(d.app.drawer.is_none());
        assert_eq!(d.taps.get(), 0);
        assert!(!d.app.frame().is_empty(), "closing redraws the screen below");
        tap(&mut d.app, Point::new(300, 780));
        assert_eq!(d.taps.get(), 1);
        // Swipe up on the drawer closes it too.
        swipe(&mut d.app, Point::new(300, 10), Point::new(300, 400));
        swipe(&mut d.app, Point::new(300, 200), Point::new(300, 10));
        assert!(d.app.drawer.is_none());
    }

    #[test]
    fn screens_and_prompts_can_keep_the_drawer_closed() {
        let mut d = drawer_app(false, false);
        swipe(&mut d.app, Point::new(300, 10), Point::new(300, 500));
        assert!(d.app.drawer.is_none(), "this screen's state doesn't allow it");
        assert_eq!(d.swipes.get(), 1);
        let mut d = drawer_app(true, true);
        d.app.on_usb(true);
        swipe(&mut d.app, Point::new(300, 10), Point::new(300, 500));
        assert!(d.app.drawer.is_none(), "the USB prompt comes first");
        d.app.on_usb(false);
        // A screen can ask for the drawer (the battery tap).
        tap(&mut d.app, Point::new(300, 600));
        assert!(d.app.drawer.is_some());
        // Prompts that appear draw and take touches above it.
        d.app.on_usb(true);
        tap(&mut d.app, Point::new(300, 100));
        assert!(d.app.usb_prompt.is_none(), "the tap dismissed the USB prompt");
        assert!(d.app.drawer.is_some(), "and didn't reach the drawer");
        assert!(d.app.on_text_input(&TextInput::Insert("q".into())), "typing doesn't reach the screen below");
        assert!(d.app.on_text_input(&TextInput::Dismiss));
        assert!(d.app.drawer.is_none());
    }

    #[test]
    fn sleep_closes_the_drawer_and_a_late_frontlight_reading_fills_it() {
        let mut d = drawer_app(true, true);
        tap(&mut d.app, Point::new(300, 600));
        assert!(d.app.drawer.as_ref().unwrap().layout().sliders.is_empty(), "no reading yet");
        d.app.set_initial_frontlight(Some(Frontlight { brightness: 9, warmth: Some(4) }));
        assert_eq!(d.app.drawer.as_ref().unwrap().layout().sliders.len(), 2);
        // Dragging light reaches the hardware.
        let row = d.app.drawer.as_ref().unwrap().layout().sliders[0];
        let p = Point::new(row.right() - d.app.theme.mm(8.0), row.center().y);
        d.app.on_gesture(Gesture::Down(p));
        let light = d.app.light_request().expect("light changed");
        assert!(light.brightness > 9 && light.warmth == Some(4), "{light:?}");
        assert_eq!(d.app.frontlight, Some(light));
        d.app.on_gesture(Gesture::Tap(p));
        d.app.on_sleep();
        assert!(d.app.drawer.is_none());
        d.app.on_wake();
        assert!(d.app.drawer.is_none());
    }

    #[test]
    fn drawer_tiles_flash_open_screens_turn_and_switch_wifi() {
        use top_drawer::Tile;
        let mut d = drawer_app(true, true);
        tap(&mut d.app, Point::new(300, 600));
        settle(&mut d.app);
        assert_eq!(d.app.drawer.as_ref().unwrap().net().wifi, Some(false), "Wi-Fi read in the background");
        let p = tile(&d.app, Tile::Refresh);
        tap(&mut d.app, p);
        assert!(d.app.drawer.is_none());
        assert_eq!(d.app.frame(), vec![Update { rect: Rect::new(0, 0, 600, 800), mode: RefreshMode::Full }]);
        // Wi-Fi switches in the background; the drawer shows it pending, then on.
        tap(&mut d.app, Point::new(300, 600));
        let gate = d.ops.gated();
        let p = tile(&d.app, Tile::Wifi);
        tap(&mut d.app, p);
        assert_eq!(d.app.drawer.as_ref().unwrap().net().wifi_pending, Some(true));
        assert!(d.app.busy());
        tap(&mut d.app, p);
        gate.send(()).unwrap();
        settle(&mut d.app);
        assert_eq!(d.ops.calls(), ["wifi true"], "the second tap was ignored while pending");
        let net = d.app.drawer.as_ref().unwrap().net().clone();
        assert_eq!((net.wifi, net.wifi_pending), (Some(true), None));
        drop(gate);
        // Orientation saves the choice and asks for the turn.
        let p = tile(&d.app, Tile::Orientation);
        tap(&mut d.app, p);
        assert!(d.app.drawer.is_none());
        assert_eq!(d.app.orientation_request, Some(Orientation::Landscape));
        assert_eq!(d.store.settings().orientation, Orientation::Landscape);
        d.app.orientation_request = None;
        // Settings opens over the screen.
        tap(&mut d.app, Point::new(300, 600));
        let p = tile(&d.app, Tile::Settings);
        tap(&mut d.app, p);
        assert!(d.app.drawer.is_none());
        assert_eq!(d.app.stack.len(), 2);
    }

    #[test]
    fn ssh_tile_starts_the_server_with_the_saved_name_and_failures_show() {
        use top_drawer::Tile;
        let mut d = drawer_app(true, true);
        let mut settings = d.store.settings();
        settings.developer = true;
        settings.hostname = "reader".into();
        d.store.save_settings(&settings).unwrap();
        *d.ops.calls.lock().unwrap() = Vec::new();
        tap(&mut d.app, Point::new(300, 600));
        settle(&mut d.app);
        let p = tile(&d.app, Tile::Ssh);
        tap(&mut d.app, p);
        settle(&mut d.app);
        assert_eq!(d.ops.calls(), ["ssh start reader"]);
        assert!(d.app.drawer.as_ref().unwrap().net().ssh.running);
        d.ops.fail_stop.store(true, std::sync::atomic::Ordering::SeqCst);
        tap(&mut d.app, p);
        settle(&mut d.app);
        assert!(d.app.toast.is_some(), "a failed stop says so");
        assert!(d.app.drawer.as_ref().unwrap().net().ssh.running);
    }

    #[test]
    fn popping_tells_the_screen_below_and_the_drawer_tells_the_screen_it_covers() {
        struct Lower {
            returns: Rc<Cell<usize>>,
            covered: Rc<Cell<usize>>,
        }
        impl Screen for Lower {
            fn draw(&mut self, _: &mut DrawCx) {}
            fn on_gesture(&mut self, g: Gesture, _: &mut EventCx) -> Action {
                if matches!(g, Gesture::Tap(_)) { Action::Push(Box::new(Upper)) } else { Action::None }
            }
            fn allows_top_drawer(&self) -> bool {
                true
            }
            fn top_drawer_opened(&mut self) {
                self.covered.set(self.covered.get() + 1);
            }
            fn on_return(&mut self, _: &mut EventCx) {
                self.returns.set(self.returns.get() + 1);
            }
        }
        struct Upper;
        impl Screen for Upper {
            fn draw(&mut self, _: &mut DrawCx) {}
            fn on_gesture(&mut self, g: Gesture, _: &mut EventCx) -> Action {
                if matches!(g, Gesture::Tap(_)) { Action::Pop } else { Action::None }
            }
        }
        let (returns, covered) = (Rc::new(Cell::new(0)), Rc::new(Cell::new(0)));
        let info =
            DeviceInfo { name: "test".into(), width: 600, height: 800, dpi: 150, color: crate::gfx::ColorMode::Gray8 };
        let lower = Lower { returns: returns.clone(), covered: covered.clone() };
        let mut app = App::new(&info, Theme::for_dpi(150), Box::new(lower));
        let dir = tempfile::tempdir().unwrap();
        let ops = Arc::new(top_drawer::tests::FakeNet::new(false, false));
        app.set_services(Services { store: Store::at(dir.path().to_path_buf()), waker: Waker::new(|| {}), net: ops });
        tap(&mut app, Point::new(300, 400));
        assert_eq!((app.stack.len(), returns.get()), (2, 0));
        tap(&mut app, Point::new(300, 400));
        assert_eq!((app.stack.len(), returns.get()), (1, 1));
        assert_eq!(covered.get(), 0);
        swipe(&mut app, Point::new(300, 5), Point::new(300, 500));
        assert!(app.drawer.is_some());
        assert_eq!(covered.get(), 1);
    }

    #[test]
    fn in_book_drawer_header_closes_the_book_and_opens_search() {
        use crate::screens::TopDrawerBook;
        struct InBook {
            closed: Rc<Cell<usize>>,
            searched: Rc<Cell<usize>>,
        }
        impl Screen for InBook {
            fn draw(&mut self, _: &mut DrawCx) {}
            fn on_gesture(&mut self, _: Gesture, _: &mut EventCx) -> Action {
                Action::None
            }
            fn allows_top_drawer(&self) -> bool {
                true
            }
            fn top_drawer_book(&self) -> Option<TopDrawerBook> {
                Some(TopDrawerBook { title: "Ender's Game".into() })
            }
            fn on_top_drawer_book(&mut self, action: TopDrawerBookAction, _: &mut EventCx) -> Action {
                match action {
                    TopDrawerBookAction::CloseBook => self.closed.set(self.closed.get() + 1),
                    TopDrawerBookAction::Search => self.searched.set(self.searched.get() + 1),
                }
                Action::None
            }
        }
        let (closed, searched) = (Rc::new(Cell::new(0)), Rc::new(Cell::new(0)));
        let info = DeviceInfo {
            name: "test".into(),
            width: 1272,
            height: 1696,
            dpi: 300,
            color: crate::gfx::ColorMode::Gray8,
        };
        let screen = InBook { closed: closed.clone(), searched: searched.clone() };
        let mut app = App::new(&info, Theme::for_dpi(300), Box::new(screen));
        let dir = tempfile::tempdir().unwrap();
        let ops = Arc::new(top_drawer::tests::FakeNet::new(false, false));
        app.set_services(Services { store: Store::at(dir.path().to_path_buf()), waker: Waker::new(|| {}), net: ops });
        // Swipe down opens the in-book drawer; Search closes it and triggers Search.
        swipe(&mut app, Point::new(636, 20), Point::new(636, 900));
        let search = app.drawer.as_ref().unwrap().search_rect().expect("in-book search button").center();
        tap(&mut app, search);
        assert!(app.drawer.is_none());
        assert_eq!((closed.get(), searched.get()), (0, 1));
        // Swipe down again; Back closes it and triggers CloseBook.
        swipe(&mut app, Point::new(636, 20), Point::new(636, 900));
        let back = app.drawer.as_ref().unwrap().back_rect().expect("in-book back button").center();
        tap(&mut app, back);
        assert!(app.drawer.is_none());
        assert_eq!((closed.get(), searched.get()), (1, 1));
    }

    #[test]
    fn dark_mode_tile_saves_relayouts_and_ignores_failed_save() {
        use top_drawer::Tile;
        let mut d = drawer_app(true, true);
        tap(&mut d.app, Point::new(300, 600));
        let p = tile(&d.app, Tile::DarkMode);
        tap(&mut d.app, p);
        assert!(d.app.drawer.is_none());
        assert!(d.store.settings().dark_mode);
        assert!(d.app.canvas.is_dark());
        assert_eq!(d.app.frame(), vec![Update { rect: Rect::new(0, 0, 600, 800), mode: RefreshMode::Full }]);
        let landscape =
            DeviceInfo { name: "test".into(), width: 800, height: 600, dpi: 150, color: crate::gfx::ColorMode::Gray8 };
        d.app.relayout(&landscape, Orientation::Landscape, |_, _| {
            vec![Box::new(Under {
                allows: true,
                taps: d.taps.clone(),
                cancels: d.cancels.clone(),
                swipes: d.swipes.clone(),
                open_on_tap: true,
            })]
        });
        assert!(d.app.canvas.is_dark(), "dark polarity survives relayout");

        // A failed settings save leaves polarity unchanged and shows a toast.
        let settings_path = d._dir.path().join("settings.json");
        std::fs::remove_file(&settings_path).unwrap();
        std::fs::create_dir_all(settings_path.join("block-rename")).unwrap();
        tap(&mut d.app, Point::new(300, 600));
        let p = tile(&d.app, Tile::DarkMode);
        tap(&mut d.app, p);
        assert!(d.app.canvas.is_dark(), "failed save leaves dark mode on");
        assert!(d.app.toast.is_some(), "failed save reports toast");
    }

    #[test]
    fn dark_mode_wait_freezes_canvas_and_commits_on_reply_timeout_sleep_or_relayout() {
        use top_drawer::Tile;
        struct WaitingScreen {
            waiting: Rc<Cell<bool>>,
            calls: Rc<std::cell::RefCell<Vec<bool>>>,
        }
        impl Screen for WaitingScreen {
            fn draw(&mut self, cx: &mut DrawCx) {
                cx.canvas.fill(0xcc);
            }
            fn on_gesture(&mut self, _: Gesture, _: &mut EventCx) -> Action {
                Action::None
            }
            fn allows_top_drawer(&self) -> bool {
                true
            }
            fn set_dark(&mut self, dark: bool, _: &mut EventCx) {
                self.calls.borrow_mut().push(dark);
            }
            fn waiting_for_dark(&self) -> bool {
                self.waiting.get()
            }
        }
        let info =
            DeviceInfo { name: "test".into(), width: 600, height: 800, dpi: 150, color: crate::gfx::ColorMode::Gray8 };
        let waiting = Rc::new(Cell::new(true));
        let calls = Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut app = App::new(
            &info,
            Theme::for_dpi(150),
            Box::new(WaitingScreen { waiting: waiting.clone(), calls: calls.clone() }),
        );
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().to_path_buf());
        let ops = Arc::new(top_drawer::tests::FakeNet::new(false, false));
        app.set_services(Services { store: store.clone(), waker: Waker::new(|| {}), net: ops });
        app.frame();
        let light_pixels = app.canvas.raw().to_vec();

        // 1. Reply arriving within 500 ms commits cleanly.
        swipe(&mut app, Point::new(300, 5), Point::new(300, 500));
        settle(&mut app);
        let p = tile(&app, Tile::DarkMode);
        tap(&mut app, p);
        assert_eq!(*calls.borrow(), [true]);
        assert!(app.busy() && app.deadline().is_some());
        assert!(!app.canvas.is_dark(), "canvas stays light while waiting");
        assert!(app.frame().is_empty(), "closing the drawer must not draw a light frame during the wait");
        assert_eq!(app.canvas.raw(), light_pixels.as_slice(), "canvas pixels stay untouched during wait");
        waiting.set(false);
        app.on_background();
        assert!(app.canvas.is_dark() && !app.busy());
        assert_eq!(app.frame(), vec![Update { rect: Rect::new(0, 0, 600, 800), mode: RefreshMode::Full }]);

        // 2. Timeout (500 ms) commits canvas.dark even while screen still waits.
        waiting.set(true);
        swipe(&mut app, Point::new(300, 5), Point::new(300, 500));
        let p = tile(&app, Tile::DarkMode);
        tap(&mut app, p);
        let due = app.dark_pending.expect("wait started").1;
        assert!(app.canvas.is_dark()); // still dark while switching back to light
        app.on_tick(due);
        assert!(app.dark_pending.is_none() && !app.canvas.is_dark());
        assert_eq!(app.frame(), vec![Update { rect: Rect::new(0, 0, 600, 800), mode: RefreshMode::Full }]);

        // 3. Sleep mid-wait cancels wait and commits canvas.dark immediately.
        swipe(&mut app, Point::new(300, 5), Point::new(300, 500));
        let p = tile(&app, Tile::DarkMode);
        tap(&mut app, p);
        assert!(app.dark_pending.is_some());
        app.on_sleep();
        assert!(app.dark_pending.is_none() && app.canvas.is_dark());
        app.on_wake();

        // 4. Orientation preparation mid-wait cancels wait and commits canvas.dark immediately.
        swipe(&mut app, Point::new(300, 5), Point::new(300, 500));
        let p = tile(&app, Tile::DarkMode);
        tap(&mut app, p);
        assert!(app.dark_pending.is_some());
        app.orientation_request = Some(Orientation::Landscape);
        assert_eq!(app.prepare_orientation(), Some((Orientation::Landscape, RelayoutState::default())));
        assert!(app.dark_pending.is_none() && !app.canvas.is_dark());
    }

    #[test]
    fn dark_mode_broadcasts_across_stack_without_waiting_on_buried_screens() {
        use top_drawer::Tile;
        struct Stacked {
            calls: Rc<std::cell::RefCell<Vec<bool>>>,
            waiting: bool,
            allows: bool,
        }
        impl Screen for Stacked {
            fn draw(&mut self, _: &mut DrawCx) {}
            fn on_gesture(&mut self, _: Gesture, _: &mut EventCx) -> Action {
                Action::None
            }
            fn allows_top_drawer(&self) -> bool {
                self.allows
            }
            fn set_dark(&mut self, dark: bool, _: &mut EventCx) {
                self.calls.borrow_mut().push(dark);
            }
            fn waiting_for_dark(&self) -> bool {
                self.waiting
            }
        }
        let info =
            DeviceInfo { name: "test".into(), width: 600, height: 800, dpi: 150, color: crate::gfx::ColorMode::Gray8 };
        let buried = Rc::new(std::cell::RefCell::new(Vec::new()));
        let top = Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut app = App::new(
            &info,
            Theme::for_dpi(150),
            Box::new(Stacked { calls: buried.clone(), waiting: true, allows: false }),
        );
        app.push_screen(Box::new(Stacked { calls: top.clone(), waiting: false, allows: true }));
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().to_path_buf());
        let ops = Arc::new(top_drawer::tests::FakeNet::new(false, false));
        app.set_services(Services { store, waker: Waker::new(|| {}), net: ops });
        app.frame();
        swipe(&mut app, Point::new(300, 5), Point::new(300, 500));
        let p = tile(&app, Tile::DarkMode);
        tap(&mut app, p);
        assert_eq!(*buried.borrow(), [true], "buried screen receives set_dark");
        assert_eq!(*top.borrow(), [true]);
        assert!(app.dark_pending.is_none() && app.canvas.is_dark(), "does not wait on buried screen");
    }

    #[test]
    fn dark_refresh_action_queues_platform_update_and_flashes_only_when_dark() {
        struct VariantScreen(DarkRefresh);
        impl Screen for VariantScreen {
            fn draw(&mut self, _: &mut DrawCx) {}
            fn on_gesture(&mut self, g: Gesture, _: &mut EventCx) -> Action {
                if matches!(g, Gesture::Tap(_)) { Action::DarkRefresh(self.0) } else { Action::None }
            }
        }
        let info =
            DeviceInfo { name: "test".into(), width: 600, height: 800, dpi: 150, color: crate::gfx::ColorMode::Gray8 };
        let full = DarkRefresh { full_updates: true, ..DarkRefresh::default() };
        let mut app = App::new(&info, Theme::for_dpi(150), Box::new(VariantScreen(full)));
        app.frame();

        tap(&mut app, Point::new(100, 100));
        assert_eq!(app.dark_refresh_request(), Some(full));
        assert_eq!(app.dark_refresh_request(), None);
        assert!(app.frame().is_empty(), "light mode does not flash when switching dark refresh settings");

        app.set_dark(true);
        let driver = DarkRefresh { rendering: crate::platform::DarkRendering::DriverNight, ..full };
        app.stack[0] = Box::new(VariantScreen(driver));
        tap(&mut app, Point::new(100, 100));
        assert_eq!(app.dark_refresh_request(), Some(driver));
        assert_eq!(app.frame(), vec![Update { rect: Rect::new(0, 0, 600, 800), mode: RefreshMode::Full }]);
    }
}
