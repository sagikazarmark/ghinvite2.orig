//! Session-validation tower layer + admin recheck cache.

use crate::error::WebError;
use crate::session::{self, AdminCheck, Session};
use crate::state::AppState;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use chrono::{Duration, Utc};
use github::oauth::UserApiClient;

const ADMIN_CACHE_TTL_SECS: i64 = 60;

/// Extractor: returns the Session from request extensions or a 302→/login.
/// Routes that require authentication take this as a parameter.
pub struct RequireSession(pub Session);

impl<S: Send + Sync> FromRequestParts<S> for RequireSession {
    type Rejection = WebError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let tower: tower_sessions::Session = parts
            .extensions
            .get::<tower_sessions::Session>()
            .cloned()
            .ok_or_else(|| WebError::Session("no tower session in request extensions".into()))?;
        let session = session::load(&tower)
            .await
            .map_err(|e| WebError::Session(e.to_string()))?;
        if !session.is_authenticated() {
            return Err(WebError::Unauthenticated);
        }
        Ok(RequireSession(session))
    }
}

/// Re-check whether the signed-in user is an admin of `account_login`.
/// Hits GitHub's `/user/memberships/orgs/{login}` once per 60s per login,
/// caching the result in the session.
///
/// Returns `Ok(true)` if admin, `Ok(false)` if not, `Err` on transport
/// failure.
pub async fn check_admin(
    state: &AppState,
    session: &mut Session,
    account_login: &str,
) -> Result<bool, WebError> {
    if let Some(cached) = session.admin_checks.get(account_login)
        && Utc::now() - cached.checked_at < Duration::seconds(ADMIN_CACHE_TTL_SECS)
    {
        return Ok(cached.is_admin);
    }
    let user_api = UserApiClient::new(state.github_transport.clone(), session.access_token.clone());
    let is_admin = match user_api.get_org_membership(account_login).await {
        Ok(m) => m.role == "admin" && m.state == "active",
        Err(github::Error::Status { status: 404, .. }) => false,
        Err(e) => return Err(WebError::Github(e)),
    };
    session.admin_checks.insert(
        account_login.to_string(),
        AdminCheck {
            is_admin,
            checked_at: Utc::now(),
        },
    );
    Ok(is_admin)
}
