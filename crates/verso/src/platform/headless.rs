//! A simulator without a window, for screenshots: plays scripted touches,
//! saves the screen as a PNG after every update, then quits.
//! The device follows the simulator's `--device` preset and its overrides
//! (sim_device.rs): a color preset saves RGB PNGs, through the Kaleido 3
//! preview with VERSO_SIM_KALEIDO=1. The battery follows VERSO_SIM_BATTERY.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;

use super::desktop::sim_battery;
use super::sim_device::SimDevice;
use super::{DeviceInfo, Event, Frontlight, Orientation, Platform, ScriptStep, Touch, TouchPhase, Update, Waker};
use crate::geom::Point;
use crate::gfx::{Canvas, save_gray_png, save_rgba_png};

pub struct Headless {
    info: DeviceInfo,
    /// VERSO_SIM_SIZE: the panel in portrait.
    native: (u32, u32),
    /// `None` is an idle moment between steps, so the app can finish slow
    /// work (like opening a book) before the next step.
    events: VecDeque<Option<Event>>,
    /// What the "screen" shows: 8-bit gray, or RGBA on a color preset
    /// (saved as an RGB PNG).
    panel: Vec<u8>,
    out: PathBuf,
    wake_tx: std::sync::mpsc::Sender<()>,
    wake_rx: std::sync::mpsc::Receiver<()>,
    light: Option<Frontlight>,
    device: SimDevice,
    busy: bool,
    barrier: bool,
    barrier_since: Instant,
}

impl Headless {
    pub fn new(out: &Path, script: Vec<ScriptStep>, device: SimDevice) -> Result<Self> {
        let (width, height, dpi) = (device.width, device.height, device.dpi);
        let info = DeviceInfo {
            name: format!("Headless simulator ({})", device.name),
            width,
            height,
            dpi,
            color: device.color_mode(),
        };
        // Touch times are made up: each step a second after the last, so
        // nothing counts as a double tap or a long-press by accident.
        let mut t = Instant::now();
        let mut events = VecDeque::new();
        events.push_back(None); // Wait for initial library discovery before playing touches.
        let touch = |events: &mut VecDeque<_>, phase, pos, at: Instant| {
            events.push_back(Some(Event::Touch(Touch { id: 0, phase, pos, time: at })))
        };
        for step in script {
            t += Duration::from_secs(1);
            match step {
                ScriptStep::Text(input) => events.push_back(Some(Event::TextInput { input, repeat: false })),
                ScriptStep::Key(key) => events.push_back(Some(Event::Key(key))),
                ScriptStep::Tap(p) => {
                    touch(&mut events, TouchPhase::Down, p, t);
                    touch(&mut events, TouchPhase::Up, p, t + Duration::from_millis(60));
                }
                ScriptStep::LongPress(p) => {
                    let held_at = Instant::now() - Duration::from_secs(1);
                    touch(&mut events, TouchPhase::Down, p, held_at);
                    events.push_back(None); // Let the gesture timer fire before lifting.
                    touch(&mut events, TouchPhase::Up, p, held_at + Duration::from_millis(650));
                }
                ScriptStep::HoldDrag(a, b) => {
                    let held_at = Instant::now() - Duration::from_secs(1);
                    touch(&mut events, TouchPhase::Down, a, held_at);
                    events.push_back(None);
                    touch(&mut events, TouchPhase::Move, b, held_at + Duration::from_millis(600));
                    touch(&mut events, TouchPhase::Up, b, held_at + Duration::from_millis(650));
                }
                ScriptStep::Press(p) => touch(&mut events, TouchPhase::Down, p, t),
                ScriptStep::Swipe(a, b) => {
                    let mid = Point::new((a.x + b.x) / 2, (a.y + b.y) / 2);
                    touch(&mut events, TouchPhase::Down, a, t);
                    touch(&mut events, TouchPhase::Move, mid, t + Duration::from_millis(50));
                    touch(&mut events, TouchPhase::Move, b, t + Duration::from_millis(100));
                    touch(&mut events, TouchPhase::Up, b, t + Duration::from_millis(120));
                }
            }
            events.push_back(None);
        }
        events.push_back(None);
        events.push_back(Some(Event::Quit));
        let (wake_tx, wake_rx) = std::sync::mpsc::channel();
        Ok(Self {
            info,
            native: (width, height),
            events,
            panel: vec![255; (width * height) as usize * if device.color { 4 } else { 1 }],
            out: out.to_owned(),
            wake_tx,
            wake_rx,
            light: device.initial_light(),
            device,
            busy: false,
            barrier: false,
            barrier_since: Instant::now(),
        })
    }
}

impl Platform for Headless {
    fn battery(&mut self) -> Option<super::Battery> {
        sim_battery()
    }
    fn set_busy(&mut self, busy: bool) {
        self.busy = busy;
    }
    fn waker(&self) -> Waker {
        let tx = self.wake_tx.clone();
        Waker::new(move || {
            let _ = tx.send(());
        })
    }
    fn frontlight(&mut self) -> Option<Frontlight> {
        self.light
    }
    fn set_frontlight(&mut self, value: Frontlight) -> Result<()> {
        self.light = self.device.accept_light(value);
        Ok(())
    }

    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn model(&self) -> super::DeviceModel {
        self.device.model()
    }

    fn session_details(&self) -> Vec<String> {
        vec![self.device.details()]
    }

    fn set_orientation(&mut self, orientation: Orientation) -> Result<()> {
        // The screenshot simply comes out in the new orientation.
        let (w, h) = orientation.screen_size(self.native.0, self.native.1);
        (self.info.width, self.info.height) = (w, h);
        self.panel = vec![255; (w * h) as usize * if self.device.color { 4 } else { 1 }];
        Ok(())
    }

    fn next_event(&mut self, timeout: Option<Duration>) -> Result<Option<Event>> {
        if self.wake_rx.try_recv().is_ok() {
            return Ok(Some(Event::Background));
        }
        if self.events.front().is_some_and(Option::is_none) {
            if !self.barrier {
                self.barrier = true;
                self.barrier_since = Instant::now();
                return Ok(None);
            }
            if self.busy {
                anyhow::ensure!(
                    self.barrier_since.elapsed() < Duration::from_secs(90),
                    "screenshot timed out waiting for background work"
                );
                let wait = timeout.unwrap_or(Duration::from_millis(50)).min(Duration::from_millis(50));
                return Ok(self.wake_rx.recv_timeout(wait).ok().map(|()| Event::Background));
            }
            self.events.pop_front();
            self.barrier = false;
        }
        Ok(self.events.pop_front().unwrap_or(Some(Event::Quit)))
    }

    fn present(&mut self, canvas: &Canvas, updates: &[Update]) -> Result<()> {
        let (w, h) = (self.info.width, self.info.height);
        if self.device.color {
            for u in updates {
                canvas.copy_rgba(u.rect, &mut self.panel, w as usize);
            }
            if self.device.kaleido {
                let mut shown = self.panel.clone();
                super::kaleido::preview(&mut shown, w as usize, h as usize);
                save_rgba_png(&self.out, w, h, &shown)?;
            } else {
                save_rgba_png(&self.out, w, h, &self.panel)?;
            }
        } else {
            for u in updates {
                canvas.copy_gray(u.rect, &mut self.panel, w as usize);
            }
            save_gray_png(&self.out, w, h, &self.panel)?;
        }
        log::info!("saved {}", self.out.display());
        Ok(())
    }

    fn wait_for_complete(&mut self) -> Result<()> {
        Ok(())
    }
}
