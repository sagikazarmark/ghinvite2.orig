pub mod authority_http_fixture;
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
