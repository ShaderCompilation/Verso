//! Reading speed for the time-left items, in characters of text per second,
//! so it holds across font, size and margin changes.
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// A page read faster than this was skimmed or turned by accident.
pub const MIN_SAMPLE: Duration = Duration::from_secs(2);
/// A page read slower than this means the reader stepped away.
pub const MAX_SAMPLE: Duration = Duration::from_secs(5 * 60);
/// Samples before an average is trusted. A seed counts as this many.
pub const ESTABLISHED: u32 = 5;
/// Weight of a new sample once the average has ten or more.
const ALPHA: f32 = 0.1;

/// An average reading speed and how many samples it holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ReadingSpeed {
    pub chars_per_sec: f32,
    pub samples: u32,
}
impl ReadingSpeed {
    /// Where a book starts: its own average, else the average carried over
    /// from previous books (counted as `ESTABLISHED` samples), else nothing.
    pub fn start(book: Option<ReadingSpeed>, global: Option<ReadingSpeed>) -> Self {
        match (book.filter(|s| s.valid()), global.and_then(|g| g.value())) {
            (Some(book), _) => book,
            (None, Some(seed)) => Self { chars_per_sec: seed, samples: ESTABLISHED },
            (None, None) => Self::default(),
        }
    }
    /// Adds a sample: a plain mean for the first ten, then an exponential
    /// moving average.
    pub fn add(&mut self, chars_per_sec: f32) {
        self.samples = self.samples.saturating_add(1);
        let weight = (1.0 / self.samples as f32).max(ALPHA);
        self.chars_per_sec += weight * (chars_per_sec - self.chars_per_sec);
    }
    /// The speed, once there are enough samples.
    pub fn value(&self) -> Option<f32> {
        (self.samples >= ESTABLISHED && self.valid()).then_some(self.chars_per_sec)
    }
    fn valid(&self) -> bool {
        self.chars_per_sec.is_finite() && self.chars_per_sec > 0.0
    }
}

/// How a page came on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Turn {
    /// One page forward, by tap or swipe.
    Forward,
    Back,
    /// Anything else: opening, contents, slider, search, re-layout.
    Jump,
}

/// Times pages between forward turns.
#[derive(Debug, Default)]
pub struct Sampler {
    /// When the page being read appeared, and its text offset.
    start: Option<(Instant, u64)>,
}
impl Sampler {
    /// A page appeared at `now`. Returns the speed of the page just left
    /// when that is a usable sample.
    pub fn shown(&mut self, now: Instant, offset: Option<u64>, turn: Turn) -> Option<f32> {
        let previous = self.start.take();
        let sample = match (turn, previous, offset) {
            (Turn::Forward, Some((at, from)), Some(to)) if to > from => {
                let took = now.saturating_duration_since(at);
                ((MIN_SAMPLE..=MAX_SAMPLE).contains(&took)).then(|| (to - from) as f32 / took.as_secs_f32())
            }
            _ => None,
        };
        // Going back re-reads text already read, so the next forward turn
        // starts timing again rather than measuring it.
        if turn != Turn::Back {
            self.start = offset.map(|o| (now, o));
        }
        sample
    }
    /// Menus, selection, lookups and sleep: the page shown now isn't timed.
    pub fn reset(&mut self) {
        self.start = None;
    }
}

