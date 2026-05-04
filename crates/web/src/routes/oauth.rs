//! OAuth flow routes: /login, /oauth/callback, /install, /logout.

use crate::error::{Result, WebError};
use crate::session;
use crate::state::AppState;
use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Redirect};
use axum::routing::get;
use chrono::Utc;
use github::oauth::{AuthorizeUrl, UserApiClient, exchange_code};
use rand::RngCore;
use rand::rngs::OsRng;
use serde::Deserialize;
use tower_sessions::Session as TowerSession;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/login", get(login))
        .route("/logout", get(logout))
        .route("/install", get(install))
        .route("/oauth/callback", get(oauth_callback))
}

/// Generate a CSRF state, stash it in the session, redirect to GitHub's
/// authorize endpoint.
async fn login(State(state): State<AppState>, tower: TowerSession) -> Result<impl IntoResponse> {
    let csrf = generate_csrf_token();
    let mut session = session::load(&tower)
        .await
        .map_err(|e| WebError::Session(e.to_string()))?;
    session.oauth_csrf = Some(csrf.clone());
    session::save(&tower, &session)
        .await
        .map_err(|e| WebError::Session(e.to_string()))?;

    let authorize =
        AuthorizeUrl::build(&state.config.oauth, csrf, &[]).map_err(WebError::Github)?;

    Ok(Redirect::to(&authorize.url))
}

async fn logout(tower: TowerSession) -> impl IntoResponse {
    session::clear(&tower).await;
    Redirect::to("/")
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

    // Exchange the code for a user access token.
    let token = exchange_code(state.github_transport.as_ref(), &state.config.oauth, &code).await?;

    // Fetch /user via the user-token client.
    let user_api = UserApiClient::new(state.github_transport.clone(), token.access_token.clone());
    let gh_user = user_api.get_user().await?;

    // Upsert into our `users` table (Plan 1's storage).
    let user_row = domain::User {
        user_id: gh_user.id,
        login: gh_user.login.clone(),
        avatar_url: gh_user.avatar_url.clone(),
        last_seen_at: Utc::now(),
    };
    state.storage.upsert_user(&user_row).await?;

    // Populate the session.
    session.user_id = gh_user.id;
    session.login = gh_user.login.clone();
    session.access_token = token.access_token;
    session::save(&tower, &session)
        .await
        .map_err(|e| WebError::Session(e.to_string()))?;

    if let Some(installation_id) = q.installation_id {
        tracing::info!(
            installation_id,
            "OAuth callback carried installation_id; awaiting webhook onboarding"
        );
    }

    Ok(Redirect::to("/"))
}
