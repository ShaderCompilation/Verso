//! Whether Verso's frame stayed on screen after a launch or a wake, for
//! problem reports: two seconds later and at the first touch, the log says
//! whether the framebuffer still holds what Verso drew, and then what that
//! touch refreshed. "Overwritten" means something else drew over Verso;
//! "intact" while the screen looked wrong points at the refresh or the panel.
//!
//! After Amazon's own sleep screen the frame is also guarded: Amazon's Home,
//! which the launcher doesn't pause, redraws itself when the sleep screen
//! closes, usually after Verso's frame is out. Nothing tells Verso when, so
//! the framebuffer is checked often and drawn again where it was overwritten.
//! For the log, it is also sampled once a second until 30 s after the frame
//! or the last late power event (`wakeupFromSuspend`), whichever is later:
//! the first result, every change and the last one.

use std::time::{Duration, Instant};

use crate::geom::Rect;
use crate::platform::{TouchPhase, Update};

/// After the frame, and past the wake cleanup's repaint.
const CHECK_AFTER: Duration = Duration::from_secs(2);
/// How long after a native wake frame Amazon's Home may still draw over it,
/// and how often to look.
const GUARD_FOR: Duration = Duration::from_secs(5);
const GUARD_EVERY: Duration = Duration::from_millis(100);
/// How long after a native wake frame, or a late power event, to sample it.
const SAMPLE_FOR: Duration = Duration::from_secs(30);
const SAMPLE_EVERY: Duration = Duration::from_secs(1);

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
    /// Whether to guard the next frame; when the guard looks next.
    guard: bool,
    guard_due: Option<Instant>,
    /// Whether this wake was from Amazon's sleep screen.
    native: bool,
    /// The next sample, the last one, and what the last logged one said.
    sample_due: Option<Instant>,
    sample_until: Option<Instant>,
    sampled: Option<String>,
    touch: Touch,
}

impl FrameWatch {
    /// Watches for the first frame.
    pub fn new(timed: bool) -> Self {
        Self {
            timed,
            after: "launch",
            shown: None,
            check_due: None,
            guard: false,
            guard_due: None,
            native: false,
            sample_due: None,
            sample_until: None,
            sampled: None,
            touch: Touch::Waiting,
        }
    }

    /// Going to sleep: nothing to watch until the next wake.
    pub fn sleep(&mut self) {
        (self.shown, self.check_due, self.guard_due, self.touch) = (None, None, None, Touch::Done);
        (self.native, self.sample_due, self.sample_until, self.sampled) = (false, None, None, None);
    }

    /// Watches for the first frame after a wake, and guards it after Amazon's
    /// own sleep screen (`native`).
    pub fn wake(&mut self, native: bool) {
        (self.after, self.shown, self.check_due, self.touch) = ("wake", None, None, Touch::Waiting);
        (self.guard, self.guard_due) = (native, None);
        (self.native, self.sample_due, self.sample_until, self.sampled) = (native, None, None, None);
    }

    /// A late power event: after a native wake, sample until `SAMPLE_FOR`
    /// after it. Before the frame is out, its own window covers it.
    pub fn late_power(&mut self, now: Instant) {
        if self.native && self.shown.is_some() {
            self.sample_until = self.sample_until.max(Some(now + SAMPLE_FOR));
            if self.sample_due.is_none() {
                (self.sample_due, self.sampled) = (Some(now + SAMPLE_EVERY), None);
            }
        }
    }