/// `< 1 min`, `12 min`, `1 h 5 min`, `2 h`.
pub fn format_duration(seconds: f32) -> String {
    let minutes = (seconds / 60.0).round() as u64;
    match (minutes / 60, minutes % 60) {
        (0, 0) => "< 1 min".into(),
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn forward_turns_within_limits_are_samples() {
        let t = Instant::now();
        let mut s = Sampler::default();
        assert_eq!(s.shown(t, Some(1000), Turn::Jump), None, "opening starts the timer");
        assert_eq!(s.shown(t + secs(30), Some(1600), Turn::Forward), Some(20.0));
        // Too short: skimmed. Too long: stepped away. Neither is a sample,
        // but each starts timing the new page.
        assert_eq!(s.shown(t + secs(31), Some(2200), Turn::Forward), None);
        assert_eq!(s.shown(t + secs(31) + secs(301), Some(2800), Turn::Forward), None);
        assert_eq!(s.shown(t + secs(31) + secs(301) + secs(60), Some(4000), Turn::Forward), Some(20.0));
        // Exactly at the limits still counts.
        let mut s = Sampler::default();
        s.shown(t, Some(0), Turn::Jump);
        assert_eq!(s.shown(t + MIN_SAMPLE, Some(100), Turn::Forward), Some(50.0));
        assert_eq!(s.shown(t + MIN_SAMPLE + MAX_SAMPLE, Some(400), Turn::Forward), Some(1.0));
    }

    #[test]
    fn jumps_back_turns_and_interruptions_produce_no_sample() {
        let t = Instant::now();
        let mut s = Sampler::default();
        s.shown(t, Some(1000), Turn::Jump);
        // A jump restarts the timer at its destination instead of sampling.
        assert_eq!(s.shown(t + secs(30), Some(50_000), Turn::Jump), None);
        assert_eq!(s.shown(t + secs(60), Some(50_600), Turn::Forward), Some(20.0));
        // Back, then forward again over text already read: no sample.
        assert_eq!(s.shown(t + secs(70), Some(50_000), Turn::Back), None);
        assert_eq!(s.shown(t + secs(100), Some(50_600), Turn::Forward), None);
        assert_eq!(s.shown(t + secs(130), Some(51_200), Turn::Forward), Some(20.0));
        // A menu (or selection, lookup, sleep) during a page drops that page.
        s.reset();
        assert_eq!(s.shown(t + secs(160), Some(51_800), Turn::Forward), None);
        assert_eq!(s.shown(t + secs(190), Some(52_400), Turn::Forward), Some(20.0));
        // Unknown offsets and text that didn't advance aren't samples.
        assert_eq!(s.shown(t + secs(220), None, Turn::Forward), None);
        assert_eq!(s.shown(t + secs(250), Some(53_000), Turn::Forward), None);
        assert_eq!(s.shown(t + secs(280), Some(53_000), Turn::Forward), None, "an image-only page");
    }

    #[test]
    fn the_average_starts_as_a_mean_and_needs_five_samples() {
        let mut speed = ReadingSpeed::default();
        for sample in [10.0, 20.0, 30.0, 40.0] {
            speed.add(sample);
            assert_eq!(speed.value(), None);
        }
        speed.add(50.0);
        assert_eq!(speed.value(), Some(30.0), "a plain mean of the first samples");
        for _ in 0..5 {
            speed.add(30.0);
        }
        assert_eq!(speed.samples, 10);
        // From then on a new sample moves it by a tenth of the difference.
        speed.add(130.0);
        assert!((speed.value().unwrap() - 40.0).abs() < 1e-3);
    }

    #[test]
    fn a_new_book_starts_from_the_global_average() {
        let global = ReadingSpeed { chars_per_sec: 25.0, samples: 40 };
        let started = ReadingSpeed::start(None, Some(global));
        assert_eq!(started, ReadingSpeed { chars_per_sec: 25.0, samples: ESTABLISHED });
        assert_eq!(started.value(), Some(25.0), "usable straight away");
        // The book's own samples soon outweigh the seed.
        let mut speed = started;
        speed.add(13.0);
        assert!((speed.value().unwrap() - 23.0).abs() < 1e-3);
        // A book's own history wins; nothing at all shows nothing.
        let own = ReadingSpeed { chars_per_sec: 18.0, samples: 3 };
        assert_eq!(ReadingSpeed::start(Some(own), Some(global)), own);
        assert_eq!(ReadingSpeed::start(None, None).value(), None);
        // A global average still being collected isn't a seed.
        let young = ReadingSpeed { chars_per_sec: 25.0, samples: 2 };
        assert_eq!(ReadingSpeed::start(None, Some(young)), ReadingSpeed::default());
        let broken = ReadingSpeed { chars_per_sec: f32::NAN, samples: 9 };
        assert_eq!(ReadingSpeed::start(Some(broken), None), ReadingSpeed::default());
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(format_duration(0.0), "< 1 min");
        assert_eq!(format_duration(29.0), "< 1 min");
        assert_eq!(format_duration(31.0), "1 min");
        assert_eq!(format_duration(12.0 * 60.0), "12 min");
        assert_eq!(format_duration(59.0 * 60.0 + 40.0), "1 h");
        assert_eq!(format_duration(65.0 * 60.0), "1 h 5 min");
        assert_eq!(format_duration(320.0 * 60.0), "5 h 20 min");
    }
}
