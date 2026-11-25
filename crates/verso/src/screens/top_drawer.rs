//! The Top Drawer: quick settings pulled down from the top edge over Home,
//! Library and (later) the reader.
//!
//! The app owns the drawer (`App`), drawn above the screen stack and below
//! its prompts and toasts. It shows the clock, battery, Light and Warmth, and
//! a grid of tiles: Wi-Fi, SSH, Orientation, Refresh, Wi-Fi transfer and
//! Settings. Wi-Fi and SSH switch on
//! [`DrawerWorker`]'s thread, so opening the drawer or tapping a tile never
//! waits on `lipc` or the mDNS responder.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::geom::{Point, Rect};
use crate::gfx::{HAlign, Icon, Weight};
use crate::platform::{Frontlight, LightRange, Orientation, RefreshMode, Waker, ssh, wifi};
use crate::screens::TopDrawerBook;
use crate::store::Store;
use crate::ui::gesture::{Direction, Gesture};
use crate::ui::theme::{FILL, INK, INK2, LINE, PAPER, Theme};
use crate::ui::widget::{DrawCx, Press, PressEvent};
use crate::ui::widgets::{IconButton, Slider};

/// While the server runs and Wi-Fi is on but the SSH address or name isn't
/// there yet (Wi-Fi still connecting), the status is read again this often,
/// for this long. Like Settings.
const SSH_RECHECK: Duration = Duration::from_secs(2);
const SSH_RECHECK_FOR: Duration = Duration::from_secs(90);
/// A landscape row of tiles narrower than this wraps into rows of three.
const MIN_ROW_TILE_MM: f32 = 19.0;
/// Sub-labels on an "on" (black) tile.
const ON_SUB: u8 = 0xdd;

/// A quick-settings tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tile {
    Wifi,
    Ssh,
    Orientation,
    Refresh,
    Transfer,
    Settings,
}

/// How a tile looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileState {
    /// White, black outline: off, or an action.
    Off,
    /// Solid black: Wi-Fi on, SSH running, Landscape.
    On,
    /// Light gray with a dashed outline: switching; taps are ignored.
    Pending,
}

/// What a touch on the drawer asks the app to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawerEvent {
    Close,
    /// Close the open book and return to Home/Library.
    CloseBook,
    /// Open in-book search.
    Search,
    /// Light or warmth moved: set the frontlight.
    Light(Frontlight),
    /// Turn Wi-Fi on or off.
    Wifi(bool),
    /// Read the Wi-Fi state again (it couldn't be read).
    Probe,
    /// Start or stop the SSH server.
    Ssh(bool),
    /// Turn the screen.
    Orientation(Orientation),
    /// Flash the whole screen to clear ghosting.
    Refresh,
    /// Open Wi-Fi transfer.
    Transfer,
    /// Open Settings.
    Settings,
}

/// Where everything in the drawer goes, from the screen size alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrawerLayout {
    /// The visible panel, flush with the top, left and right edges.
    pub panel: Rect,
    /// Status strip (Home/Library) or book header (in a book).
    pub status: Rect,
    /// Compact SSH address line below the book header while SSH runs.
    pub ssh_note: Option<Rect>,
    /// Top edges of the hairlines below the status strip and the sliders.
    pub dividers: Vec<i32>,
    /// Whole slider rows (icon, label, slider, value): Light, then Warmth.
    pub sliders: Vec<Rect>,
    pub tiles: Vec<Rect>,
    /// The pill above the bottom edge.
    pub handle: Rect,
}

