//! The virtual clock: real time from a chosen start, plus every advance.

use chrono::{DateTime, TimeDelta, Utc};
use std::time::Instant;

/// The emulator's one clock. It ticks with real time from its start and
/// jumps forward on [`Clock::advance`]; it never goes back.
#[derive(Debug)]
pub(crate) struct Clock {
    origin: DateTime<Utc>,
    started: Instant,
    advanced: TimeDelta,
}

/// The largest single advance: a century, far past every GitHub expiry.
pub(crate) const MAX_ADVANCE_SECONDS: u64 = 100 * 366 * 24 * 60 * 60;

impl Clock {
    /// A clock reading `start` now, or the real time.
    pub(crate) fn new(start: Option<DateTime<Utc>>) -> Self {
        Self {
            origin: start.unwrap_or_else(Utc::now),
            started: Instant::now(),
            advanced: TimeDelta::zero(),
        }
    }

    /// Where the clock started: the default creation time of seeded
    /// objects.
    pub(crate) fn start(&self) -> DateTime<Utc> {
        self.origin
    }

    /// The current virtual time.
    pub(crate) fn now(&self) -> DateTime<Utc> {
        let elapsed = TimeDelta::from_std(self.started.elapsed()).unwrap_or(TimeDelta::MAX);
        self.origin + elapsed + self.advanced
    }

    /// Moves the clock forward. `seconds` is at most [`MAX_ADVANCE_SECONDS`].
    pub(crate) fn advance(&mut self, seconds: u64) {
        let seconds = seconds.min(MAX_ADVANCE_SECONDS);
        self.advanced += TimeDelta::seconds(i64::try_from(seconds).unwrap_or(i64::MAX));
    }

    /// The total of every advance, in seconds.
    pub(crate) fn advanced_seconds(&self) -> i64 {
        self.advanced.num_seconds()
    }
}

/// `at` as an HTTP date (RFC 9110 IMF-fixdate, RFC 7231's preferred format):
/// `Tue, 01 Jan 2030 00:00:00 GMT`.
pub(crate) fn http_date(at: DateTime<Utc>) -> String {
    at.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_imf_fixdate() {
        let at = DateTime::parse_from_rfc3339("2030-01-01T00:00:09.75Z")
            .unwrap()
            .to_utc();
        assert_eq!(http_date(at), "Tue, 01 Jan 2030 00:00:09 GMT");
        // octoauth reads `Date` with chrono's RFC 2822 parser.
        let parsed = DateTime::parse_from_rfc2822(&http_date(at)).unwrap();
        assert_eq!(parsed.timestamp(), at.timestamp());
    }

    #[test]
    fn advances_from_the_start_and_ticks() {
        let start = DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z")
            .unwrap()
            .to_utc();
        let mut clock = Clock::new(Some(start));
        assert!(clock.now() >= start);
        clock.advance(3600);
        let now = clock.now();
        assert!(now >= start + TimeDelta::hours(1));
        assert!(now < start + TimeDelta::hours(1) + TimeDelta::seconds(5));
        assert_eq!(clock.advanced_seconds(), 3600);
        clock.advance(u64::MAX);
        assert_eq!(
            clock.advanced_seconds(),
            3600 + i64::try_from(MAX_ADVANCE_SECONDS).unwrap()
        );
    }
}
