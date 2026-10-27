//! The clock the server reads time from, so that tests can move it.
//!
//! Code that needs the time takes an `Arc<dyn Clock>` or a plain `now` argument in unix seconds. Only [`SystemClock`] reads
//! the system time. [`ManualClock`] is public and unconditional because the integration tests compile the library without
//! `cfg(test)`.

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// A source of the current time.
pub trait Clock: Send + Sync {
    /// The current time.
    fn now(&self) -> SystemTime;

    /// The current time as whole seconds since the unix epoch (negative before it).
    fn unix(&self) -> i64 {
        unix_seconds(self.now())
    }
}

/// The clock of the machine the server runs on.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// A clock that only moves when a test says so.
#[derive(Debug)]
pub struct ManualClock {
    seconds: AtomicI64,
}

impl ManualClock {
    /// A clock that shows `unix` seconds since the epoch.
    pub fn new(unix: i64) -> ManualClock {
        ManualClock { seconds: AtomicI64::new(unix) }
    }

    /// Sets the time.
    pub fn set(&self, unix: i64) {
        self.seconds.store(unix, Ordering::SeqCst);
    }

    /// Moves the time forward by `seconds` (backward when negative).
    pub fn advance(&self, seconds: i64) {
        self.seconds.fetch_add(seconds, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> SystemTime {
        from_unix(self.seconds.load(Ordering::SeqCst))
    }
}

/// The system time for a count of seconds since the epoch.
pub fn from_unix(seconds: i64) -> SystemTime {
    if seconds >= 0 {
        UNIX_EPOCH + Duration::from_secs(seconds.unsigned_abs())
    } else {
        UNIX_EPOCH - Duration::from_secs(seconds.unsigned_abs())
    }
}

/// Whole seconds since the epoch of a system time, rounded toward the past.
pub fn unix_seconds(at: SystemTime) -> i64 {
    match at.duration_since(UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_secs()).unwrap_or(i64::MAX),
        Err(before) => {
            let d = before.duration();
            let whole = i64::try_from(d.as_secs()).unwrap_or(i64::MAX);
            if d.subsec_nanos() == 0 { -whole } else { whole.saturating_neg().saturating_sub(1) }
        }
    }
}

/// The time as an RFC 3339 string in UTC with whole seconds, for example `2026-03-01T08:30:00Z`. A time the format cannot
/// show (beyond the year 9999) is shown as the epoch.
pub fn rfc3339(at: SystemTime) -> String {
    let moment = OffsetDateTime::from_unix_timestamp(unix_seconds(at)).unwrap_or(OffsetDateTime::UNIX_EPOCH);
    moment.format(&Rfc3339).unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

/// The UTC calendar day of a time as `YYYY-MM-DD`.
pub fn utc_date(at: SystemTime) -> String {
    let moment = OffsetDateTime::from_unix_timestamp(unix_seconds(at)).unwrap_or(OffsetDateTime::UNIX_EPOCH);
    let date = moment.date();
    format!("{:04}-{:02}-{:02}", date.year(), u8::from(date.month()), date.day())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_seconds_round_toward_the_past() {
        assert_eq!(unix_seconds(UNIX_EPOCH + Duration::from_millis(1_999)), 1);
        assert_eq!(unix_seconds(UNIX_EPOCH - Duration::from_millis(1)), -1);
        assert_eq!(unix_seconds(UNIX_EPOCH - Duration::from_secs(5)), -5);
        assert_eq!(unix_seconds(from_unix(-7)), -7);
    }

    #[test]
    fn a_time_is_shown_in_utc() {
        assert_eq!(rfc3339(from_unix(1_772_353_800)), "2026-03-01T08:30:00Z");
        assert_eq!(utc_date(from_unix(1_772_353_800)), "2026-03-01");
        assert_eq!(rfc3339(UNIX_EPOCH + Duration::from_millis(1_500)), "1970-01-01T00:00:01Z");
    }
}
