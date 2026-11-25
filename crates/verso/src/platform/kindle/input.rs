//! The touchscreen, read straight from the Linux input device (evdev).
//!
//! What its events mean is in `platform/touch.rs`.
//!
//! Page-turn buttons (Oasis) and remotes are separate key devices
//! (`KeySource`), read alongside. Remotes come and go: `watch` follows
//! `/dev/input` with inotify. Their rules are in `platform/remote.rs`.

use std::collections::{HashMap, VecDeque};
use std::ffi::CStr;
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::platform::remote::{self, Gate, Kind};
use crate::platform::touch::{
    ABS_MT_POSITION_X, ABS_MT_POSITION_Y, ABS_MT_SLOT, BTN_TOUCH, Decoder, MAX_SLOTS, Mapping,
};
use crate::platform::{Event, KeyAction, Orientation, PageButtons, Unsupported};

// Event types and codes from <linux/input-event-codes.h>.
const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;
const KEY_VOLUMEDOWN: u16 = 114;
const KEY_POWER: u16 = 116;

/// `struct input_event` on 32-bit ARM: the timestamp is two `long`s.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct InputEvent {
    sec: libc::c_long,
    usec: libc::c_long,
    kind: u16,
    code: u16,
    value: i32,
}

/// `struct input_absinfo`.
#[repr(C)]
#[derive(Default, Debug)]
struct AbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

// ioctl request numbers, built like the kernel's _IOC macros.
const fn ioc(dir: u32, nr: u32, size: u32) -> u32 {
    (dir << 30) | (size << 16) | ((b'E' as u32) << 8) | nr
}
const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;
const EVIOCGID: u32 = ioc(IOC_READ, 0x02, size_of::<[u16; 4]>() as u32);
const fn eviocgname(len: u32) -> u32 {
    ioc(IOC_READ, 0x06, len)
}
const fn eviocguniq(len: u32) -> u32 {
    ioc(IOC_READ, 0x08, len)
}
const fn eviocgbit_abs(len: u32) -> u32 {
    ioc(IOC_READ, 0x20 + EV_ABS as u32, len)
}
const fn eviocgbit_key(len: u32) -> u32 {
    ioc(IOC_READ, 0x20 + EV_KEY as u32, len)
}
const fn eviocgabs(axis: u16) -> u32 {
    ioc(IOC_READ, 0x40 + axis as u32, size_of::<AbsInfo>() as u32)
}
const EVIOCGKEY: u32 = ioc(IOC_READ, 0x18, KEY_BYTES as u32);
const EVIOCGRAB: u32 = ioc(IOC_WRITE, 0x90, size_of::<libc::c_int>() as u32);

pub struct TouchInput {
    grabbed: bool,
    file: File,
    path: String,
    device_name: String,
    touches: Decoder,
    buf: Vec<InputEvent>,
}

impl TouchInput {
    fn from_file(file: File, path: &Path, map: Mapping) -> Result<Self> {
        let fd = file.as_raw_fd();
        let name = device_name(fd);
        let x = abs_info(fd, ABS_MT_POSITION_X)?;
        let y = abs_info(fd, ABS_MT_POSITION_Y)?;
        // Exclusive grab: the paused Kindle UI must not collect our taps.
        // The kernel releases it automatically if we exit or crash.
        // SAFETY: EVIOCGRAB takes an int argument by value.
        if unsafe { libc::ioctl(fd, EVIOCGRAB as _, 1 as libc::c_int) } < 0 {
            bail!("can't grab {}: {}", path.display(), std::io::Error::last_os_error());
        }
        let slotted = abs_bits(fd).is_some_and(|bits| has_abs(&bits, ABS_MT_SLOT));
        let mut input = Self {
            grabbed: true,
            file,
            path: path.display().to_string(),
            device_name: name,
            touches: Decoder::new(map, (x.minimum, x.maximum), (y.minimum, y.maximum), slotted),
            buf: vec![InputEvent::default(); 64],
        };
        input.reset_contacts();
        Ok(input)
    }

    pub fn summary(&self) -> String {
        let Decoder { map, x_range, y_range, slotted, .. } = &self.touches;
        format!(
            "{} ({}), x {}..{}, y {}..{}, swap={}, mirror_x={}, mirror_y={}, slots={slotted}",
            self.path,
            self.device_name,
            x_range.0,
            x_range.1,
            y_range.0,
            y_range.1,
            map.swap_axes,
            map.mirror_x,
            map.mirror_y,
        )
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn set_orientation(&mut self, orientation: Orientation) {
        self.touches.map.orientation = orientation;
    }

    pub fn fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }

