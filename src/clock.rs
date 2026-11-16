//! The local calendar date, for the bar's done window.
//!
//! Days back are counted on the calendar, not as multiples of 24 hours, so a
//! daylight-saving shift near midnight cannot land the marker a day off.

use std::ffi::{c_char, c_int, c_long};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Date {
    pub year: i64,
    pub month: u32,
    pub day: u32,
}

const WEEKDAYS: [&str; 7] = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];

impl Date {
    pub fn today() -> Date {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        local_date(secs).unwrap_or_else(|| Date::from_days(secs.div_euclid(86_400)))
    }

    pub fn minus_days(self, n: u32) -> Date {
        Date::from_days(self.days() - i64::from(n))
    }

    /// `DD-MM`, as `date +%d-%m` prints it.
    pub fn dd_mm(self) -> String {
        format!("{:02}-{:02}", self.day, self.month)
    }

    /// The done window's near edge: a two-letter weekday and `DD-MM`.
    pub fn marker(self) -> String {
        // 1970-01-01 was a Thursday.
        let weekday = (self.days() + 3).rem_euclid(7) as usize;
        format!("{} {}", WEEKDAYS[weekday], self.dd_mm())
    }

    /// Days since 1970-01-01 (Howard Hinnant's days_from_civil).
    fn days(self) -> i64 {
        let y = if self.month <= 2 {
            self.year - 1
        } else {
            self.year
        };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let m = i64::from(self.month);
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(self.day) - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    fn from_days(z: i64) -> Date {
        let z = z + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        let year = yoe + era * 400 + i64::from(month <= 2);
        Date { year, month, day }
    }
}

// glibc's and musl's `struct tm`; only the date fields are read.
#[allow(dead_code)]
#[repr(C)]
struct Tm {
    tm_sec: c_int,
    tm_min: c_int,
    tm_hour: c_int,
    tm_mday: c_int,
    tm_mon: c_int,
    tm_year: c_int,
    tm_wday: c_int,
    tm_yday: c_int,
    tm_isdst: c_int,
    tm_gmtoff: c_long,
    tm_zone: *const c_char,
}

unsafe extern "C" {
    fn localtime_r(timep: *const c_long, result: *mut Tm) -> *mut Tm;
}

fn local_date(secs: i64) -> Option<Date> {
    let t = c_long::try_from(secs).ok()?;
    let mut tm = Tm {
        tm_sec: 0,
        tm_min: 0,
        tm_hour: 0,
        tm_mday: 0,
        tm_mon: 0,
        tm_year: 0,
        tm_wday: 0,
        tm_yday: 0,
        tm_isdst: 0,
        tm_gmtoff: 0,
        tm_zone: std::ptr::null(),
    };
    // SAFETY: both pointers are valid for the call, and localtime_r writes only
    // into the struct it is given.
    let ok = unsafe { !localtime_r(&t, &mut tm).is_null() };
    ok.then(|| Date {
        year: i64::from(tm.tm_year) + 1900,
        month: (tm.tm_mon + 1) as u32,
        day: tm.tm_mday as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i64, month: u32, day: u32) -> Date {
        Date { year, month, day }
    }

    #[test]
    fn days_round_trip_through_the_calendar() {
        for z in [-800_000, -1, 0, 1, 59, 60, 11_016, 20_408, 100_000] {
            assert_eq!(Date::from_days(z).days(), z);
        }
        assert_eq!(Date::from_days(0), date(1970, 1, 1));
        assert_eq!(date(2000, 3, 1).days() - date(2000, 2, 28).days(), 2);
    }

    #[test]
    fn days_back_cross_months_and_years() {
        assert_eq!(date(2026, 11, 16).minus_days(7), date(2026, 11, 9));
        assert_eq!(date(2026, 3, 2).minus_days(2), date(2026, 2, 28));
        assert_eq!(date(2024, 3, 1).minus_days(1), date(2024, 2, 29));
        assert_eq!(date(2027, 1, 3).minus_days(7), date(2026, 12, 27));
    }

    #[test]
    fn the_marker_is_a_two_letter_weekday_and_day_month() {
        assert_eq!(date(2026, 11, 16).marker(), "Mo 16-11");
        assert_eq!(date(2026, 11, 9).marker(), "Mo 09-11");
        assert_eq!(date(2026, 11, 15).marker(), "Su 15-11");
        assert_eq!(date(1970, 1, 1).marker(), "Th 01-01");
        assert_eq!(date(2026, 1, 2).dd_mm(), "02-01");
    }

    #[test]
    fn today_is_a_real_date() {
        let today = Date::today();
        assert!((1..=12).contains(&today.month) && (1..=31).contains(&today.day));
        assert!(today.year >= 2024);
    }
}
