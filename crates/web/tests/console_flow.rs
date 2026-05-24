//! Console route integration tests.

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

fn oauth_expectations_without_membership() -> Vec<Expectation> {
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
    ]
}

fn oauth_sign_in_expectations() -> Vec<Expectation> {
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
    ]
}

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

async fn build_signed_in_admin_app() -> (axum::Router, String) {
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

async fn build_signed_in_admin_app_with_console_installations(
    logins: Vec<&str>,
) -> (axum::Router, String) {
    let storage = Arc::new(storage::SqlxStorage::in_memory().await.unwrap());
    for (index, login) in logins.iter().enumerate() {
        storage
            .insert_installation(&Account {
                installation_id: 77 + index as u64,
                account_id: 9001 + index as u64,
                account_login: (*login).into(),
                account_type: if *login == "octocat" {
                    AccountType::User
                } else {
                    AccountType::Organization
                },
                installed_at: Utc::now(),
                uninstalled_at: None,
                selected_repos: SelectedRepos::All,
            })
            .await
            .unwrap();
    }

    let mut expectations = oauth_sign_in_expectations();
    let installations: Vec<_> = logins
        .iter()
        .enumerate()
        .map(|(index, login)| {
            let target_type = if *login == "octocat" {
                "User"
            } else {
                "Organization"
            };
            serde_json::json!({
                "id": 77 + index as u64,
                "account": {"id": 9001 + index as u64, "login": login, "type": target_type},
                "repository_selection": "all",
                "target_type": target_type,
                "target_id": 9001 + index as u64
            })
        })
        .collect();
    expectations.push(Expectation::ok_json(
        Method::Get,
        "https://api.github.com/user/installations?per_page=100",
        serde_json::json!({
            "total_count": installations.len(),
            "installations": installations
        }),
    ));
    for login in logins.iter().filter(|login| **login != "octocat") {
        expectations.push(Expectation::ok_json(
            Method::Get,
            format!("https://api.github.com/user/memberships/orgs/{login}"),
            serde_json::json!({"role": "admin", "state": "active"}),
        ));
    }

    let storage: Arc<dyn storage::Storage> = storage;
    let transport: Arc<dyn github::HttpTransport> = Arc::new(MockTransport::scripted(expectations));
    let restate = Arc::new(RestateClient::new("http://127.0.0.1:8080").unwrap());
    let commands = Arc::new(RestateCommands::new(restate));
    let state = AppState::new(storage, transport, commands, WebConfig::for_local_dev());
    let session_store = tower_sessions::MemoryStore::default();
    let app = build_app(state, session_store);

    sign_in(app).await
}

async fn build_signed_in_app_with_failed_installation_discovery() -> (axum::Router, String) {
    let storage: Arc<dyn storage::Storage> =
        Arc::new(storage::SqlxStorage::in_memory().await.unwrap());
    let mut expectations = oauth_sign_in_expectations();
    expectations.push(Expectation {
        method: Method::Get,
        url: "https://api.github.com/user/installations?per_page=100".into(),
        required_headers: BTreeMap::new(),
        expected_body: None,
        response: Response {
            status: 500,
            headers: BTreeMap::new(),
            body: b"server error".to_vec(),
        },
    });
    let transport: Arc<dyn github::HttpTransport> = Arc::new(MockTransport::scripted(expectations));
    let restate = Arc::new(RestateClient::new("http://127.0.0.1:8080").unwrap());
    let commands = Arc::new(RestateCommands::new(restate));
    let state = AppState::new(storage, transport, commands, WebConfig::for_local_dev());
    let session_store = tower_sessions::MemoryStore::default();
    let app = build_app(state, session_store);

    sign_in(app).await
}

async fn build_signed_in_app_without_installation() -> (axum::Router, String) {
    let storage: Arc<dyn storage::Storage> =
        Arc::new(storage::SqlxStorage::in_memory().await.unwrap());
    let mut expectations = oauth_expectations_without_membership();
    expectations.push(Expectation::ok_json(
        Method::Get,
        "https://api.github.com/user/installations?per_page=100",
        serde_json::json!({"total_count": 0, "installations": []}),
    ));
    let transport: Arc<dyn github::HttpTransport> = Arc::new(MockTransport::scripted(expectations));
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
async fn console_index_unauthenticated_redirects_to_login() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/console")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(location, "/login?return_to=%2Fconsole");
}

#[tokio::test]
async fn console_index_redirects_when_one_admin_account() {
    let (app, cookie) = build_signed_in_admin_app_with_console_installations(vec!["acme"]).await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/console")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(location, "/console/accounts/acme");
}

#[tokio::test]
async fn console_index_renders_picker_for_multiple_admin_accounts() {
    let (app, cookie) =
        build_signed_in_admin_app_with_console_installations(vec!["octocat", "acme"]).await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/console")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("Choose an account"));
    assert!(
        text.find("/console/accounts/acme").unwrap()
            < text.find("/console/accounts/octocat").unwrap()
    );
}

#[tokio::test]
async fn console_index_renders_empty_state_for_no_admin_accounts() {
    let (app, cookie) = build_signed_in_app_without_installation().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/console")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("No accounts connected"));
    assert!(text.contains("Install GitHub App"));
}

#[tokio::test]
async fn console_index_renders_error_when_github_installations_fail() {
    let (app, cookie) = build_signed_in_app_with_failed_installation_discovery().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/console")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("We couldn't load your accounts"));
    assert!(text.contains("Try again"));
}

#[tokio::test]
async fn console_overview_unauthenticated_redirects_to_login() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/console/accounts/acme")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(location, "/login?return_to=%2Fconsole%2Faccounts%2Facme");
}

