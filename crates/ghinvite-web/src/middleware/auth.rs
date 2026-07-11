//! Session-validation tower layer + admin recheck cache.

use crate::error::WebError;
use crate::session::{self, AdminCheck, Session};
use crate::state::AppState;
use axum::extract::FromRef;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::response::{Html, IntoResponse};
use chrono::{Duration, Utc};
use dioxus::prelude::*;
use ghinvite_github::oauth::UserApiClient;

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
        Err(ghinvite_github::Error::Status { status: 404, .. }) => false,
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

fn generic_not_found_response(signed_in_login: Option<String>) -> axum::response::Response {
    let html = crate::views::render::render(move || {
        rsx! {
            crate::views::not_found::PublicNotFoundPage {
                signed_in_login: signed_in_login.clone(),
            }
        }
    });
    (axum::http::StatusCode::NOT_FOUND, Html(html)).into_response()
}

/// Extractor for routes nested under `/console/accounts/{login}/...`. Loads the
/// session, resolves `:login` → `ghinvite_core::Account` via storage, and runs the
/// admin recheck (60s cache per `crate::session::AdminCheck`). On any
/// concealment path — no install, not admin, install is uninstalled — surfaces
/// as a generic public 404. Unauthenticated console requests redirect to login
/// before account authorization checks run.
///
/// The handler MUST save the session after any mutation via
/// `session::save(&tower, &session)` to persist cache updates.
pub struct RequireConsoleAdminOf {
    pub session: Session,
    pub account: ghinvite_core::Account,
    pub tower: tower_sessions::Session,
}

impl<S> FromRequestParts<S> for RequireConsoleAdminOf
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = axum::response::Response;

    async fn from_request_parts(
        parts: &mut Parts,
        outer_state: &S,
    ) -> Result<Self, Self::Rejection> {
        let state = AppState::from_ref(outer_state);

        let tower: tower_sessions::Session = parts
            .extensions
            .get::<tower_sessions::Session>()
            .cloned()
            .ok_or_else(|| {
                WebError::Session("no tower session in request extensions".into()).into_response()
            })?;

        let mut session = crate::session::load(&tower)
            .await
            .map_err(|e| WebError::Session(e.to_string()).into_response())?;

        if !session.is_authenticated() {
            let return_to = parts
                .uri
                .path_and_query()
                .map(|value| value.as_str())
                .unwrap_or("/console");
            let encoded: String =
                url::form_urlencoded::byte_serialize(return_to.as_bytes()).collect();
            return Err(
                axum::response::Redirect::to(&format!("/login?return_to={encoded}"))
                    .into_response(),
            );
        }

        let axum::extract::Path(params): axum::extract::Path<
            std::collections::HashMap<String, String>,
        > = axum::extract::Path::from_request_parts(parts, outer_state)
            .await
            .map_err(|e| WebError::BadRequest(format!("path: {e}")).into_response())?;
        let login = params
            .get("login")
            .ok_or_else(|| {
                WebError::BadRequest("missing :login path param".into()).into_response()
            })?
            .clone();

        let account = match state.storage.get_active_installation_by_login(&login).await {
            Ok(Some(account)) => account,
            Ok(None) => return Err(generic_not_found_response(Some(session.login.clone()))),
            Err(error) => return Err(WebError::Storage(error).into_response()),
        };

        // Personal-account installations: `get_org_membership` returns 404 for
        // personal accounts, which maps to `is_admin = false` — locking the owner
        // out. Short-circuit: match session login against account login directly.
        let is_admin = if account.account_type == ghinvite_core::AccountType::User {
            session.login == account.account_login
        } else {
            match check_admin(&state, &mut session, &login).await {
                Ok(is_admin) => is_admin,
                Err(error) => return Err(error.into_response()),
            }
        };
        if !is_admin {
            return Err(generic_not_found_response(Some(session.login.clone())));
        }

        crate::session::save(&tower, &session)
            .await
            .map_err(|e| WebError::Session(e.to_string()).into_response())?;

        Ok(RequireConsoleAdminOf {
            session,
            account,
            tower,
        })
    }
}
