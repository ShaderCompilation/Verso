//! Kindle backend: FBInk for the screen, evdev for touch.
//!
//! `poll` waits on the touchscreen fd, the page-key fds (Oasis/Voyage buttons,
//! remotes) and an `eventfd` `Waker` shared with background workers, the remote
//! threads (`verso-remotes`, `verso-remote-port`), the `lipc-wait-event`
//! `powerd` listener (`PowerEvents`, thread `verso-power`) and the one-shot
//! startup frontlight thread (`verso-frontlight`). Opening the touchscreen
//! takes ~80 ms in the kernel, so `verso-input-init` does it while the first
//! frame draws; `finish_startup` (or the first call needing input) joins it.
//! Battery and cable state read `/sys/class/power_supply`
//! directly (<1 ms) with a cached `battStateInfo` `lipc` fallback. Native
//! Amazon sleep handoff signals `verso.sh` via acknowledged `SIGUSR1`/`SIGUSR2`
//! so the launcher remains the sole owner of Amazon UI process state.

mod display;
mod input;

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

use super::kindle_profiles::{self, Lights};
use super::power::Link;
use super::remote::{self, Gate};
use super::{
    Battery, DeviceInfo, DeviceModel, Event, FrameCheck, Frontlight, Orientation, PageButtons, Platform, Unsupported,
    Update, Waker,
};
use crate::gfx::Canvas;
use display::Display;
use input::{KeySource, Mapping, Remote, TouchInput};

/// Set by SIGINT/SIGTERM/SIGHUP, so we can shut down cleanly.
static QUIT: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    QUIT.store(true, Ordering::Relaxed);
}

pub struct Kindle {
    info: DeviceInfo,
    model: DeviceModel,
    /// The model's lights and their range.
    lights: Lights,
    /// The profile's facts for the session header.
    profile_line: String,
    display: Display,
    /// `None` until `finish_startup` joins `opening`.
    inputs: Option<Inputs>,
    opening: Option<std::thread::JoinHandle<Result<Inputs>>>,
    queue: VecDeque<Event>,
    wake_fd: std::sync::Arc<std::os::fd::OwnedFd>,
    power: PowerEvents,
    power_rx: std::sync::mpsc::Receiver<Event>,
    frontlight_rx: Option<std::sync::mpsc::Receiver<Option<Frontlight>>>,
    battery_due: Instant,
    sleeping: bool,
    native_sleep: bool,
    status: Option<(Instant, Option<super::power::Status>)>,
    /// Battery and cable from sysfs; powerd (`lipc`) when missing.
    power_files: Option<super::power::PowerFiles>,
    cable: super::power::Cable,
}

struct Inputs {
    touch: TouchInput,
    /// The Kindle's page-turn buttons, if it has any, and the remotes
    /// connected now.
    keys: Vec<KeySource>,
    remotes: std::sync::mpsc::Receiver<Remote>,
    gate: Gate,
    /// When a remote last postponed the idle sleep.
    postponed: Option<Instant>,
}

impl Inputs {
    /// Takes in what the remote threads sent.
    fn take_remotes(&mut self, queue: &mut VecDeque<Event>) {
        while let Ok(remote) = self.remotes.try_recv() {
            match remote {
                Remote::Found(source) => self.keys.push(source),
                Remote::Page(action) => {
                    log::debug!("remote port: {action:?}");
                    if self.gate.accept((input::PORT_ID, 0), Instant::now()) {
                        queue.push_back(Event::Key(action));
                        self.remote_used();
                    }
                }
            }
        }
    }

