//! Dates and moments in time: reading them as people type or paste them, and
//! how they are stored.
//!
//! Dates are stored as `2026-09-18`. Moments ("date & time") are stored in UTC
//! as RFC 3339 (`2026-09-18T10:46:16.130244Z`) and shown in local time.

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone, Utc};

use crate::{Error, Result};

const DATE_FORMATS: &[&str] = &["%Y-%m-%d", "%d.%m.%Y", "%d/%m/%Y", "%Y/%m/%d", "%d-%m-%Y", "%d %b %Y", "%d %B %Y", "%b %d %Y", "%B %d %Y"];
const DATETIME_FORMATS: &[&str] = &[
    "%Y-%m-%d %H:%M:%S%.f",
    "%Y-%m-%dT%H:%M:%S%.f",
    "%Y-%m-%d %H:%M",
    "%Y-%m-%dT%H:%M",
    "%d.%m.%Y %H:%M:%S",
    "%d.%m.%Y %H:%M",
    "%d/%m/%Y %H:%M:%S",
    "%d/%m/%Y %H:%M",
];

/// Seconds since 1970 that fall between 2000 and 2100: a Unix timestamp, most likely.
pub const EPOCH_MIN: f64 = 946_684_800.0;
pub const EPOCH_MAX: f64 = 4_102_444_800.0;

/// A Unix timestamp in seconds, milliseconds or microseconds (picked by size).
pub fn from_epoch(n: f64) -> Option<DateTime<Utc>> {
    let secs = if n.abs() < 1e11 {
        n
    } else if n.abs() < 1e14 {
        n / 1e3
    } else {
        n / 1e6
    };
    if !(EPOCH_MIN..EPOCH_MAX).contains(&secs) {
        return None;
    }
    // To the microsecond: finer digits in a float are noise.
    let micros = (secs * 1e6).round() as i64;
    Utc.timestamp_micros(micros).single()
}

/// `2026-09-18`, `18.09.2026`, `18/09/2026` (day first), `18 Sep 2026`; a date with
/// a time keeps just the date.
pub fn parse_date(s: &str) -> Result<NaiveDate> {
    let t = s.trim();
    for f in DATE_FORMATS {
        if let Ok(d) = NaiveDate::parse_from_str(t, f) {
            return Ok(d);
        }
    }
    parse_moment(t).map(|m| m.with_timezone(&Local).date_naive()).map_err(|_| Error::InvalidValue(format!("{t:?} is not a date")))
}

/// RFC 3339 (`2026-09-18T10:46:16+00:00`), a local date and time (`2026-09-18 13:46`),
/// a date (midnight, local), or a Unix timestamp (`1789728376.13`).
pub fn parse_moment(s: &str) -> Result<DateTime<Utc>> {
    let t = s.trim();
    let bad = || Error::InvalidValue(format!("{t:?} is not a date and time"));
    if let Ok(d) = DateTime::parse_from_rfc3339(t) {
        return Ok(d.with_timezone(&Utc));
    }
    // RFC 3339 with a space instead of the T, or a zone without the colon.
    for f in ["%Y-%m-%d %H:%M:%S%.f%:z", "%Y-%m-%d %H:%M:%S%.f%z", "%Y-%m-%dT%H:%M:%S%.f%z"] {
        if let Ok(d) = DateTime::parse_from_str(t, f) {
            return Ok(d.with_timezone(&Utc));
        }
    }
    if let Some(stripped) = t.strip_suffix('Z').or_else(|| t.strip_suffix(" UTC")) {
        for f in DATETIME_FORMATS {
            if let Ok(n) = NaiveDateTime::parse_from_str(stripped, f) {
                return Ok(Utc.from_utc_datetime(&n));
            }
        }
    }
    for f in DATETIME_FORMATS {
        if let Ok(n) = NaiveDateTime::parse_from_str(t, f) {
            return Local.from_local_datetime(&n).earliest().map(|d| d.with_timezone(&Utc)).ok_or_else(bad);
        }
    }
    for f in DATE_FORMATS {
        if let Ok(d) = NaiveDate::parse_from_str(t, f) {
            let n = d.and_hms_opt(0, 0, 0).ok_or_else(bad)?;
            return Local.from_local_datetime(&n).earliest().map(|d| d.with_timezone(&Utc)).ok_or_else(bad);
        }
    }
    if let Ok(n) = t.parse::<f64>() {
        return from_epoch(n).ok_or_else(|| Error::InvalidValue(format!("{t} is not a timestamp between 2000 and 2100")));
    }
    Err(bad())
}

/// How a moment is stored: UTC, RFC 3339, with as many decimals as it needs.
pub fn store_moment(m: &DateTime<Utc>) -> String {
    m.to_rfc3339_opts(SecondsFormat::AutoSi, true)
}

pub fn store_date(d: &NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

/// A moment as people read it: local time, to the second.
pub fn show_moment(m: &DateTime<Utc>) -> String {
    m.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        let d = NaiveDate::from_ymd_opt(2026, 9, 18).unwrap();
        for s in ["2026-09-18", "18.09.2026", "18/09/2026", "18 Sep 2026", "2026-09-18T10:46:16Z"] {
            assert_eq!(parse_date(s).unwrap(), d, "{s}");
        }
        assert!(parse_date("hello").is_err());
    }

    #[test]
    fn moments() {
        let a = parse_moment("2026-09-18T10:46:16.130244+00:00").unwrap();
        assert_eq!(store_moment(&a), "2026-09-18T10:46:16.130244Z");
        let b = parse_moment("1789728376.130244").unwrap();
        assert_eq!(b.timestamp(), a.timestamp());
        assert!((b.timestamp_subsec_micros() as i64 - 130244).abs() <= 1);
        assert_eq!(parse_moment("1789728376130").unwrap().timestamp(), a.timestamp());
        assert_eq!(parse_moment("2026-09-18 10:46:16Z").unwrap().timestamp(), a.timestamp());
        assert!(parse_moment("42").is_err());
        assert!(parse_moment("2026-09-18 13:46").is_ok());
    }
}