    pub fn grab(&mut self, grab: bool) -> Result<()> {
        // Discard touches accumulated while Amazon owned the screensaver.
        self.read(&mut VecDeque::new())?;
        self.reset_contacts();
        if self.grabbed == grab {
            return Ok(());
        }
        if unsafe { libc::ioctl(self.fd(), EVIOCGRAB as _, i32::from(grab)) } < 0 {
            bail!("can't change touchscreen grab: {}", std::io::Error::last_os_error());
        }
        self.grabbed = grab;
        Ok(())
    }

    fn reset_contacts(&mut self) {
        // evdev suppresses unchanged coordinates, even across EVIOCGRAB. Keep
        // the kernel's per-slot positions or the first tap after wake may have
        // one axis at zero. (Without slots evdev keeps and suppresses nothing.)
        for code in [ABS_MT_POSITION_X, ABS_MT_POSITION_Y] {
            let mut values = [0i32; MAX_SLOTS + 1];
            values[0] = code as i32;
            let request = ioc(IOC_READ, 0x0a, size_of_val(&values) as u32); // EVIOCGMTSLOTS
            if unsafe { libc::ioctl(self.fd(), request as _, values.as_mut_ptr()) } >= 0 {
                self.touches.set_positions(code, &values[1..]);
            }
        }
        let slot = abs_info(self.fd(), ABS_MT_SLOT).map_or(0, |info| info.value);
        let held = key_state(self.fd()).is_some_and(|keys| keys.has(BTN_TOUCH));
        self.touches.reset(slot, held);
    }

    /// Reads everything available (without blocking) and queues touch events.
    pub fn read(&mut self, out: &mut VecDeque<Event>) -> Result<()> {
        loop {
            let event_size = size_of::<InputEvent>();
            // SAFETY: InputEvent is plain old data; any bytes are a valid value.
            let bytes = unsafe {
                std::slice::from_raw_parts_mut(self.buf.as_mut_ptr() as *mut u8, self.buf.len() * event_size)
            };
            let n = match self.file.read(bytes) {
                Ok(n) => n,
                Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(()),
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(e).with_context(|| format!("reading {}", self.path)),
            };
            if n == 0 {
                bail!("{} closed", self.path);
            }
            let now = Instant::now();
            for i in 0..n / event_size {
                let ev = self.buf[i];
                if self.touches.handle(ev.kind, ev.code, ev.value, now, out) {
                    self.reset_contacts();
                }
            }
        }
    }
}

/// Page keys: the Kindle's own buttons or a remote (`platform/remote.rs`).
pub struct KeySource {
    id: u32,
    kind: Kind,
    /// For the Kindle's own buttons; `None` for remotes found by `watch`.
    layout: Option<PageButtons>,
    file: File,
    path: String,
    device_name: String,
    /// Only the Kindle's buttons are grabbed, and only when the device has no
    /// power key, which powerd must keep seeing (if a Kindle ever puts both on
    /// one device). Remotes are shared: the mapper may read the same device.
    can_grab: bool,
    grabbed: bool,
    has_volume_down: bool,
    counts: Counts,
    buf: Vec<InputEvent>,
}

/// Presses since the source was opened.
#[derive(Default)]
struct Counts {
    forward: u32,
    back: u32,
    /// Dropped by the `Gate`.
    echoes: u32,
    other: u32,
}

/// What the remote threads send the event loop.
pub enum Remote {
    Found(KeySource),
    /// Asked for on the local port.
    Page(KeyAction),
}

/// The `Gate`'s id for the local port; devices count up from 1.
pub const PORT_ID: u32 = 0;
static NEXT_ID: AtomicU32 = AtomicU32::new(PORT_ID + 1);

fn event_paths() -> Result<Vec<PathBuf>> {
    let mut paths: Vec<_> = std::fs::read_dir("/dev/input")
        .context("can't list /dev/input")?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("event")))
        .collect();
    paths.sort();
    Ok(paths)
}

fn open_device(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC).open(path)
}

