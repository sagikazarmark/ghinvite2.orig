//! Route-existence smoke tests. Each route from spec §11 must be registered
//! and respond with a non-500. Some return 501 (Plans 5–6 fill them in);
//! some return 200/302; auth-required ones return 302→/login.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;
use web::{AppState, RestateClient, WebConfig, build_app};

async fn build_test_app() -> axum::Router {
    use github::mocks::MockTransport;
    let storage: Arc<dyn storage::Storage> = Arc::new(storage::SqlxStorage::in_memory().await.unwrap());
    let transport: Arc<dyn github::HttpTransport> = Arc::new(MockTransport::scripted(vec![]));
    let restate = Arc::new(RestateClient::new("http://127.0.0.1:8080").unwrap());
    let state = AppState::new(storage, transport, restate, WebConfig::for_local_dev());
    let session_store = tower_sessions::MemoryStore::default();
    build_app(state, session_store)
}

#[tokio::test]
async fn health_returns_ok() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"ok");
}

#[tokio::test]
async fn login_redirects_to_github_authorize() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(Request::builder().uri("/login").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert!(location.starts_with("https://github.com/login/oauth/authorize?"));
    assert!(location.contains("client_id="));
    assert!(location.contains("state="));
}

#[tokio::test]
async fn install_redirects_to_install_url() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(Request::builder().uri("/install").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert!(location.starts_with("https://github.com/apps/"));
}

#[tokio::test]
async fn logout_clears_session_and_redirects_home() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(Request::builder().uri("/logout").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(location, "/");
}

#[tokio::test]
async fn static_styles_returns_css() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(Request::builder().uri("/static/styles.css").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp.headers().get("content-type").unwrap().to_str().unwrap();
    assert!(ct.contains("text/css"));
}
