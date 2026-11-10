//! The provider's time source.

/// Wall-clock source in whole Unix seconds; tests inject a movable one.
pub(crate) trait Clock: Send + Sync {
    fn now(&self) -> i64;
}

/// `SystemTime`, or JavaScript `Date.now()` on `wasm32-unknown-unknown`
/// (Cloudflare Workers), where `SystemTime::now()` panics.
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
