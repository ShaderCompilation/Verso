//! Kindle backend: FBInk for the screen, evdev for touch.

mod display;
mod input;

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use super::kindle_profiles::{self, Lights};
use super::{Battery, DeviceInfo, DeviceModel, Event, Frontlight, Orientation, Platform, Unsupported, Update, Waker};
use crate::gfx::Canvas;
use display::Display;
use input::{Buttons, Mapping, TouchInput};

/// Set by SIGINT/SIGTERM/SIGHUP, so we can shut down cleanly.
static QUIT: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    QUIT.store(true, Ordering::Relaxed);
}

pub struct Kindle {
    info: DeviceInfo,
    model: DeviceModel,
    /// The model's lights and their range (docs/device-support.md, Phase 3).
    lights: Lights,
    /// The profile's facts for the session header.
    profile_line: String,
    display: Display,
    touch: TouchInput,
    /// Page-turn buttons; `None` (and never polled) on touch-only Kindles.
    buttons: Option<Buttons>,
    queue: VecDeque<Event>,
    wake_fd: std::sync::Arc<std::os::fd::OwnedFd>,
    _power: PowerEvents,
    power_rx: std::sync::mpsc::Receiver<Event>,
    frontlight_rx: Option<std::sync::mpsc::Receiver<Option<Frontlight>>>,
    battery_due: Instant,
    sleeping: bool,
    native_sleep: bool,
    status: Option<(Instant, Option<super::power::Status>)>,
    /// Battery and cable from sysfs; powerd (`lipc`) when missing.
    power_files: Option<super::power::PowerFiles>,
}

impl Kindle {
    pub fn open() -> Result<Self> {
        install_signal_handlers();
        let display = Display::open()?;
        let touch = match TouchInput::open(Mapping {
            width: display.width,
            height: display.height,
            swap_axes: display.touch_swap_axes,
            mirror_x: display.touch_mirror_x,
            mirror_y: display.touch_mirror_y,
            orientation: Orientation::Portrait,
        }) {
            Ok(touch) => touch,
            Err(e) => {
                if let Some(Unsupported(reason)) = e.downcast_ref::<Unsupported>() {
                    display.show_refusal(reason);
                }
                return Err(e);
            }
        };
        let buttons = Buttons::open();
        let id = display.session.device_id;
        let profile = kindle_profiles::lookup(id);
        let model = kindle_profiles::model(profile, id, &display.name);
        let lights = Lights::for_profile(profile);
        let profile_line = match profile {
            Some(p) => format!(
                "model: {} ({}), light: {}, page buttons: {}, epdc: {:?}",
                p.model,
                if p.tested { "tested" } else { "untested" },
                lights.describe(),
                if p.page_buttons { "yes" } else { "no" },
                p.epdc,
            ),
            None => format!("model: not in the profile table, {} (untested), light: {}", model.name, lights.describe()),
        };
        let info =
            DeviceInfo { name: display.name.clone(), width: display.width, height: display.height, dpi: display.dpi };
        use std::os::fd::FromRawFd;
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if fd < 0 {
            return Err(io::Error::last_os_error().into());
        }
        let wake_fd = std::sync::Arc::new(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) });
        let (tx, power_rx) = std::sync::mpsc::channel();
        let power = PowerEvents::start(tx, fd_waker(wake_fd.clone()))?;
        let frontlight_rx = spawn_frontlight_read(fd_waker(wake_fd.clone()), lights);
        Ok(Self {
            info,
            model,
            lights,
            profile_line,
            display,
            touch,
            buttons,
            queue: VecDeque::new(),
            wake_fd,
            _power: power,
            power_rx,
            frontlight_rx,
            battery_due: Instant::now() + Duration::from_secs(60),
            sleeping: false,
            native_sleep: false,
            status: None,
            power_files: super::power::PowerFiles::find(std::path::Path::new("/sys/class/power_supply")),
        })
    }

    fn system_status(&mut self) -> Option<super::power::Status> {
        // The fallback when sysfs is missing. Battery and USB queries share
        // one snapshot: power events, USB and wake replace it at once, the
        // once-a-minute poll only every 5 minutes, since each read starts a
        // process (~29 ms) on the UI thread.
        if let Some((at, status)) = self.status
            && at.elapsed() < Duration::from_secs(5 * 60)
        {
            return status;
        }
        let status = std::process::Command::new("lipc-get-prop")
            .args(["com.lab126.powerd", "battStateInfo"])
            .output()
            .ok()
            .filter(|out| out.status.success())
            .and_then(|out| super::power::Status::parse(&out.stdout));
        self.status = Some((Instant::now(), status));
        status
    }

    fn system_event(&mut self, event: Event) -> Event {
        if matches!(event, Event::PowerChanged | Event::Usb(_) | Event::Wake) {
            self.status = None;
        }
        event
    }

    /// Page buttons follow the touchscreen's grab; a failure only costs the
    /// buttons' exclusivity, so it's logged rather than fatal.
    fn grab_buttons(&mut self, grab: bool) {
        if let Some(buttons) = &mut self.buttons
            && let Err(e) = buttons.grab(grab)
        {
            log::warn!("{e:#}");
        }
    }

    fn take_frontlight(&mut self) -> Option<Option<Frontlight>> {
        let rx = self.frontlight_rx.as_ref()?;
        match rx.try_recv() {
            Ok(light) => {
                self.frontlight_rx = None;
                Some(light)
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.frontlight_rx = None;
                Some(None)
            }
        }
    }
}

