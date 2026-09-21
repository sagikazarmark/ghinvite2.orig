//! Session-validation tower layer + admin recheck cache.

use crate::error::WebError;
use crate::session::{AdminCheck, Session};
use crate::state::AppState;
use axum::extract::FromRef;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::response::{Html, IntoResponse};
use chrono::{Duration, Utc};
use dioxus::prelude::*;
use ghinvite_github::oauth::UserApiClient;

const ADMIN_CACHE_TTL_SECS: i64 = 60;

/// Check the signed-in user's authority over an installed account.
/// Personal ownership is bound to the authenticated GitHub user ID.
/// Organization membership checks are cached in the session for 60 seconds.
///
/// Returns `Ok(true)` if admin, `Ok(false)` if not, `Err` on transport
/// failure.
pub async fn check_admin(
    state: &AppState,
    session: &mut Session,
    account: &ghinvite_core::Account,
) -> Result<bool, WebError> {
    if account.account_type == ghinvite_core::AccountType::User {
        return Ok(session.user_id == account.account_id);
    }
    let cache_key = format!("{}:{}", session.user_id, account.account_id);
    let now = Utc::now();
    if let Some(cached) = session.admin_checks.get(&cache_key)
        && cached.checked_at <= now
        && now - cached.checked_at < Duration::seconds(ADMIN_CACHE_TTL_SECS)
    {
        return Ok(cached.is_admin);
    }
    let user_api = UserApiClient::new(state.github_transport.clone(), session.access_token.clone());
    let is_admin = match user_api.get_org_membership(&account.account_login).await {
        Ok(m) => {
            m.organization.id == account.account_id && m.role == "admin" && m.state == "active"
        }
        Err(ghinvite_github::Error::Status { status: 404, .. }) => false,
        Err(e) => return Err(WebError::Github(e)),
    };
    session.admin_checks.insert(
        cache_key,
        AdminCheck {
            is_admin,
            checked_at: Utc::now(),
        },
    );
    Ok(is_admin)
}

fn generic_not_found_response(session: &Session) -> axum::response::Response {
    let signed_in_login = Some(session.login.clone());
    let html = crate::views::render::render_with_csrf(session.csrf_token.clone(), move || {
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
/// concealment path — no account history or not admin — surfaces
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
        resolve_console_admin(parts, outer_state).await
    }
}

// The rejection is the rendered response axum returns as-is.
#[allow(clippy::result_large_err)]
async fn resolve_console_admin<S>(
    parts: &mut Parts,
    outer_state: &S,
) -> Result<RequireConsoleAdminOf, axum::response::Response>
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
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
        let encoded: String = url::form_urlencoded::byte_serialize(return_to.as_bytes()).collect();
        return Err(
            axum::response::Redirect::to(&format!("/login?return_to={encoded}")).into_response(),
        );
    }

    let axum::extract::Path(params): axum::extract::Path<
        std::collections::HashMap<String, String>,
    > = axum::extract::Path::from_request_parts(parts, outer_state)
        .await
        .map_err(|e| WebError::BadRequest(format!("path: {e}")).into_response())?;
    let login = params
        .get("login")
        .ok_or_else(|| WebError::BadRequest("missing :login path param".into()).into_response())?
        .clone();

    let account = match state.storage.get_active_installation_by_login(&login).await {
        Ok(Some(account)) => account,
        // An uninstalled account keeps its history readable to its admins.
        Ok(None) => match state.storage.get_latest_installation_by_login(&login).await {
            Ok(Some(account)) => account,
            Ok(None) => return Err(generic_not_found_response(&session)),
            Err(error) => return Err(WebError::Storage(error).into_response()),
        },
        Err(error) => return Err(WebError::Storage(error).into_response()),
    };

    let is_admin = match check_admin(&state, &mut session, &account).await {
        Ok(is_admin) => is_admin,
        Err(error) => return Err(error.into_response()),
    };
    if !is_admin {
        return Err(generic_not_found_response(&session));
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
