//! Web binary core. axum app builder + tower-sessions + OAuth flow + Dioxus
//! layouts. Plan 7 wraps `build_app()` in a Workers `#[event(fetch)]`; Plans 5
//! and 6 fill in the dashboard and recipient routes.

pub(crate) mod account_admin_reads;
pub mod commands;
pub mod config;
pub mod error;
pub mod middleware;
pub mod restate_client;
pub mod routes;
pub mod session;
pub(crate) mod share_link_resolution;
pub mod state;
pub mod views;
pub mod wasm_compat;

// Re-exports filled in as types appear:
pub use commands::{GhinviteCommands, RestateCommands};
pub use config::WebConfig;
pub use error::{Result, WebError};
pub use restate_client::RestateClient;
pub use state::AppState;

use axum::Router;

/// Build the axum app with all routes registered. The session store is
/// supplied externally so Plan 7 can swap in a D1-backed store; for native
/// dev pass `tower_sessions_sqlx_store::SqliteStore`.
///
/// `state` carries storage, github transport, command facade, and config.
pub fn build_app<S>(state: AppState, session_store: S) -> Router
where
    S: tower_sessions::SessionStore + Clone + 'static,
{
    use tower_sessions::{Expiry, SessionManagerLayer};

    let session_layer = SessionManagerLayer::new(session_store)
        .with_secure(state.config.cookie_secure)
        .with_http_only(true)
        .with_same_site(tower_sessions::cookie::SameSite::Lax)
        .with_expiry(Expiry::OnInactivity(
            tower_sessions::cookie::time::Duration::days(30),
        ));

    Router::new()
        .merge(routes::health::router())
        .merge(routes::home::router())
        .merge(routes::oauth::router())
        .merge(routes::setup::router())
        .merge(routes::dashboard::router())
        .merge(routes::invitation::router())
        .merge(routes::webhook::router())
        .route("/static/styles.css", axum::routing::get(serve_styles_css))
        .layer(session_layer)
        .with_state(state)
}

async fn serve_styles_css() -> impl axum::response::IntoResponse {
    use axum::http::header;
    use axum::response::IntoResponse;
    let css = include_str!("../assets/styles.built.css");
    ([(header::CONTENT_TYPE, "text/css")], css).into_response()
}
