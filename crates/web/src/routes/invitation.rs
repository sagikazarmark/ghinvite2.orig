//! `/i/{slug}...` routes. Plan 6: recipient flow.

use crate::session;
use crate::state::AppState;
use crate::views::render::render;
use axum::Router;
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use chrono::Utc;
use dioxus::prelude::*;
use tower_sessions::Session as TowerSession;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/i/{slug}", get(landing))
        .route(
            "/i/{slug}/request",
            get(request_form).post(submit_request),
        )
        .route("/i/{slug}/pending/{request_id}", get(pending))
}

async fn landing(
    State(state): State<AppState>,
    tower: TowerSession,
    axum::extract::Path(slug): axum::extract::Path<String>,
) -> impl IntoResponse {
    let now = Utc::now();
    let link = match state.storage.get_share_link_by_slug(&slug).await {
        Ok(Some(l)) if l.is_active(now) => l,
        _ => return crate::error::WebError::NotFound.into_response(),
    };
    let session = session::load(&tower).await.unwrap_or_default();
    let signed_in_login = if session.is_authenticated() {
        Some(session.login.clone())
    } else {
        None
    };
    let html = render(move || rsx! {
        crate::views::invitation::LandingPage {
            slug: slug.clone(),
            link: link.clone(),
            signed_in_login: signed_in_login.clone(),
            now,
        }
    });
    Html(html).into_response()
}

async fn request_form(
    State(_state): State<AppState>,
    _tower: TowerSession,
    axum::extract::Path(_slug): axum::extract::Path<String>,
) -> impl IntoResponse {
    (axum::http::StatusCode::NOT_IMPLEMENTED, "TODO Task 6")
}

async fn submit_request(
    State(_state): State<AppState>,
    _tower: TowerSession,
    axum::extract::Path(_slug): axum::extract::Path<String>,
    serde_qs::axum::QsForm(_form): serde_qs::axum::QsForm<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    (axum::http::StatusCode::NOT_IMPLEMENTED, "TODO Task 7")
}

async fn pending(
    State(_state): State<AppState>,
    _tower: TowerSession,
    axum::extract::Path(_path): axum::extract::Path<(String, String)>,
) -> impl IntoResponse {
    (axum::http::StatusCode::NOT_IMPLEMENTED, "TODO Task 8")
}
