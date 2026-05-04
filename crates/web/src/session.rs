//! Session struct + tower-sessions integration.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tower_sessions::Session as TowerSession;

/// Per spec §10.1: session contents.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Session {
    pub user_id: u64,
    pub login: String,
    /// User access token from GitHub OAuth. Stored encrypted at rest by
    /// tower-sessions's cookie-encryption layer (see [`crate::config::WebConfig::session_secret`]).
    pub access_token: String,
    /// CSRF token for the OAuth state round-trip. Set on `/login`, verified on
    /// `/oauth/callback`.
    pub oauth_csrf: Option<String>,
    /// Last admin-check timestamp per `account_login`. Used by the auth
    /// middleware to throttle GitHub `/user/memberships/orgs/{login}` calls
    /// (60-second cache per spec §10.3).
    pub admin_checks: HashMap<String, AdminCheck>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AdminCheck {
    pub is_admin: bool,
    pub checked_at: DateTime<Utc>,
}

impl Session {
    pub fn is_authenticated(&self) -> bool {
        !self.login.is_empty()
    }
}

const SESSION_KEY: &str = "ghinvite";

/// Read the session from a tower-sessions handle. Returns the default empty
/// session if none is set.
pub async fn load(tower: &TowerSession) -> Result<Session, tower_sessions::session::Error> {
    Ok(tower.get(SESSION_KEY).await?.unwrap_or_default())
}

/// Persist the session.
pub async fn save(tower: &TowerSession, session: &Session) -> Result<(), tower_sessions::session::Error> {
    tower.insert(SESSION_KEY, session).await
}

/// Clear the session entirely.
pub async fn clear(tower: &TowerSession) {
    tower.flush().await.ok();
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn default_session_is_unauthenticated() {
        let s = Session::default();
        assert!(!s.is_authenticated());
    }

    #[test]
    fn populated_session_is_authenticated() {
        let s = Session {
            user_id: 7,
            login: "octocat".into(),
            access_token: "u_xxx".into(),
            oauth_csrf: None,
            admin_checks: HashMap::new(),
        };
        assert!(s.is_authenticated());
    }

    #[test]
    fn admin_check_round_trips_via_serde() {
        let mut s = Session::default();
        s.admin_checks.insert(
            "acme".into(),
            AdminCheck {
                is_admin: true,
                checked_at: Utc.with_ymd_and_hms(2026, 5, 4, 12, 0, 0).unwrap(),
            },
        );
        let json = serde_json::to_string(&s).unwrap();
        let parsed: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.admin_checks.len(), 1);
        assert!(parsed.admin_checks.get("acme").unwrap().is_admin);
    }
}
