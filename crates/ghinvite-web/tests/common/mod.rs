pub mod apps;
pub mod authority_http_fixture;
pub mod browser_fixture;
pub mod capability;
pub mod restate_recorder;
pub mod sign_in;

use axum::{body::Body, http::Request};
use http_body_util::BodyExt;
use tower::ServiceExt;

/// Obtain authority from a real native form, never from session-store internals.
#[allow(dead_code)] // Not every test binary that shares these helpers posts forms.
pub async fn csrf_token(app: &axum::Router, cookie: &str) -> String {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let html = String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    html.split("name=\"csrf_token\" value=\"")
        .nth(1)
        .expect("native CSRF field")
        .split('"')
        .next()
        .unwrap()
        .to_owned()
}

/// Runtime-equivalent throttles with an explicit trusted socket peer. Tests do
/// not implicitly disable production throttles or trust forwarding headers.
/// Without a sign-in app of its own, the app signs in against its scripted
/// GitHub (`state.github_transport`) through a [`sign_in::signin_bridge`].
#[allow(dead_code)]
pub fn build_app<S: tower_sessions::SessionStore + Clone + 'static>(
    state: ghinvite_web::AppState,
    store: S,
) -> axum::Router {
    build_app_with(state, store, None)
}
#[allow(dead_code)]
pub fn build_app_with<S: tower_sessions::SessionStore + Clone + 'static>(
    state: ghinvite_web::AppState,
    store: S,
    extra: Option<axum::Router<ghinvite_web::AppState>>,
) -> axum::Router {
    ghinvite_web::build_app_with(
        sign_in::with_signin_bridge(state)
            .with_authority_cache(std::sync::Arc::new(
                ghinvite_web::authority_cache::MemoryAuthorityCache::default(),
            ))
            .with_edge_limits(std::sync::Arc::new(
                ghinvite_web::rate_limit::MemoryRateLimiter::default(),
            )),
        store,
        extra,
    )
    .layer(axum::Extension(
        ghinvite_web::rate_limit::TrustedClientIp::new(std::net::Ipv4Addr::LOCALHOST.into()),
    ))
}
