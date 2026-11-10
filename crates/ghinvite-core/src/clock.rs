//! Wall-clock time for code outside Restate handlers (handlers read time
//! through the journal instead).
//!
//! Same semantics as gh-toolkit's `jwt::Clock`, which it replaces at ghinvite's
//! own sites (GM-07): whole Unix seconds as `i64`, and JavaScript's `Date` on
//! wasm32, where `std::time::SystemTime` panics.

/// Unix seconds; never a floating-point value.
pub type UnixTime = i64;

/// An injectable time source, so tests can pin or move the clock.
pub trait Clock: Send + Sync {
    /// Current Unix timestamp in seconds.
    fn now(&self) -> UnixTime;
}

/// The host's wall clock: `SystemTime` natively, `Date.now()` on wasm32.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> UnixTime {
        #[cfg(target_arch = "wasm32")]
        {
            (js_sys::Date::now() / 1000.0) as i64
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs().min(i64::MAX as u64) as i64)
                .unwrap_or(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_clock_reads_whole_unix_seconds() {
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let now = SystemClock.now();
        let after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert!((before..=after).contains(&now));
    }
}
