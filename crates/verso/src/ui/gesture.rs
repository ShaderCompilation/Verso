//! Turns raw touches into gestures: tap, long-press, swipe and drag.
//!
//! Every touch produces `Down` first (so a button can show it's pressed right
//! away). Once the finger has moved further than a tap allows, each movement
//! produces `Move` (for dragging a slider). The touch then ends with exactly
//! one of `Tap`, `Swipe` or `Cancel`, or with `LongPress` while the finger is
//! still down followed by `End` when it lifts.
//! Only one finger is tracked; extra fingers are ignored until pinch is added.

use std::time::{Duration, Instant};

use crate::geom::Point;
use crate::platform::{Touch, TouchPhase};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gesture {
    Down(Point),
    /// The finger moved (only sent once it's moved further than a tap allows).
    Move(Point),
    Tap(Point),
    /// The finger has been held still; it's still down. `End` follows.
    LongPress(Point),
    Swipe {
        dir: Direction,
        from: Point,
        to: Point,
    },
    /// The touch ended without forming a gesture (e.g. a slow drag), here.
    Cancel(Point),
    /// The finger lifted after a long-press, here.
    End(Point),
}

impl Gesture {
    /// True for the last gesture of a touch.
    pub fn ends_touch(&self) -> bool {
        matches!(self, Gesture::Tap(_) | Gesture::Swipe { .. } | Gesture::Cancel(_) | Gesture::End(_))
    }

