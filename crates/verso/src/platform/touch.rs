//! Touchscreen events to touches, everything but the device file
//! (`kindle/input.rs`). A SYN_REPORT ends each batch of changes. Kindles have
//! two kinds of panels, told apart by whether they have ABS_MT_SLOT:
//!
//! - **With slots** ("protocol B", nearly all): each finger lives in a numbered
//!   slot and has a tracking id, which becomes -1 when it lifts.
//! - **Without** (some Basic 3s): every batch names its fingers by tracking id,
//!   which a later finger may have again. It never becomes -1: BTN_TOUCH = 0
//!   says that the last finger lifted.

use std::collections::VecDeque;
use std::time::Instant;

use crate::geom::Point;
use crate::platform::{Event, Orientation, Touch, TouchPhase};

// Event types and codes from <linux/input-event-codes.h>.
const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0;
const SYN_DROPPED: u16 = 3;
pub const BTN_TOUCH: u16 = 0x14a;
pub const ABS_MT_SLOT: u16 = 0x2f;
pub const ABS_MT_POSITION_X: u16 = 0x35;
pub const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;

pub const MAX_SLOTS: usize = 10;

#[derive(Clone, Copy)]
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

pub struct Decoder {
    pub map: Mapping,
    pub x_range: (i32, i32),
    pub y_range: (i32, i32),
    /// The panel has ABS_MT_SLOT.
    pub slotted: bool,
    slots: [Slot; MAX_SLOTS],
    slot: usize,
    /// After SYN_DROPPED, ignore everything up to the next SYN_REPORT.
    dropping: bool,
    /// Without slots: fingers held through a `reset`, ignored until they lift.
    held: bool,
}

impl Decoder {
    pub fn new(map: Mapping, x_range: (i32, i32), y_range: (i32, i32), slotted: bool) -> Self {
        let empty = Slot { id: -1, x: 0, y: 0, reported_id: -1, reported: Point::default(), changed: false };
        Self { map, x_range, y_range, slotted, slots: [empty; MAX_SLOTS], slot: 0, dropping: false, held: false }
    }

    /// The kernel's value of a position axis for each slot.
    pub fn set_positions(&mut self, axis: u16, values: &[i32]) {
        for (slot, value) in self.slots.iter_mut().zip(values) {
            if axis == ABS_MT_POSITION_X {
                slot.x = *value;
            } else {
                slot.y = *value;
            }
        }
    }

    /// Forgets every finger. Those still held count again once they lifted.
    /// `slot`: the kernel's current one; `held`: its BTN_TOUCH.
    pub fn reset(&mut self, slot: i32, held: bool) {
        for slot in &mut self.slots {
            slot.id = -1;
            slot.reported_id = -1;
            slot.changed = false;
        }
        self.slot = slot_index(slot);
        self.dropping = false;
        self.held = held && !self.slotted;
    }

    /// Takes one event and queues touches at the end of a batch. True after
    /// lost events: the caller reads the kernel's state and calls `reset`.
    pub fn handle(&mut self, kind: u16, code: u16, value: i32, now: Instant, out: &mut VecDeque<Event>) -> bool {
        match (kind, code) {
            (EV_SYN, SYN_DROPPED) => {
                log::warn!("touch events dropped; resetting");
                self.dropping = true;
            }
            (EV_SYN, SYN_REPORT) if self.dropping => {
                // We lost track: lift every finger we reported.
                for slot in &self.slots {
                    if slot.reported_id >= 0 {
                        out.push_back(touch(slot.reported_id, TouchPhase::Up, slot.reported, now));
                    }
                }
                return true;
            }
            (EV_SYN, SYN_REPORT) => self.flush(now, out),
            _ if self.dropping => {}
            (EV_KEY, BTN_TOUCH) if !self.slotted && value == 0 => {
                self.held = false;
                for slot in &mut self.slots {
                    slot.changed |= slot.id >= 0;
                    slot.id = -1;
                }
            }
            _ if self.held => {}
            (EV_ABS, ABS_MT_SLOT) => self.slot = slot_index(value),
            (EV_ABS, ABS_MT_TRACKING_ID) => {
                if !self.slotted && value >= 0 {
                    self.slot = self.slot_of(value);
                }
                self.slots[self.slot].id = value;
                self.slots[self.slot].changed = true;
            }
            (EV_ABS, ABS_MT_POSITION_X) => {
                self.slots[self.slot].x = value;
                self.slots[self.slot].changed = true;
            }
            (EV_ABS, ABS_MT_POSITION_Y) => {
                self.slots[self.slot].y = value;
                self.slots[self.slot].changed = true;
            }
            _ => {}
        }
        false
    }