impl DrawerLayout {
    /// `sliders`: 0 (no frontlight reading), 1 (light) or 2 (light and
    /// warmth). `tiles`: 5, or 6 with SSH. `book`: in-book header instead of
    /// the Home/Library status strip.
    pub fn new(t: &Theme, screen: Rect, sliders: usize, tiles: usize, book: bool, ssh_note: bool) -> Self {
        let wide = screen.w > screen.h;
        let m = t.margin;
        let content_w = screen.w - 2 * m;
        let (status, ssh_rect, div0, after_div0) = if book {
            let pad = t.mm(0.35);
            let head_h = t.mm(if wide { 9.5 } else { 11.2 });
            let status = Rect::new(screen.x + m, screen.y + pad, content_w, head_h);
            if ssh_note {
                let note = Rect::new(screen.x + m, status.bottom(), content_w, t.mm(4.2));
                (status, Some(note), note.bottom() + t.mm(0.6), t.mm(if wide { 1.4 } else { 1.7 }))
            } else {
                (status, None, status.bottom(), t.mm(if wide { 1.4 } else { 1.7 }))
            }
        } else {
            let pad = t.mm(if wide { 1.5 } else { 1.7 });
            let status = Rect::new(screen.x + m, screen.y + pad, content_w, t.mm(6.4));
            (status, None, status.bottom() + t.mm(0.35), t.mm(if wide { 1.5 } else { 1.9 }))
        };
        let mut dividers = vec![div0];
        let mut y = dividers[0] + t.hairline + after_div0;
        let row_h = t.mm(if wide { 6.6 } else { 7.1 });
        let mut rows = Vec::new();
        if sliders > 0 {
            if wide && sliders == 2 {
                // Side by side, so the drawer stays short in landscape.
                let gap = t.mm(4.7);
                let w = (content_w - gap) / 2;
                rows.push(Rect::new(status.x, y, w, row_h));
                rows.push(Rect::new(status.right() - w, y, w, row_h));
                y += row_h;
            } else {
                for i in 0..sliders {
                    if i > 0 {
                        y += t.mm(1.2);
                    }
                    rows.push(Rect::new(status.x, y, content_w, row_h));
                    y += row_h;
                }
            }
            y += t.mm(1.5);
            dividers.push(y);
            y += t.hairline;
        }
        y += t.mm(if wide { 1.7 } else { 2.0 });
        let tile_h = t.mm(13.4);
        let per_row = tile_columns(t, content_w, tiles);
        let mut rects = Vec::with_capacity(tiles);
        let mut left = tiles;
        while left > 0 {
            let n = left.min(per_row);
            let gap = t.mm(if n >= 6 { 1.7 } else { 2.0 });
            // Equal tiles with exact gaps; rounding goes into the tiles.
            let span = content_w + gap;
            for i in 0..n as i32 {
                let x0 = status.x + span * i / n as i32;
                let x1 = status.x + span * (i + 1) / n as i32 - gap;
                rects.push(Rect::new(x0, y, x1 - x0, tile_h));
            }
            left -= n;
            y += tile_h;
            if left > 0 {
                y += t.mm(2.0);
            }
        }
        y += t.mm(if wide { 1.9 } else { 2.2 });
        let handle_w = t.mm(12.2);
        let handle = Rect::new(screen.x + (screen.w - handle_w) / 2, y, handle_w, t.mm(0.8).max(2));
        let bottom = handle.bottom() + t.mm(if wide { 1.7 } else { 2.0 });
        Self {
            panel: Rect::new(screen.x, screen.y, screen.w, (bottom - screen.y).min(screen.h)),
            status,
            ssh_note: ssh_rect,
            dividers,
            sliders: rows,
            tiles: rects,
            handle,
        }
    }
}

/// Tiles per row: one row when the tiles stay wide enough (landscape, and
/// 10.2" portrait), otherwise rows of three (5 = 3 + 2, 6 = 3 × 2).
fn tile_columns(t: &Theme, content_w: i32, tiles: usize) -> usize {
    if tiles <= 3 {
        return tiles.max(1);
    }
    let gap = t.mm(if tiles >= 6 { 1.7 } else { 2.0 });
    let w = (content_w - gap * (tiles as i32 - 1)) / tiles as i32;
    if w >= t.mm(MIN_ROW_TILE_MM) { tiles } else { 3 }
}

/// The parts of one slider row.
struct RowParts {
    icon: Rect,
    label: Rect,
    slider: Rect,
    value: Rect,
}

fn row_parts(t: &Theme, row: Rect, wide: bool) -> RowParts {
    let gap = t.mm(if wide { 1.0 } else { 1.2 });
    let icon = Rect::new(row.x, row.y, t.mm(3.7), row.h);
    let label = Rect::new(icon.right() + gap, row.y, t.mm(if wide { 11.5 } else { 12.4 }), row.h);
    let value_w = t.mm(if wide { 4.7 } else { 5.2 });
    let value = Rect::new(row.right() - value_w, row.y, value_w, row.h);
    let x = label.right() + gap;
    let h = t.mm(6.0);
    let slider = Rect::new(x, row.center().y - h / 2, value.x - gap - x, h);
    RowParts { icon, label, slider, value }
}

/// Wi-Fi and SSH as the drawer shows them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetState {
    /// Whether Wi-Fi is on. None until the first reading arrives, or when it
    /// couldn't be read.
    pub wifi: Option<bool>,
    /// A Wi-Fi reading is on its way ("Checking…" until the first one).
    pub probing: bool,
    /// Turning Wi-Fi on (true) or off.
    pub wifi_pending: Option<bool>,
    pub ssh: ssh::Status,
    /// Starting (true) or stopping the SSH server.
    pub ssh_pending: Option<bool>,
    /// Counts finished switches and readings.
    pub finished: u64,
}

/// What the worker calls: the device, or a fake in tests.
pub trait NetOps: Send + Sync {
    fn wifi_enabled(&self) -> anyhow::Result<bool>;
    fn set_wifi(&self, on: bool) -> anyhow::Result<()>;
    fn ssh_status(&self) -> ssh::Status;
    fn ssh_start(&self, hostname: &str) -> anyhow::Result<()>;
    fn ssh_stop(&self) -> anyhow::Result<()>;
}

