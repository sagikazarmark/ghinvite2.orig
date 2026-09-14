//! Web binary core. axum app builder + tower-sessions + OAuth flow + Dioxus
//! layouts. Plan 7 wraps `build_app()` in a Workers `#[event(fetch)]`; Plans 5
//! and 6 fill in the console and recipient routes.

pub(crate) mod account_admin_reads;
pub mod commands;
pub mod config;
pub mod error;
pub(crate) mod forms;
pub(crate) mod invitation_link_resolution;
pub mod lifecycle;
pub mod middleware;
pub mod restate_client;
pub mod routes;
pub mod session;
pub mod session_store;
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
/// supplied externally. Native and Worker entry points pass a `ProtectedStore`
/// over SQLite and KV respectively; tests can inject failure stores.
///
/// `state` carries storage, github transport, command facade, and config.
pub fn build_app<S>(mut state: AppState, session_store: S) -> Router
where
    S: tower_sessions::SessionStore + Clone + 'static,
{
    use tower_sessions::{Expiry, SessionManagerLayer};
    state.attempt_store = Some(std::sync::Arc::new(session_store.clone()));

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

    let router = island_assets(router, &state.config);

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

/// Serve the Dioxus island bundle under `/assets/*` from
/// `WebConfig::island_assets_dir` — native builds only. A missing file is a
/// plain 404, which is what the new-link page's `<script type="module">`
/// relies on to degrade to the server-rendered form when no bundle is built.
///
/// On Workers, Cloudflare Static Assets answer `/assets/*` before the Worker
/// is invoked (`[assets]` in `wrangler/web.toml`), so no route exists there
/// and `tower-http/fs` is not compiled in.
#[cfg(not(target_arch = "wasm32"))]
fn island_assets(router: Router<AppState>, config: &WebConfig) -> Router<AppState> {
    match &config.island_assets_dir {
        Some(dir) => router.nest_service("/assets", tower_http::services::ServeDir::new(dir)),
        None => router,
    }
}

#[cfg(target_arch = "wasm32")]
fn island_assets(router: Router<AppState>, _config: &WebConfig) -> Router<AppState> {
    router
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
pub mod admission;
