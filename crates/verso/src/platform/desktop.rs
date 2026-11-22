//! Desktop simulator: shows the Kindle screen in a window.
//!
//! - The window keeps the panel's aspect ratio and scales to fit (start size
//!   set by `VERSO_SIM_SCALE`, default 0.5). `--device <name>` or
//!   `VERSO_SIM_DEVICE` picks a Kindle model (sim_device.rs); its screen size
//!   and density can be changed with `VERSO_SIM_SIZE=1072x1448` and
//!   `VERSO_SIM_DPI=300`, its lights with `VERSO_SIM_LIGHT=none|white|warm`.
//! - `VERSO_SIM_BATTERY=82` shows a battery at 82%; `82+` is charging.
//! - The left mouse button acts as a finger.
//! - Keys: Q / Esc quit · S sleep/wake · V toggle refresh visualisation ·
//!   P save a screenshot to `screenshots/`.
//! - Like the real panel, `Fast` refreshes show only black and white.
//! - With visualisation on, refreshed areas flash a colour for a moment:
//!   red = Fast, blue = Ui, green = Page, orange = Full.

use std::collections::VecDeque;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, Ime, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};
use winit::window::{Window, WindowId};

use super::sim_device::SimDevice;
use super::{
    DeviceInfo, Event, Frontlight, Orientation, Platform, RefreshMode, TextInput, Touch, TouchPhase, Update, Waker,
};
use crate::geom::{Point, Rect};
use crate::gfx::{Canvas, save_gray_png};

const FLASH_TIME: Duration = Duration::from_millis(350);
const LETTERBOX: u32 = 0x20_20_20;

pub struct Desktop {
    info: DeviceInfo,
    /// VERSO_SIM_SIZE: the panel in portrait.
    native: (u32, u32),
    event_loop: EventLoop<()>,
    sim: Sim,
    device: SimDevice,
}

impl Desktop {
    pub fn open(device: SimDevice) -> Result<Self> {
        let (width, height, dpi) = (device.width, device.height, device.dpi);
        let info = DeviceInfo { name: format!("Simulator ({})", device.name), width, height, dpi };

        let mut event_loop = EventLoop::<()>::with_user_event().build().context("can't open a window (no display?)")?;
        let mut sim = Sim::new(width as usize, height as usize, device.scale);
        sim.title = format!("Verso simulator · {}", device.label());
        sim.light = device.initial_light();
        // Let winit create the window.
        let start = Instant::now();
        while sim.surface.is_none() {
            if let Some(e) = sim.error.take() {
                return Err(e);
            }
            if start.elapsed() > Duration::from_secs(5) {
                return Err(anyhow!("the simulator window didn't appear"));
            }
            if let PumpStatus::Exit(_) = event_loop.pump_app_events(Some(Duration::from_millis(10)), &mut sim) {
                return Err(anyhow!("window closed during startup"));
            }
        }
        log::info!("simulator keys: Q/Esc quit · S sleep/wake · V refresh visualisation · P screenshot");
        Ok(Self { info, native: (width, height), event_loop, sim, device })
    }
}

impl Platform for Desktop {
    fn battery(&mut self) -> Option<super::Battery> {
        sim_battery()
    }
    fn waker(&self) -> Waker {
        let proxy = self.event_loop.create_proxy();
        Waker::new(move || {
            let _ = proxy.send_event(());
        })
    }
    fn unhandled_text_input(&mut self, input: &TextInput, repeat: bool) {
        if repeat {
            return;
        }
        match input {
            TextInput::Insert(text) => self.sim.key(&Key::Character(text.as_str().into())),
            TextInput::Dismiss => self.sim.key(&Key::Named(NamedKey::Escape)),
            _ => {}
        }
    }
    fn frontlight(&mut self) -> Option<Frontlight> {
        self.sim.light
    }
    fn set_frontlight(&mut self, light: Frontlight) -> Result<()> {
        self.sim.light = self.device.accept_light(light);
        self.sim.dirty = true;
        Ok(())
    }

    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn session_details(&self) -> Vec<String> {
        vec![self.device.details()]
    }

