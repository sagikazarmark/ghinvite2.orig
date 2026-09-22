use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

mod common;

use common::sign_in::{
    ACME_ADMIN, acme_installation, oauth_expectations, signed_in_app, unreachable_restate,
};

async fn build_signed_in_app_with_installation() -> (axum::Router, String) {
    signed_in_app(
        &[acme_installation()],
        oauth_expectations(ACME_ADMIN),
        unreachable_restate(),
    )
    .await
}

#[tokio::test]
async fn signed_in_admin_with_existing_installation_sees_public_home() {
    let (app, cookie) = build_signed_in_app_with_installation().await;
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("Invitation code"));
    assert!(text.contains("data-invitation-code-input=\"true\""));
    assert!(text.contains("data-open-invitation-code=\"true\""));
    assert!(text.contains("data-invitation-code-message=\"true\""));
    assert!(text.contains("<script src=\"/static/app.js\"></script>"));
    assert!(!text.contains("Enter an invitation code first."));
    assert!(!text.contains("Invitation Link Code"));
    assert!(text.contains("Create invitation link"));
    assert!(text.contains("href=\"/console\""));
    assert!(text.contains("Console"));
    assert!(text.contains("@octocat"));
    assert!(!text.contains("home-features"));
}
