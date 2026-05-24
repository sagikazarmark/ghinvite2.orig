use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use domain::{Account, AccountType, SelectedRepos};
use github::mocks::{Expectation, MockTransport};
use github::transport::{Method, Response};
use http_body_util::BodyExt;
use std::collections::BTreeMap;
use std::sync::Arc;
use storage::Storage;
use tower::ServiceExt;
use web::{AppState, RestateClient, RestateCommands, WebConfig, build_app};

fn session_cookie(resp: &axum::response::Response, fallback: Option<String>) -> String {
    resp.headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string())
        .or(fallback)
        .expect("session cookie available")
}

fn state_from_location(location: &str) -> &str {
    location
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
}

fn oauth_expectations() -> Vec<Expectation> {
    vec![
        Expectation {
            method: Method::Post,
            url: "https://github.com/login/oauth/access_token".into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 200,
                headers: BTreeMap::new(),
                body: br#"{"access_token":"u_xxx","token_type":"bearer","scope":"read:user read:org"}"#
                    .to_vec(),
            },
        },
        Expectation::ok_json(
            Method::Get,
            "https://api.github.com/user",
            serde_json::json!({"id": 42, "login": "octocat"}),
        ),
        Expectation::ok_json(
            Method::Get,
            "https://api.github.com/user/memberships/orgs/acme",
            serde_json::json!({"role": "admin", "state": "active"}),
        ),
    ]
}

async fn build_signed_in_app_with_installation() -> (axum::Router, String) {
    let storage = Arc::new(storage::SqlxStorage::in_memory().await.unwrap());
    storage
        .insert_installation(&Account {
            installation_id: 77,
            account_id: 9001,
            account_login: "acme".into(),
            account_type: AccountType::Organization,
            installed_at: Utc::now(),
            uninstalled_at: None,
            selected_repos: SelectedRepos::All,
        })
        .await
        .unwrap();

    let storage: Arc<dyn storage::Storage> = storage;
    let transport: Arc<dyn github::HttpTransport> =
        Arc::new(MockTransport::scripted(oauth_expectations()));
    let restate = Arc::new(RestateClient::new("http://127.0.0.1:8080").unwrap());
    let commands = Arc::new(RestateCommands::new(restate));
    let state = AppState::new(storage, transport, commands, WebConfig::for_local_dev());
    let session_store = tower_sessions::MemoryStore::default();
    let app = build_app(state, session_store);

    let resp1 = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp1.status(), StatusCode::SEE_OTHER);
    let cookie1 = session_cookie(&resp1, None);
    let location = resp1.headers().get("location").unwrap().to_str().unwrap();
    let state = state_from_location(location);

    let resp2 = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/oauth/callback?code=test-code&state={state}"))
                .header("cookie", &cookie1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::SEE_OTHER);
    let cookie2 = session_cookie(&resp2, Some(cookie1));

    (app, cookie2)
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
    assert!(text.contains("Share Link Code"));
    assert!(text.contains("Create invitation link"));
    assert!(text.contains("href=\"/console\""));
    assert!(text.contains("Console"));
    assert!(text.contains("@octocat"));
    assert!(!text.contains("home-features"));
}