    pub fn presented(&mut self, updates: &[Update], now: Instant) {
        if self.shown.is_none() && matches!(self.touch, Touch::Waiting) {
            self.shown = Some(now);
            self.check_due = self.timed.then(|| now + CHECK_AFTER);
            self.guard_due = std::mem::take(&mut self.guard).then(|| now + GUARD_EVERY);
            if self.native {
                (self.sample_due, self.sample_until) = (Some(now + SAMPLE_EVERY), Some(now + SAMPLE_FOR));
            }
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
        [self.check_due, self.guard_due, self.sample_due].into_iter().flatten().min()
    }

    /// Samples the frame with `check` when due: the line to log, if this
    /// one's result is new or it's the last.
    pub fn take_sample(&mut self, now: Instant, check: impl FnOnce() -> Option<String>) -> Option<String> {
        let (shown, until) = (self.shown?, self.sample_until?);
        self.sample_due.take_if(|due| now >= *due)?;
        let next = now + SAMPLE_EVERY;
        self.sample_due = (next <= until).then_some(next);
        let check = check()?;
        let after = (now - shown).as_secs_f32();
        let last = self.sample_due.is_none();
        let new = self.sampled.as_ref() != Some(&check);
        let line = match (new, last) {
            (_, true) => format!("last frame sample {after:.1} s after the wake frame: {check}"),
            (true, false) => format!("frame sample {after:.1} s after the wake frame: {check}"),
            (false, false) => return None,
        };
        self.sampled = Some(check);
        Some(line)
    }

    /// Whether to look now if the guarded frame was overwritten: what to
    /// call it in the log.
    pub fn take_guard_due(&mut self, now: Instant) -> Option<String> {
        let shown = self.shown?;
        self.guard_due.take_if(|due| now >= *due)?;
        let next = now + GUARD_EVERY;
        self.guard_due = (next <= shown + GUARD_FOR).then_some(next);
        Some(format!("frame check {:.1} s after the {} frame", (now - shown).as_secs_f32(), self.after))
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
        watch.wake(false);
        assert_eq!(watch.touch(TouchPhase::Down, at(6000)), None, "the wake frame isn't out yet");
        watch.presented(&[update(100, 100)], at(6100));
        let first = watch.touch(TouchPhase::Down, at(6600));
        assert_eq!(first.as_deref(), Some("first touch 0.5 s after the wake frame"));
        watch.touch(TouchPhase::Up, at(6700));
        assert_eq!(watch.take_touch_summary().as_deref(), Some("first touch: refreshed nothing"));
        assert!(watch.take_due(at(8100)).is_some());

        assert_eq!(FrameWatch::new(false).deadline(), None);
    }

    #[test]
    fn guards_only_the_frame_after_a_native_wake() {
        let t = Instant::now();
        let at = |ms| t + Duration::from_millis(ms);
        let update = [Update { rect: Rect::new(0, 0, 100, 100), mode: RefreshMode::Full }];
        let mut watch = FrameWatch::new(true);
        watch.presented(&update, at(0));
        assert_eq!(watch.take_guard_due(at(1000)), None, "launch");
        watch.sleep();
        watch.wake(false);
        watch.presented(&update, at(10_000));
        assert_eq!(watch.take_guard_due(at(11_000)), None, "a wake from Verso's own cover");
        watch.sleep();
        watch.wake(true);
        assert_eq!(watch.take_guard_due(at(20_000)), None, "the wake frame isn't out yet");
        watch.presented(&update, at(20_000));
        assert_eq!(watch.deadline(), Some(at(20_100)));
        assert_eq!(watch.take_guard_due(at(20_050)), None);
        assert!(watch.take_guard_due(at(20_100)).is_some());
        // The repaint it asks for doesn't start the window over.
        watch.presented(&update, at(20_120));
        let looks = (20_101..40_000).filter(|&ms| watch.take_guard_due(at(ms)).is_some()).count();
        assert_eq!(looks, 49, "every 0.1 s until 5 s after the frame");
        watch.sleep();
        watch.wake(true);
        watch.presented(&update, at(50_000));
        watch.sleep();
        assert_eq!((watch.take_guard_due(at(50_100)), watch.deadline()), (None, None), "asleep again");
    }

    #[test]
    fn samples_a_native_wake_frame_for_the_log_and_logs_what_changes() {
        let t = Instant::now();
        let at = |ms| t + Duration::from_millis(ms);
        let update = [Update { rect: Rect::new(0, 0, 100, 100), mode: RefreshMode::Full }];
        let intact = || Some("intact".to_owned());
        let mut watch = FrameWatch::new(true);
        watch.presented(&update, at(0));
        assert_eq!(watch.take_sample(at(1000), || panic!("not after a launch")), None);
        watch.sleep();
        watch.wake(true);
        watch.late_power(at(9_000));
        watch.presented(&update, at(10_000));
        let sample = |watch: &mut FrameWatch, ms, check: &str| watch.take_sample(at(ms), || Some(check.to_owned()));
        assert_eq!(sample(&mut watch, 10_500, "intact"), None, "not due");
        assert_eq!(
            sample(&mut watch, 11_000, "intact").as_deref(),
            Some("frame sample 1.0 s after the wake frame: intact")
        );
        assert_eq!(sample(&mut watch, 12_000, "intact"), None, "the same");
        assert_eq!(sample(&mut watch, 13_000, "bad").as_deref(), Some("frame sample 3.0 s after the wake frame: bad"));
        // wakeupFromSuspend 20 s in: until 30 s after it.
        watch.late_power(at(30_000));
        let looks = (13_001..70_000).filter(|&ms| watch.take_sample(at(ms), intact).is_some()).count();
        assert_eq!(looks, 2, "back to intact, and the last at 50 s");
        // A late event after the samples ended starts them again.
        watch.late_power(at(80_000));
        assert_eq!(
            sample(&mut watch, 81_000, "intact").as_deref(),
            Some("frame sample 71.0 s after the wake frame: intact")
        );
        watch.sleep();
        assert_eq!(watch.take_sample(at(82_000), intact), None, "asleep");
        // A wake from Verso's own cover isn't sampled.
        watch.wake(false);
        watch.presented(&update, at(90_000));
        watch.late_power(at(90_500));
        assert_eq!(watch.take_sample(at(92_000), || panic!("not native")), None);
    }
}