impl Platform for Kindle {
    fn battery(&mut self) -> Option<Battery> {
        if let Some(files) = &self.power_files {
            let started = Instant::now();
            let battery = files.battery();
            log::debug!("battery read in {} us: {battery:?}", started.elapsed().as_micros());
            if battery.is_some() {
                return battery;
            }
        }
        if let Some(battery) = self.system_status().and_then(|s| s.battery) {
            return Some(battery);
        }
        let percent = property("battLevel")?;
        let charging = property("isCharging")? != 0;
        Some(Battery { percent: percent.clamp(0, 100) as u8, charging })
    }

    fn usb_connected(&mut self) -> Option<bool> {
        // Unlike isCharging, this stays true for a full battery on a cable.
        // usbState is an undocumented enum and does not mean "connected != 0".
        // The charger supply's `online` matched powerd's `usb` plugged and
        // unplugged (measured).
        if let Some(usb) = self.power_files.as_ref().and_then(|f| f.usb()) {
            return Some(usb);
        }
        self.system_status()?.usb
    }

    fn prepare_sleep(&mut self, cover: bool) -> Result<()> {
        self.sleeping = true;
        self.queue.clear();
        if !cover && !self.native_sleep {
            // Let any in-flight flash finish before Amazon's UI draws its sleep screen.
            self.display.wait_for_complete();
            self.touch.grab(false)?;
            self.grab_buttons(false);
            // A missed handoff (no launcher, or one too busy to ack) must not end
            // the session: keep our last frame, stay in charge of the touchscreen
            // and sleep like a cover would.
            match launcher_handoff(true) {
                Ok(()) => self.native_sleep = true,
                Err(e) => {
                    log::warn!("sleep handoff failed, sleeping on Verso's own screen: {e:#}");
                    self.touch.grab(true)?;
                    self.grab_buttons(true);
                }
            }
        }
        Ok(())
    }

    fn resume(&mut self) -> Result<()> {
        if self.native_sleep {
            // Verso takes the screen back either way: a missed ack only means the
            // next draw may race the launcher's resume, not worth quitting for.
            if let Err(e) = launcher_handoff(false) {
                log::warn!("wake handoff failed, resuming anyway: {e:#}");
            }
            self.touch.grab(true)?;
            self.native_sleep = false;
        } else {
            self.touch.grab(true)?;
        }
        self.grab_buttons(true);
        self.queue.clear();
        self.sleeping = false;
        self.battery_due = Instant::now() + Duration::from_secs(60);
        Ok(())
    }
    fn waker(&self) -> Waker {
        fd_waker(self.wake_fd.clone())
    }
    fn frontlight(&mut self) -> Option<Frontlight> {
        self.lights.reading(property("flIntensity"), property("currentAmberLevel"))
    }
    fn initial_frontlight(&mut self) -> Option<Option<Frontlight>> {
        self.take_frontlight()
    }
    fn set_frontlight(&mut self, value: Frontlight) -> Result<()> {
        let value = self.lights.clamp(value);
        set_property("flIntensity", value.brightness)?;
        if let Some(warmth) = value.warmth {
            set_property("currentAmberLevel", warmth)?;
        }
        Ok(())
    }

    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn model(&self) -> DeviceModel {
        self.model.clone()
    }