/// The real Wi-Fi and SSH controls.
pub struct DeviceNet;

impl NetOps for DeviceNet {
    fn wifi_enabled(&self) -> anyhow::Result<bool> {
        wifi::enabled()
    }
    fn set_wifi(&self, on: bool) -> anyhow::Result<()> {
        wifi::set_enabled(on)
    }
    fn ssh_status(&self) -> ssh::Status {
        ssh::status()
    }
    fn ssh_start(&self, hostname: &str) -> anyhow::Result<()> {
        ssh::start(hostname)
    }
    fn ssh_stop(&self) -> anyhow::Result<()> {
        ssh::stop()
    }
}

enum Job {
    Probe,
    Wifi(bool),
    Ssh { on: bool, hostname: String },
}

struct Shared {
    state: NetState,
    /// Failures to show as toasts.
    errors: Vec<&'static str>,
}

/// Reads and switches Wi-Fi and SSH on one background thread, one job at a
/// time, so quick taps or closing the drawer mid-switch can't race. Lives
/// in the app, not the drawer, so a switch carries on (and its result is
/// kept) after the drawer closes. The thread starts with the first job.
pub struct DrawerWorker {
    ops: Arc<dyn NetOps>,
    shared: Arc<Mutex<Shared>>,
    waker: Waker,
    tx: Option<mpsc::Sender<Job>>,
    join: Option<JoinHandle<()>>,
}

impl DrawerWorker {
    pub fn new(ops: Arc<dyn NetOps>, waker: Waker) -> Self {
        // A pidfile and getifaddrs: no subprocess, fine on the UI thread.
        let ssh = ops.ssh_status();
        let state = NetState { wifi: None, probing: false, wifi_pending: None, ssh, ssh_pending: None, finished: 0 };
        Self { ops, shared: Arc::new(Mutex::new(Shared { state, errors: Vec::new() })), waker, tx: None, join: None }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn state(&self) -> NetState {
        self.lock().state.clone()
    }

    /// Failures since the last call, to show as toasts.
    pub fn take_errors(&self) -> Vec<&'static str> {
        std::mem::take(&mut self.lock().errors)
    }

    /// A reading or switch is on its way.
    pub fn busy(&self) -> bool {
        let s = &self.lock().state;
        s.probing || s.wifi_pending.is_some() || s.ssh_pending.is_some()
    }

    /// Reads the SSH status now, on this thread (cheap: no subprocess).
    pub fn read_ssh(&self) -> ssh::Status {
        let status = self.ops.ssh_status();
        let mut shared = self.lock();
        if shared.state.ssh_pending.is_none() && shared.state.wifi_pending.is_none() {
            shared.state.ssh = status.clone();
        }
        status
    }

    /// Reads Wi-Fi in the background, unless a reading or switch is coming.
    pub fn probe(&mut self) {
        {
            let mut shared = self.lock();
            let s = &mut shared.state;
            if s.probing || s.wifi_pending.is_some() {
                return;
            }
            s.probing = true;
        }
        self.send(Job::Probe);
    }

    /// Turns Wi-Fi on or off in the background (stopping SSH first when
    /// turning it off). False if a Wi-Fi switch is already running.
    pub fn set_wifi(&mut self, on: bool) -> bool {
        {
            let mut shared = self.lock();
            if shared.state.wifi_pending.is_some() {
                return false;
            }
            shared.state.wifi_pending = Some(on);
        }
        self.send(Job::Wifi(on));
        true
    }

    /// Starts (as `hostname`) or stops the SSH server in the background.
    /// False if an SSH switch is already running.
    pub fn set_ssh(&mut self, on: bool, hostname: String) -> bool {
        {
            let mut shared = self.lock();
            if shared.state.ssh_pending.is_some() {
                return false;
            }
            shared.state.ssh_pending = Some(on);
        }
        self.send(Job::Ssh { on, hostname });
        true
    }

    fn send(&mut self, job: Job) {
        if self.tx.is_none() {
            let (tx, rx) = mpsc::channel();
            let (ops, shared, waker) = (self.ops.clone(), self.shared.clone(), self.waker.clone());
            let spawned = std::thread::Builder::new()
                .name("verso-net".into())
                .spawn(move || run_jobs(&*ops, &shared, &waker, rx));
            match spawned {
                Ok(join) => {
                    self.tx = Some(tx);
                    self.join = Some(join);
                }
                Err(e) => {
                    log::error!("starting the Wi-Fi thread: {e:#}");
                    let mut shared = self.lock();
                    let s = &mut shared.state;
                    (s.probing, s.wifi_pending, s.ssh_pending) = (false, None, None);
                    shared.errors.push("Couldn't switch Wi-Fi or SSH");
                    return;
                }
            }
        }
        if let Some(tx) = &self.tx {
            let _ = tx.send(job);
        }
    }
}

