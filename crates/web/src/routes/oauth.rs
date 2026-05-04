//! OAuth flow routes: /login, /oauth/callback, /install, /logout.

use crate::error::{Result, WebError};
use crate::session;
use crate::state::AppState;
use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Redirect};
use axum::routing::get;
use github::oauth::AuthorizeUrl;
use rand::RngCore;
use rand::rngs::OsRng;
use tower_sessions::Session as TowerSession;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/login", get(login))
        .route("/logout", get(logout))
        .route("/install", get(install))
        // Callback handler comes in Tasks 10 + 11.
}

/// Generate a CSRF state, stash it in the session, redirect to GitHub's
/// authorize endpoint.
async fn login(
    State(state): State<AppState>,
    tower: TowerSession,
) -> Result<impl IntoResponse> {
    let csrf = generate_csrf_token();
    let mut session = session::load(&tower)
        .await
        .map_err(|e| WebError::Session(e.to_string()))?;
    session.oauth_csrf = Some(csrf.clone());
    session::save(&tower, &session)
        .await
        .map_err(|e| WebError::Session(e.to_string()))?;

    let authorize = AuthorizeUrl::build(&state.config.oauth, csrf, &[])
        .map_err(WebError::Github)?;

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
