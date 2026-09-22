//! Web binary core. axum app builder + tower-sessions + OAuth flow + Dioxus
//! layouts. `ghinvite-web-worker` serves `build_app()` on Cloudflare Workers.

pub(crate) mod attempt_continuations;
pub mod commands;
pub mod config;
pub mod error;
pub(crate) mod forms;
pub mod link_authority;
pub mod middleware;
pub mod render;
pub mod restate_client;
pub mod routes;
pub mod session;
pub mod session_store;
pub mod state;
pub mod wasm_compat;

// Re-exports filled in as types appear:
pub use commands::RestateCommands;
pub use config::WebConfig;
pub use error::{IngressFailure, OAuthFailure, Result, WebError};
pub use link_authority::{AuthorityError, LinkAuthority};
pub use restate_client::RestateClient;
pub use state::{AppState, WebStorage};

use axum::Router;

/// Build the axum app with all routes registered. The session store is
/// supplied externally. Native and Worker entry points pass a `ProtectedStore`
/// over SQLite and KV respectively; tests can inject failure stores.
///
/// `state` carries storage, github transport, command facade, and config.
pub fn build_app<S>(state: AppState, session_store: S) -> Router
where
    S: tower_sessions::SessionStore + Clone + 'static,
{
    build_app_with(state, session_store, None)
}

/// [`build_app`] plus an optional extra router merged in before the shared
/// layers, so whatever it mounts is covered by CSP, cache-control and the
/// session layer just like a built-in route.
///
/// Its one caller is the native server's `/assets` mount
/// (`ghinvite_web_server::island_assets`): serving files off the filesystem is
/// a property of that deployment, not of the app, and keeping it out of here
/// keeps `tower-http/fs` — and the tokio filesystem support behind it — out of
/// the Workers build.
pub fn build_app_with<S>(
    state: AppState,
    session_store: S,
    extra: Option<Router<AppState>>,
) -> Router
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

    let router = Router::new()
        .merge(routes::health::router())
        .merge(routes::home::router())
        .merge(routes::oauth::router())
        .merge(routes::setup::router())
        .merge(routes::console::router())
        .merge(routes::invitation::router())
        .merge(routes::webhook::router())
        .route("/static/styles.css", axum::routing::get(serve_styles_css))
        .route("/static/app.js", axum::routing::get(serve_app_js));

    let router = match extra {
        Some(extra) => router.merge(extra),
        None => router,
    };

    router
        .fallback(routes::not_found::public)
        .layer(middleware::csp::layer())
        // Every rendered page can contain logout authority, including errors.
        .layer(tower_http::set_header::SetResponseHeaderLayer::overriding(
            axum::http::header::CACHE_CONTROL,
            |response: &axum::http::Response<axum::body::Body>| {
                response
                    .headers()
                    .get(axum::http::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| value.starts_with("text/html"))
                    .then(|| axum::http::HeaderValue::from_static("private, no-store"))
            },
        ))
        .layer(session_layer)
        .with_state(state)
}

/// Built Tailwind/DaisyUI stylesheet (`npm run build:css`), embedded at
/// compile time.
async fn serve_styles_css() -> impl axum::response::IntoResponse {
    static_asset("text/css", include_str!("../assets/styles.built.css"))
}

/// The one shared client script. Loaded synchronously from every layout's
/// `<head>`; the CSP forbids inline `<script>` blocks, so all client behaviour
/// goes here (see `crates/ghinvite-web/README.md`).
async fn serve_app_js() -> impl axum::response::IntoResponse {
    static_asset(
        "text/javascript; charset=utf-8",
        include_str!("../assets/app.js"),
    )
}

fn static_asset(content_type: &'static str, body: &'static str) -> axum::response::Response {
    use axum::http::header;
    use axum::response::IntoResponse;
    ([(header::CONTENT_TYPE, content_type)], body).into_response()
}
