//! The touchscreen, read straight from the Linux input device (evdev).
//!
//! The Kindle's touchscreen speaks the multi-touch "protocol B": each finger
//! lives in a numbered slot and has a tracking id (-1 when lifted), and a
//! SYN_REPORT marks the end of each batch of changes.
//!
//! Page-turn buttons (Oasis) are a separate key device, read alongside.

use std::collections::VecDeque;
use std::ffi::CStr;
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::time::Instant;

use anyhow::{Context, Result, bail};

use crate::geom::Point;
use crate::platform::{Event, KeyAction, Orientation, Touch, TouchPhase, Unsupported};

// Event types and codes from <linux/input-event-codes.h>.
const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;
const KEY_POWER: u16 = 116;
/// The page keys FBInk's input scan looks for on a Kindle: PAGEUP, PAGEDOWN,
/// YEN and F23 (see `KeyAction::from_evdev`).
const PAGE_KEYS: [u16; 4] = [104, 109, 124, 193];
const SYN_REPORT: u16 = 0;
const SYN_DROPPED: u16 = 3;
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;

const MAX_SLOTS: usize = 10;

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
const fn eviocgname(len: u32) -> u32 {
    ioc(IOC_READ, 0x06, len)
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
const EVIOCGRAB: u32 = ioc(IOC_WRITE, 0x90, size_of::<libc::c_int>() as u32);

#[derive(Clone, Copy, Default)]
struct Slot {
    /// Tracking id the kernel reports; -1 when no finger.
    id: i32,
    x: i32,
    y: i32,
    /// What we last told the app about this slot.
    reported_id: i32,
    reported: Point,
    changed: bool,
}

/// Raw-to-screen coordinate mapping.
pub struct Mapping {
    /// The panel's native (portrait) size.
    pub width: u32,
    pub height: u32,
    pub swap_axes: bool,
    pub mirror_x: bool,
    pub mirror_y: bool,
    pub orientation: Orientation,
}

impl Mapping {
    /// Panel pixel → app screen pixel; the inverse of the display's rotation
    /// (`Canvas::copy_gray_rotated`).
    fn rotate(&self, p: Point) -> Point {
        match self.orientation {
            Orientation::Portrait => p,
            Orientation::Landscape => Point::new(self.height as i32 - 1 - p.y, p.x),
        }
    }
}

pub struct TouchInput {
    grabbed: bool,
    file: File,
    path: String,
    device_name: String,
    x_range: (i32, i32),
    y_range: (i32, i32),
    map: Mapping,
    slots: [Slot; MAX_SLOTS],
    slot: usize,
    /// After SYN_DROPPED, ignore everything up to the next SYN_REPORT.
    dropping: bool,
    buf: Vec<InputEvent>,
}

impl TouchInput {
    fn from_file(file: File, path: &std::path::Path, map: Mapping) -> Result<Self> {
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
        let empty = Slot { id: -1, reported_id: -1, ..Slot::default() };
        let mut input = Self {
            grabbed: true,
            file,
            path: path.display().to_string(),
            device_name: name,
            x_range: (x.minimum, x.maximum),
            y_range: (y.minimum, y.maximum),
            map,
            slots: [empty; MAX_SLOTS],
            slot: 0,
            dropping: false,
            buf: vec![InputEvent::default(); 64],
        };
        input.reset_contacts();
        Ok(input)
    }

    pub fn summary(&self) -> String {
        format!(
            "{} ({}), x {}..{}, y {}..{}, swap={}, mirror_x={}, mirror_y={}",
            self.path,
            self.device_name,
            self.x_range.0,
            self.x_range.1,
            self.y_range.0,
            self.y_range.1,
            self.map.swap_axes,
            self.map.mirror_x,
            self.map.mirror_y,
        )
    }

    pub fn set_orientation(&mut self, orientation: Orientation) {
        self.map.orientation = orientation;
    }

    pub fn fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }

    pub fn grab(&mut self, grab: bool) -> Result<()> {
        // Discard touches accumulated while Amazon owned the screensaver.
        self.read(&mut VecDeque::new())?;
        self.reset_contacts();
        self.dropping = false;
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
        // one axis at zero. Ignore already-held fingers until a new tracking id.
        for code in [ABS_MT_POSITION_X, ABS_MT_POSITION_Y] {
            let mut values = [0i32; MAX_SLOTS + 1];
            values[0] = code as i32;
            let request = ioc(IOC_READ, 0x0a, size_of_val(&values) as u32); // EVIOCGMTSLOTS
            if unsafe { libc::ioctl(self.fd(), request as _, values.as_mut_ptr()) } >= 0 {
                for (slot, value) in self.slots.iter_mut().zip(&values[1..]) {
                    if code == ABS_MT_POSITION_X {
                        slot.x = *value;
                    } else {
                        slot.y = *value;
                    }
                }
            }
        }
        for slot in &mut self.slots {
            slot.id = -1;
            slot.reported_id = -1;
            slot.changed = false;
        }
        self.slot = abs_info(self.fd(), ABS_MT_SLOT).map_or(0, |info| (info.value.max(0) as usize).min(MAX_SLOTS - 1));
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
                self.handle(ev, now, out);
            }
        }
    }

    fn handle(&mut self, ev: InputEvent, now: Instant, out: &mut VecDeque<Event>) {
        match (ev.kind, ev.code) {
            (EV_SYN, SYN_DROPPED) => {
                log::warn!("touch events dropped; resetting");
                self.dropping = true;
            }
            (EV_SYN, SYN_REPORT) if self.dropping => {
                self.dropping = false;
                // We lost track: lift every finger we reported.
                for slot in &mut self.slots {
                    if slot.reported_id >= 0 {
                        let pos = slot.reported;
                        out.push_back(touch(slot.reported_id, TouchPhase::Up, pos, now));
                    }
                }
                self.reset_contacts();
            }
            (EV_SYN, SYN_REPORT) => self.flush(now, out),
            _ if self.dropping => {}
            (EV_ABS, ABS_MT_SLOT) => self.slot = (ev.value.max(0) as usize).min(MAX_SLOTS - 1),
            (EV_ABS, ABS_MT_TRACKING_ID) => {
                self.slots[self.slot].id = ev.value;
                self.slots[self.slot].changed = true;
            }
            (EV_ABS, ABS_MT_POSITION_X) => {
                self.slots[self.slot].x = ev.value;
                self.slots[self.slot].changed = true;
            }
            (EV_ABS, ABS_MT_POSITION_Y) => {
                self.slots[self.slot].y = ev.value;
                self.slots[self.slot].changed = true;
            }
            _ => {}
        }
    }

    /// End of a batch: report what changed in each slot.
    fn flush(&mut self, now: Instant, out: &mut VecDeque<Event>) {
        for i in 0..MAX_SLOTS {
            let s = self.slots[i];
            if !s.changed {
                continue;
            }
            let pos = self.to_screen(s.x, s.y);
            let slot = &mut self.slots[i];
            slot.changed = false;
            // A finger lifted, or replaced by another in the same batch.
            if slot.reported_id >= 0 && slot.reported_id != s.id {
                out.push_back(touch(slot.reported_id, TouchPhase::Up, slot.reported, now));
                slot.reported_id = -1;
            }
            if s.id >= 0 {
                if slot.reported_id < 0 {
                    out.push_back(touch(s.id, TouchPhase::Down, pos, now));
                } else if pos != slot.reported {
                    out.push_back(touch(s.id, TouchPhase::Move, pos, now));
                }
                slot.reported_id = s.id;
                slot.reported = pos;
            }
        }
    }

    fn to_screen(&self, raw_x: i32, raw_y: i32) -> Point {
        let (mut rx, mut ry, mut xr, mut yr) = (raw_x, raw_y, self.x_range, self.y_range);
        if self.map.swap_axes {
            (rx, ry, xr, yr) = (ry, rx, yr, xr);
        }
        let mut x = scale_axis(rx, xr, self.map.width);
        let mut y = scale_axis(ry, yr, self.map.height);
        if self.map.mirror_x {
            x = self.map.width as i32 - 1 - x;
        }
        if self.map.mirror_y {
            y = self.map.height as i32 - 1 - y;
        }
        self.map.rotate(Point::new(x, y))
    }
}

