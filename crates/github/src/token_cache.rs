//! In-process installation-token cache.

use chrono::{DateTime, Duration, Utc};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

/// 60s buffer below the actual `expires_at` — refresh anytime we'd return a
/// token with less than 60s of remaining life.
const REFRESH_BUFFER_SECS: i64 = 60;

#[derive(Clone, Debug)]
struct CachedToken {
    token: String,
    expires_at: DateTime<Utc>,
}

/// Thread-safe, clone-friendly cache. Keys are installation ids.
#[derive(Clone, Default)]
pub struct TokenCache {
    inner: Arc<RwLock<HashMap<u64, CachedToken>>>,
}

impl TokenCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the cached token if it has more than [`REFRESH_BUFFER_SECS`] of
    /// remaining life at `now`. Otherwise returns `None` and the caller should
    /// re-mint via [`crate::installation::InstallationClient::mint_installation_token`].
    pub fn get_fresh(&self, installation_id: u64, now: DateTime<Utc>) -> Option<String> {
        let guard = self.inner.read();
        let entry = guard.get(&installation_id)?;
        if entry.expires_at - Duration::seconds(REFRESH_BUFFER_SECS) > now {
            Some(entry.token.clone())
        } else {
            None
        }
    }

    pub fn insert(&self, installation_id: u64, token: String, expires_at: DateTime<Utc>) {
        self.inner
            .write()
            .insert(installation_id, CachedToken { token, expires_at });
    }

    /// For tests / shutdown.
    pub fn invalidate(&self, installation_id: u64) {
        self.inner.write().remove(&installation_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn fresh_when_far_from_expiry() {
        let c = TokenCache::new();
        c.insert(1, "tok".into(), at("2026-05-04T13:00:00Z"));
        let now = at("2026-05-04T12:00:00Z");
        assert_eq!(c.get_fresh(1, now).as_deref(), Some("tok"));
    }

    #[test]
    fn stale_when_within_buffer() {
        let c = TokenCache::new();
        c.insert(1, "tok".into(), at("2026-05-04T12:00:30Z")); // 30s out
        let now = at("2026-05-04T12:00:00Z");
        assert!(
            c.get_fresh(1, now).is_none(),
            "30s remaining should be stale"
        );
    }

    #[test]
    fn missing_installation_is_none() {
        let c = TokenCache::new();
        assert!(
            c.get_fresh(99, Utc.with_ymd_and_hms(2026, 5, 4, 12, 0, 0).unwrap())
                .is_none()
        );
    }

    #[test]
    fn invalidate_drops_entry() {
        let c = TokenCache::new();
        c.insert(1, "tok".into(), at("2099-01-01T00:00:00Z"));
        c.invalidate(1);
        assert!(c.get_fresh(1, Utc::now()).is_none());
    }
}
