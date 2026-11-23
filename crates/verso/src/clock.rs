//! Local wall-clock time for the reader's clock items and Settings.
//!
//! The C library reads the time zone from `TZ` or `/etc/localtime`. On the
//! Kindle `/etc/localtime` links to `/var/local/system/tz`, which the Kindle's
//! time zone setting writes (measured). The zone is read once, so a change
//! made while Verso runs shows after the next start.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTime {
    pub hour: u8,
    pub minute: u8,
}
impl LocalTime {
    pub fn now() -> Option<Self> {
        // SAFETY: localtime_r only writes the tm we pass; null means failure.
        unsafe {
            let now = libc::time(std::ptr::null_mut());
            let mut tm: libc::tm = std::mem::zeroed();
            if libc::localtime_r(&now, &mut tm).is_null() {
                return None;
            }
            Some(Self { hour: tm.tm_hour.clamp(0, 23) as u8, minute: tm.tm_min.clamp(0, 59) as u8 })
        }
    }
    /// `14:41`, `09:05`.
    pub fn h24(self) -> String {
        format!("{:02}:{:02}", self.hour, self.minute)
    }
    /// `2:41 PM`, `12:05 AM`.
    pub fn h12(self) -> String {
        let hour = match self.hour % 12 {
            0 => 12,
            h => h,
        };
        format!("{hour}:{:02} {}", self.minute, if self.hour < 12 { "AM" } else { "PM" })
    }
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// A moment in local time, for saying when something happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalDateTime {
    pub year: i32,
    /// 1–12.
    pub month: u8,
    pub day: u8,
    pub time: LocalTime,
}
impl LocalDateTime {
    /// `unix` seconds in the local time zone.
    pub fn at(unix: u64) -> Option<Self> {
        let t = libc::time_t::try_from(unix).ok()?;
        // SAFETY: as in LocalTime::now.
        unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            if libc::localtime_r(&t, &mut tm).is_null() {
                return None;
            }
            Some(Self {
                year: tm.tm_year + 1900,
                month: (tm.tm_mon + 1).clamp(1, 12) as u8,
                day: tm.tm_mday.clamp(1, 31) as u8,
                time: LocalTime { hour: tm.tm_hour.clamp(0, 23) as u8, minute: tm.tm_min.clamp(0, 59) as u8 },
            })
        }
    }
    /// Days since 1970-01-01 for the calendar date (Howard Hinnant's
    /// days_from_civil), so "yesterday" works across months and years.
    fn days(self) -> i64 {
        let (m, d) = (i64::from(self.month), i64::from(self.day));
        let y = i64::from(self.year) - i64::from(m <= 2);
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
        era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
    }
    /// When `self` was, seen from `now`: `today at 14:02`, `yesterday at
    /// 9:05`, `on 3 Nov` or `on 3 Nov 2025`.
    pub fn since(self, now: Self) -> String {
        let time = self.time.h24();
        match now.days() - self.days() {
            0 => format!("today at {time}"),
            1 => format!("yesterday at {time}"),
            _ if self.year == now.year => format!("on {} {}", self.day, MONTHS[usize::from(self.month - 1)]),
            _ => format!("on {} {} {}", self.day, MONTHS[usize::from(self.month - 1)], self.year),
        }
    }
    /// How long ago `self` was, in days, seen from `now`: `today`,
    /// `yesterday`, `3 days ago`, and after a week the date: `3 Nov` or
    /// `3 Nov 2025`.
    pub fn ago(self, now: Self) -> String {
        match now.days() - self.days() {
            0 => "today".into(),
            1 => "yesterday".into(),
            days @ 2..=7 => format!("{days} days ago"),
            _ if self.year == now.year => format!("{} {}", self.day, MONTHS[usize::from(self.month - 1)]),
            _ => format!("{} {} {}", self.day, MONTHS[usize::from(self.month - 1)], self.year),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_when_relative_to_now() {
        let at = |year, month, day, hour, minute| LocalDateTime { year, month, day, time: LocalTime { hour, minute } };
        let now = at(2026, 3, 1, 10, 0);
        assert_eq!(at(2026, 3, 1, 9, 5).since(now), "today at 09:05");
        assert_eq!(at(2026, 2, 28, 23, 59).since(now), "yesterday at 23:59");
        assert_eq!(at(2026, 2, 27, 8, 0).since(now), "on 27 Feb");
        assert_eq!(at(2025, 12, 31, 8, 0).since(at(2026, 1, 1, 0, 1)), "yesterday at 08:00");
        assert_eq!(at(2025, 11, 3, 8, 0).since(now), "on 3 Nov 2025");
        // A time ahead of the clock (it was set back) shows its date.
        assert_eq!(at(2026, 3, 4, 8, 0).since(now), "on 4 Mar");
        assert_eq!(at(1970, 1, 1, 0, 0).days(), 0);
        assert_eq!(at(2000, 3, 1, 0, 0).days(), 11_017);
        // In days, for KOReader's last save.
        assert_eq!(at(2026, 3, 1, 0, 1).ago(now), "today");
        assert_eq!(at(2026, 2, 28, 23, 59).ago(now), "yesterday");
        assert_eq!(at(2026, 2, 26, 8, 0).ago(now), "3 days ago");
        assert_eq!(at(2026, 2, 22, 8, 0).ago(now), "7 days ago");
        assert_eq!(at(2026, 2, 21, 8, 0).ago(now), "21 Feb");
        assert_eq!(at(2025, 11, 3, 8, 0).ago(now), "3 Nov 2025");
        assert_eq!(at(2026, 3, 4, 8, 0).ago(now), "4 Mar", "ahead of the clock");
        let local = LocalDateTime::at(1_800_000_000).unwrap();
        assert!(local.year == 2027 && local.month == 1, "{local:?}");
    }

    #[test]
    fn formats_both_clocks() {
        let at = |hour, minute| LocalTime { hour, minute };
        assert_eq!(at(14, 41).h24(), "14:41");
        assert_eq!(at(9, 5).h24(), "09:05");
        assert_eq!(at(0, 0).h24(), "00:00");
        assert_eq!(at(14, 41).h12(), "2:41 PM");
        assert_eq!(at(0, 5).h12(), "12:05 AM");
        assert_eq!(at(12, 0).h12(), "12:00 PM");
        assert_eq!(at(11, 59).h12(), "11:59 AM");
        assert_eq!(at(23, 9).h12(), "11:09 PM");
    }

    #[test]
    fn the_local_time_is_available() {
        assert!(LocalTime::now().is_some());
    }
}