    /// Reads the key devices whose poll flags are set. Losing one isn't worth
    /// ending the session over: the touchscreen still turns pages.
    fn read_keys(&mut self, pfds: &[libc::pollfd], orientation: Orientation, queue: &mut VecDeque<Event>) {
        let mut remote_used = false;
        for i in (0..pfds.len()).rev().filter(|&i| pfds[i].revents != 0) {
            let source = &mut self.keys[i];
            let before = queue.len();
            let read = if pfds[i].revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                Err(anyhow!("poll flags {:#x}", pfds[i].revents))
            } else {
                source.read(orientation, &mut self.gate, queue)
            };
            remote_used |= source.is_remote() && queue.len() > before;
            if let Err(e) = read {
                log::info!("{}: gone: {e:#}", source.label());
                self.keys.remove(i);
            }
        }
        if remote_used {
            self.remote_used();
        }
    }

    /// A remote in use keeps the Kindle awake like a touch does; powerd
    /// doesn't see every remote.
    fn remote_used(&mut self) {
        const EVERY: Duration = Duration::from_secs(60);
        if self.postponed.is_some_and(|at| at.elapsed() < EVERY) {
            return;
        }
        self.postponed = Some(Instant::now());
        // Starting lipc-set-prop takes ~29 ms.
        let postpone = std::thread::Builder::new().name("verso-postpone".into()).spawn(|| {
            if let Err(e) = super::awake::postpone_sleep() {
                log::debug!("{e:#}");
            }
        });
        if let Err(e) = postpone {
            log::warn!("couldn't start the sleep postponing thread: {e}");
        }
    }
}

/// Follows remotes on two threads, until the process ends.
fn start_remotes(held: Vec<String>, tx: std::sync::mpsc::Sender<Remote>, waker: Waker) {
    let (port_tx, port_waker) = (tx.clone(), waker.clone());
    let watch = std::thread::Builder::new().name("verso-remotes".into()).spawn(move || {
        let held: Vec<&str> = held.iter().map(String::as_str).collect();
        input::watch(&held, |source| {
            let _ = tx.send(Remote::Found(source));
            waker.wake();
        });
    });
    let port = std::thread::Builder::new().name("verso-remote-port".into()).spawn(move || match remote::listen() {
        Ok(listener) => {
            log::info!("remote port: {}", remote::PORT);
            remote::serve(&listener, |action| {
                let _ = port_tx.send(Remote::Page(action));
                port_waker.wake();
            });
        }
        Err(e) => log::warn!("remote port {}: {e}", remote::PORT),
    });
    for e in [watch.err(), port.err()].into_iter().flatten() {
        log::warn!("couldn't start a remote thread: {e}");
    }
}

/// Starts a process, about 29 ms.
fn powerd_status() -> Option<super::power::Status> {
    let out = std::process::Command::new("lipc-get-prop").args(["com.lab126.powerd", "battStateInfo"]).output().ok()?;
    out.status.success().then(|| super::power::Status::parse(&out.stdout))?
}