/// Maps a raw evdev coordinate in `lo..=hi` onto `0..size`, clamping out-of-range
/// values and returning `0` for degenerate (`hi <= lo`) or empty (`size == 0`) ranges.
fn scale_axis(v: i32, (lo, hi): (i32, i32), size: u32) -> i32 {
    if hi <= lo || size == 0 {
        return 0;
    }
    let span = i64::from(hi) - i64::from(lo);
    let clamped = (i64::from(v) - i64::from(lo)).clamp(0, span);
    ((i128::from(clamped) * i128::from(size - 1)) / i128::from(span)) as i32
}

/// The page-turn buttons: an evdev key device with page keys.
pub struct Buttons {
    file: File,
    path: String,
    device_name: String,
    /// Grabbed only when the device has no power key, which powerd must
    /// keep seeing (if a Kindle ever puts both on one device).
    can_grab: bool,
    grabbed: bool,
    buf: Vec<InputEvent>,
}

/// Scans `/dev/input/event*` once to find the touchscreen and, when requested,
/// the page-turn buttons. Touch-only Kindles pass `probe_buttons = false` and
/// stop scanning as soon as the touchscreen is found.
pub fn open(map: Mapping, probe_buttons: bool) -> Result<(TouchInput, Option<Buttons>)> {
    let mut paths: Vec<_> = std::fs::read_dir("/dev/input")
        .context("can't list /dev/input")?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("event")))
        .collect();
    paths.sort();
    let mut touch: Option<TouchInput> = None;
    let mut buttons: Option<Buttons> = None;
    let mut map = Some(map);
    for path in paths {
        if touch.is_some() && (!probe_buttons || buttons.is_some()) {
            break;
        }
        let Ok(file) = OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC).open(&path)
        else {
            continue;
        };
        let fd = file.as_raw_fd();
        if has_mt_position(fd) {
            if touch.is_none() {
                touch = Some(TouchInput::from_file(file, &path, map.take().expect("touch opened once"))?);
            }
            continue;
        }
        if probe_buttons && buttons.is_none() {
            buttons = Buttons::from_file(file, &path);
        }
    }
    let touch = touch.ok_or_else(|| Unsupported("no touchscreen found in /dev/input".into()))?;
    Ok((touch, buttons))
}