    /// Where the finger is (or was, when the touch ended).
    pub fn pos(&self) -> Point {
        match *self {
            Gesture::Down(p)
            | Gesture::Move(p)
            | Gesture::Tap(p)
            | Gesture::LongPress(p)
            | Gesture::Cancel(p)
            | Gesture::End(p) => p,
            Gesture::Swipe { to, .. } => to,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct GestureConfig {
    /// How far a finger may wander and still count as a tap or long-press.
    pub slop: f32,
    /// Hold this long without moving for a long-press.
    pub long_press: Duration,
    /// Minimum travel for a swipe.
    pub swipe_min: f32,
    /// A swipe must finish within this time.
    pub swipe_max_time: Duration,
}

impl GestureConfig {
    /// Distances in millimetres, converted for the screen's density.
    pub fn for_dpi(dpi: u32) -> Self {
        let mm = |v: f32| v * dpi as f32 / 25.4;
        Self {
            slop: mm(3.0),
            long_press: Duration::from_millis(500),
            swipe_min: mm(8.0),
            swipe_max_time: Duration::from_millis(1000),
        }
    }
}

struct Contact {
    id: i32,
    start: Point,
    start_time: Instant,
    last: Point,
    /// Moved further than `slop` at some point.
    wandered: bool,
}

pub struct GestureDetector {
    cfg: GestureConfig,
    contact: Option<Contact>,
    /// Set after a long-press fired: that finger's lift becomes `End`.
    held: Option<i32>,
}

impl GestureDetector {
    pub fn new(cfg: GestureConfig) -> Self {
        Self { cfg, contact: None, held: None }
    }

    /// Forgets any touch in progress (e.g. on sleep).
    pub fn reset(&mut self) {
        self.contact = None;
        self.held = None;
    }

    pub fn on_touch(&mut self, t: &Touch) -> Option<Gesture> {
        match t.phase {
            TouchPhase::Down => {
                if self.contact.is_some() || self.held.is_some() {
                    return None; // a second finger
                }
                self.contact =
                    Some(Contact { id: t.id, start: t.pos, start_time: t.time, last: t.pos, wandered: false });
                Some(Gesture::Down(t.pos))
            }
            TouchPhase::Move => {
                let c = self.contact.as_mut().filter(|c| c.id == t.id)?;
                c.last = t.pos;
                if c.start.distance(t.pos) > self.cfg.slop {
                    c.wandered = true;
                }
                c.wandered.then_some(Gesture::Move(t.pos))
            }
            TouchPhase::Up => {
                if self.held == Some(t.id) {
                    self.held = None;
                    return Some(Gesture::End(t.pos));
                }
                let c = self.contact.take_if(|c| c.id == t.id)?;
                Some(self.classify(&c, t.pos, t.time))
            }
        }
    }

    /// Call when `next_deadline` passes; fires a long-press if one is due.
    pub fn on_tick(&mut self, now: Instant) -> Option<Gesture> {
        let deadline = self.next_deadline()?;
        if now < deadline {
            return None;
        }
        let c = self.contact.take()?;
        self.held = Some(c.id);
        Some(Gesture::LongPress(c.start))
    }

    /// When `on_tick` should next be called, if ever.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.contact.as_ref().filter(|c| !c.wandered).map(|c| c.start_time + self.cfg.long_press)
    }

    fn classify(&self, c: &Contact, end: Point, time: Instant) -> Gesture {
        let elapsed = time.saturating_duration_since(c.start_time);
        if !c.wandered && c.start.distance(end) <= self.cfg.slop {
            // A late release still counts as a long-press if the tick was missed.
            return if elapsed >= self.cfg.long_press { Gesture::LongPress(c.start) } else { Gesture::Tap(c.start) };
        }
        if c.start.distance(end) >= self.cfg.swipe_min && elapsed <= self.cfg.swipe_max_time {
            let (dx, dy) = (end.x - c.start.x, end.y - c.start.y);
            let dir = if dx.abs() >= dy.abs() {
                if dx > 0 { Direction::Right } else { Direction::Left }
            } else if dy > 0 {
                Direction::Down
            } else {
                Direction::Up
            };
            return Gesture::Swipe { dir, from: c.start, to: end };
        }
        Gesture::Cancel(end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CFG: GestureConfig = GestureConfig {
        slop: 30.0,
        long_press: Duration::from_millis(500),
        swipe_min: 100.0,
        swipe_max_time: Duration::from_millis(1000),
    };

    struct Script {
        g: GestureDetector,
        t0: Instant,
    }

    impl Script {
        fn new() -> Self {
            Self { g: GestureDetector::new(CFG), t0: Instant::now() }
        }
        fn at(&self, ms: u64) -> Instant {
            self.t0 + Duration::from_millis(ms)
        }
        fn touch(&mut self, id: i32, phase: TouchPhase, x: i32, y: i32, ms: u64) -> Option<Gesture> {
            let t = Touch { id, phase, pos: Point::new(x, y), time: self.at(ms) };
            self.g.on_touch(&t)
        }
    }

    use TouchPhase::*;

    #[test]
    fn tap() {
        let mut s = Script::new();
        assert_eq!(s.touch(1, Down, 100, 100, 0), Some(Gesture::Down(Point::new(100, 100))));
        assert_eq!(s.touch(1, Move, 110, 105, 50), None);
        assert_eq!(s.touch(1, Up, 110, 105, 120), Some(Gesture::Tap(Point::new(100, 100))));
        assert_eq!(s.g.next_deadline(), None);
    }

    #[test]
    fn long_press_fires_on_tick_then_ends_on_release() {
        let mut s = Script::new();
        s.touch(1, Down, 100, 100, 0);
        assert_eq!(s.g.next_deadline(), Some(s.at(500)));
        assert_eq!(s.g.on_tick(s.at(499)), None);
        assert_eq!(s.g.on_tick(s.at(500)), Some(Gesture::LongPress(Point::new(100, 100))));
        assert_eq!(s.touch(1, Move, 200, 100, 700), None, "moves after a long-press are ignored");
        assert_eq!(s.touch(3, Down, 400, 400, 800), None, "so is another finger");
        assert_eq!(s.touch(1, Up, 102, 100, 900), Some(Gesture::End(Point::new(102, 100))));
        // The next touch works normally again.
        s.touch(2, Down, 5, 5, 1000);
        assert_eq!(s.touch(2, Up, 5, 5, 1050), Some(Gesture::Tap(Point::new(5, 5))));
    }

    #[test]
    fn swipes_in_all_directions() {
        for (dx, dy, dir) in [
            (200, 10, Direction::Right),
            (-200, 10, Direction::Left),
            (5, 200, Direction::Down),
            (5, -200, Direction::Up),
        ] {
            let mut s = Script::new();
            s.touch(1, Down, 500, 500, 0);
            s.touch(1, Move, 500 + dx / 2, 500 + dy / 2, 100);
            let g = s.touch(1, Up, 500 + dx, 500 + dy, 200);
            assert!(matches!(g, Some(Gesture::Swipe { dir: d, .. }) if d == dir), "{g:?}");
        }
    }

    #[test]
    fn moving_cancels_long_press_and_slow_drag_cancels() {
        let mut s = Script::new();
        s.touch(1, Down, 100, 100, 0);
        assert_eq!(s.touch(1, Move, 110, 100, 50), None, "small moves aren't reported");
        assert_eq!(s.touch(1, Move, 150, 100, 100), Some(Gesture::Move(Point::new(150, 100))));
        assert_eq!(
            s.touch(1, Move, 120, 100, 150),
            Some(Gesture::Move(Point::new(120, 100))),
            "even back near the start"
        );
        assert_eq!(s.g.next_deadline(), None);
        assert_eq!(s.g.on_tick(s.at(2000)), None);
        let end = s.touch(1, Up, 300, 100, 2000);
        assert_eq!(end, Some(Gesture::Cancel(Point::new(300, 100))));
        assert!(end.unwrap().ends_touch());
    }

    #[test]
    fn second_finger_is_ignored() {
        let mut s = Script::new();
        s.touch(1, Down, 100, 100, 0);
        assert_eq!(s.touch(2, Down, 400, 400, 10), None);
        assert_eq!(s.touch(2, Up, 400, 400, 50), None);
        assert_eq!(s.touch(1, Up, 100, 100, 80), Some(Gesture::Tap(Point::new(100, 100))));
    }
}