impl Kindle {
    pub fn open() -> Result<Self> {
        install_signal_handlers();
        let t0 = Instant::now();
        log::info!("startup: opening display (FBInk)");
        let display = Display::open()?;
        let display_us = t0.elapsed().as_micros();
        let id = display.session.device_id;
        log::info!(
            "startup: display ready ({}, {}x{}, id={:#06x}); opening input and power",
            display.name,
            display.width,
            display.height,
            id,
        );
        let profile = kindle_profiles::lookup(id);
        // A model missing from the table may have buttons.
        let buttons = profile.map_or(Some(PageButtons::Named), |p| p.page_buttons);
        let mapping = Mapping {
            width: display.width,
            height: display.height,
            swap_axes: display.touch_swap_axes,
            mirror_x: display.touch_mirror_x,
            mirror_y: display.touch_mirror_y,
            // `finish_startup` applies the settings' orientation.
            orientation: Orientation::Portrait,
        };
        use std::os::fd::FromRawFd;
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if fd < 0 {
            return Err(io::Error::last_os_error().into());
        }
        let wake_fd = std::sync::Arc::new(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) });
        let waker = fd_waker(wake_fd.clone());
        let opening = std::thread::Builder::new().name("verso-input-init".into()).spawn(move || {
            let started = Instant::now();
            let (touch, buttons) = input::open(mapping, buttons)?;
            log::debug!("startup stage: kindle input open input_us={}", started.elapsed().as_micros());
            let held = std::iter::once(touch.path()).chain(buttons.as_ref().map(KeySource::path));
            let held = held.map(str::to_owned).collect();
            let (tx, remotes) = std::sync::mpsc::channel();
            start_remotes(held, tx, waker);
            let keys = buttons.into_iter().collect();
            Ok(Inputs { touch, keys, remotes, gate: Gate::default(), postponed: None })
        })?;
        let model = kindle_profiles::model(profile, id, &display.name);
        let lights = Lights::for_profile(profile);
        let profile_line = match profile {
            Some(p) => format!(
                "model: {} ({}), light: {}, page buttons: {}, epdc: {:?}",
                p.model,
                if p.tested { "tested" } else { "untested" },
                lights.describe(),
                if p.page_buttons.is_some() { "yes" } else { "no" },
                p.epdc,
            ),
            None => format!("model: not in the profile table, {} (untested), light: {}", model.name, lights.describe()),
        };
        let color = if display.session.has_color_panel && display.session.bpp == 32 {
            crate::gfx::ColorMode::Color32
        } else {
            crate::gfx::ColorMode::Gray8
        };
        let info = DeviceInfo {
            name: display.name.clone(),
            width: display.width,
            height: display.height,
            dpi: display.dpi,
            color,
        };
        let t_pw = Instant::now();
        let (tx, power_rx) = std::sync::mpsc::channel();
        let power = PowerEvents::start(tx, fd_waker(wake_fd.clone()))?;
        let power_events_us = t_pw.elapsed().as_micros();
        let frontlight_rx = spawn_frontlight_read(fd_waker(wake_fd.clone()), lights);
        let t_pf = Instant::now();
        let power_files = super::power::PowerFiles::find(std::path::Path::new("/sys/class/power_supply"));
        let power_files_us = t_pf.elapsed().as_micros();
        log::debug!(
            "startup stage: kindle open display_us={display_us} power_events_us={power_events_us} power_files_us={power_files_us} total_us={} (input opening in the background)",
            t0.elapsed().as_micros(),
        );
        Ok(Self {
            info,
            model,
            lights,
            profile_line,
            display,
            inputs: None,
            opening: Some(opening),
            queue: VecDeque::new(),
            wake_fd,
            power,
            power_rx,
            frontlight_rx,
            battery_due: Instant::now() + Duration::from_secs(60),
            sleeping: false,
            native_sleep: false,
            status: None,
            power_files,
            cable: Default::default(),
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
        let status = powerd_status();
        self.status = Some((Instant::now(), status));
        status
    }

    fn system_event(&mut self, event: Event) -> Event {
        if matches!(event, Event::PowerChanged | Event::Usb(_) | Event::Wake) {
            self.status = None;
        }
        if event == Event::Wake {
            self.cable.forget();
        }
        if let Event::Usb(connected) = event {
            self.cable.recheck(Instant::now());
            self.log_power(if connected { "charging event" } else { "notCharging event" });
        }
        event
    }

    fn log_power(&self, when: &str) {
        let files = self.power_files.as_ref().map_or_else(|| "no sysfs".to_owned(), |f| f.describe());
        log::info!("power: {when}: {files}");
    }

    /// Page buttons follow the touchscreen's grab, and remotes drop what was
    /// pressed meanwhile. A failure is logged rather than fatal: it only costs
    /// the buttons' exclusivity, or the remote is gone and the next poll says so.
    fn grab_buttons(&mut self, grab: bool) {
        for source in &mut self.inputs_mut().keys {
            if let Err(e) = source.grab(grab) {
                log::warn!("{e:#}");
            }
        }
    }

    fn inputs_mut(&mut self) -> &mut Inputs {
        self.inputs.as_mut().expect("finish_startup opens the inputs first")
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
        let usb = match self.power_files.as_ref().and_then(|f| f.link()) {
            // Asking powerd would delay the first frame: ask at the first
            // recheck instead.
            Some(Link::Unsure) if self.opening.is_some() => {
                self.cable.recheck(Instant::now());
                return None;
            }
            Some(link) => self.cable.computer(link, || {
                let usb = powerd_status().and_then(|s| s.usb);
                log::info!("power: powerd usb={usb:?}");
                usb
            }),
            None => self.system_status()?.usb?,
        };
        Some(self.cable.settle(usb, Instant::now()))
    }

    fn finish_startup(&mut self) -> Result<()> {
        let Some(opening) = self.opening.take() else { return Ok(()) };
        let started = Instant::now();
        let opened = opening.join().map_err(|_| anyhow!("the input thread panicked")).and_then(|r| r);
        let mut inputs = match opened {
            Ok(inputs) => inputs,
            Err(e) => {
                if let Some(Unsupported(reason)) = e.downcast_ref::<Unsupported>() {
                    self.display.show_refusal(reason);
                }
                return Err(e);
            }
        };
        self.power.wait_started()?;
        inputs.touch.set_orientation(self.display.orientation);
        log::info!("session touch: {}", inputs.touch.summary());
        // Only the Kindle's own so far; remotes log themselves.
        log::info!("session page buttons: {}", inputs.keys.first().map_or("none".into(), KeySource::summary));
        log::debug!("startup stage: kindle input ready waited_us={}", started.elapsed().as_micros());
        self.inputs = Some(inputs);
        Ok(())
    }

    fn prepare_sleep(&mut self, cover: bool) -> Result<()> {
        self.finish_startup()?;
        self.sleeping = true;
        self.queue.clear();
        if !cover && !self.native_sleep {
            // Let any in-flight flash finish before Amazon's UI draws its sleep screen.
            self.display.wait_for_complete();
            self.inputs_mut().touch.grab(false)?;
            self.grab_buttons(false);
            // A missed handoff (no launcher, or one too busy to ack) must not end
            // the session: keep our last frame, stay in charge of the touchscreen
            // and sleep like a cover would.
            match launcher_handoff(true) {
                Ok(()) => self.native_sleep = true,
                Err(e) => {
                    log::warn!("sleep handoff failed, sleeping on Verso's own screen: {e:#}");
                    self.inputs_mut().touch.grab(true)?;
                    self.grab_buttons(true);
                }
            }
        }
        Ok(())
    }

    fn resume(&mut self) -> Result<bool> {
        self.finish_startup()?;
        let native = std::mem::take(&mut self.native_sleep);
        if native {
            // Verso takes the screen back either way: a missed ack only means the
            // next draw may race the launcher's resume, not worth quitting for.
            if let Err(e) = launcher_handoff(false) {
                log::warn!("wake handoff failed, resuming anyway: {e:#}");
            }
        }
        self.inputs_mut().touch.grab(true)?;
        self.grab_buttons(true);
        self.queue.clear();
        self.sleeping = false;
        self.battery_due = Instant::now() + Duration::from_secs(60);
        Ok(native)
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
            // Touch and page buttons are logged by `finish_startup`.
            format!("battery: {}", if self.power_files.is_some() { "sysfs" } else { "powerd" }),
        ]
    }

    fn set_orientation(&mut self, orientation: Orientation) -> Result<()> {
        self.display.orientation = orientation;
        // Before `finish_startup` (the settings' orientation, ahead of the
        // first frame), it picks this up from the display.
        if let Some(inputs) = &mut self.inputs {
            inputs.touch.set_orientation(orientation);
        }
        (self.info.width, self.info.height) = orientation.screen_size(self.display.width, self.display.height);
        // Touches and button presses already queued were mapped for the old
        // orientation.
        self.queue.retain(|e| !matches!(e, Event::Touch(_) | Event::Key(_)));
        Ok(())
    }

    fn next_event(&mut self, timeout: Option<Duration>) -> Result<Option<Event>> {
        self.finish_startup()?;
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
            let inputs = self.inputs.as_mut().expect("finish_startup opened the inputs");
            inputs.take_remotes(&mut self.queue);
            if let Some(e) = self.queue.pop_front() {
                return Ok(Some(e));
            }
            if !self.sleeping && Instant::now() >= self.battery_due {
                self.battery_due = Instant::now() + Duration::from_secs(60);
                // Unlike real power events, the poll keeps the powerd snapshot.
                return Ok(Some(Event::PowerChanged));
            }
            if !self.sleeping && self.cable.take_due(Instant::now()) {
                self.status = None;
                self.log_power("read again");
                return Ok(Some(Event::PowerChanged));
            }
            // Poll at least once, even when the deadline has passed (a zero
            // timeout means "anything waiting?").
            let poll_deadline = if self.sleeping {
                deadline
            } else {
                [deadline, Some(self.battery_due), self.cable.next_due()].into_iter().flatten().min()
            };
            let wait_ms = match poll_deadline {
                None => -1,
                // Round up so we don't spin just short of the deadline.
                Some(d) => {
                    d.saturating_duration_since(Instant::now()).as_micros().div_ceil(1000).min(i32::MAX as u128) as i32
                }
            };
            use std::os::fd::AsRawFd;
            let inputs = self.inputs.as_mut().expect("finish_startup opened the inputs");
            let mut pfds: Vec<_> = [inputs.touch.fd(), self.wake_fd.as_raw_fd()]
                .into_iter()
                .chain(inputs.keys.iter().map(KeySource::fd))
                .map(|fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 })
                .collect();
            // SAFETY: initialized pollfds backed by live file descriptors.
            let rc = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, wait_ms) };
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
                inputs.touch.read(&mut self.queue)?;
                inputs.read_keys(&pfds[2..], self.display.orientation, &mut self.queue);
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

    fn check_frame(&mut self, canvas: &Canvas) -> Option<FrameCheck> {
        Some(self.display.check_frame(canvas))
    }

    fn failed_refreshes(&self) -> u32 {
        self.display.failed_refreshes
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
    /// The `lipc-wait-event` child, once `verso-power` has started it.
    child: std::sync::Arc<std::sync::Mutex<Option<std::process::Child>>>,
    join: Option<std::thread::JoinHandle<()>>,
    shutdown: std::sync::Arc<AtomicBool>,
    /// Whether the child started; taken by `wait_started`.
    started: Option<std::sync::mpsc::Receiver<io::Result<()>>>,
}
impl PowerEvents {
    /// Starts the listener thread, which spawns `lipc-wait-event` itself:
    /// the fork (~5 ms) stays off the UI thread, and `PR_SET_PDEATHSIG`
    /// fires when the *thread* that forked the child exits, so that must be
    /// the thread that lives as long as the listener (never a short-lived one).
    fn start(tx: std::sync::mpsc::Sender<Event>, waker: Waker) -> Result<Self> {
        use std::io::BufRead;
        let child = std::sync::Arc::new(std::sync::Mutex::new(None));
        let shutdown = std::sync::Arc::new(AtomicBool::new(false));
        let (started_tx, started) = std::sync::mpsc::channel();
        let (slot, stopping) = (child.clone(), shutdown.clone());
        let join = std::thread::Builder::new().name("verso-power".into()).spawn(move || {
            let stdout = {
                // Under the lock, so `drop` either sees the child or stops it starting.
                let mut slot = slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if stopping.load(Ordering::Relaxed) {
                    return;
                }
                match listener_command().spawn() {
                    Ok(mut child) => {
                        let stdout = child.stdout.take().expect("piped power events");
                        *slot = Some(child);
                        let _ = started_tx.send(Ok(()));
                        stdout
                    }
                    Err(e) => {
                        let _ = started_tx.send(Err(e));
                        return;
                    }
                }
            };
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
        })?;
        Ok(Self { child, join: Some(join), shutdown, started: Some(started) })
    }

    /// Waits until the listener runs; failing to start it ends startup.
    fn wait_started(&mut self) -> Result<()> {
        if let Some(started) = self.started.take() {
            started.recv().context("the power event thread stopped")?.context("couldn't start lipc-wait-event")?;
        }
        Ok(())
    }
}

fn listener_command() -> std::process::Command {
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
    command
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
        if let Some(mut child) = self.child.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}
