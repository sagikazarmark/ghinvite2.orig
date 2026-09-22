use axum::body::Body;
use axum::http::{Request, StatusCode};
use ghinvite_github::mocks::{Expectation, MockTransport};
use ghinvite_github::transport::{Method, Response};
use ghinvite_web::{AppState, WebConfig, build_app};
use std::collections::BTreeMap;
use std::sync::Arc;
use tower::ServiceExt;

mod common;

use common::restate_recorder::RestateRecorder;

async fn build_app_with(mock: MockTransport) -> (axum::Router, RestateRecorder) {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let transport: Arc<dyn ghinvite_github::HttpTransport> = Arc::new(mock);
    let restate = RestateRecorder::start().await;
    let state = AppState::new(
        storage,
        transport,
        restate.client(),
        WebConfig::for_local_dev_with_secret([7; 32]),
    );
    let session_store = tower_sessions::MemoryStore::default();
    (build_app(state, session_store), restate)
}

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

async fn sign_in(app: axum::Router) -> (axum::Router, String) {
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

fn oauth_expectations(login: &str, user_id: u64) -> Vec<Expectation> {
    vec![
        Expectation {
            method: Method::Post,
            url: "https://github.com/login/oauth/access_token".into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 200,
                headers: BTreeMap::new(),
                body: br#"{"access_token":"u_xxx","token_type":"bearer","scope":"read:user read:org"}"#.to_vec(),
            },
        },
        Expectation::ok_json(
            Method::Get,
            "https://api.github.com/user",
            serde_json::json!({"id": user_id, "login": login}),
        ),
    ]
}

#[tokio::test]
async fn setup_unauthenticated_redirects_to_login_with_return_to() {
    let (app, _restate) = build_app_with(MockTransport::scripted(vec![])).await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/setup/github?installation_id=77&setup_action=install")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(
        location,
        "/login?return_to=%2Fsetup%2Fgithub%3Finstallation_id%3D77%26setup_action%3Dinstall"
    );
}

#[tokio::test]
async fn setup_rejects_spoofed_installation_id() {
    let mut expectations = oauth_expectations("octocat", 42);
    expectations.push(Expectation::ok_json(
        Method::Get,
        "https://api.github.com/user/installations?per_page=100",
        serde_json::json!({
            "total_count": 1,
            "installations": [{
                "id": 77,
                "account": {"id": 9001, "login": "acme", "type": "Organization"},
                "repository_selection": "all",
                "target_type": "Organization",
                "target_id": 9001
            }]
        }),
    ));
    let (app, restate) = build_app_with(MockTransport::scripted(expectations)).await;
    let (app, cookie) = sign_in(app).await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/setup/github?installation_id=999&setup_action=install")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(restate.calls().is_empty());
}

#[tokio::test]
async fn setup_verified_org_install_calls_onboard_and_redirects() {
    let mut expectations = oauth_expectations("octocat", 42);
    expectations.push(Expectation::ok_json(
        Method::Get,
        "https://api.github.com/user/installations?per_page=100",
        serde_json::json!({
            "total_count": 1,
            "installations": [{
                "id": 77,
                "account": {"id": 9001, "login": "acme", "type": "Organization"},
                "repository_selection": "selected",
                "target_type": "Organization",
                "target_id": 9001
            }]
        }),
    ));
    expectations.push(Expectation::ok_json(
        Method::Get,
        "https://api.github.com/user/installations/77/repositories?per_page=100",
        serde_json::json!({
            "total_count": 2,
            "repositories": [
                {"id": 10, "full_name": "acme/api", "private": true},
                {"id": 11, "full_name": "acme/web", "private": false}
            ]
        }),
    ));
    let (app, restate) = build_app_with(MockTransport::scripted(expectations)).await;
    let (app, cookie) = sign_in(app).await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/setup/github?installation_id=77&setup_action=install")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp.headers().get("location").unwrap().to_str().unwrap(),
        "/console/accounts/acme"
    );

    let calls = restate.calls();
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    assert_eq!(call.service, "Installation");
    assert_eq!(call.key, "77");
    assert_eq!(call.method, "onboard");
    assert!(!call.send);
    assert_eq!(call.body["installation_id"], 77);
    assert_eq!(call.body["actor_user_id"], 42);
    assert_eq!(call.body["account_id"], 9001);
    assert_eq!(call.body["account_login"], "acme");
    assert_eq!(call.body["account_type"], "Organization");
    assert_eq!(call.body["selected_repos"], serde_json::json!([10, 11]));
}

#[tokio::test]
async fn setup_verified_user_install_calls_onboard_and_redirects() {
    let mut expectations = oauth_expectations("octocat", 42);
    expectations.push(Expectation::ok_json(
        Method::Get,
        "https://api.github.com/user/installations?per_page=100",
        serde_json::json!({
            "total_count": 1,
            "installations": [{
                "id": 88,
                "account": {"id": 42, "login": "octocat", "type": "User"},
                "repository_selection": "all",
                "target_type": "User",
                "target_id": 42
            }]
        }),
    ));
    let (app, restate) = build_app_with(MockTransport::scripted(expectations)).await;
    let (app, cookie) = sign_in(app).await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/setup/github?installation_id=88&setup_action=install")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp.headers().get("location").unwrap().to_str().unwrap(),
        "/console/accounts/octocat"
    );

    let calls = restate.calls();
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    assert_eq!(call.service, "Installation");
    assert_eq!(call.key, "88");
    assert_eq!(call.method, "onboard");
    assert!(!call.send);
    assert_eq!(call.body["installation_id"], 88);
    assert_eq!(call.body["actor_user_id"], 42);
    assert_eq!(call.body["account_id"], 42);
    assert_eq!(call.body["account_login"], "octocat");
    assert_eq!(call.body["account_type"], "User");
    assert_eq!(call.body["selected_repos"], "all");
}

#[tokio::test]
async fn setup_update_calls_repos_changed_and_redirects() {
    let mut expectations = oauth_expectations("octocat", 42);
    expectations.push(Expectation::ok_json(
        Method::Get,
        "https://api.github.com/user/installations?per_page=100",
        serde_json::json!({
            "total_count": 1,
            "installations": [{
                "id": 77,
                "account": {"id": 9001, "login": "acme", "type": "Organization"},
                "repository_selection": "selected",
                "target_type": "Organization",
                "target_id": 9001
            }]
        }),
    ));
    expectations.push(Expectation::ok_json(
        Method::Get,
        "https://api.github.com/user/installations/77/repositories?per_page=100",
        serde_json::json!({
            "total_count": 1,
            "repositories": [
                {"id": 12, "full_name": "acme/new", "private": true}
            ]
        }),
    ));
    let (app, restate) = build_app_with(MockTransport::scripted(expectations)).await;
    let (app, cookie) = sign_in(app).await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/setup/github?installation_id=77&setup_action=update")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp.headers().get("location").unwrap().to_str().unwrap(),
        "/console/accounts/acme"
    );

    let calls = restate.calls();
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    assert_eq!(call.service, "Installation");
    assert_eq!(call.key, "77");
    assert_eq!(call.method, "repos_changed");
    // A setup return waits for the change, where a webhook would only send it.
    assert!(!call.send);
    assert_eq!(call.body["installation_id"], 77);
    assert_eq!(call.body["selected_repos"], serde_json::json!([12]));
}
