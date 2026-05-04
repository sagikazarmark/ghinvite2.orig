//! `GET /` — home page (logged-out marketing or signed-in redirect).

use crate::session;
use crate::state::AppState;
use crate::views::home::HomePage;
use crate::views::render::render;
use axum::extract::State;
use axum::response::{Html, IntoResponse, Redirect};
use axum::routing::get;
use axum::Router;
use dioxus::prelude::*;
use tower_sessions::Session as TowerSession;

pub fn router() -> Router<AppState> {
    Router::new().route("/", get(home))
}

async fn home(State(_state): State<AppState>, tower: TowerSession) -> impl IntoResponse {
    let session = session::load(&tower).await.unwrap_or_default();

    // If signed in: per spec §11, "logged-in redirect to last account".
    // Plan 4 doesn't yet track the "last account" cookie — for now we let
    // the user pick from the install URL (until Plan 5 adds account-list
    // routing).
    if session.is_authenticated() {
        // Fall through — render the page with Sign Out + Install CTA.
    }

    let signed_in_login = if session.is_authenticated() {
        Some(session.login.clone())
    } else {
        None
    };

    let html = render(move || rsx! { HomePage { signed_in_login: signed_in_login.clone() } });
    Html(html).into_response()
}

// Suppress unused if Redirect isn't reached in Plan 4:
#[allow(dead_code)]
fn _suppress_unused(r: Redirect) -> Redirect {
    r
}
