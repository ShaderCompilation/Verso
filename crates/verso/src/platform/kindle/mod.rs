//! Kindle backend: FBInk for the screen, evdev for touch.

mod display;
mod input;

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use super::{DeviceInfo, Event, Frontlight, Platform, Update, Waker};
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
    wake_fd: std::sync::Arc<std::os::fd::OwnedFd>,
    _power: Option<PowerEvents>,
    power_rx: std::sync::mpsc::Receiver<Event>,
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
        use std::os::fd::FromRawFd;
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if fd < 0 {
            return Err(io::Error::last_os_error().into());
        }
        let wake_fd = std::sync::Arc::new(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) });
        let (tx, power_rx) = std::sync::mpsc::channel();
        let power = PowerEvents::start(tx, fd_waker(wake_fd.clone()));
        Ok(Self { info, display, touch, queue: VecDeque::new(), wake_fd, _power: power, power_rx })
    }
}

impl Platform for Kindle {
    fn waker(&self) -> Waker {
        fd_waker(self.wake_fd.clone())
    }
    fn frontlight(&mut self) -> Option<Frontlight> {
        Some(Frontlight {
            brightness: property("flIntensity")?.clamp(0, 24) as u8,
            warmth: property("currentAmberLevel").map(|v| v.clamp(0, 24) as u8),
        })
    }
    fn set_frontlight(&mut self, value: Frontlight) -> Result<()> {
        set_property("flIntensity", value.brightness.min(24))?;
        if let Some(warmth) = value.warmth {
            set_property("currentAmberLevel", warmth.min(24))?;
        }
        Ok(())
    }

    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn next_event(&mut self, timeout: Option<Duration>) -> Result<Option<Event>> {
        let deadline = timeout.map(|t| Instant::now() + t);
        loop {
            if QUIT.load(Ordering::Relaxed) {
                return Ok(Some(Event::Quit));
            }
            if let Ok(e) = self.power_rx.try_recv() {
                return Ok(Some(e));
            }
            if let Some(e) = self.queue.pop_front() {
                return Ok(Some(e));
            }
            // Poll at least once, even when the deadline has passed (a zero
            // timeout means "anything waiting?").
            let wait_ms = match deadline {
                None => -1,
                // Round up so we don't spin just short of the deadline.
                Some(d) => {
                    d.saturating_duration_since(Instant::now()).as_micros().div_ceil(1000).min(i32::MAX as u128) as i32
                }
            };
            use std::os::fd::AsRawFd;
            let mut pfds = [
                libc::pollfd { fd: self.touch.fd(), events: libc::POLLIN, revents: 0 },
                libc::pollfd { fd: self.wake_fd.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            ];
            // SAFETY: two initialized pollfds backed by live file descriptors.
            let rc = unsafe { libc::poll(pfds.as_mut_ptr(), 2, wait_ms) };
            if rc < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue; // a signal; QUIT is checked at the top
                }
                return Err(err.into());
            }
            if rc > 0 {
                if pfds[1].revents & libc::POLLIN != 0 {
                    let mut value = 0u64;
                    unsafe {
                        libc::read(self.wake_fd.as_raw_fd(), (&mut value as *mut u64).cast(), 8);
                    }
                    return Ok(Some(self.power_rx.try_recv().unwrap_or(Event::Background)));
                }
                let pfd = &pfds[0];
                if pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                    bail!("the touchscreen stopped working (poll flags {:#x})", pfd.revents);
                }
                self.touch.read(&mut self.queue)?;
            }
            if self.queue.is_empty() && deadline.is_some_and(|d| Instant::now() >= d) {
                return Ok(None);
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

fn fd_waker(fd: std::sync::Arc<std::os::fd::OwnedFd>) -> Waker {
    use std::os::fd::AsRawFd;
    Waker::new(move || {
        let one = 1u64;
        unsafe {
            libc::write(fd.as_raw_fd(), (&one as *const u64).cast(), 8);
        }
    })
}
fn property(name: &str) -> Option<i32> {
    let out = std::process::Command::new("lipc-get-prop").args(["com.lab126.powerd", name]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().parse().ok()).flatten()
}
fn set_property(name: &str, value: u8) -> Result<()> {
    let status =
        std::process::Command::new("lipc-set-prop").args(["com.lab126.powerd", name, &value.to_string()]).status()?;
    anyhow::ensure!(status.success(), "couldn't change front light");
    Ok(())
}
struct PowerEvents {
    child: std::process::Child,
    join: Option<std::thread::JoinHandle<()>>,
}
impl PowerEvents {
    fn start(tx: std::sync::mpsc::Sender<Event>, waker: Waker) -> Option<Self> {
        use std::io::BufRead;
        use std::os::unix::process::CommandExt;
        let mut command = std::process::Command::new("lipc-wait-event");
        command
            .args(["-m", "com.lab126.powerd", "goingToScreenSaver,outOfScreenSaver"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        let parent = std::process::id() as libc::pid_t;
        // SAFETY: only async-signal-safe syscalls run between fork and exec.
        // The listener must also exit when Verso is killed without running Drop.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    return Err(io::Error::from_raw_os_error(libc::ECHILD));
                }
                Ok(())
            });
        }
        let child = command.spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                log::warn!("power events unavailable: {e}");
                return None;
            }
        };
        let stdout = child.stdout.take()?;
        let join = std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
                let e = if line.contains("goingToScreenSaver") {
                    Event::Sleep
                } else if line.contains("outOfScreenSaver") {
                    Event::Wake
                } else {
                    continue;
                };
                if tx.send(e).is_err() {
                    break;
                }
                waker.wake();
            }
        });
        Some(Self { child, join: Some(join) })
    }
}
impl Drop for PowerEvents {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}
