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
    /// User access token from GitHub OAuth. Production entry points use
    /// [`crate::session_store::ProtectedStore`] to encrypt complete stored records.
    pub access_token: String,
    /// CSRF token for the OAuth state round-trip. Set on `/login`, verified on
    /// `/oauth/callback`.
    pub oauth_csrf: Option<String>,
    /// Browser mutation authority, replaced with identity on authentication.
    /// Every authenticated session carries one; a record without it is invalid.
    pub csrf_token: Option<String>,
    /// Organization authority keyed by `"{user_id}:{account_id}"` (numeric
    /// GitHub IDs), with a 60-second TTL. Clear this map on session reset.
    pub admin_checks: HashMap<String, AdminCheck>,
    /// Stored by `GET /login?return_to=<path>` and consumed by the OAuth callback
    /// to bounce the user back after sign-in. Only known relative paths are
    /// stored (open-redirect prevention).
    pub return_to: Option<String>,
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

    /// Authenticated sessions always carry browser mutation authority.
    fn is_well_formed(&self) -> bool {
        !self.is_authenticated() || self.csrf_token.is_some()
    }
}

/// Validates a `return_to` query parameter. Only accepts relative paths for
/// invite flows and GitHub App setup returns — prevents open redirect and
/// path-traversal bypasses.
/// Returns `None` on rejection.
pub fn validate_return_to(raw: &str) -> Option<String> {
    if raw.starts_with("//") || raw.contains("..") {
        None
    } else if raw.starts_with("/i/")
        || raw == "/console"
        || raw.starts_with("/console/")
        || raw == "/setup/github"
        || raw.starts_with("/setup/github?")
    {
        Some(raw.to_string())
    } else {
        None
    }
}

const SESSION_KEY: &str = "ghinvite";

/// Decode the stored session, or `None` when the record is malformed
/// (unreadable, or authenticated without browser mutation authority).
pub(crate) fn from_record(record: &tower_sessions::session::Record) -> Option<Session> {
    let session = match record.data.get(SESSION_KEY) {
        Some(value) => serde_json::from_value(value.clone()).ok()?,
        None => Session::default(),
    };
    session.is_well_formed().then_some(session)
}

/// Read the session from a tower-sessions handle. Returns the default empty
/// session if none is set. A malformed session confers no authority.
pub async fn load(tower: &TowerSession) -> Result<Session, tower_sessions::session::Error> {
    let session: Session = tower.get(SESSION_KEY).await?.unwrap_or_default();
    Ok(if session.is_well_formed() {
        session
    } else {
        Session::default()
    })
}

/// Persist the session.
pub async fn save(
    tower: &TowerSession,
    session: &Session,
) -> Result<(), tower_sessions::session::Error> {
    tower.insert(SESSION_KEY, session).await?;
    crate::session_store::apply_lifetime(tower).await
}

/// One-shot status message shown to the user after a redirect. Defined in
/// `ghinvite-ui` (the layouts render it); re-exported here because the session
/// is where it is stored and read.
pub use ghinvite_ui::flash::{Flash, FlashLevel};

const FLASH_KEY: &str = "ghinvite_flash";

/// Store a flash message to be shown on the next request.
pub async fn set_flash(
    tower: &TowerSession,
    flash: Flash,
) -> Result<(), tower_sessions::session::Error> {
    tower.insert(FLASH_KEY, flash).await?;
    crate::session_store::apply_lifetime(tower).await
}

/// Read + clear the flash. Returns `None` if no flash is present.
pub async fn take_flash(
    tower: &TowerSession,
) -> Result<Option<Flash>, tower_sessions::session::Error> {
    let flash: Option<Flash> = tower.get(FLASH_KEY).await?;
    if flash.is_some() {
        tower.remove::<Flash>(FLASH_KEY).await?;
    }
    Ok(flash)
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
            csrf_token: None,
            admin_checks: HashMap::new(),
            return_to: None,
        };
        assert!(s.is_authenticated());
    }

    #[test]
    fn authenticated_record_without_csrf_token_is_malformed() {
        let record = |session: serde_json::Value| tower_sessions::session::Record {
            id: Default::default(),
            data: [(SESSION_KEY.to_string(), session)].into(),
            expiry_date: tower_sessions::cookie::time::OffsetDateTime::now_utc(),
        };
        let fields = |csrf_token: Option<&str>| {
            serde_json::json!({
                "user_id": 42, "login": "octocat", "access_token": "u_xxx",
                "oauth_csrf": null, "csrf_token": csrf_token, "admin_checks": {},
                "return_to": null
            })
        };
        assert!(from_record(&record(fields(None))).is_none());
        assert!(from_record(&record(fields(Some("token")))).is_some_and(|s| s.is_authenticated()));
        assert!(
            from_record(&record(serde_json::to_value(Session::default()).unwrap()))
                .is_some_and(|s| !s.is_authenticated())
        );
    }

    #[test]
    fn admin_check_round_trips_via_serde() {
        let mut s = Session::default();
        s.admin_checks.insert(
            "42:9001".into(),
            AdminCheck {
                is_admin: true,
                checked_at: Utc.with_ymd_and_hms(2026, 5, 4, 12, 0, 0).unwrap(),
            },
        );
        let json = serde_json::to_string(&s).unwrap();
        let parsed: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.admin_checks.len(), 1);
        assert!(parsed.admin_checks.get("42:9001").unwrap().is_admin);
    }

    #[test]
    fn flash_round_trips_via_serde() {
        let f = Flash {
            level: FlashLevel::Success,
            message: "link created".into(),
        };
        let json = serde_json::to_string(&f).unwrap();
        let parsed: Flash = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.message, "link created");
        assert_eq!(parsed.level, FlashLevel::Success);
    }

    #[test]
    fn return_to_validation() {
        use super::validate_return_to;
        assert_eq!(
            validate_return_to("/i/AAAAAAAAAAAAAAAA/request"),
            Some("/i/AAAAAAAAAAAAAAAA/request".to_string())
        );
        assert_eq!(
            validate_return_to("/setup/github?installation_id=77&setup_action=install"),
            Some("/setup/github?installation_id=77&setup_action=install".to_string())
        );
        assert_eq!(validate_return_to("/console"), Some("/console".to_string()));
        assert_eq!(
            validate_return_to("/console/accounts/acme/links/new"),
            Some("/console/accounts/acme/links/new".to_string())
        );
        assert_eq!(validate_return_to("/"), None);
        assert_eq!(validate_return_to("/setup"), None);
        assert_eq!(validate_return_to("/setup/github/extra"), None);
        assert_eq!(validate_return_to("https://evil.com"), None);
        assert_eq!(validate_return_to("//evil.com/i/foo"), None);
        assert_eq!(validate_return_to("/console/../admin"), None);
        assert_eq!(validate_return_to("//evil.com/console"), None);
        assert_eq!(validate_return_to("/i/../../admin"), None); // path traversal
        assert_eq!(
            validate_return_to("/setup/github?installation_id=77/../admin"),
            None
        );
    }
}