    fn session_details(&self) -> Vec<String> {
        let s = &self.display.session;
        vec![
            format!("fbink: {}", s.fbink_version),
            format!("device: {} ({}, {}, id={:#06x})", s.device_name, s.codename, s.platform, s.device_id),
            self.profile_line.clone(),
            format!(
                "display: view {}x{}+{}+{}, screen {}x{}, stride {}, {} bpp, pxfmt={}, color={}, {} dpi, mtk={}, inverted={}",
                s.view_width,
                s.view_height,
                s.view_hori_origin,
                s.view_vert_origin,
                s.screen_width,
                s.screen_height,
                s.stride,
                s.bpp,
                s.pixel_format,
                s.has_color_panel,
                s.dpi,
                s.is_mtk,
                s.inverted,
            ),
            format!("touch: {}", self.touch.summary()),
            format!("page buttons: {}", self.buttons.as_ref().map_or("none".into(), Buttons::summary)),
            format!("battery: {}", if self.power_files.is_some() { "sysfs" } else { "powerd" }),
        ]
    }

    fn set_orientation(&mut self, orientation: Orientation) -> Result<()> {
        self.display.orientation = orientation;
        self.touch.set_orientation(orientation);
        (self.info.width, self.info.height) = orientation.screen_size(self.display.width, self.display.height);
        // Touches and button presses already queued were mapped for the old
        // orientation.
        self.queue.retain(|e| !matches!(e, Event::Touch(_) | Event::Key(_)));
        Ok(())
    }