    /// Without slots: the finger's slot, or a free one for a new finger.
    fn slot_of(&self, id: i32) -> usize {
        let find = |id| self.slots.iter().position(|s| s.id == id);
        find(id).or_else(|| find(-1)).unwrap_or(MAX_SLOTS - 1)
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

fn slot_index(value: i32) -> usize {
    (value.max(0) as usize).min(MAX_SLOTS - 1)
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

fn touch(id: i32, phase: TouchPhase, pos: Point, time: Instant) -> Event {
    Event::Touch(Touch { id, phase, pos, time })
}

#[cfg(test)]
mod tests {
    use super::*;
    use TouchPhase::*;

    const X: u16 = ABS_MT_POSITION_X;
    const Y: u16 = ABS_MT_POSITION_Y;
    const ID: u16 = ABS_MT_TRACKING_ID;
    const SYN: (u16, u16, i32) = (EV_SYN, SYN_REPORT, 0);
    const LIFT: [(u16, u16, i32); 2] = [(EV_KEY, BTN_TOUCH, 0), SYN];

    fn decoder(slotted: bool) -> Decoder {
        let map = Mapping {
            width: 600,
            height: 800,
            swap_axes: false,
            mirror_x: false,
            mirror_y: false,
            orientation: Orientation::Portrait,
        };
        Decoder::new(map, (0, 599), (0, 799), slotted)
    }

    fn feed(d: &mut Decoder, events: &[(u16, u16, i32)]) -> Vec<(i32, TouchPhase, i32, i32)> {
        let mut out = VecDeque::new();
        for &(kind, code, value) in events {
            assert!(!d.handle(kind, code, value, Instant::now(), &mut out));
        }
        let touches = out.into_iter().map(|e| match e {
            Event::Touch(t) => (t.id, t.phase, t.pos.x, t.pos.y),
            e => panic!("{e:?}"),
        });
        touches.collect()
    }

    /// A finger as a panel without slots reports it, in every batch.
    fn finger(id: i32, x: i32, y: i32) -> [(u16, u16, i32); 3] {
        [(EV_ABS, ID, id), (EV_ABS, X, x), (EV_ABS, Y, y)]
    }

    fn down(id: i32, x: i32, y: i32) -> Vec<(u16, u16, i32)> {
        [&[(EV_KEY, BTN_TOUCH, 1)], &finger(id, x, y)[..], &[SYN]].concat()
    }

    #[test]
    fn without_slots_fingers_go_by_tracking_id_and_lift_on_btn_touch() {
        let mut d = decoder(false);
        // Two taps with the same id.
        for (x, y) in [(100, 200), (300, 400)] {
            assert_eq!(feed(&mut d, &down(0, x, y)), [(0, Down, x, y)]);
            assert_eq!(feed(&mut d, &[&finger(0, x + 1, y)[..], &[SYN]].concat()), [(0, Move, x + 1, y)]);
            assert_eq!(feed(&mut d, &LIFT), [(0, Up, x + 1, y)]);
        }
        // Two fingers in one batch.
        assert_eq!(feed(&mut d, &down(3, 10, 20)), [(3, Down, 10, 20)]);
        let both = [&finger(3, 10, 20)[..], &finger(4, 50, 60), &[SYN]].concat();
        assert_eq!(feed(&mut d, &both), [(4, Down, 50, 60)]);
        assert_eq!(feed(&mut d, &LIFT), [(3, Up, 10, 20), (4, Up, 50, 60)]);
        // Held while Verso takes the screen back: ignored until it lifts.
        d.reset(0, true);
        assert_eq!(feed(&mut d, &[&finger(0, 10, 20)[..], &[SYN]].concat()), []);
        assert_eq!(feed(&mut d, &LIFT), []);
        assert_eq!(feed(&mut d, &down(0, 10, 20)), [(0, Down, 10, 20)]);
    }

    #[test]
    fn with_slots_fingers_lift_on_their_tracking_id_alone() {
        let mut d = decoder(true);
        let slot = |n| (EV_ABS, ABS_MT_SLOT, n);
        let tap = [&[slot(0)], &finger(7, 100, 200)[..], &[slot(1)], &finger(8, 300, 400), &[SYN]].concat();
        assert_eq!(feed(&mut d, &tap), [(7, Down, 100, 200), (8, Down, 300, 400)]);
        assert_eq!(feed(&mut d, &LIFT), [], "BTN_TOUCH says nothing about a slot");
        assert_eq!(feed(&mut d, &[slot(0), (EV_ABS, ID, -1), SYN]), [(7, Up, 100, 200)]);
        // evdev leaves out an axis that didn't change.
        assert_eq!(feed(&mut d, &[(EV_ABS, ID, 9), (EV_ABS, X, 110), SYN]), [(9, Down, 110, 200)]);
    }

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
