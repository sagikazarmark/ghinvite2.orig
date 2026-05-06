//! `GET /` — home page (logged-out marketing or signed-in redirect).

use crate::session;
use crate::state::AppState;
use crate::views::home::HomePage;
use crate::views::render::render;
use axum::Router;
use axum::extract::State;
use axum::response::{Html, IntoResponse, Redirect};
use axum::routing::get;
use dioxus::prelude::*;
use tower_sessions::Session as TowerSession;

pub fn router() -> Router<AppState> {
    Router::new().route("/", get(home))
}

async fn home(State(state): State<AppState>, tower: TowerSession) -> impl IntoResponse {
    let session = session::load(&tower).await.unwrap_or_default();

    if session.is_authenticated() {
        // Find the first active installation and redirect to its dashboard.
        if let Ok(accounts) = state.storage.list_active_installations().await {
            if let Some(first) = accounts.into_iter().next() {
                return Redirect::to(&format!("/accounts/{}", first.account_login)).into_response();
            }
        }
        // No installation found: fall through to home page with install CTA.
    }

    let signed_in_login = if session.is_authenticated() {
        Some(session.login.clone())
    } else {
        None
    };

    let html = render(move || rsx! { HomePage { signed_in_login: signed_in_login.clone() } });
    Html(html).into_response()
}