    fn set_orientation(&mut self, orientation: Orientation) -> Result<()> {
        // Unlike the Kindle, the simulator turns its window instead of
        // rotating pixels, which is easier to use with a mouse.
        let (w, h) = orientation.screen_size(self.native.0, self.native.1);
        (self.info.width, self.info.height) = (w, h);
        let sim = &mut self.sim;
        if (sim.width, sim.height) == (w as usize, h as usize) {
            return Ok(());
        }
        (sim.width, sim.height) = (w as usize, h as usize);
        sim.panel = vec![255; sim.width * sim.height];
        sim.flashes.clear();
        if let Some(window) = &sim.window {
            let size = window.inner_size();
            // Keep the panel's scale: swap the window's sides.
            let _ = window.request_inner_size(winit::dpi::PhysicalSize::new(size.height, size.width));
        }
        sim.dirty = true;
        Ok(())
    }

    fn next_event(&mut self, timeout: Option<Duration>) -> Result<Option<Event>> {
        let deadline = timeout.map(|t| Instant::now() + t);
        // Even with a zero timeout, look at the window's events once.
        let mut pumped = false;
        loop {
            if let Some(e) = self.sim.error.take() {
                return Err(e);
            }
            if let Some(e) = self.sim.events.pop_front() {
                return Ok(Some(e));
            }
            let now = Instant::now();
            if self.sim.flash_until.is_some_and(|t| now >= t) {
                self.sim.flash_until = None;
                self.sim.flashes.clear();
                self.sim.dirty = true;
            }
            if self.sim.dirty {
                self.sim.paint()?;
            }
            if pumped && deadline.is_some_and(|d| now >= d) {
                return Ok(None);
            }
            pumped = true;
            let wake = [deadline, self.sim.flash_until].into_iter().flatten().min();
            let wait = wake.map(|w| w.saturating_duration_since(now));
            if let PumpStatus::Exit(_) = self.event_loop.pump_app_events(wait, &mut self.sim) {
                return Ok(Some(Event::Quit));
            }
        }
    }

    fn present(&mut self, canvas: &Canvas, updates: &[Update]) -> Result<()> {
        let sim = &mut self.sim;
        for u in updates {
            canvas.copy_gray(u.rect, &mut sim.panel, sim.width);
            if u.mode == RefreshMode::Fast {
                sim.threshold(u.rect);
            }
        }
        if sim.visualize {
            sim.flashes.extend_from_slice(updates);
            sim.flash_until = Some(Instant::now() + FLASH_TIME);
        }
        sim.paint()
    }

