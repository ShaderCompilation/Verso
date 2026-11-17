//! Local wall-clock time for the reader's clock items.
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

#[cfg(test)]
mod tests {
    use super::*;

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
