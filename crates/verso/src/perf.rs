//! The performance line logged at a clean exit, so a problem report from a slower Kindle carries its numbers:
//!
//! `perf: first_frame_ms=812 page_turns=48 turn_median_ms=22.1 turn_p95_ms=35.0 peak_rss_kib=45123`
//!
//! A page turn is timed from the finger lifting to the refresh request
//! returning, including a page the worker had to render first. Fields
//! without a measurement read `-`.

use std::time::Duration;

/// Page turns kept; a session longer than this keeps its first ones.
const MAX_TURNS: usize = 20_000;

#[derive(Default)]
pub struct SessionPerf {
    first_frame: Option<Duration>,
    /// Microseconds.
    turns: Vec<u32>,
}

impl SessionPerf {
    pub fn first_frame(&mut self, elapsed: Duration) {
        self.first_frame.get_or_insert(elapsed);
    }

    pub fn page_turn(&mut self, elapsed: Duration) {
        if self.turns.len() < MAX_TURNS {
            self.turns.push(elapsed.as_micros().min(u32::MAX as u128) as u32);
        }
    }

    /// The one-line summary; `peak_rss_kib` is `VmHWM`.
    pub fn summary(&self, peak_rss_kib: Option<u64>) -> String {
        let mut sorted = self.turns.clone();
        sorted.sort_unstable();
        let ms = |us: Option<f64>| us.map_or_else(|| "-".to_owned(), |us| format!("{:.1}", us / 1000.0));
        format!(
            "perf: first_frame_ms={} page_turns={} turn_median_ms={} turn_p95_ms={} peak_rss_kib={}",
            self.first_frame.map_or_else(|| "-".to_owned(), |d| d.as_millis().to_string()),
            sorted.len(),
            ms(median(&sorted)),
            ms(nearest_rank(&sorted, 0.95).map(f64::from)),
            peak_rss_kib.map_or_else(|| "-".to_owned(), |k| k.to_string()),
        )
    }
}

fn median(sorted: &[u32]) -> Option<f64> {
    let n = sorted.len();
    match n {
        0 => None,
        _ if n % 2 == 1 => Some(sorted[n / 2].into()),
        _ => Some((f64::from(sorted[n / 2 - 1]) + f64::from(sorted[n / 2])) / 2.0),
    }
}

/// The nearest-rank percentile: the smallest value at least `p` of the
/// values are at or below.
fn nearest_rank(sorted: &[u32], p: f64) -> Option<u32> {
    let rank = (p * sorted.len() as f64).ceil() as usize;
    sorted.get(rank.max(1) - 1).copied()
}

/// The process's peak resident memory (`VmHWM` in /proc/self/status), KiB.
pub fn peak_rss_kib() -> Option<u64> {
    parse_vm_hwm(&std::fs::read_to_string("/proc/self/status").ok()?)
}

fn parse_vm_hwm(status: &str) -> Option<u64> {
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    line["VmHWM:".len()..].trim().trim_end_matches("kB").trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_has_first_frame_turn_median_p95_and_peak_rss() {
        let mut perf = SessionPerf::default();
        perf.first_frame(Duration::from_millis(812));
        perf.first_frame(Duration::from_millis(5000)); // only the first counts
        for ms in [30, 10, 20, 40, 100, 25, 15, 22, 18, 21] {
            perf.page_turn(Duration::from_millis(ms));
        }
        assert_eq!(
            perf.summary(Some(45123)),
            "perf: first_frame_ms=812 page_turns=10 turn_median_ms=21.5 turn_p95_ms=100.0 peak_rss_kib=45123"
        );
    }

    #[test]
    fn missing_measurements_read_as_dashes() {
        assert_eq!(
            SessionPerf::default().summary(None),
            "perf: first_frame_ms=- page_turns=0 turn_median_ms=- turn_p95_ms=- peak_rss_kib=-"
        );
        let mut one = SessionPerf::default();
        one.page_turn(Duration::from_micros(22_070));
        assert!(one.summary(None).contains("page_turns=1 turn_median_ms=22.1 turn_p95_ms=22.1"));
    }

    #[test]
    fn percentiles_use_nearest_rank() {
        let v: Vec<u32> = (1..=100).collect();
        assert_eq!(nearest_rank(&v, 0.95), Some(95));
        assert_eq!(nearest_rank(&v[..20], 0.95), Some(19));
        assert_eq!(nearest_rank(&[], 0.95), None);
        assert_eq!(median(&v), Some(50.5));
    }

    #[test]
    fn reads_vm_hwm() {
        let status = "Name:\tverso\nVmPeak:\t  90000 kB\nVmHWM:\t   45123 kB\nVmRSS:\t   40000 kB\n";
        assert_eq!(parse_vm_hwm(status), Some(45123));
        assert_eq!(parse_vm_hwm("Name:\tverso\n"), None);
        assert!(peak_rss_kib().is_some_and(|k| k > 0), "this process has a VmHWM on Linux");
    }
}
