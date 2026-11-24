//! Web binary core. axum app builder + tower-sessions + OAuth flow + Dioxus
//! layouts. `ghinvite-web-worker` serves `build_app()` on Cloudflare Workers.

pub(crate) mod attempt_continuations;
pub mod continuation_rotation;
#[cfg(feature = "runtime-tests")]
pub use attempt_continuations::verify_continuation_envelope;
mod admission_telemetry;
pub mod authority;
pub use authority::bulk_review;
pub mod authority_cache;
pub mod bulk_authority;
pub mod bulk_submission;
pub mod commands;
pub mod config;
pub mod delivery_authority;
pub mod error;
pub(crate) mod forms;
#[cfg(test)]
mod golden;
mod installed_apps;
pub mod link_authority;
pub(crate) mod link_secret;
#[doc(hidden)]
pub mod link_secret_entropy;
pub mod middleware;
pub mod rate_limit;
pub mod render;
pub mod request_authority;
pub mod restate_client;
pub mod routes;
pub mod session;
pub mod session_store;
pub mod signin;
pub mod state;
pub mod sweep_watchdog;
pub mod wasm_compat;
mod webhook_events;

// Re-exports filled in as types appear:
pub use commands::RestateCommands;
pub use config::WebConfig;
pub use error::{IngressFailure, OAuthFailure, Result, WebError};
pub use link_authority::{AuthorityError, LinkAuthority};
pub use restate_client::RestateClient;
pub use state::{AppState, WebGithub, WebStorage};

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

    // The sign-in bounce's final hop is GitHub's web origin as configured.
    let policy = middleware::csp::policy(&state.config.github);
    let session_layer = SessionManagerLayer::new(session_store)
        .with_secure(state.config.cookie_secure)
        .with_http_only(true)
        .with_same_site(tower_sessions::cookie::SameSite::Lax)
        .with_expiry(Expiry::OnInactivity(
            tower_sessions::cookie::time::Duration::days(30),
        ));

    let router = Router::new()
        .route("/console/keyring", axum::routing::get(keyring_fingerprint))
        .merge(routes::operations::router())
        .merge(routes::continuation_rotation::router())
        .merge(routes::health::router())
        .merge(routes::home::router())
        .merge(routes::oauth::router())
        .merge(routes::setup::router())
        .merge(routes::onboarding::router())
        .merge(routes::console::router())
        .merge(routes::invitation::router())
        .merge(routes::webhook::router())
        .route("/static/styles.css", axum::routing::get(serve_styles_css))
        .route("/static/app.js", axum::routing::get(serve_app_js))
        .route("/static/i.js", axum::routing::get(serve_i_js))
        .route(
            "/robots.txt",
            axum::routing::get(middleware::security::robots),
        );

    let router = match extra {
        Some(extra) => router.merge(extra),
        None => router,
    };

    router
        .fallback(routes::not_found::public)
        .layer(middleware::csp::layer(policy.clone()))
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
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            rate_limit::enforce,
        ))
        .layer(axum::middleware::from_fn(
            session_store::require_readable_session,
        ))
        .layer(session_layer)
        .layer(axum::middleware::from_fn_with_state(
            policy,
            middleware::security::headers,
        ))
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

async fn serve_i_js() -> impl axum::response::IntoResponse {
    static_asset(
        "text/javascript; charset=utf-8",
        include_str!("../assets/i.js"),
    )
}

/// Operator IDs are numeric identities, independent of organization authority.
async fn keyring_fingerprint(
    axum::extract::State(state): axum::extract::State<AppState>,
    tower: tower_sessions::Session,
) -> Result<impl axum::response::IntoResponse> {
    let session = session::load(&tower)
        .await
        .map_err(|_| WebError::Session("session unavailable".into()))?;
    if !session.is_authenticated() || !state.config.operator_ids.contains(&session.user_id) {
        return Err(WebError::NotFound);
    }
    Ok((
        [(axum::http::header::CACHE_CONTROL, "private, no-store")],
        state.config.keyring.fingerprint().to_string(),
    ))
}

mod organization_delivery;

pub mod personal_link;

pub mod email_rows;
