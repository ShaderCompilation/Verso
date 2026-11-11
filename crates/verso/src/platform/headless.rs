//! A simulator without a window, for screenshots: plays scripted touches,
//! saves the screen as a PNG after every update, then quits.
//! Screen size and density follow the simulator's VERSO_SIM_SIZE / VERSO_SIM_DPI.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;

use super::desktop::sim_screen;
use super::{DeviceInfo, Event, Platform, ScriptStep, Touch, TouchPhase, Update};
use crate::geom::Point;
use crate::gfx::{Canvas, save_gray_png};

pub struct Headless {
    info: DeviceInfo,
    /// `None` is an idle moment between steps, so the app can finish slow
    /// work (like opening a book) before the next step.
    events: VecDeque<Option<Event>>,
    /// What the "screen" shows (8-bit gray).
    panel: Vec<u8>,
    out: PathBuf,
}

impl Headless {
    pub fn new(out: &Path, script: Vec<ScriptStep>) -> Result<Self> {
        let (width, height, dpi) = sim_screen()?;
        let info = DeviceInfo { name: "Headless simulator".into(), width, height, dpi };
        // Touch times are made up: each step a second after the last, so
        // nothing counts as a double tap or a long-press by accident.
        let mut t = Instant::now();
        let mut events = VecDeque::new();
        let touch = |events: &mut VecDeque<_>, phase, pos, at: Instant| {
            events.push_back(Some(Event::Touch(Touch { id: 0, phase, pos, time: at })))
        };
        for step in script {
            t += Duration::from_secs(1);
            match step {
                ScriptStep::Tap(p) => {
                    touch(&mut events, TouchPhase::Down, p, t);
                    touch(&mut events, TouchPhase::Up, p, t + Duration::from_millis(60));
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
        Ok(Self { info, events, panel: vec![255; (width * height) as usize], out: out.to_owned() })
    }
}

impl Platform for Headless {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn next_event(&mut self, _timeout: Option<Duration>) -> Result<Option<Event>> {
        Ok(self.events.pop_front().unwrap_or(Some(Event::Quit)))
    }

    fn present(&mut self, canvas: &Canvas, updates: &[Update]) -> Result<()> {
        for u in updates {
            canvas.copy_gray(u.rect, &mut self.panel, self.info.width as usize);
        }
        save_gray_png(&self.out, self.info.width, self.info.height, &self.panel)?;
        log::info!("saved {}", self.out.display());
        Ok(())
    }

    fn wait_for_complete(&mut self) -> Result<()> {
        Ok(())
    }
}