impl Buttons {
    fn from_file(file: File, path: &std::path::Path) -> Option<Self> {
        let fd = file.as_raw_fd();
        let keys = key_bits(fd)?;
        if !PAGE_KEYS.iter().any(|&k| keys.has(k)) {
            return None;
        }
        let mut buttons = Self {
            file,
            path: path.display().to_string(),
            device_name: device_name(fd),
            can_grab: !keys.has(KEY_POWER),
            grabbed: false,
            buf: vec![InputEvent::default(); 16],
        };
        if let Err(e) = buttons.grab(true) {
            log::warn!("{e:#}");
        }
        Some(buttons)
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

    pub fn fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }

    /// Takes or releases the buttons; presses made meanwhile are dropped.
    pub fn grab(&mut self, grab: bool) -> Result<()> {
        self.read(Orientation::Portrait, &mut VecDeque::new())?;
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
    pub fn read(&mut self, orientation: Orientation, out: &mut VecDeque<Event>) -> Result<()> {
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
            for ev in &self.buf[..n / event_size] {
                if ev.kind == EV_KEY
                    && ev.value == 1
                    && let Some(action) = KeyAction::from_evdev(ev.code, orientation)
                {
                    out.push_back(Event::Key(action));
                }
            }
        }
    }
}

impl Drop for Buttons {
    fn drop(&mut self) {
        if self.grabbed {
            // SAFETY: EVIOCGRAB takes an int argument by value.
            unsafe { libc::ioctl(self.file.as_raw_fd(), EVIOCGRAB as _, 0 as libc::c_int) };
        }
    }
}

