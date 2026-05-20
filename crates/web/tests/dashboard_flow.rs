//! Dashboard route integration tests.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;
use web::{AppState, RestateClient, RestateCommands, WebConfig, build_app};

async fn build_test_app() -> axum::Router {
    use github::mocks::MockTransport;
    let storage: Arc<dyn storage::Storage> =
        Arc::new(storage::SqlxStorage::in_memory().await.unwrap());
    let transport: Arc<dyn github::HttpTransport> = Arc::new(MockTransport::scripted(vec![]));
    let restate = Arc::new(RestateClient::new("http://127.0.0.1:8080").unwrap());
    let commands = Arc::new(RestateCommands::new(restate));
    let state = AppState::new(storage, transport, commands, WebConfig::for_local_dev());
    let session_store = tower_sessions::MemoryStore::default();
    build_app(state, session_store)
}

#[tokio::test]
async fn dashboard_overview_unauthenticated_returns_404() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/accounts/acme")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // No session cookie → unauthenticated → RequireAdminOf returns NotFound.
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"Not Found");
}
