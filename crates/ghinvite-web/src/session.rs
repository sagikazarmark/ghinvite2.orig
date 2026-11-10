//! Session struct + tower-sessions integration.

use serde::{Deserialize, Serialize};
use tower_sessions::Session as TowerSession;

/// Per spec §10.1: session contents. Token-free: sign-in keeps no GitHub
/// credential, and its OAuth state lives in pending sign-in storage.
///
/// Unknown fields are ignored, never refused: records written by earlier
/// deployments still load, and the next write stores only these fields
/// (`session_store` re-serializes the payload). That retires legacy OAuth's
/// `access_token` and `oauth_csrf`, which nothing reads.
#[derive(Clone, Default, Deserialize, Serialize)]
pub struct Session {
    pub user_id: u64,
    pub login: String,
    /// Browser mutation authority, replaced with identity on authentication.
    /// Every authenticated session carries one; a record without it is invalid.
    pub csrf_token: Option<String>,
    /// Stored by `GET /login?return_to=<path>` and consumed by the OAuth callback
    /// to bounce the user back after sign-in. Only known relative paths are
    /// stored (open-redirect prevention).
    pub return_to: Option<String>,
    /// Routing and authority evidence only; the plaintext capability is never retained.
    #[serde(default)]
    pub pending_link: Option<PendingLink>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain_evidence: Option<ghinvite_core::email_domain::DomainEvidence>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Diagnostic formatting protects browser CSRF, domain evidence and
        // any caller-supplied return destination.
        f.debug_struct("Session")
            .field("user_id", &self.user_id)
            .field("authenticated", &self.is_authenticated())
            .field("pending_link", &self.pending_link.is_some())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PendingLink {
    pub link_id: ghinvite_core::InvitationLinkId,
    pub secret_sha256: ghinvite_core::invitation_link::SecretHash,
    pub secret_generation: u32,
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
    if raw.starts_with("//")
        || raw.contains("..")
        || raw.contains(['#', '\\'])
        || raw.chars().any(char::is_control)
    {
        None
    } else if let Some(query) = raw.strip_prefix("/join?") {
        // Only typed recovery controls may be persisted and redirected. In
        // particular, a supplied bearer secret must never enter this channel.
        let mut seen = std::collections::BTreeSet::new();
        let valid = url::form_urlencoded::parse(query.as_bytes()).all(|(key, value)| {
            seen.insert(key.to_string())
                && match key.as_ref() {
                    "operation_id" => {
                        ghinvite_core::admission::AdmissionOperationId::try_from(value.into_owned())
                            .is_ok()
                    }
                    "fresh" => matches!(value.as_ref(), "true" | "false"),
                    _ => false,
                }
        });
        (valid && !seen.is_empty()).then(|| raw.to_owned())
    } else if let Some(tail) = raw.strip_prefix("/requests/") {
        let id = tail
            .strip_suffix("/refresh")
            .unwrap_or(tail)
            .parse::<ghinvite_core::RequestId>()
            .ok()?;
        Some(format!("/requests/{id}"))
    } else if raw == "/join"
        || raw == "/console"
        || matches!(
            raw,
            "/onboarding"
                | "/onboarding/requests"
                | "/onboarding/requests?app=repo"
                | "/onboarding/requests?app=org"
        )
        || raw.starts_with("/console/")
        || raw == "/setup/repo"
        || raw.starts_with("/setup/repo?")
        || raw == "/setup/org"
        || raw.starts_with("/setup/org?")
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

/// Rewrite the record's stored session in its current shape, dropping fields
/// earlier deployments wrote. Fails on a malformed session, which
/// [`from_record`] refuses as well.
pub(crate) fn canonicalize(
    record: &mut tower_sessions::session::Record,
) -> Result<(), serde_json::Error> {
    if let Some(value) = record.data.get_mut(SESSION_KEY) {
        let session: Session = serde_json::from_value(value.clone())?;
        *value = serde_json::to_value(session)?;
    }
    Ok(())
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

    #[test]
    fn default_session_is_unauthenticated() {
        let s = Session::default();
        assert!(!s.is_authenticated());
    }

    #[test]
    fn diagnostic_formatting_never_exposes_browser_credentials_or_return_data() {
        let session = Session {
            user_id: 7,
            login: "LOGIN_MARKER".into(),
            csrf_token: Some("CSRF_MARKER".into()),
            return_to: Some("/console?value=RETURN_MARKER".into()),
            ..Default::default()
        };
        for diagnostic in [format!("{session:?}"), format!("{session:#?}")] {
            assert!(!diagnostic.contains("MARKER"));
            assert!(diagnostic.contains("user_id: 7"));
        }
    }

    #[test]
    fn populated_session_is_authenticated() {
        let s = Session {
            user_id: 7,
            login: "octocat".into(),
            csrf_token: None,
            return_to: None,
            pending_link: None,
            domain_evidence: None,
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
                "user_id": 42, "login": "octocat",
                "csrf_token": csrf_token, "admin_checks": {},
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

    /// A session signed in through legacy OAuth (before GM-09) carries its
    /// user token and OAuth state. It still loads with its identity, and
    /// serializing it again drops both fields.
    #[test]
    fn legacy_sign_in_session_loads_without_its_token() {
        let legacy = serde_json::json!({
            "user_id": 42, "login": "octocat", "access_token": "ghu_LEGACY_TOKEN_MARKER",
            "oauth_csrf": "STATE_MARKER", "csrf_token": "token", "return_to": null,
            "pending_link": null
        });
        let record = tower_sessions::session::Record {
            id: Default::default(),
            data: [(SESSION_KEY.to_string(), legacy)].into(),
            expiry_date: tower_sessions::cookie::time::OffsetDateTime::now_utc(),
        };
        let session = from_record(&record).unwrap();
        assert!(session.is_authenticated());
        assert_eq!((session.user_id, session.login.as_str()), (42, "octocat"));
        let written = serde_json::to_value(&session).unwrap();
        assert!(written.get("access_token").is_none());
        assert!(written.get("oauth_csrf").is_none());
        assert!(!written.to_string().contains("MARKER"));
    }

    #[test]
    fn obsolete_session_authority_is_discarded() {
        let mut value = serde_json::to_value(Session::default()).unwrap();
        value["admin_checks"] =
            serde_json::json!({"42:9001":{"is_admin":true,"checked_at":"2099-01-01T00:00:00Z"}});
        let session: Session = serde_json::from_value(value).unwrap();
        assert!(
            serde_json::to_value(session)
                .unwrap()
                .get("admin_checks")
                .is_none()
        );
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
        assert_eq!(validate_return_to("/join"), Some("/join".to_string()));
        let id = ghinvite_core::RequestId::new();
        for suffix in ["", "/refresh"] {
            assert_eq!(
                validate_return_to(&format!("/requests/{id}{suffix}")),
                Some(format!("/requests/{id}"))
            );
        }
        for suffix in ["?secret=PRIVATE", "/refresh/extra", "/other"] {
            assert_eq!(validate_return_to(&format!("/requests/{id}{suffix}")), None);
        }
        for rejected in [
            "/i",
            "/i/01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "/i/AAAAAAAAAAAAAAAA/request",
            "/i#SECRET_MARKER",
        ] {
            assert_eq!(validate_return_to(rejected), None);
        }
        assert_eq!(
            validate_return_to("/setup/repo?installation_id=77&setup_action=install"),
            Some("/setup/repo?installation_id=77&setup_action=install".to_string())
        );
        assert_eq!(validate_return_to("/console"), Some("/console".to_string()));
        assert_eq!(
            validate_return_to("/console/accounts/acme/links/new"),
            Some("/console/accounts/acme/links/new".to_string())
        );
        assert_eq!(validate_return_to("/"), None);
        assert_eq!(validate_return_to("/setup"), None);
        assert_eq!(validate_return_to("/setup/repo/extra"), None);
        assert_eq!(validate_return_to("https://evil.com"), None);
        assert_eq!(validate_return_to("//evil.com/i/foo"), None);
        assert_eq!(validate_return_to("/console/../admin"), None);
        assert_eq!(validate_return_to("//evil.com/console"), None);
        assert_eq!(validate_return_to("/i/../../admin"), None); // path traversal
        assert_eq!(
            validate_return_to("/setup/repo?installation_id=77/../admin"),
            None
        );
    }
}