/// Scans `/dev/input/event*` once to find the touchscreen and, when requested,
/// the Kindle's page-turn buttons. Touch-only Kindles pass no `layout`
/// and stop scanning as soon as the touchscreen is found; remotes are
/// left to `watch`, off the startup path.
pub fn open(map: Mapping, layout: Option<PageButtons>) -> Result<(TouchInput, Option<KeySource>)> {
    let mut touch: Option<TouchInput> = None;
    let mut buttons: Option<KeySource> = None;
    let mut map = Some(map);
    for path in event_paths()? {
        if touch.is_some() && (layout.is_none() || buttons.is_some()) {
            break;
        }
        let Ok(file) = open_device(&path) else { continue };
        let fd = file.as_raw_fd();
        if has_mt_position(fd) {
            if touch.is_none() {
                touch = Some(TouchInput::from_file(file, &path, map.take().expect("touch opened once"))?);
            }
            continue;
        }
        if layout.is_some() && buttons.is_none() {
            buttons = KeySource::from_file(file, &path, layout).filter(|s| s.kind == Kind::Buttons);
            if let Some(buttons) = &mut buttons
                && let Err(e) = buttons.grab(true)
            {
                log::warn!("{e:#}");
            }
        }
    }
    let touch = touch.ok_or_else(|| Unsupported("no touchscreen found in /dev/input".into()))?;
    Ok((touch, buttons))
}

/// Sends every remote there is and every one that appears later, until the
/// process ends. `held`: the devices `open` took. Opening a device can be
/// slow, so this has its own thread (`verso-remotes`).
pub fn watch(held: &[&str], mut found: impl FnMut(KeySource)) {
    // Which device each path was when we looked at it. A remote that comes
    // back may get its old path.
    let mut seen: HashMap<PathBuf, (u64, u64)> = HashMap::new();
    for path in held.iter().map(PathBuf::from) {
        if let Some(id) = identity(&path) {
            seen.insert(path, id);
        }
    }
    // Before the first scan, so nothing appears unnoticed in between.
    let notify = watch_directory(c"/dev/input");
    scan(&mut seen, &mut found);
    let mut notify = match notify {
        Ok(notify) => notify,
        Err(e) => {
            log::warn!("remotes connected from now on won't be noticed: {e}");
            return;
        }
    };
    let mut events = [0u8; 4096];
    loop {
        match notify.read(&mut events) {
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => {
                log::warn!("remotes connected from now on won't be noticed: {e}");
                return;
            }
        }
        // A new device isn't ready at once: udev sets its permissions, and
        // Bluetooth devices get their address (which `mapper_binds` needs) late.
        for delay in [500, 2000] {
            std::thread::sleep(Duration::from_millis(delay));
            scan(&mut seen, &mut found);
        }
    }
}

fn scan(seen: &mut HashMap<PathBuf, (u64, u64)>, found: &mut impl FnMut(KeySource)) {
    let paths = match event_paths() {
        Ok(paths) => paths,
        Err(e) => return log::warn!("{e:#}"),
    };
    seen.retain(|path, _| paths.contains(path));
    for path in paths {
        let Some(id) = identity(&path) else { continue };
        if seen.get(&path) == Some(&id) {
            continue;
        }
        let file = match open_device(&path) {
            Ok(file) => file,
            Err(e) => {
                log::info!("input device {}: can't open: {e}", path.display());
                continue;
            }
        };
        seen.insert(path.clone(), id);
        log::info!("input device {}: {}", path.display(), device_name(file.as_raw_fd()));
        if has_mt_position(file.as_raw_fd()) {
            continue;
        }
        if let Some(source) = KeySource::from_file(file, &path, None) {
            if source.kind == Kind::Remote && source.bound_in_mapper() {
                log::info!("{}: left to {}, which has bindings for it", source.label(), remote::MAPPER);
                continue;
            }
            log::info!("{}: connected ({:?})", source.label(), source.kind);
            found(source);
        }
    }
}

/// Tells a device from a later one at the same path: its file in `/dev`, and
/// its folder in sysfs, which is new even if the `/dev` file was left behind.
fn identity(path: &Path) -> Option<(u64, u64)> {
    let node = std::fs::metadata(path).ok()?.ino();
    let sysfs = std::fs::metadata(Path::new("/sys/class/input").join(path.file_name()?)).map_or(0, |m| m.ino());
    Some((node, sysfs))
}

fn watch_directory(dir: &CStr) -> std::io::Result<File> {
    // SAFETY: plain syscalls; the new descriptor is owned by the File at once.
    unsafe {
        let fd = libc::inotify_init1(libc::IN_CLOEXEC);
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let file = File::from_raw_fd(fd);
        if libc::inotify_add_watch(fd, dir.as_ptr(), libc::IN_CREATE | libc::IN_MOVED_TO | libc::IN_ATTRIB) < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(file)
    }
}

