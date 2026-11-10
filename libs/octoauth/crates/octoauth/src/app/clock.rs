//! The app's time source: the credential's validity window is computed from
//! it. Crate-private; tests inject a fixed clock.

/// Wall-clock source.
pub(crate) trait Clock: Send + Sync {
    /// Current Unix time in whole seconds.
    fn now(&self) -> i64;
}

/// The host wall clock: `std::time::SystemTime`, or JavaScript `Date.now()` on
/// `wasm32-unknown-unknown` (Cloudflare Workers), where `SystemTime::now()`
/// panics.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> i64 {
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        {
            (js_sys::Date::now() / 1000.0) as i64
        }

        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
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
    fn system_clock_is_after_2025() {
        assert!(SystemClock.now() > 1_735_689_600);
    }
}
