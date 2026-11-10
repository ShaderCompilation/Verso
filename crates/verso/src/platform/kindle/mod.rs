//! Kindle backend: FBInk for the screen, evdev for touch.

mod display;
mod input;

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use super::{DeviceInfo, Event, Platform, Update};
use crate::gfx::Canvas;
use display::Display;
use input::{Mapping, TouchInput};

/// Set by SIGINT/SIGTERM/SIGHUP, so we can shut down cleanly.
static QUIT: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    QUIT.store(true, Ordering::Relaxed);
}

pub struct Kindle {
    info: DeviceInfo,
    display: Display,
    touch: TouchInput,
    queue: VecDeque<Event>,
}

impl Kindle {
    pub fn open() -> Result<Self> {
        install_signal_handlers();
        let display = Display::open()?;
        let touch = TouchInput::open(Mapping {
            width: display.width,
            height: display.height,
            swap_axes: display.touch_swap_axes,
            mirror_x: display.touch_mirror_x,
            mirror_y: display.touch_mirror_y,
        })?;
        let info =
            DeviceInfo { name: display.name.clone(), width: display.width, height: display.height, dpi: display.dpi };
        Ok(Self { info, display, touch, queue: VecDeque::new() })
    }
}

impl Platform for Kindle {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn next_event(&mut self, timeout: Option<Duration>) -> Result<Option<Event>> {
        let deadline = timeout.map(|t| Instant::now() + t);
        loop {
            if QUIT.load(Ordering::Relaxed) {
                return Ok(Some(Event::Quit));
            }
            if let Some(e) = self.queue.pop_front() {
                return Ok(Some(e));
            }
            let wait_ms = match deadline {
                None => -1,
                Some(d) => {
                    let now = Instant::now();
                    if now >= d {
                        return Ok(None);
                    }
                    // Round up so we don't spin just short of the deadline.
                    (d - now).as_micros().div_ceil(1000).min(i32::MAX as u128) as i32
                }
            };
            let mut pfd = libc::pollfd { fd: self.touch.fd(), events: libc::POLLIN, revents: 0 };
            // SAFETY: one valid pollfd.
            let rc = unsafe { libc::poll(&mut pfd, 1, wait_ms) };
            if rc < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue; // a signal; QUIT is checked at the top
                }
                return Err(err.into());
            }
            if rc > 0 {
                if pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                    bail!("the touchscreen stopped working (poll flags {:#x})", pfd.revents);
                }
                self.touch.read(&mut self.queue)?;
            }
        }
    }

    fn present(&mut self, canvas: &Canvas, updates: &[Update]) -> Result<()> {
        self.display.present(canvas, updates)
    }

    fn wait_for_complete(&mut self) -> Result<()> {
        self.display.wait_for_complete();
        Ok(())
    }
}

fn install_signal_handlers() {
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    // No SA_RESTART, so a blocking poll() returns EINTR and we notice the flag.
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_signal as *const () as usize;
        libc::sigemptyset(&mut sa.sa_mask);
        for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            libc::sigaction(sig, &sa, std::ptr::null_mut());
        }
    }
}