    fn next_event(&mut self, timeout: Option<Duration>) -> Result<Option<Event>> {
        let deadline = timeout.map(|t| Instant::now() + t);
        loop {
            // A failed persistence barrier can refuse this exit. Consume the
            // request once so input remains usable and the user can retry.
            if QUIT.swap(false, Ordering::Relaxed) {
                return Ok(Some(Event::Quit));
            }
            if let Some(light) = self.take_frontlight() {
                return Ok(Some(Event::Frontlight(light)));
            }
            if let Ok(e) = self.power_rx.try_recv() {
                return Ok(Some(self.system_event(e)));
            }
            if let Some(e) = self.queue.pop_front() {
                return Ok(Some(e));
            }
            if !self.sleeping && Instant::now() >= self.battery_due {
                self.battery_due = Instant::now() + Duration::from_secs(60);
                // Unlike real power events, the poll keeps the powerd snapshot.
                return Ok(Some(Event::PowerChanged));
            }
            // Poll at least once, even when the deadline has passed (a zero
            // timeout means "anything waiting?").
            let poll_deadline = if self.sleeping {
                deadline
            } else {
                Some(deadline.map_or(self.battery_due, |d| d.min(self.battery_due)))
            };
            let wait_ms = match poll_deadline {
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
                libc::pollfd { fd: self.buttons.as_ref().map_or(-1, Buttons::fd), events: libc::POLLIN, revents: 0 },
            ];
            // Touch-only Kindles poll just the first two.
            let count = if self.buttons.is_some() { 3 } else { 2 };
            // SAFETY: `count` initialized pollfds backed by live file descriptors.
            let rc = unsafe { libc::poll(pfds.as_mut_ptr(), count, wait_ms) };
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
                    if let Some(light) = self.take_frontlight() {
                        if value > 1 {
                            self.queue.push_back(Event::Background);
                        }
                        return Ok(Some(Event::Frontlight(light)));
                    }
                    let event = self.power_rx.try_recv().unwrap_or(Event::Background);
                    if value > 1 && !matches!(event, Event::Background) {
                        self.queue.push_back(Event::Background);
                    }
                    return Ok(Some(self.system_event(event)));
                }
                let pfd = &pfds[0];
                if pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                    bail!("the touchscreen stopped working (poll flags {:#x})", pfd.revents);
                }
                self.touch.read(&mut self.queue)?;
                if pfds[2].revents != 0
                    && let Some(buttons) = &mut self.buttons
                {
                    // Losing the buttons isn't worth ending the session over:
                    // the touchscreen still turns pages.
                    let failed = pfds[2].revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0;
                    let read = if failed {
                        Err(anyhow::anyhow!("poll flags {:#x}", pfds[2].revents))
                    } else {
                        buttons.read(self.display.orientation, &mut self.queue)
                    };
                    if let Err(e) = read {
                        log::warn!("page buttons stopped working, touch only from now: {e:#}");
                        self.buttons = None;
                    }
                }
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
/// Reads the light off the UI thread (two lipc calls). For a model missing
/// from the profile table this is also the probe: no `flIntensity` hides
/// the light controls, no `currentAmberLevel` the warmth slider.
fn spawn_frontlight_read(waker: Waker, lights: Lights) -> Option<std::sync::mpsc::Receiver<Option<Frontlight>>> {
    let (tx, rx) = std::sync::mpsc::channel();
    match std::thread::Builder::new().name("verso-frontlight".into()).spawn(move || {
        let started = Instant::now();
        let fl = property("flIntensity");
        let amber = property("currentAmberLevel");
        log::info!(
            "session frontlight: flIntensity={}, currentAmberLevel={}",
            if fl.is_some() { "yes" } else { "no" },
            if amber.is_some() { "yes" } else { "no" },
        );
        let value = lights.reading(fl, amber);
        log::debug!("frontlight read off-thread in {} us: {value:?}", started.elapsed().as_micros());
        let _ = tx.send(value);
        waker.wake();
    }) {
        Ok(_) => Some(rx),
        Err(e) => {
            log::warn!("couldn't start frontlight thread: {e}");
            None
        }
    }
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
    shutdown: std::sync::Arc<AtomicBool>,
}
impl PowerEvents {
    fn start(tx: std::sync::mpsc::Sender<Event>, waker: Waker) -> Result<Self> {
        use std::io::BufRead;
        use std::os::unix::process::CommandExt;
        let mut command = std::process::Command::new("lipc-wait-event");
        command
            .args(["-m", "com.lab126.powerd", "goingToScreenSaver,outOfScreenSaver,exitingScreenSaver,wakeupFromSuspend,charging,notCharging,battLevelChanged"])
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
        let mut child = command.spawn()?;
        let stdout = child.stdout.take().expect("piped power events");
        let shutdown = std::sync::Arc::new(AtomicBool::new(false));
        let stopping = shutdown.clone();
        let join = std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
                log::debug!("powerd: {line}");
                let Some(e) = super::power::parse(&line) else { continue };
                if tx.send(e).is_err() {
                    break;
                }
                waker.wake();
            }
            // Without power events we cannot safely own the display. Exit via
            // the launcher rather than leave an unresponsive reader running.
            if !stopping.load(Ordering::Relaxed) {
                log::warn!("power event listener stopped; returning to Kindle UI");
                let _ = tx.send(Event::Quit);
                waker.wake();
            }
        });
        Ok(Self { child, join: Some(join), shutdown })
    }
}

fn launcher_handoff(native: bool) -> Result<()> {
    let pid: i32 = std::env::var("VERSO_LAUNCHER_PID")?.parse()?;
    anyhow::ensure!(pid > 1 && unsafe { libc::getppid() } == pid, "invalid Verso launcher");
    let ack = std::env::var("VERSO_SYSTEM_ACK")?;
    anyhow::ensure!(ack == format!("/tmp/verso-system-{pid}"), "invalid launcher acknowledgement path");
    match std::fs::remove_file(&ack) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    if unsafe { libc::kill(pid, if native { libc::SIGUSR1 } else { libc::SIGUSR2 }) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    let expected = if native { "native" } else { "verso" };
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if std::fs::read_to_string(&ack).is_ok_and(|s| s.trim() == expected) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    bail!("launcher did not acknowledge {expected} display ownership")
}
impl Drop for PowerEvents {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}
