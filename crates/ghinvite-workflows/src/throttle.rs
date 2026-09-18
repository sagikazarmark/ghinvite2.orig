//! Shared wait policy for GitHub throttling.
//!
//! GitHub's retry guidance says when a limit clears, not how long a handler may
//! block. Delivery paths turn it into a bounded wait and schedule a durable
//! continuation for it, so the invitation object stays free while the limit
//! clears instead of holding its lock across an external retry.

use std::time::Duration;

/// Shortest wait worth deferring for: below this the scheduled continuation
/// costs more than the wait saves.
const MINIMUM: Duration = Duration::from_secs(1);

/// Wait used when GitHub named a limit but sent no guidance. GitHub's own
/// documented advice for an unguided secondary limit is at least one minute.
const UNGUIDED: Duration = Duration::from_secs(60);

/// Longest wait a throttled retry defers by, and the cadence a delivery
/// rechecks on when nothing throttled it. The primary quota resets within the
/// hour, so waiting longer buys nothing, and a single absurd `retry-after`
/// cannot park an invitation for a day.
///
/// Each wait is bounded; the retries are not. Giving up would settle the
/// invitation on a limit that has not been shown to be permanent, which is the
/// failure this policy exists to prevent.
pub const MAXIMUM: Duration = Duration::from_secs(3600);

/// Clamp GitHub's retry guidance into the bounded wait a delivery path defers
/// the next attempt by.
pub fn backoff(retry_after: Option<Duration>) -> Duration {
    retry_after.unwrap_or(UNGUIDED).clamp(MINIMUM, MAXIMUM)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guidance_is_honoured_within_the_bounds() {
        assert_eq!(
            backoff(Some(Duration::from_secs(45))),
            Duration::from_secs(45)
        );
        assert_eq!(backoff(Some(MAXIMUM)), MAXIMUM);
    }

    #[test]
    fn absent_guidance_falls_back_to_the_documented_minute() {
        assert_eq!(backoff(None), UNGUIDED);
    }

    #[test]
    fn guidance_outside_the_bounds_is_clamped() {
        // A limit that claims to have already cleared still costs one wait, and
        // one that claims a day is capped at the recheck cadence it replaces.
        assert_eq!(backoff(Some(Duration::ZERO)), MINIMUM);
        assert_eq!(backoff(Some(Duration::from_secs(86_400))), MAXIMUM);
    }
}