#[tokio::test]
async fn console_account_route_unauthenticated_redirects_to_login() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/console/accounts/acme/audit")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(
        location,
        "/login?return_to=%2Fconsole%2Faccounts%2Facme%2Faudit"
    );
}

#[tokio::test]
async fn console_unknown_route_unauthenticated_redirects_to_login() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/console/accounts/acme/missing")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(
        location,
        "/login?return_to=%2Fconsole%2Faccounts%2Facme%2Fmissing"
    );
}

#[tokio::test]
async fn console_trailing_slash_unauthenticated_redirects_to_login() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/console/accounts/acme/")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(location, "/login?return_to=%2Fconsole%2Faccounts%2Facme%2F");
}

#[tokio::test]
async fn console_unknown_account_for_signed_in_user_renders_generic_404() {
    let (app, cookie) = build_signed_in_app_without_installation().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/console/accounts/acme")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("Page not found"));
    assert!(text.contains("app-header"));
    assert!(text.contains("@octocat"));
    assert!(!text.contains("console-frame"));
    assert!(!text.contains("acme"));
}

#[tokio::test]
async fn console_unknown_route_for_admin_renders_console_404() {
    let (app, cookie) = build_signed_in_admin_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/console/accounts/acme/missing")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("console-frame"));
    assert!(text.contains("Page not found"));
    assert!(text.contains("This console page is not available."));
    assert!(text.contains("Go to account overview"));
    assert!(text.contains("href=\"/console/accounts/acme\""));
    assert!(!text.contains("app-nav-row-active"));
}

#[tokio::test]
async fn console_trailing_slash_for_admin_renders_console_404() {
    let (app, cookie) = build_signed_in_admin_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/console/accounts/acme/")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("console-frame"));
    assert!(text.contains("Page not found"));
    assert!(text.contains("This console page is not available."));
    assert!(text.contains("Go to account overview"));
    assert!(!text.contains("app-nav-row-active"));
}

#[tokio::test]
async fn console_audit_page_for_admin_renders_coming_soon_state() {
    let (app, cookie) = build_signed_in_admin_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/console/accounts/acme/audit")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("console-frame"));
    assert!(text.contains("Audit log coming soon"));
    assert!(text.contains("ghinvite records account activity for share links, requests, and GitHub invitations. Console browsing is not available yet."));
    assert!(text.contains("Back to overview"));
    assert!(text.contains("href=\"/console/accounts/acme\""));
    assert!(text.contains("href=\"/console/accounts/acme/audit\""));
    assert!(text.contains("aria-current=\"page\""));
    assert!(text.contains("app-nav-row-active"));
    assert!(!text.contains("Audit log is not yet implemented."));
    assert!(!text.contains("<table"));
    assert!(!text.contains("Filter"));
    assert!(!text.contains("No events"));
}

#[tokio::test]
async fn console_unknown_post_route_stays_plain_404() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/console/accounts/acme/missing")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"Not Found");
}

#[tokio::test]
async fn console_post_missing_resource_stays_plain_404() {
    let (app, cookie) = build_signed_in_admin_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/console/accounts/acme/links/not-a-link-id/revoke")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"Not Found");
}
