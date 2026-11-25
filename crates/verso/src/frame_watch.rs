//! Whether Verso's frame stayed on screen after a launch or a wake, for
//! problem reports: two seconds later and at the first touch, the log says
//! whether the framebuffer still holds what Verso drew, and then what that
//! touch refreshed. "Overwritten" means something else drew over Verso;
//! "intact" while the screen looked wrong points at the refresh or the panel.

use std::time::{Duration, Instant};

use crate::geom::Rect;
use crate::platform::{TouchPhase, Update};

/// After the frame, and past the wake cleanup's repaint.
const CHECK_AFTER: Duration = Duration::from_secs(2);

#[derive(Clone, Copy)]
enum Touch {
    Waiting,
    /// The first touch is down, or just lifted: its refreshes are counted.
    Counting {
        lifted: bool,
        refreshes: usize,
        largest: Rect,
    },
    Done,
}

pub struct FrameWatch {
    /// Whether to wake up for the timed check.
    timed: bool,
    after: &'static str,
    /// When the frame was sent; `None` until then.
    shown: Option<Instant>,
    check_due: Option<Instant>,
    touch: Touch,
}

impl FrameWatch {
    /// Watches for the first frame.
    pub fn new(timed: bool) -> Self {
        Self { timed, after: "launch", shown: None, check_due: None, touch: Touch::Waiting }
    }

    /// Going to sleep: nothing to watch until the next wake.
    pub fn sleep(&mut self) {
        (self.shown, self.check_due, self.touch) = (None, None, Touch::Done);
    }

    /// Watches for the first frame after a wake.
    pub fn wake(&mut self) {
        (self.after, self.shown, self.check_due, self.touch) = ("wake", None, None, Touch::Waiting);
    }

    pub fn presented(&mut self, updates: &[Update], now: Instant) {
        if self.shown.is_none() && matches!(self.touch, Touch::Waiting) {
            self.shown = Some(now);
            self.check_due = self.timed.then(|| now + CHECK_AFTER);
        }
        if let Touch::Counting { refreshes, largest, .. } = &mut self.touch {
            *refreshes += updates.len();
            for u in updates {
                if i64::from(u.rect.w) * i64::from(u.rect.h) > i64::from(largest.w) * i64::from(largest.h) {
                    *largest = u.rect;
                }
            }
        }
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.check_due
    }

    /// When the timed check is due, once: what to call the frame in the log.
    pub fn take_due(&mut self, now: Instant) -> Option<String> {
        let shown = self.shown?;
        self.check_due.take_if(|due| now >= *due)?;
        Some(format!("frame check {:.1} s after the {} frame", (now - shown).as_secs_f32(), self.after))
    }

    /// Follows the touches; what to call the first one in the log.
    pub fn touch(&mut self, phase: TouchPhase, now: Instant) -> Option<String> {
        match (phase, &mut self.touch) {
            (TouchPhase::Down, Touch::Waiting) => {
                let shown = self.shown?;
                self.touch = Touch::Counting { lifted: false, refreshes: 0, largest: Rect::default() };
                let after = now.saturating_duration_since(shown).as_secs_f32();
                return Some(format!("first touch {after:.1} s after the {} frame", self.after));
            }
            (TouchPhase::Up, Touch::Counting { lifted, .. }) => *lifted = true,
            _ => {}
        }
        None
    }

    /// Once the frame after the first touch's lift went out (or there was
    /// none to send): what the touch refreshed.
    pub fn take_touch_summary(&mut self) -> Option<String> {
        let Touch::Counting { lifted: true, refreshes, largest } = self.touch else { return None };
        self.touch = Touch::Done;
        Some(if refreshes == 0 {
            "first touch: refreshed nothing".into()
        } else {
            format!("first touch: {refreshes} refresh(es), largest {}x{}", largest.w, largest.h)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::RefreshMode;

    #[test]
    fn checks_each_shown_frame_once_and_counts_what_the_first_touch_refreshed() {
        let t = Instant::now();
        let at = |ms| t + Duration::from_millis(ms);
        let update = |w, h| Update { rect: Rect::new(0, 0, w, h), mode: RefreshMode::Ui };
        let mut watch = FrameWatch::new(true);
        // Nothing to check before the frame is out.
        assert_eq!(watch.touch(TouchPhase::Down, at(0)), None);
        assert_eq!(watch.deadline(), None);
        watch.presented(&[update(100, 100)], at(100));
        watch.presented(&[update(10, 10)], at(1000));
        assert_eq!(watch.deadline(), Some(at(2100)));
        assert_eq!(watch.take_due(at(2000)), None);
        assert_eq!(watch.take_due(at(2100)).as_deref(), Some("frame check 2.0 s after the launch frame"));
        assert_eq!((watch.take_due(at(2200)), watch.deadline()), (None, None));

        let first = watch.touch(TouchPhase::Down, at(3600));
        assert_eq!(first.as_deref(), Some("first touch 3.5 s after the launch frame"));
        watch.presented(&[update(20, 20)], at(3610));
        assert_eq!(watch.take_touch_summary(), None, "still down");
        watch.touch(TouchPhase::Up, at(3700));
        watch.presented(&[update(60, 80), update(5, 5)], at(3710));
        assert_eq!(watch.take_touch_summary().as_deref(), Some("first touch: 3 refresh(es), largest 60x80"));
        assert_eq!(watch.touch(TouchPhase::Down, at(4000)), None, "only the first");
        assert_eq!(watch.take_touch_summary(), None);

        // Asleep, the sleep cover and stray touches aren't watched.
        watch.sleep();
        watch.presented(&[update(100, 100)], at(5000));
        assert_eq!((watch.deadline(), watch.touch(TouchPhase::Down, at(5100))), (None, None));
        watch.wake();
        assert_eq!(watch.touch(TouchPhase::Down, at(6000)), None, "the wake frame isn't out yet");
        watch.presented(&[update(100, 100)], at(6100));
        let first = watch.touch(TouchPhase::Down, at(6600));
        assert_eq!(first.as_deref(), Some("first touch 0.5 s after the wake frame"));
        watch.touch(TouchPhase::Up, at(6700));
        assert_eq!(watch.take_touch_summary().as_deref(), Some("first touch: refreshed nothing"));
        assert!(watch.take_due(at(8100)).is_some());

        assert_eq!(FrameWatch::new(false).deadline(), None);
    }
}
