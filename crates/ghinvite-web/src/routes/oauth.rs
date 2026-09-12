//! OAuth flow routes: /login, /oauth/callback, /install, /logout.

use crate::error::{Result, WebError};
use crate::session;
use crate::state::AppState;
use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Redirect};
use axum::routing::get;
use chrono::Utc;
use ghinvite_github::oauth::{AuthorizeUrl, UserApiClient, exchange_code};
use rand::RngCore;
use rand::rngs::OsRng;
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
        .route("/oauth/callback", get(oauth_callback))
}

/// Generate a CSRF state, stash it in the session, redirect to GitHub's
/// authorize endpoint.
async fn login(
    State(state): State<AppState>,
    tower: TowerSession,
    axum::extract::Query(q): axum::extract::Query<LoginQuery>,
) -> Result<impl IntoResponse> {
    let csrf = generate_csrf_token();
    let mut session = session::load(&tower)
        .await
        .map_err(|e| WebError::Session(e.to_string()))?;
    session.oauth_csrf = Some(csrf.clone());
    session.return_to = q
        .return_to
        .as_deref()
        .and_then(crate::session::validate_return_to);
    session::save(&tower, &session)
        .await
        .map_err(|e| WebError::Session(e.to_string()))?;

    let authorize =
        AuthorizeUrl::build(&state.config.oauth, csrf, &[]).map_err(WebError::Github)?;

    Ok(Redirect::to(&authorize.url))
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
    if let Err(response) =
        crate::middleware::csrf::CsrfForm::<crate::middleware::csrf::EmptyForm>::from_request(
            request, &state,
        )
        .await
    {
        return response;
    }
    match tower.flush().await {
        Ok(()) => Redirect::to("/").into_response(),
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

fn generate_csrf_token() -> String {
    use base64::Engine;
    let mut buf = [0u8; 24]; // 24 bytes -> 32 base64url chars
    OsRng.fill_bytes(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

#[derive(Debug, Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    installation_id: Option<u64>,
    #[allow(dead_code)]
    setup_action: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

async fn oauth_callback(
    State(state): State<AppState>,
    tower: TowerSession,
    axum::extract::Query(q): axum::extract::Query<CallbackQuery>,
) -> Result<impl IntoResponse> {
    // GitHub may redirect with `?error=access_denied` if the user clicked
    // cancel. Surface as a friendly message.
    if let Some(err) = q.error {
        let desc = q.error_description.unwrap_or_default();
        return Err(WebError::OAuth(format!("{err}: {desc}")));
    }

    let code = q
        .code
        .ok_or_else(|| WebError::BadRequest("missing 'code'".into()))?;
    let supplied_state = q
        .state
        .ok_or_else(|| WebError::BadRequest("missing 'state'".into()))?;

    let mut session = session::load(&tower)
        .await
        .map_err(|e| WebError::Session(e.to_string()))?;
    let expected_state = session
        .oauth_csrf
        .clone()
        .ok_or_else(|| WebError::OAuth("no CSRF state in session".into()))?;
    // Constant-time-ish comparison (the strings are short and not secret in
    // the timing-attack sense, but defensive).
    if expected_state.len() != supplied_state.len()
        || expected_state
            .as_bytes()
            .iter()
            .zip(supplied_state.as_bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            != 0
    {
        return Err(WebError::OAuth("CSRF state mismatch".into()));
    }
    // Consume the CSRF token so it can't be replayed.
    session.oauth_csrf = None;
    session::save(&tower, &session)
        .await
        .map_err(|e| WebError::Session(e.to_string()))?;
    // Persist before calling GitHub: the session middleware does not save on
    // server errors, including a failed token exchange or user lookup.
    tower
        .save()
        .await
        .map_err(|e| WebError::Session(e.to_string()))?;

    // Exchange the code for a user access token.
    let token = exchange_code(state.github_transport.as_ref(), &state.config.oauth, &code).await?;

    // Fetch /user via the user-token client.
    let user_api = UserApiClient::new(state.github_transport.clone(), token.access_token.clone());
    let gh_user = user_api.get_user().await?;

    // Upsert into our `users` table (Plan 1's storage).
    let user_row = ghinvite_core::User {
        user_id: gh_user.id,
        login: gh_user.login.clone(),
        avatar_url: gh_user.avatar_url.clone(),
        last_seen_at: Utc::now(),
    };
    state.storage.upsert_user(&user_row).await?;

    tower
        .cycle_id()
        .await
        .map_err(|e| WebError::Session(e.to_string()))?;

    // Start with fresh identity-bound state, even when signing in as the same
    // user. Only the validated return destination survives the old session.
    let destination = session.return_to.take().unwrap_or_else(|| "/".to_string());
    tower.clear().await;
    crate::session_store::establish_authenticated_lifetime(&tower)
        .await
        .map_err(|e| WebError::Session(e.to_string()))?;
    let session = session::Session {
        user_id: gh_user.id,
        login: gh_user.login,
        access_token: token.access_token,
        csrf_token: Some(crate::middleware::csrf::generate_token()),
        ..Default::default()
    };
    session::save(&tower, &session)
        .await
        .map_err(|e| WebError::Session(e.to_string()))?;

    if let Some(installation_id) = q.installation_id {
        tracing::info!(
            installation_id,
            "OAuth callback carried installation_id; setup URL handles verified onboarding"
        );
    }

    // SessionManagerLayer persists the new record and emits its cookie before
    // sending this redirect. A store failure replaces the redirect with a 500.
    Ok(Redirect::to(&destination))
}
