//! Sign-in and installation routes: /login, /oauth/callback, /install, /logout.
//! Sign-in is token-free (`typed`, on octologin).
mod typed;

use crate::error::{OAuthFailure, Result, WebError};
use crate::session;
use crate::state::AppState;
use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Redirect};
use axum::routing::get;
use chrono::Utc;
use serde::Deserialize;
use tower_sessions::Session as TowerSession;

#[derive(Debug, Deserialize)]
struct LoginQuery {
    return_to: Option<String>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/login", get(login))
        .route("/logout", axum::routing::post(logout))
        .route("/install", get(install))
        .route("/install/org", get(install_org))
        .route("/oauth/callback", get(oauth_callback))
}

/// Start sign-in: store pending state and redirect to GitHub's authorize
/// endpoint (see [`typed::login`]).
async fn login(
    State(state): State<AppState>,
    tower: TowerSession,
    axum::extract::Query(q): axum::extract::Query<LoginQuery>,
) -> Result<impl IntoResponse> {
    typed::login(&state, &tower, q.return_to.as_deref()).await
}

async fn logout(
    State(state): State<AppState>,
    tower: TowerSession,
    request: axum::extract::Request,
) -> impl IntoResponse {
    use axum::extract::FromRequest;
    // A storage outage prevents CSRF verification. Clear only the browser's
    // cookie in that case; no server-side mutation bypasses CSRF verification.
    if session::load(&tower).await.is_err() {
        return unconfirmed_signout(state.config.cookie_secure);
    }
    let form = match crate::middleware::csrf::CsrfForm::<LoginQuery>::from_request(request, &state)
        .await
    {
        Ok(form) => form.0,
        Err(response) => return response,
    };
    let destination = form
        .return_to
        .as_deref()
        .and_then(session::validate_return_to)
        .unwrap_or_else(|| "/".to_owned());
    let pending_link = if destination.starts_with("/join") {
        session::load(&tower)
            .await
            .ok()
            .and_then(|session| session.pending_link)
    } else {
        None
    };
    match tower.flush().await {
        Ok(()) => {
            if pending_link.is_some() {
                let anonymous = session::Session {
                    pending_link,
                    csrf_token: Some(crate::middleware::csrf::generate_token()),
                    ..Default::default()
                };
                if session::save(&tower, &anonymous).await.is_err() {
                    return unconfirmed_signout(state.config.cookie_secure);
                }
            }
            Redirect::to(&destination).into_response()
        }
        Err(_) => unconfirmed_signout(state.config.cookie_secure),
    }
}

fn unconfirmed_signout(secure: bool) -> axum::response::Response {
    // flush clears local data before revoking. On failure its ID remains;
    // a 5xx suppresses Tower's otherwise implicit save of that empty record.
    let mut response = (
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "Browser session cleared; server sign-out could not be confirmed. Please try again later.",
    )
        .into_response();
    let cookie = if secure {
        "id=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax; Secure"
    } else {
        "id=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax"
    };
    response
        .headers_mut()
        .insert(axum::http::header::SET_COOKIE, cookie.parse().unwrap());
    tracing::warn!("session revocation could not be confirmed");
    response
}

async fn install(State(state): State<AppState>) -> impl IntoResponse {
    Redirect::to(&state.config.github_install_url)
}

async fn install_org(State(state): State<AppState>) -> axum::response::Response {
    match &state.config.github_org_install_url {
        Some(url) => Redirect::to(url).into_response(),
        None => (axum::http::StatusCode::SERVICE_UNAVAILABLE, axum::response::Html("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Organization app installation unavailable</title></head><body><main><h1>Organization app installation is unavailable</h1><p>This deployment has not configured the organization app installation page. Contact the operator, or continue with repository invitations.</p><a href=\"/console\">Return to Console</a></main></body></html>")).into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    installation_id: Option<u64>,
    #[allow(dead_code)]
    setup_action: Option<String>,
    error: Option<String>,
    /// Present on GitHub's error redirects and deliberately never read: it is
    /// upstream prose on a browser-controlled URL, and reflecting it was how
    /// upstream diagnostics used to reach the response body.
    #[allow(dead_code)]
    error_description: Option<String>,
}

async fn oauth_callback(
    State(state): State<AppState>,
    tower: TowerSession,
    axum::extract::Query(q): axum::extract::Query<CallbackQuery>,
) -> Result<impl IntoResponse> {
    if let Some(installation_id) = q.installation_id {
        tracing::info!(
            installation_id,
            "OAuth callback carried installation_id; setup URL handles verified onboarding"
        );
    }
    typed::callback(&state, &tower, q).await
}