impl KeySource {
    fn from_file(file: File, path: &Path, layout: Option<PageButtons>) -> Option<Self> {
        let fd = file.as_raw_fd();
        let keys = key_bits(fd)?;
        let device_name = device_name(fd);
        let kind = remote::classify(&device_name, bus(fd), |code| keys.has(code), layout.is_some())?;
        Some(Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            kind,
            layout,
            file,
            path: path.display().to_string(),
            device_name,
            can_grab: kind == Kind::Buttons && !keys.has(KEY_POWER),
            grabbed: false,
            has_volume_down: keys.has(KEY_VOLUMEDOWN),
            counts: Counts::default(),
            buf: vec![InputEvent::default(); 16],
        })
    }

    fn bound_in_mapper(&self) -> bool {
        Path::new(remote::MAPPER_BINARY).exists()
            && std::fs::read(remote::MAPPER_CONFIG).is_ok_and(|config| {
                remote::mapper_binds(&String::from_utf8_lossy(&config), &self.device_name, &device_uniq(self.fd()))
            })
    }

    pub fn summary(&self) -> String {
        format!(
            "{} ({}), {}",
            self.path,
            self.device_name,
            if self.grabbed {
                "grabbed"
            } else if self.can_grab {
                "not grabbed"
            } else {
                "shared with the power key, not grabbed"
            }
        )
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// For the log (`report.sh` picks lines by it).
    pub fn label(&self) -> String {
        let what = if self.is_remote() { "remote" } else { "page buttons" };
        format!("{what} {}", self.device_name)
    }

    pub fn is_remote(&self) -> bool {
        self.kind != Kind::Buttons
    }

    pub fn fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }

    /// Takes or releases the buttons (remotes are never taken); presses made
    /// meanwhile are dropped.
    pub fn grab(&mut self, grab: bool) -> Result<()> {
        while self.read_batch()? > 0 {}
        let grab = grab && self.can_grab;
        if self.grabbed == grab {
            return Ok(());
        }
        // SAFETY: EVIOCGRAB takes an int argument by value.
        if unsafe { libc::ioctl(self.fd(), EVIOCGRAB as _, i32::from(grab)) } < 0 {
            bail!("can't change the page buttons' grab: {}", std::io::Error::last_os_error());
        }
        self.grabbed = grab;
        Ok(())
    }

    /// Reads everything available (without blocking) and queues a key
    /// event for each page-key press. Releases and auto-repeat are ignored.
    pub fn read(&mut self, orientation: Orientation, gate: &mut Gate, out: &mut VecDeque<Event>) -> Result<()> {
        loop {
            let n = self.read_batch()?;
            if n == 0 {
                return Ok(());
            }
            for i in 0..n {
                let ev = self.buf[i];
                if ev.kind == EV_KEY && ev.value == 1 {
                    self.press(ev.code, orientation, gate, out);
                }
            }
        }
    }

    fn press(&mut self, code: u16, orientation: Orientation, gate: &mut Gate, out: &mut VecDeque<Event>) {
        // The Gate takes the Kindle's buttons as one: quick presses of two of
        // them are real.
        let (name, key, button) = match self.kind {
            Kind::Buttons => {
                let key = self.layout.and_then(|layout| KeyAction::from_evdev(code, layout, orientation));
                ("page button", key, 0)
            }
            Kind::Virtual | Kind::Remote => match remote::page_key(code, self.has_volume_down) {
                Some((name, action)) => (name, Some(action), code),
                None => ("", None, 0),
            },
        };
        let Some(action) = key else {
            self.counts.other += 1;
            return;
        };
        if !gate.accept((self.id, button), Instant::now()) {
            self.counts.echoes += 1;
            return;
        }
        if self.counts.forward + self.counts.back == 0 {
            log::info!("{}: first {name}, {orientation:?}, {action:?}", self.label());
        }
        match action {
            KeyAction::PageNext => self.counts.forward += 1,
            KeyAction::PagePrev => self.counts.back += 1,
        }
        out.push_back(Event::Key { action, button: self.kind == Kind::Buttons });
    }

    /// The number of events now in `buf`; 0 when there are none waiting.
    fn read_batch(&mut self) -> Result<usize> {
        let event_size = size_of::<InputEvent>();
        loop {
            // SAFETY: InputEvent is plain old data; any bytes are a valid value.
            let bytes = unsafe {
                std::slice::from_raw_parts_mut(self.buf.as_mut_ptr() as *mut u8, self.buf.len() * event_size)
            };
            return match self.file.read(bytes) {
                Ok(0) => bail!("{} closed", self.path),
                Ok(n) => Ok(n / event_size),
                Err(e) if e.kind() == ErrorKind::WouldBlock => Ok(0),
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => Err(e).with_context(|| format!("reading {}", self.path)),
            };
        }
    }
}

