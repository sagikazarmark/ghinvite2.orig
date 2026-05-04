//! End-to-end OAuth sign-in tests. Drives the full /login → /oauth/callback
//! flow through axum + the in-process MockTransport from Plan 2.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use github::mocks::{Expectation, MockTransport};
use github::transport::{Method, Response};
use http_body_util::BodyExt;
use std::collections::BTreeMap;
use std::sync::Arc;
use tower::ServiceExt;
use web::{AppState, RestateClient, WebConfig, build_app};

async fn build_app_with_mock(mock: MockTransport) -> axum::Router {
    let storage: Arc<dyn storage::Storage> = Arc::new(storage::SqlxStorage::in_memory().await.unwrap());
    let transport: Arc<dyn github::HttpTransport> = Arc::new(mock);
    let restate = Arc::new(RestateClient::new("http://127.0.0.1:8080").unwrap());
    let state = AppState::new(storage, transport, restate, WebConfig::for_local_dev());
    let session_store = tower_sessions::MemoryStore::default();
    build_app(state, session_store)
}

#[tokio::test]
async fn signin_happy_path() {
    let mock = MockTransport::scripted(vec![
        Expectation {
            method: Method::Post,
            url: "https://github.com/login/oauth/access_token".into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 200,
                headers: BTreeMap::new(),
                body: br#"{"access_token":"u_xxx","token_type":"bearer","scope":"read:user"}"#.to_vec(),
            },
        },
        Expectation::ok_json(
            Method::Get,
            "https://api.github.com/user",
            serde_json::json!({"id": 42, "login": "octocat"}),
        ),
    ]);
    let app = build_app_with_mock(mock).await;

    // Step 1: hit /login, capture the cookie + state.
    let resp1 = app
        .clone()
        .oneshot(Request::builder().uri("/login").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp1.status(), StatusCode::SEE_OTHER);
    let cookie = resp1
        .headers()
        .get("set-cookie")
        .expect("session cookie set")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let location = resp1.headers().get("location").unwrap().to_str().unwrap();
    let state_param = location
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();

    // Step 2: hit /oauth/callback with the same cookie and the captured state.
    let resp2 = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/oauth/callback?code=test-code&state={state_param}"))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::SEE_OTHER);
    let dest = resp2.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(dest, "/");
}

#[tokio::test]
async fn callback_csrf_mismatch_rejects() {
    let mock = MockTransport::scripted(vec![]); // no GitHub calls expected
    let app = build_app_with_mock(mock).await;
    // Hit /login first to create a session with a real CSRF.
    let resp1 = app
        .clone()
        .oneshot(Request::builder().uri("/login").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let cookie = resp1
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    // Then send a wrong state.
    let resp2 = app
        .oneshot(
            Request::builder()
                .uri("/oauth/callback?code=any&state=wrong-csrf")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::BAD_REQUEST);
    let body = resp2.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("CSRF"));
}

#[tokio::test]
async fn signin_with_installation_id_logs_and_proceeds() {
    let mock = MockTransport::scripted(vec![
        Expectation {
            method: Method::Post,
            url: "https://github.com/login/oauth/access_token".into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 200,
                headers: BTreeMap::new(),
                body: br#"{"access_token":"u_xxx","token_type":"bearer","scope":"read:user"}"#.to_vec(),
            },
        },
        Expectation::ok_json(
            Method::Get,
            "https://api.github.com/user",
            serde_json::json!({"id": 42, "login": "octocat"}),
        ),
    ]);
    let app = build_app_with_mock(mock).await;

    let resp1 = app
        .clone()
        .oneshot(Request::builder().uri("/login").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let cookie = resp1
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let location = resp1.headers().get("location").unwrap().to_str().unwrap();
    let state_param = location
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();

    let resp2 = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/oauth/callback?code=test-code&state={state_param}&installation_id=99&setup_action=install"
                ))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::SEE_OTHER);
}
