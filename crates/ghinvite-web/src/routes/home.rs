//! `GET /` — public home page.

use crate::session;
use crate::state::AppState;
use crate::views::home::HomePage;
use crate::views::render::render_with_csrf;
use axum::Router;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use dioxus::prelude::*;
use tower_sessions::Session as TowerSession;

pub fn router() -> Router<AppState> {
    Router::new().route("/", get(home))
}

async fn home(tower: TowerSession) -> impl IntoResponse {
    let session = match session::load(&tower).await {
        Ok(session) => session,
        Err(error) => return crate::WebError::Session(error.to_string()).into_response(),
    };
    let signed_in_login = if session.is_authenticated() {
        Some(session.login.clone())
    } else {
        None
    };

    let html = render_with_csrf(
        session.csrf_token,
        move || rsx! { HomePage { signed_in_login: signed_in_login.clone() } },
    );
    Html(html).into_response()
}