impl Drop for KeySource {
    fn drop(&mut self) {
        if self.grabbed {
            // SAFETY: EVIOCGRAB takes an int argument by value.
            unsafe { libc::ioctl(self.file.as_raw_fd(), EVIOCGRAB as _, 0 as libc::c_int) };
        }
        let Counts { forward, back, echoes, other } = self.counts;
        if forward + back + echoes + other > 0 {
            log::info!("{}: {forward} forward, {back} back, {echoes} echoes dropped, {other} other keys", self.label());
        }
    }
}

/// A bit for each key (KEY_MAX is 0x2ff).
const KEY_BYTES: usize = 96;
struct KeyBits([u8; KEY_BYTES]);

impl KeyBits {
    fn has(&self, code: u16) -> bool {
        self.0.get(code as usize / 8).is_some_and(|b| b & (1 << (code % 8)) != 0)
    }
}

/// The keys a device has.
fn key_bits(fd: RawFd) -> Option<KeyBits> {
    read_keys(fd, eviocgbit_key(KEY_BYTES as u32))
}

/// The keys held down now.
fn key_state(fd: RawFd) -> Option<KeyBits> {
    read_keys(fd, EVIOCGKEY)
}

fn read_keys(fd: RawFd, request: u32) -> Option<KeyBits> {
    let mut bits = KeyBits([0; KEY_BYTES]);
    // SAFETY: the buffer is as long as the size encoded in both requests.
    let rc = unsafe { libc::ioctl(fd, request as _, bits.0.as_mut_ptr()) };
    (rc >= 0).then_some(bits)
}

impl Drop for TouchInput {
    fn drop(&mut self) {
        // SAFETY: EVIOCGRAB takes an int argument by value.
        unsafe { libc::ioctl(self.file.as_raw_fd(), EVIOCGRAB as _, 0 as libc::c_int) };
    }
}

fn has_mt_position(fd: RawFd) -> bool {
    abs_bits(fd).is_some_and(|bits| has_abs(&bits, ABS_MT_POSITION_X) && has_abs(&bits, ABS_MT_POSITION_Y))
}

/// The axes a device has (ABS_MAX is 0x3f, so 64 bits are enough).
fn abs_bits(fd: RawFd) -> Option<[u8; 8]> {
    let mut bits = [0u8; 8];
    // SAFETY: the buffer is as long as the size encoded in the request.
    let rc = unsafe { libc::ioctl(fd, eviocgbit_abs(bits.len() as u32) as _, bits.as_mut_ptr()) };
    (rc >= 0).then_some(bits)
}

fn has_abs(bits: &[u8; 8], code: u16) -> bool {
    bits[code as usize / 8] & (1 << (code % 8)) != 0
}

fn device_name(fd: RawFd) -> String {
    device_string(fd, eviocgname).unwrap_or_else(|| "unknown".into())
}

/// A Bluetooth device's address; never logged.
fn device_uniq(fd: RawFd) -> String {
    device_string(fd, eviocguniq).unwrap_or_default()
}

fn device_string(fd: RawFd, request: fn(u32) -> u32) -> Option<String> {
    let mut text = [0u8; 128];
    // SAFETY: the buffer is as long as the size encoded in the request.
    let rc = unsafe { libc::ioctl(fd, request(text.len() as u32 - 1) as _, text.as_mut_ptr()) };
    if rc < 0 {
        return None;
    }
    CStr::from_bytes_until_nul(&text).ok().map(|s| s.to_string_lossy().into_owned())
}

/// The bus a device is on (`BUS_USB`, `BUS_BLUETOOTH`, …).
fn bus(fd: RawFd) -> u16 {
    let mut id = [0u16; 4]; // struct input_id: bustype, vendor, product, version
    // SAFETY: the buffer is as long as the size encoded in the request.
    let rc = unsafe { libc::ioctl(fd, EVIOCGID as _, id.as_mut_ptr()) };
    if rc < 0 { 0 } else { id[0] }
}

fn abs_info(fd: RawFd, axis: u16) -> Result<AbsInfo> {
    let mut info = AbsInfo::default();
    // SAFETY: AbsInfo matches struct input_absinfo, whose size is in the request.
    if unsafe { libc::ioctl(fd, eviocgabs(axis) as _, &mut info as *mut AbsInfo) } < 0 {
        bail!("can't read the range of touch axis {axis:#x}: {}", std::io::Error::last_os_error());
    }
    Ok(info)
}