/// A device's EV_KEY capability bits (KEY_MAX is 0x2ff).
struct KeyBits([u8; 96]);

impl KeyBits {
    fn has(&self, code: u16) -> bool {
        self.0.get(code as usize / 8).is_some_and(|b| b & (1 << (code % 8)) != 0)
    }
}

fn key_bits(fd: RawFd) -> Option<KeyBits> {
    let mut bits = KeyBits([0; 96]);
    // SAFETY: the buffer is as long as the size encoded in the request.
    let rc = unsafe { libc::ioctl(fd, eviocgbit_key(bits.0.len() as u32) as _, bits.0.as_mut_ptr()) };
    (rc >= 0).then_some(bits)
}

impl Drop for TouchInput {
    fn drop(&mut self) {
        // SAFETY: EVIOCGRAB takes an int argument by value.
        unsafe { libc::ioctl(self.file.as_raw_fd(), EVIOCGRAB as _, 0 as libc::c_int) };
    }
}

fn touch(id: i32, phase: TouchPhase, pos: Point, time: Instant) -> Event {
    Event::Touch(Touch { id, phase, pos, time })
}

fn has_mt_position(fd: RawFd) -> bool {
    let mut bits = [0u8; 8]; // ABS_MAX is 0x3f, so 64 bits are enough
    // SAFETY: the buffer is as long as the size encoded in the request.
    let rc = unsafe { libc::ioctl(fd, eviocgbit_abs(bits.len() as u32) as _, bits.as_mut_ptr()) };
    let has = |code: u16| bits[code as usize / 8] & (1 << (code % 8)) != 0;
    rc >= 0 && has(ABS_MT_POSITION_X) && has(ABS_MT_POSITION_Y)
}

fn device_name(fd: RawFd) -> String {
    let mut name = [0u8; 128];
    // SAFETY: the buffer is as long as the size encoded in the request.
    let rc = unsafe { libc::ioctl(fd, eviocgname(name.len() as u32 - 1) as _, name.as_mut_ptr()) };
    if rc < 0 {
        return "unknown".into();
    }
    CStr::from_bytes_until_nul(&name).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

fn abs_info(fd: RawFd, axis: u16) -> Result<AbsInfo> {
    let mut info = AbsInfo::default();
    // SAFETY: AbsInfo matches struct input_absinfo, whose size is in the request.
    if unsafe { libc::ioctl(fd, eviocgabs(axis) as _, &mut info as *mut AbsInfo) } < 0 {
        bail!("can't read the range of touch axis {axis:#x}: {}", std::io::Error::last_os_error());
    }
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::scale_axis;

    #[test]
    fn scales_clamps_and_handles_degenerate_axis_ranges() {
        // Normal scaling across a typical touch axis range.
        assert_eq!(scale_axis(0, (0, 4095), 1236), 0);
        assert_eq!(scale_axis(4095, (0, 4095), 1236), 1235);
        assert_eq!(scale_axis(500, (0, 1000), 101), 50);
        assert_eq!(scale_axis(250, (100, 400), 301), 150);

        // Out-of-range values clamp cleanly to the screen bounds.
        assert_eq!(scale_axis(-50, (0, 1000), 101), 0);
        assert_eq!(scale_axis(2000, (0, 1000), 101), 100);

        // Degenerate/inverted ranges, empty screen, and extreme i32 bounds never panic or overflow.
        assert_eq!(scale_axis(10, (100, 100), 1000), 0);
        assert_eq!(scale_axis(10, (100, 50), 1000), 0);
        assert_eq!(scale_axis(500, (0, 1000), 0), 0);
        assert_eq!(scale_axis(500, (0, 1000), 1), 0);
        assert_eq!(scale_axis(0, (i32::MIN, i32::MAX), 1001), 500);
        assert_eq!(scale_axis(i32::MAX, (i32::MIN, i32::MAX), 1001), 1000);
        assert_eq!(scale_axis(i32::MIN, (i32::MIN, i32::MAX), 1001), 0);
    }
}