    fn wait_for_complete(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Where the panel sits inside the window.
#[derive(Clone, Copy)]
struct View {
    scale: f64,
    ox: f64,
    oy: f64,
}

struct Sim {
    width: usize,
    height: usize,
    start_scale: f64,
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    /// What the simulated e-ink panel currently shows (8-bit gray).
    panel: Vec<u8>,
    events: VecDeque<Event>,
    cursor: Option<PhysicalPosition<f64>>,
    mouse_down: bool,
    view: View,
    visualize: bool,
    flashes: Vec<Update>,
    flash_until: Option<Instant>,
    asleep: bool,
    /// `None`: the device has no frontlight.
    light: Option<Frontlight>,
    title: String,
    dirty: bool,
    error: Option<anyhow::Error>,
}

impl Sim {
    fn new(width: usize, height: usize, start_scale: f64) -> Self {
        Self {
            width,
            height,
            start_scale,
            window: None,
            surface: None,
            panel: vec![255; width * height],
            events: VecDeque::new(),
            cursor: None,
            mouse_down: false,
            view: View { scale: start_scale, ox: 0.0, oy: 0.0 },
            visualize: true,
            flashes: Vec::new(),
            flash_until: None,
            asleep: false,
            light: None,
            title: "Verso simulator".into(),
            dirty: true,
            error: None,
        }
    }

    fn create_window(&mut self, el: &ActiveEventLoop) -> Result<()> {
        let size = LogicalSize::new(self.width as f64 * self.start_scale, self.height as f64 * self.start_scale);
        let attrs = Window::default_attributes().with_title(&self.title).with_inner_size(size);
        let window = Rc::new(el.create_window(attrs)?);
        window.set_ime_allowed(true);
        let context = softbuffer::Context::new(window.clone()).map_err(|e| anyhow!("softbuffer: {e}"))?;
        let surface = softbuffer::Surface::new(&context, window.clone()).map_err(|e| anyhow!("softbuffer: {e}"))?;
        self.window = Some(window);
        self.surface = Some(surface);
        self.dirty = true;
        Ok(())
    }

    /// `Fast` (DU) refreshes can only show black or white.
    fn threshold(&mut self, rect: Rect) {
        let Some(r) = rect.intersect(&Rect::new(0, 0, self.width as i32, self.height as i32)) else { return };
        for y in r.y..r.bottom() {
            let row = y as usize * self.width;
            for v in &mut self.panel[row + r.x as usize..row + r.right() as usize] {
                *v = if *v < 128 { 0 } else { 255 };
            }
        }
    }

    fn paint(&mut self) -> Result<()> {
        self.dirty = false;
        let (Some(window), Some(surface)) = (&self.window, &mut self.surface) else { return Ok(()) };
        let size = window.inner_size();
        let (Some(ww), Some(wh)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
            return Ok(());
        };
        surface.resize(ww, wh).map_err(|e| anyhow!("softbuffer: {e}"))?;
        let (ww, wh) = (size.width as usize, size.height as usize);

        let scale = (ww as f64 / self.width as f64).min(wh as f64 / self.height as f64);
        let (dw, dh) = ((self.width as f64 * scale) as usize, (self.height as f64 * scale) as usize);
        let (ox, oy) = ((ww - dw) / 2, (wh - dh) / 2);
        self.view = View { scale, ox: ox as f64, oy: oy as f64 };

        // For each window column/row, the range of panel pixels it covers
        // (averaged when shrinking, so text stays legible).
        let spans = |n: usize, limit: usize| -> Vec<(usize, usize)> {
            (0..n)
                .map(|i| {
                    let a = ((i as f64 / scale) as usize).min(limit - 1);
                    let b = (((i + 1) as f64 / scale) as usize).clamp(a + 1, limit);
                    (a, b)
                })
                .collect()
        };
        let xs = spans(dw, self.width);
        let ys = spans(dh, self.height);

        let mut buf = surface.buffer_mut().map_err(|e| anyhow!("softbuffer: {e}"))?;
        buf.fill(LETTERBOX);
        for (dy, &(y0, y1)) in ys.iter().enumerate() {
            let out = &mut buf[(oy + dy) * ww + ox..(oy + dy) * ww + ox + dw];
            for (dx, &(x0, x1)) in xs.iter().enumerate() {
                let mut sum = 0u32;
                for y in y0..y1 {
                    let row = &self.panel[y * self.width..];
                    sum += row[x0..x1].iter().map(|&v| v as u32).sum::<u32>();
                }
                let g = sum / ((y1 - y0) * (x1 - x0)) as u32;
                let (mut r, mut gr, mut b) = (g, g, g);
                let p = Point::new(x0 as i32, y0 as i32);
                if let Some(u) = self.flashes.iter().rev().find(|u| u.rect.contains(p)) {
                    let (tr, tg, tb) = tint(u.mode);
                    r = (r * 6 + tr * 4) / 10;
                    gr = (gr * 6 + tg * 4) / 10;
                    b = (b * 6 + tb * 4) / 10;
                }
                if self.asleep {
                    (r, gr, b) = (r / 4, gr / 4, b / 3 + 20);
                }
                // Without a frontlight, the paper looks like a dimly lit one.
                let (dim, warm) = self
                    .light
                    .map_or((0.85, 0.0), |l| (0.75 + l.brightness as f32 / 96.0, l.warmth.unwrap_or(0) as f32 / 24.0));
                r = (r as f32 * dim) as u32;
                gr = (gr as f32 * dim * (1.0 - warm * 0.06)) as u32;
                b = (b as f32 * dim * (1.0 - warm * 0.18)) as u32;
                out[dx] = (r << 16) | (gr << 8) | b;
            }
        }
        buf.present().map_err(|e| anyhow!("softbuffer: {e}"))?;
        Ok(())
    }

    /// Window position → panel position, if it's on the panel.
    fn to_panel(&self, pos: PhysicalPosition<f64>) -> Option<Point> {
        let x = (pos.x - self.view.ox) / self.view.scale;
        let y = (pos.y - self.view.oy) / self.view.scale;
        (x >= 0.0 && y >= 0.0 && x < self.width as f64 && y < self.height as f64)
            .then(|| Point::new(x as i32, y as i32))
    }

    fn touch(&mut self, phase: TouchPhase, pos: Point) {
        self.events.push_back(Event::Touch(Touch { id: 0, phase, pos, time: Instant::now() }));
    }

    fn clamp_to_panel(&self, pos: PhysicalPosition<f64>) -> Point {
        let x = ((pos.x - self.view.ox) / self.view.scale).clamp(0.0, self.width as f64 - 1.0);
        let y = ((pos.y - self.view.oy) / self.view.scale).clamp(0.0, self.height as f64 - 1.0);
        Point::new(x as i32, y as i32)
    }

    fn key(&mut self, key: &Key) {
        match key.as_ref() {
            Key::Named(NamedKey::Escape) | Key::Character("q" | "Q") => self.events.push_back(Event::Quit),
            Key::Character("s" | "S") => {
                self.asleep = !self.asleep;
                self.events.push_back(if self.asleep { Event::Sleep } else { Event::Wake });
                self.dirty = true;
            }
            Key::Character("p" | "P") => {
                let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../screenshots");
                let millis =
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
                let path = dir.join(format!("verso-{millis}.png"));
                let saved = std::fs::create_dir_all(&dir)
                    .map_err(anyhow::Error::from)
                    .and_then(|_| save_gray_png(&path, self.width as u32, self.height as u32, &self.panel));
                match saved {
                    Ok(()) => log::info!("screenshot saved to {}", path.display()),
                    Err(e) => log::error!("couldn't save a screenshot: {e:#}"),
                }
            }
            Key::Character("v" | "V") => {
                self.visualize = !self.visualize;
                log::info!("refresh visualisation {}", if self.visualize { "on" } else { "off" });
            }
            _ => {}
        }
    }
}

impl ApplicationHandler for Sim {
    fn user_event(&mut self, _el: &ActiveEventLoop, _event: ()) {
        self.events.push_back(Event::Background);
    }

    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_none()
            && let Err(e) = self.create_window(el)
        {
            self.error = Some(e);
        }
    }

    fn window_event(&mut self, _el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.events.push_back(Event::Quit),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } | WindowEvent::RedrawRequested => {
                self.dirty = true
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = Some(position);
                if self.mouse_down {
                    let p = self.clamp_to_panel(position);
                    self.touch(TouchPhase::Move, p);
                }
            }
            WindowEvent::CursorLeft { .. } => {
                if self.mouse_down {
                    self.mouse_down = false;
                    let p = self.cursor.map(|c| self.clamp_to_panel(c)).unwrap_or_default();
                    self.touch(TouchPhase::Up, p);
                }
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                let Some(cursor) = self.cursor else { return };
                match state {
                    ElementState::Pressed if !self.mouse_down => {
                        if let Some(p) = self.to_panel(cursor) {
                            self.mouse_down = true;
                            self.touch(TouchPhase::Down, p);
                        }
                    }
                    ElementState::Released if self.mouse_down => {
                        self.mouse_down = false;
                        let p = self.clamp_to_panel(cursor);
                        self.touch(TouchPhase::Up, p);
                    }
                    _ => {}
                }
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                self.events.push_back(Event::TextInput { input: TextInput::Insert(text), repeat: false });
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let input = match event.logical_key {
                    Key::Named(NamedKey::Backspace) => Some(TextInput::Backspace),
                    Key::Named(NamedKey::Delete) => Some(TextInput::Delete),
                    Key::Named(NamedKey::ArrowLeft) => Some(TextInput::Left),
                    Key::Named(NamedKey::ArrowRight) => Some(TextInput::Right),
                    Key::Named(NamedKey::Home) => Some(TextInput::Home),
                    Key::Named(NamedKey::End) => Some(TextInput::End),
                    Key::Named(NamedKey::Enter) => Some(TextInput::Submit),
                    Key::Named(NamedKey::Escape) => Some(TextInput::Dismiss),
                    _ => event.text.map(|text| TextInput::Insert(text.to_string())),
                };
                if let Some(input) = input {
                    self.events.push_back(Event::TextInput { input, repeat: event.repeat });
                }
            }
            _ => {}
        }
    }
}

fn tint(mode: RefreshMode) -> (u32, u32, u32) {
    match mode {
        RefreshMode::Fast => (230, 50, 50),
        RefreshMode::Ui => (50, 110, 230),
        RefreshMode::Page => (40, 170, 80),
        RefreshMode::Full => (240, 150, 30),
    }
}

/// VERSO_SIM_BATTERY: `82` is 82%, `82+` charging; unset, no battery.
pub(super) fn sim_battery() -> Option<super::Battery> {
    let value = std::env::var("VERSO_SIM_BATTERY").ok()?;
    let charging = value.ends_with('+');
    let percent = value.trim_end_matches('+').parse::<u8>().ok()?.min(100);
    Some(super::Battery { percent, charging })
}
