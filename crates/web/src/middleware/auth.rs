//! Session-validation tower layer + admin recheck cache.

use crate::error::WebError;
use crate::session::{self, AdminCheck, Session};
use crate::state::AppState;
use axum::extract::FromRef;
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

/// Extractor for routes nested under `/accounts/{login}/...`. Loads the
/// session, resolves `:login` → `domain::Account` via storage, and runs the
/// admin recheck (60s cache per `crate::session::AdminCheck`). On any
/// failure path — unauthenticated, no install, not admin, install is
/// uninstalled — surfaces as `WebError::NotFound` (the framework maps
/// that and `Forbidden` to 404 per spec §10.3).
///
/// The handler MUST save the session after any mutation via
/// `session::save(&tower, &session)` to persist cache updates.
pub struct RequireAdminOf {
    pub session: Session,
    pub account: domain::Account,
    pub tower: tower_sessions::Session,
}

impl<S> FromRequestParts<S> for RequireAdminOf
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = WebError;

    async fn from_request_parts(
        parts: &mut Parts,
        outer_state: &S,
    ) -> Result<Self, Self::Rejection> {
        let state = AppState::from_ref(outer_state);

        let tower: tower_sessions::Session = parts
            .extensions
            .get::<tower_sessions::Session>()
            .cloned()
            .ok_or_else(|| WebError::Session("no tower session in request extensions".into()))?;

        let mut session = crate::session::load(&tower)
            .await
            .map_err(|e| WebError::Session(e.to_string()))?;

        if !session.is_authenticated() {
            return Err(WebError::NotFound);
        }

        let axum::extract::Path(params): axum::extract::Path<
            std::collections::HashMap<String, String>,
        > = axum::extract::Path::from_request_parts(parts, outer_state)
            .await
            .map_err(|e| WebError::BadRequest(format!("path: {e}")))?;
        let login = params
            .get("login")
            .ok_or_else(|| WebError::BadRequest("missing :login path param".into()))?
            .clone();

        let account = state
            .storage
            .get_active_installation_by_login(&login)
            .await?
            .ok_or(WebError::NotFound)?;

        // Personal-account installations: `get_org_membership` returns 404 for
        // personal accounts, which maps to `is_admin = false` — locking the owner
        // out. Short-circuit: match session login against account login directly.
        let is_admin = if account.account_type == domain::AccountType::User {
            session.login == account.account_login
        } else {
            check_admin(&state, &mut session, &login).await?
        };
        if !is_admin {
            return Err(WebError::NotFound);
        }

        crate::session::save(&tower, &session)
            .await
            .map_err(|e| WebError::Session(e.to_string()))?;

        Ok(RequireAdminOf {
            session,
            account,
            tower,
        })
    }
}
