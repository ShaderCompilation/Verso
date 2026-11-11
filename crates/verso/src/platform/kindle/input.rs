//! The touchscreen, read straight from the Linux input device (evdev).
//!
//! The Kindle's touchscreen speaks the multi-touch "protocol B": each finger
//! lives in a numbered slot and has a tracking id (-1 when lifted), and a
//! SYN_REPORT marks the end of each batch of changes.

use std::collections::VecDeque;
use std::ffi::CStr;
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::time::Instant;

use anyhow::{Context, Result, bail};

use crate::geom::Point;
use crate::platform::{Event, Touch, TouchPhase};

// Event types and codes from <linux/input-event-codes.h>.
const EV_SYN: u16 = 0x00;
const EV_ABS: u16 = 0x03;
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
    pub width: u32,
    pub height: u32,
    pub swap_axes: bool,
    pub mirror_x: bool,
    pub mirror_y: bool,
}

pub struct TouchInput {
    grabbed: bool,
    file: File,
    path: String,
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
    /// Finds the touchscreen, grabs it exclusively, and reads its coordinate range.
    pub fn open(map: Mapping) -> Result<Self> {
        let mut paths: Vec<_> = std::fs::read_dir("/dev/input")
            .context("can't list /dev/input")?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("event")))
            .collect();
        paths.sort();
        for path in paths {
            let Ok(file) = OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC).open(&path)
            else {
                continue;
            };
            if !has_mt_position(file.as_raw_fd()) {
                continue;
            }
            let fd = file.as_raw_fd();
            let name = device_name(fd);
            let x = abs_info(fd, ABS_MT_POSITION_X)?;
            let y = abs_info(fd, ABS_MT_POSITION_Y)?;
            log::info!(
                "touchscreen {} ({name}): x {}..{}, y {}..{}",
                path.display(),
                x.minimum,
                x.maximum,
                y.minimum,
                y.maximum
            );
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
                x_range: (x.minimum, x.maximum),
                y_range: (y.minimum, y.maximum),
                map,
                slots: [empty; MAX_SLOTS],
                slot: 0,
                dropping: false,
                buf: vec![InputEvent::default(); 64],
            };
            input.reset_contacts();
            return Ok(input);
        }
        bail!("no multi-touch screen found in /dev/input")
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
        let scale = |v: i32, (lo, hi): (i32, i32), size: u32| -> i32 {
            let span = (hi - lo).max(1) as i64;
            (((v - lo).clamp(0, hi - lo) as i64 * (size as i64 - 1)) / span) as i32
        };
        let mut x = scale(rx, xr, self.map.width);
        let mut y = scale(ry, yr, self.map.height);
        if self.map.mirror_x {
            x = self.map.width as i32 - 1 - x;
        }
        if self.map.mirror_y {
            y = self.map.height as i32 - 1 - y;
        }
        Point::new(x, y)
    }
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
