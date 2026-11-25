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
    pub second: u8,
}
impl LocalDateTime {
    pub fn now() -> Option<Self> {
        let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
        Self::at(unix)
    }
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
                second: tm.tm_sec.clamp(0, 59) as u8,
            })
        }
    }
    /// `2026-11-15 03:08:00`, matching KOReader's local timestamp format.
    pub fn timestamp(self) -> String {
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.time.hour, self.time.minute, self.second
        )
    }
    fn days(self) -> i64 {
        days_from_civil(self.year, self.month, self.day)
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
    /// The calendar date: `23 Nov 2026`.
    pub fn date(self) -> String {
        format!("{} {} {}", self.day, MONTHS[usize::from(self.month - 1)], self.year)
    }
    /// The start of a KOReader date, `2026-11-23`, in the local time zone:
    /// Unix seconds at local midnight. None for anything else, including
    /// dates that don't exist.
    pub fn midnight(date: &str) -> Option<u64> {
        let mut parts = date.split('-');
        let year: i32 = parts.next()?.parse().ok()?;
        let month: i32 = parts.next()?.parse().ok()?;
        let day: i32 = parts.next()?.parse().ok()?;
        if parts.next().is_some() || !(1970..=9999).contains(&year) {
            return None;
        }
        // SAFETY: mktime only reads and normalizes the tm we pass.
        unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            tm.tm_year = year - 1900;
            tm.tm_mon = month - 1;
            tm.tm_mday = day;
            tm.tm_isdst = -1;
            let t = libc::mktime(&mut tm);
            // mktime rolls 31 Feb over to March: not a real date.
            let same = tm.tm_year == year - 1900 && tm.tm_mon == month - 1 && tm.tm_mday == day;
            (same && t >= 0).then_some(t as u64)
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

/// Days since 1970-01-01 for a civil calendar date (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(year: i32, month: u8, day: u8) -> i64 {
    let (m, d) = (i64::from(month), i64::from(day));
    let y = i64::from(year) - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
            if leap { 29 } else { 28 }
        }
        _ => 0,
    }
}

fn digits(s: &str) -> Option<u32> {
    s.bytes().all(|b| b.is_ascii_digit()).then(|| s.parse().ok()).flatten()
}

/// Parses `YYYY-MM-DD HH:MM:SS UTC` to Unix seconds. Returns `None` for any
/// other shape or an impossible calendar date.
pub fn parse_utc(text: &str) -> Option<u64> {
    let s = text.strip_suffix(" UTC")?;
    let b = s.as_bytes();
    if b.len() != 19 || b[4] != b'-' || b[7] != b'-' || b[10] != b' ' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let year = digits(&s[0..4])? as i32;
    let month = digits(&s[5..7])? as u8;
    let day = digits(&s[8..10])? as u8;
    let hour = digits(&s[11..13])? as u8;
    let minute = digits(&s[14..16])? as u8;
    let second = digits(&s[17..19])? as u8;
    if !(1970..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || !(1..=days_in_month(year, month)).contains(&day)
        || hour >= 24
        || minute >= 60
        || second >= 60
    {
        return None;
    }
    let days = days_from_civil(year, month, day);
    u64::try_from(days * 86_400 + i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_when_relative_to_now() {
        let at = |year, month, day, hour, minute| LocalDateTime {
            year,
            month,
            day,
            time: LocalTime { hour, minute },
            second: 0,
        };
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
        // KOReader's dates, as local midnight.
        let midnight = LocalDateTime::midnight("2026-11-23").unwrap();
        assert_eq!(LocalDateTime::at(midnight), Some(at(2026, 11, 23, 0, 0)));
        assert_eq!(LocalDateTime::at(midnight - 1).map(|t| t.day), Some(22));
        for bad in ["", "2026-11", "2026-02-31", "2026-13-01", "2026-11-23-1", "23 Nov 2026", "1900-01-01"] {
            assert_eq!(LocalDateTime::midnight(bad), None, "{bad}");
        }
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