impl Drop for DrawerWorker {
    /// Lets a switch in progress finish (a few seconds at most), so Wi-Fi
    /// and SSH are never left half-switched when Verso exits.
    fn drop(&mut self) {
        self.tx = None;
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn read_wifi(ops: &dyn NetOps) -> Option<bool> {
    match ops.wifi_enabled() {
        Ok(on) => Some(on),
        Err(e) => {
            log::error!("Wi-Fi state: {e:#}");
            None
        }
    }
}

fn run_jobs(ops: &dyn NetOps, shared: &Mutex<Shared>, waker: &Waker, rx: mpsc::Receiver<Job>) {
    let lock = || shared.lock().unwrap_or_else(|e| e.into_inner());
    for job in rx {
        match job {
            Job::Probe => {
                let wifi = read_wifi(ops);
                let ssh = ops.ssh_status();
                let mut shared = lock();
                let s = &mut shared.state;
                s.probing = false;
                if s.wifi_pending.is_none() {
                    s.wifi = wifi;
                }
                if s.ssh_pending.is_none() {
                    s.ssh = ssh;
                }
                s.finished += 1;
            }
            Job::Wifi(on) => {
                // Stop SSH before losing the network. If stopping fails,
                // leave Wi-Fi on so the server can still be reached and
                // stopped (like Settings).
                let mut error = None;
                if !on
                    && ops.ssh_status().running
                    && let Err(e) = ops.ssh_stop()
                {
                    log::error!("stopping SSH before Wi-Fi off: {e:#}");
                    error = Some("Couldn't stop SSH. Wi-Fi left on.");
                } else if let Err(e) = ops.set_wifi(on) {
                    log::error!("Wi-Fi: {e:#}");
                    error = Some(if on { "Couldn't turn Wi-Fi on" } else { "Couldn't turn Wi-Fi off" });
                }
                let wifi = read_wifi(ops);
                let ssh = ops.ssh_status();
                let mut shared = lock();
                shared.errors.extend(error);
                let s = &mut shared.state;
                s.wifi_pending = None;
                s.wifi = wifi;
                if s.ssh_pending.is_none() {
                    s.ssh = ssh;
                }
                s.finished += 1;
            }
            Job::Ssh { on, hostname } => {
                let result = if on { ops.ssh_start(&hostname) } else { ops.ssh_stop() };
                let error = result.err().map(|e| {
                    log::error!("SSH server: {e:#}");
                    if on { "Couldn't start SSH" } else { "Couldn't stop SSH" }
                });
                let ssh = ops.ssh_status();
                let mut shared = lock();
                shared.errors.extend(error);
                let s = &mut shared.state;
                s.ssh_pending = None;
                s.ssh = ssh;
                s.finished += 1;
            }
        }
        waker.wake();
    }
}

/// Keeps the name the responder won (`verso-2` if another device had
/// `verso`), so the device is found by the same name next time.
pub fn remember_hostname(store: &Store, ssh: &ssh::Status) {
    let Some(name) = &ssh.hostname else { return };
    let mut settings = store.settings();
    if &settings.hostname == name {
        return;
    }
    settings.hostname = name.clone();
    if let Err(e) = store.save_settings(&settings) {
        log::error!("saving the device name: {e:#}");
    }
}

/// The open drawer. Created when it opens and dropped when it closes.
pub struct TopDrawer {
    screen: Rect,
    orientation: Orientation,
    /// Open book context when pulled down inside a book; None on Home/Library.
    book: Option<TopDrawerBook>,
    back: Option<IconButton>,
    search: Option<IconButton>,
    layout: DrawerLayout,
    light: Option<Slider>,
    warmth: Option<Slider>,
    /// The sliders' top step on this model.
    range: LightRange,
    tiles: Vec<(Tile, Press)>,
    net: NetState,
    clock: Option<String>,
    clock_due: Instant,
    /// The current touch started below the drawer.
    outside: bool,
    ssh_read_at: Instant,
    ssh_recheck_until: Instant,
}

impl TopDrawer {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        t: &Theme,
        screen: Rect,
        orientation: Orientation,
        light: Option<Frontlight>,
        range: LightRange,
        net: NetState,
        book: Option<TopDrawerBook>,
        now: Instant,
    ) -> Self {
        let mut drawer = Self {
            range,
            screen,
            orientation,
            book,
            back: None,
            search: None,
            layout: DrawerLayout::new(t, screen, 0, 5, false, false),
            light: None,
            warmth: None,
            tiles: Vec::new(),
            net,
            clock: None,
            clock_due: now,
            outside: false,
            ssh_read_at: now,
            ssh_recheck_until: now + SSH_RECHECK_FOR,
        };
        drawer.read_clock(now);
        if let Some(light) = light {
            drawer.make_sliders(light);
        }
        drawer.place(t);
        drawer
    }

    /// The visible panel.
    #[cfg(test)]
    pub fn rect(&self) -> Rect {
        self.layout.panel
    }

    pub fn net(&self) -> &NetState {
        &self.net
    }

    /// Snapshot of visible control state that `on_gesture` can change without
    /// emitting an event (pressed header buttons or non-pending tiles).
    pub(crate) fn visual_state(&self) -> (bool, bool, u32) {
        let back = self.back.as_ref().is_some_and(IconButton::is_pressed);
        let search = self.search.as_ref().is_some_and(IconButton::is_pressed);
        let mut tiles = 0u32;
        for (i, (tile, press)) in self.tiles.iter().enumerate() {
            if press.is_down() && self.tile_view(*tile).0 != TileState::Pending {
                tiles |= 1 << i;
            }
        }
        (back, search, tiles)
    }

    #[cfg(test)]
    pub fn layout(&self) -> &DrawerLayout {
        &self.layout
    }

    #[cfg(test)]
    pub fn back_rect(&self) -> Option<Rect> {
        self.back.as_ref().map(IconButton::rect)
    }

    #[cfg(test)]
    pub fn search_rect(&self) -> Option<Rect> {
        self.search.as_ref().map(IconButton::rect)
    }

    /// Toggles first, then actions.
    pub fn tile_kinds(&self) -> Vec<Tile> {
        vec![Tile::Wifi, Tile::Ssh, Tile::Orientation, Tile::Refresh, Tile::Transfer, Tile::Settings]
    }

    /// One step per powerd step, like the Kindle's own sliders.
    fn make_sliders(&mut self, light: Frontlight) {
        let (b, w) = (self.range.brightness as f32, self.range.warmth as f32);
        self.light = Some(Slider::new(0.0, b, 1.0, (light.brightness as f32).min(b)));
        self.warmth = light.warmth.map(|v| Slider::new(0.0, w, 1.0, (v as f32).min(w)));
    }

    fn book_narrow(&self, t: &Theme) -> bool {
        self.screen.w < t.mm(100.0)
    }

    fn place(&mut self, t: &Theme) {
        let kinds = self.tile_kinds();
        let sliders = usize::from(self.light.is_some()) + usize::from(self.warmth.is_some());
        let in_book = self.book.is_some();
        let ssh_note = in_book && self.net_note().is_some();
        self.layout = DrawerLayout::new(t, self.screen, sliders, kinds.len(), in_book, ssh_note);
        if in_book {
            let s = self.layout.status;
            let mut back = IconButton::new(Icon::ArrowLeft);
            back.place_at(Point::new(self.screen.x + t.margin + t.target / 2 - t.mm(2.0), s.center().y), t);
            let bat_w = t.mm(13.0);
            let search_right = if self.book_narrow(t) {
                s.right() - bat_w - t.mm(1.0)
            } else {
                s.right() - bat_w - t.mm(2.2) - t.mm(8.5) - t.mm(0.5)
            };
            let mut search = IconButton::new(Icon::Search);
            search.place_at(Point::new(search_right - t.target / 2, s.center().y), t);
            self.back = Some(back);
            self.search = Some(search);
        } else {
            self.back = None;
            self.search = None;
        }
        self.tiles = kinds.into_iter().map(|k| (k, Press::default())).collect();
        let wide = self.screen.w > self.screen.h;
        let rows = self.layout.sliders.clone();
        for (slider, row) in [&mut self.light, &mut self.warmth].into_iter().zip(rows) {
            if let Some(s) = slider {
                s.place(row_parts(t, row, wide).slider, t);
            }
        }
    }

    /// Adds the light controls if the drawer opened before the startup
    /// frontlight reading arrived. True if the drawer changed.
    pub fn set_frontlight(&mut self, t: &Theme, light: Frontlight) -> bool {
        if self.light.is_some() {
            return false;
        }
        self.make_sliders(light);
        self.place(t);
        true
    }

    /// New Wi-Fi/SSH state from the worker.
    pub fn set_net(&mut self, t: &Theme, net: NetState, now: Instant) {
        let finished = net.finished != self.net.finished;
        let before = self.tile_kinds();
        let had_note = self.layout.ssh_note.is_some();
        self.net = net;
        let wants_note = self.book.is_some() && self.net_note().is_some();
        if self.tile_kinds() != before || wants_note != had_note {
            self.place(t);
        }
        if finished {
            // A switch just finished: Wi-Fi may still be connecting.
            self.ssh_read_at = now;
            self.ssh_recheck_until = now + SSH_RECHECK_FOR;
        }
    }

    fn ssh_incomplete(&self) -> bool {
        let s = &self.net;
        s.ssh.running
            && s.ssh_pending.is_none()
            && s.wifi == Some(true)
            && (s.ssh.address.is_none() || s.ssh.hostname.is_none())
    }

    fn ssh_recheck_due(&self) -> Option<Instant> {
        (self.ssh_incomplete() && self.ssh_read_at < self.ssh_recheck_until)
            .then(|| (self.ssh_read_at + SSH_RECHECK).min(self.ssh_recheck_until))
    }

    fn read_clock(&mut self, now: Instant) {
        self.clock = crate::clock::LocalTime::now().map(|t| t.h24());
        let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        self.clock_due = now + Duration::from_secs(60 - unix % 60);
    }

    /// The next minute (for the clock), or the next SSH re-read.
    pub fn deadline(&self) -> Option<Instant> {
        Some(self.ssh_recheck_due().map_or(self.clock_due, |d| d.min(self.clock_due)))
    }

    /// Updates the clock and re-reads an incomplete SSH status when due.
    /// True if anything visible changed.
    pub fn on_tick(&mut self, now: Instant, worker: &DrawerWorker) -> bool {
        let mut changed = false;
        if now >= self.clock_due {
            let before = self.clock.clone();
            self.read_clock(now);
            changed |= self.clock != before;
        }
        if self.ssh_recheck_due().is_some_and(|d| now >= d) {
            self.ssh_read_at = now;
            let ssh = worker.read_ssh();
            if ssh != self.net.ssh {
                self.net.ssh = ssh;
                changed = true;
            }
        }
        changed
    }

    /// The look and labels of a tile. Transfer has long labels in the
    /// five-tile grid, where its tile is wider.
    pub fn tile_view(&self, tile: Tile) -> (TileState, Icon, &'static str, String) {
        let six = self.tiles.len() >= 6;
        match tile {
            Tile::Wifi => {
                let (state, sub) = match (self.net.wifi_pending, self.net.wifi) {
                    (Some(true), _) => (TileState::Pending, "Turning on…"),
                    (Some(false), _) => (TileState::Pending, "Turning off…"),
                    (None, Some(true)) => (TileState::On, "On"),
                    (None, Some(false)) => (TileState::Off, "Off"),
                    (None, None) if self.net.probing => (TileState::Off, "Checking…"),
                    (None, None) => (TileState::Off, "Unknown"),
                };
                (state, Icon::Wifi, "Wi-Fi", sub.to_owned())
            }
            Tile::Ssh => {
                let (state, sub) = match self.net.ssh_pending {
                    Some(true) => (TileState::Pending, "Starting…".to_owned()),
                    Some(false) => (TileState::Pending, "Stopping…".to_owned()),
                    None if self.net.ssh.running => (TileState::On, format!("On · :{}", ssh::PORT)),
                    None if self.net.ssh.unavailable.is_some() => (TileState::Off, "Unavailable".to_owned()),
                    None => (TileState::Off, "Off".to_owned()),
                };
                (state, Icon::Terminal, "SSH", sub)
            }
            Tile::Orientation => match self.orientation {
                Orientation::Portrait => (TileState::Off, Icon::Portrait, "Orientation", "Portrait".to_owned()),
                Orientation::Landscape => (TileState::On, Icon::Landscape, "Orientation", "Landscape".to_owned()),
            },
            Tile::Refresh => (TileState::Off, Icon::Refresh, "Refresh", "Clear screen".to_owned()),
            Tile::Transfer if six => (TileState::Off, Icon::Globe, "Transfer", "Web app".to_owned()),
            Tile::Transfer => (TileState::Off, Icon::Globe, "Wi-Fi transfer", "Books & files".to_owned()),
            Tile::Settings => (TileState::Off, Icon::Settings, "Settings", "All options".to_owned()),
        }
    }

    pub fn on_gesture(&mut self, g: &Gesture) -> Option<DrawerEvent> {
        if let Gesture::Down(p) = *g {
            self.outside = !self.layout.panel.contains(p);
        }
        // A slider owns a touch that started on it, wherever it ends. Their
        // touch areas can overlap a little: light takes the touch first.
        let mut moved = false;
        if let Some(s) = &mut self.light {
            moved |= s.on_gesture(g).is_some();
        }
        if !self.light.as_ref().is_some_and(Slider::is_dragging)
            && let Some(s) = &mut self.warmth
        {
            moved |= s.on_gesture(g).is_some();
        }
        if moved {
            return Some(DrawerEvent::Light(Frontlight {
                brightness: self.light.as_ref().map_or(0, |s| s.value as u8),
                warmth: self.warmth.as_ref().map(|s| s.value as u8),
            }));
        }
        if self.light.as_ref().is_some_and(Slider::is_dragging) || self.warmth.as_ref().is_some_and(Slider::is_dragging)
        {
            return None;
        }
        if self.outside {
            return matches!(g, Gesture::Tap(_) | Gesture::Swipe { dir: Direction::Up, .. })
                .then_some(DrawerEvent::Close);
        }
        if matches!(g, Gesture::Swipe { dir: Direction::Up, .. }) {
            return Some(DrawerEvent::Close);
        }
        if let Some(back) = &mut self.back
            && back.on_gesture(g)
        {
            return Some(DrawerEvent::CloseBook);
        }
        if let Some(search) = &mut self.search
            && search.on_gesture(g)
        {
            return Some(DrawerEvent::Search);
        }
        let mut tapped = None;
        for (i, (tile, press)) in self.tiles.iter_mut().enumerate() {
            let area = self.layout.tiles.get(i).copied().unwrap_or_default();
            if press.on_gesture(g, area) == Some(PressEvent::Tap) {
                tapped = Some(*tile);
            }
        }
        self.activate(tapped?)
    }

    fn activate(&self, tile: Tile) -> Option<DrawerEvent> {
        let net = &self.net;
        match tile {
            Tile::Wifi if net.wifi_pending.is_some() => None,
            Tile::Wifi => match net.wifi {
                Some(on) => Some(DrawerEvent::Wifi(!on)),
                None if net.probing => None,
                None => Some(DrawerEvent::Probe),
            },
            Tile::Ssh if net.ssh_pending.is_some() => None,
            Tile::Ssh if !net.ssh.running && net.ssh.unavailable.is_some() => None,
            Tile::Ssh => Some(DrawerEvent::Ssh(!net.ssh.running)),
            Tile::Orientation => Some(DrawerEvent::Orientation(match self.orientation {
                Orientation::Portrait => Orientation::Landscape,
                Orientation::Landscape => Orientation::Portrait,
            })),
            Tile::Refresh => Some(DrawerEvent::Refresh),
            Tile::Transfer => Some(DrawerEvent::Transfer),
            Tile::Settings => Some(DrawerEvent::Settings),
        }
    }

    pub fn draw(&mut self, cx: &mut DrawCx) {
        let t = cx.theme;
        let r = self.layout.panel;
        // Like a bottom sheet turned over: the top edge is off screen, and
        // the side borders just past the screen edges, so only the two
        // bottom corners show, rounded.
        let extra = t.radius_sheet as i32 * 2;
        let shape = Rect::new(r.x - t.border, r.y - extra, r.w + 2 * t.border, r.h + extra);
        cx.canvas.fill_rounded_rect(shape, t.radius_sheet, PAPER);
        cx.canvas.stroke_rounded_rect(shape, t.radius_sheet, t.border as f32, INK);
        self.draw_status(cx);
        for &y in &self.layout.dividers {
            cx.fill(Rect::new(self.layout.status.x, y, self.layout.status.w, t.hairline), LINE);
        }
        self.draw_sliders(cx);
        for i in 0..self.tiles.len() {
            self.draw_tile(cx, i);
        }
        let h = self.layout.handle;
        cx.canvas.fill_rounded_rect(h, h.h as f32 / 2.0, LINE);
    }

    fn draw_status(&self, cx: &mut DrawCx) {
        let t = cx.theme;
        let s = self.layout.status;
        if let Some(book) = &self.book {
            if let Some(back) = &self.back {
                back.draw(cx);
            }
            if let Some(search) = &self.search {
                search.draw(cx);
            }
            let mut right = s.right();
            if let Some(battery) = cx.battery {
                let label = format!("{}%", battery.percent.min(100));
                let w = t.mm(5.4) + t.mm(0.8) + cx.text.width(&label, t.small).ceil() as i32 + 2;
                let rect = Rect::new(s.right() - w, s.y, w, s.h);
                cx.battery_indicator(rect);
                right = rect.x - t.mm(2.2);
            }
            let search_right = self.search.as_ref().map_or(s.right(), |b| b.rect().right());
            if !self.book_narrow(t)
                && let Some(clock) = &self.clock
            {
                let clock_x = search_right + t.mm(0.5);
                if right > clock_x {
                    cx.line(clock, t.small.gray(INK2), Rect::new(clock_x, s.y, right - clock_x, s.h), HAlign::Right);
                }
            }
            let title_x = self.screen.x + t.margin + t.target - t.mm(1.0);
            let title_right = self.search.as_ref().map_or(right, |b| b.rect().x - t.mm(1.0));
            if title_right > title_x {
                cx.line(&book.title, t.button, Rect::new(title_x, s.y, title_right - title_x, s.h), HAlign::Left);
            }
            if let (Some(note_rect), Some(note)) = (self.layout.ssh_note, self.net_note()) {
                cx.line(&format!("SSH · {note}"), t.caption.gray(INK2), note_rect, HAlign::Left);
            }
            return;
        }
        let mut right = s.right();
        if let Some(battery) = cx.battery {
            let label = format!("{}%", battery.percent.min(100));
            let w = t.mm(5.4) + t.mm(0.8) + cx.text.width(&label, t.small).ceil() as i32 + 2;
            let rect = Rect::new(s.right() - w, s.y, w, s.h);
            cx.battery_indicator(rect);
            right = rect.x - t.mm(3.0);
        }
        let mut x = s.x;
        if let Some(clock) = &self.clock {
            let style = t.heading;
            cx.line(clock, style, Rect::new(x, s.y, (right - x).max(0), s.h), HAlign::Left);
            x += cx.text.width(clock, style).ceil() as i32 + t.mm(2.4);
        }
        if let Some(net) = self.net_note()
            && x < right
        {
            cx.line(&net, t.caption.gray(INK2), Rect::new(x, s.y, right - x, s.h), HAlign::Left);
        }
    }

    /// The SSH address beside the clock, while the server runs.
    fn net_note(&self) -> Option<String> {
        let ssh = &self.net.ssh;
        if !ssh.running {
            return None;
        }
        Some(match (&ssh.hostname, &ssh.address) {
            (Some(host), Some(ip)) => format!("{host}.local · {ip}"),
            (None, Some(ip)) => ip.clone(),
            _ if self.net.wifi == Some(false) => "Wi-Fi is off".to_owned(),
            _ => "Waiting for Wi-Fi…".to_owned(),
        })
    }

    fn draw_sliders(&self, cx: &mut DrawCx) {
        let t = cx.theme;
        let wide = self.screen.w > self.screen.h;
        let rows = [(Icon::Sun, "Light", &self.light), (Icon::Flame, "Warmth", &self.warmth)];
        for ((icon, label, slider), row) in rows.into_iter().zip(&self.layout.sliders) {
            let Some(slider) = slider else { continue };
            let parts = row_parts(t, *row, wide);
            cx.icon(icon, parts.icon, t.mm(3.7), INK);
            cx.line(label, t.body, parts.label, HAlign::Left);
            slider.draw(cx);
            let value = format!("{}", slider.value.round() as i32);
            cx.line(&value, t.small.weight(Weight::SemiBold).gray(INK2), parts.value, HAlign::Right);
        }
    }

    fn draw_tile(&self, cx: &mut DrawCx, i: usize) {
        let t = cx.theme;
        let (tile, press) = &self.tiles[i];
        let Some(&rect) = self.layout.tiles.get(i) else { return };
        let (state, icon, mut title, mut sub) = self.tile_view(*tile);
        let pressed = press.is_down() && state != TileState::Pending;
        // Pressed tiles invert, like buttons.
        let dark = (state == TileState::On) != pressed;
        let (fill, ink, sub_gray) = match state {
            TileState::Pending => (FILL, INK, INK2),
            _ if dark => (INK, PAPER, ON_SUB),
            _ => (PAPER, INK, INK2),
        };
        cx.canvas.fill_rounded_rect(rect, t.radius, fill);
        if state == TileState::Pending {
            cx.canvas.stroke_dashed_rounded_rect(rect, t.radius, t.border as f32, t.mm(1.2) as f32, INK2);
        } else {
            cx.canvas.stroke_rounded_rect(rect, t.radius, t.border as f32, INK);
        }
        let title_style = t.small.weight(Weight::SemiBold).gray(ink);
        let sub_style = t.caption.gray(sub_gray);
        let inner = rect.w - 2 * t.mm(1.2);
        if *tile == Tile::Transfer && cx.text.width(title, title_style) > inner as f32 {
            // Narrow five-tile rows (6" landscape) take the short labels.
            (title, sub) = ("Transfer", "Web app".to_owned());
        }
        let icon_size = t.mm(3.9);
        let gap = t.mm(0.7);
        let title_h = title_style.line_height().ceil() as i32;
        let sub_h = sub_style.line_height().ceil() as i32;
        let total = icon_size + gap + title_h + sub_h;
        let mut y = rect.y + (rect.h - total) / 2;
        let x = rect.x + t.mm(1.2);
        cx.icon(icon, Rect::new(rect.x, y, rect.w, icon_size), icon_size, ink);
        y += icon_size + gap;
        cx.line(title, title_style, Rect::new(x, y, inner, title_h), HAlign::Center);
        y += title_h;
        cx.line(&sub, sub_style, Rect::new(x, y, inner, sub_h), HAlign::Center);
        if pressed {
            cx.hint(rect, RefreshMode::Fast);
        }
    }
}

#[cfg(test)]
#[path = "top_drawer_tests.rs"]
pub(crate) mod tests;
