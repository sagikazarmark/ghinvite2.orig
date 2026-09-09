//! Console route integration tests.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, Duration, Utc};
use ghinvite_core::storage::Storage;
use ghinvite_core::{Account, AccountType, SelectedRepos};
use ghinvite_github::mocks::{Expectation, MockTransport};
use ghinvite_github::transport::{Method, Response};
use ghinvite_web::commands::{
    CreateInvitationLink, CreateInvitationLinkOutput, DecideInvitationRequest, GhinviteCommands,
    OnboardInstallation, RecordInstallationUninstalled, RecordRepositorySelectionChange,
    RevokeInvitationLink, RouteGithubInvitationWebhook, SubmitInvitationRequest,
};
use ghinvite_web::{AppState, RestateClient, RestateCommands, WebConfig, build_app};
use http_body_util::BodyExt;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

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

#[derive(Clone, Debug, PartialEq)]
enum RecordedCommand {
    CreateInvitationLink {
        description: String,
        internal_note: Option<String>,
        permission: ghinvite_core::Permission,
        approval_required: bool,
        max_uses: Option<u32>,
        expires_at: Option<DateTime<Utc>>,
        repo_ids: Vec<u64>,
    },
}

#[derive(Default)]
struct RecordingCommands {
    calls: Arc<Mutex<Vec<RecordedCommand>>>,
}

#[async_trait::async_trait]
impl GhinviteCommands for RecordingCommands {
    async fn create_invitation_link(
        &self,
        command: CreateInvitationLink,
    ) -> ghinvite_web::Result<CreateInvitationLinkOutput> {
        self.calls
            .lock()
            .unwrap()
            .push(RecordedCommand::CreateInvitationLink {
                description: command.description,
                internal_note: command.internal_note,
                permission: command.permission,
                approval_required: command.approval_required,
                max_uses: command.max_uses,
                expires_at: command.expires_at,
                repo_ids: command.repos.into_iter().map(|repo| repo.repo_id).collect(),
            });
        Ok(CreateInvitationLinkOutput {
            link_id: ghinvite_core::InvitationLinkId::new(),
            slug: "abcdEFGH01234567".to_string(),
        })
    }

    async fn revoke_invitation_link(
        &self,
        _command: RevokeInvitationLink,
    ) -> ghinvite_web::Result<()> {
        panic!("unexpected revoke_invitation_link command")
    }

    async fn submit_invitation_request(
        &self,
        _command: SubmitInvitationRequest,
    ) -> ghinvite_web::Result<()> {
        panic!("unexpected submit_invitation_request command")
    }

    async fn decide_invitation_request(
        &self,
        _command: DecideInvitationRequest,
    ) -> ghinvite_web::Result<()> {
        panic!("unexpected decide_invitation_request command")
    }

    async fn onboard_installation(
        &self,
        _command: OnboardInstallation,
    ) -> ghinvite_web::Result<()> {
        panic!("unexpected onboard_installation command")
    }

    async fn record_repository_selection_change(
        &self,
        _command: RecordRepositorySelectionChange,
    ) -> ghinvite_web::Result<()> {
        panic!("unexpected record_repository_selection_change command")
    }

    async fn record_installation_uninstalled(
        &self,
        _command: RecordInstallationUninstalled,
    ) -> ghinvite_web::Result<()> {
        panic!("unexpected record_installation_uninstalled command")
    }

    async fn route_github_invitation_webhook(
        &self,
        _command: RouteGithubInvitationWebhook,
    ) -> ghinvite_web::Result<()> {
        panic!("unexpected route_github_invitation_webhook command")
    }
}

async fn build_test_app() -> axum::Router {
    use ghinvite_github::mocks::MockTransport;
    let storage: Arc<dyn ghinvite_core::storage::Storage> = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let transport: Arc<dyn ghinvite_github::HttpTransport> =
        Arc::new(MockTransport::scripted(vec![]));
    let restate = Arc::new(RestateClient::new("http://127.0.0.1:8080").unwrap());
    let commands = Arc::new(RestateCommands::new(restate));
    let state = AppState::new(storage, transport, commands, WebConfig::for_local_dev());
    let session_store = tower_sessions::MemoryStore::default();
    build_app(state, session_store)
}

async fn build_signed_in_admin_app() -> (axum::Router, String) {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
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

    let storage: Arc<dyn ghinvite_core::storage::Storage> = storage;
    let transport: Arc<dyn ghinvite_github::HttpTransport> =
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

async fn build_signed_in_admin_app_with_recording_commands(
    expectations: Vec<Expectation>,
) -> (axum::Router, String, Arc<Mutex<Vec<RecordedCommand>>>) {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
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

    let storage: Arc<dyn ghinvite_core::storage::Storage> = storage;
    let transport: Arc<dyn ghinvite_github::HttpTransport> =
        Arc::new(MockTransport::scripted(expectations));
    let commands = Arc::new(RecordingCommands::default());
    let calls = commands.calls.clone();
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

    (app, cookie2, calls)
}

fn installation_repos_expectation() -> Expectation {
    Expectation::ok_json(
        Method::Get,
        "https://api.github.com/user/installations/77/repositories?per_page=100",
        serde_json::json!({
            "total_count": 2,
            "repositories": [
                {"id": 10, "full_name": "acme/api", "private": true},
                {"id": 11, "full_name": "acme/web", "private": true}
            ]
        }),
    )
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
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
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

    let storage: Arc<dyn ghinvite_core::storage::Storage> = storage;
    let transport: Arc<dyn ghinvite_github::HttpTransport> =
        Arc::new(MockTransport::scripted(expectations));
    let restate = Arc::new(RestateClient::new("http://127.0.0.1:8080").unwrap());
    let commands = Arc::new(RestateCommands::new(restate));
    let state = AppState::new(storage, transport, commands, WebConfig::for_local_dev());
    let session_store = tower_sessions::MemoryStore::default();
    let app = build_app(state, session_store);

    sign_in(app).await
}

async fn build_signed_in_app_with_failed_installation_discovery() -> (axum::Router, String) {
    let storage: Arc<dyn ghinvite_core::storage::Storage> = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
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
    let transport: Arc<dyn ghinvite_github::HttpTransport> =
        Arc::new(MockTransport::scripted(expectations));
    let restate = Arc::new(RestateClient::new("http://127.0.0.1:8080").unwrap());
    let commands = Arc::new(RestateCommands::new(restate));
    let state = AppState::new(storage, transport, commands, WebConfig::for_local_dev());
    let session_store = tower_sessions::MemoryStore::default();
    let app = build_app(state, session_store);

    sign_in(app).await
}

async fn build_signed_in_app_without_installation() -> (axum::Router, String) {
    let storage: Arc<dyn ghinvite_core::storage::Storage> = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let mut expectations = oauth_expectations_without_membership();
    expectations.push(Expectation::ok_json(
        Method::Get,
        "https://api.github.com/user/installations?per_page=100",
        serde_json::json!({"total_count": 0, "installations": []}),
    ));
    let transport: Arc<dyn ghinvite_github::HttpTransport> =
        Arc::new(MockTransport::scripted(expectations));
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
async fn console_unmatched_route_unauthenticated_redirects_to_login() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/console/missing")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(location, "/login?return_to=%2Fconsole%2Fmissing");
}

#[tokio::test]
async fn console_unmatched_route_for_signed_in_user_renders_generic_404() {
    let (app, cookie) = build_signed_in_app_without_installation().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/console/missing")
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
    assert!(text.contains("ghinvite records account activity for invitation links, invitation requests, and GitHub invitations. Console browsing is not available yet."));
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
async fn console_overview_carries_csp_and_only_external_script() {
    let (app, cookie) = build_signed_in_admin_app().await;
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

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("content-security-policy")
            .map(|v| v.to_str().unwrap()),
        Some(ghinvite_web::middleware::csp::CONTENT_SECURITY_POLICY)
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("console-frame"));
    let external = "<script src=\"/static/app.js\"></script>";
    assert!(text.contains(external));
    assert_eq!(
        text.matches("<script").count(),
        text.matches(external).count(),
        "console HTML contains an inline <script> block"
    );
}

#[tokio::test]
async fn create_link_invalid_description_rerenders_form_with_errors_and_preserved_values() {
    let mut expectations = oauth_expectations();
    expectations.push(installation_repos_expectation());
    let (app, cookie, calls) =
        build_signed_in_admin_app_with_recording_commands(expectations).await;

    let body = "description=%20%20%20&permission=push&approval_required=true&max_uses=7&expires_in_days=45&internal_note=Keep+this+note&repo_ids=10";
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/console/accounts/acme/links")
                .header("cookie", cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(calls.lock().unwrap().is_empty());
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("New invitation link"));
    assert!(text.contains("Fix the highlighted fields before creating this invitation link."));
    assert!(text.contains("Description is required. Use short, single-line admin-only context for this invitation link."));
    assert!(text.contains("aria-invalid=\"true\""));
    assert!(text.contains("aria-describedby=\"description-help description-error\""));
    assert!(text.contains("value=\"push\" selected"));
    assert!(text.contains("name=\"approval_required\" value=\"true\" checked"));
    assert!(text.contains("name=\"max_uses\" value=\"7\""));
    assert!(text.contains("name=\"expires_in_days\" value=\"45\""));
    assert!(text.contains("Keep this note"));
    assert!(text.contains("value=\"10\" checked"));
    assert!(text.contains("acme/api"));
}

#[tokio::test]
async fn create_link_valid_submission_invokes_command_and_redirects() {
    let mut expectations = oauth_expectations();
    expectations.push(installation_repos_expectation());
    let (app, cookie, calls) =
        build_signed_in_admin_app_with_recording_commands(expectations).await;

    let body = "description=%20AI+coding+workshop%20&permission=push&approval_required=true&max_uses=7&expires_in_days=45&internal_note=Keep+this+note&repo_ids=10";
    let before = Utc::now();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/console/accounts/acme/links")
                .header("cookie", cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let after = Utc::now();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert!(location.starts_with("/console/accounts/acme/links/"));
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let RecordedCommand::CreateInvitationLink {
        description,
        internal_note,
        permission,
        approval_required,
        max_uses,
        expires_at,
        repo_ids,
    } = &calls[0];
    assert_eq!(description, "AI coding workshop");
    assert_eq!(internal_note.as_deref(), Some("Keep this note"));
    assert_eq!(*permission, ghinvite_core::Permission::Push);
    assert!(approval_required);
    assert_eq!(*max_uses, Some(7));
    assert_expires_days_from(*expires_at, 45, before, after);
    assert_eq!(*repo_ids, vec![10]);
}

/// `expires_at` must be exactly `days` after some instant between `before`
/// and `after` (the request's `now`), i.e. `now + days` without drift.
fn assert_expires_days_from(
    expires_at: Option<DateTime<Utc>>,
    days: i64,
    before: DateTime<Utc>,
    after: DateTime<Utc>,
) {
    let expires_at = expires_at.expect("expires_at should be set");
    let earliest = before + Duration::days(days);
    let latest = after + Duration::days(days);
    assert!(
        expires_at >= earliest && expires_at <= latest,
        "expires_at {expires_at} should be {days} days after a now in [{before}, {after}]"
    );
}

#[tokio::test]
async fn create_link_invalid_numeric_guardrails_rerender_form_with_field_errors() {
    let mut expectations = oauth_expectations();
    expectations.push(installation_repos_expectation());
    let (app, cookie, calls) =
        build_signed_in_admin_app_with_recording_commands(expectations).await;

    let body = "description=AI+coding+workshop&permission=push&max_uses=abc&expires_in_days=0&internal_note=Keep+this+note&repo_ids=10";
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/console/accounts/acme/links")
                .header("cookie", cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(calls.lock().unwrap().is_empty());
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("New invitation link"));
    assert!(text.contains("Fix the highlighted fields before creating this invitation link."));
    assert!(text.contains("Max use must be a whole number of 1 or more."));
    assert!(text.contains("Expiration must be a whole number of days, 1 or more."));
    assert!(text.contains("id=\"max_uses-error\""));
    assert!(text.contains("id=\"expires_in_days-error\""));
    assert!(text.contains("aria-describedby=\"max_uses-help max_uses-error\""));
    assert!(text.contains("aria-describedby=\"expires_in_days-help expires_in_days-error\""));
    assert_eq!(text.matches("aria-invalid=\"true\"").count(), 2);
    assert!(!text.contains("description-error"));
    assert!(text.contains("name=\"description\" value=\"AI coding workshop\""));
    assert!(text.contains("name=\"max_uses\" value=\"abc\""));
    assert!(text.contains("name=\"expires_in_days\" value=\"0\""));
    assert!(text.contains("value=\"push\" selected"));
    assert!(text.contains("Keep this note"));
    assert!(text.contains("value=\"10\" checked"));
    assert!(text.contains("acme/api"));
    assert!(!text.contains("Bad Request"));
}

#[tokio::test]
async fn create_link_valid_numeric_guardrails_reach_command() {
    let mut expectations = oauth_expectations();
    expectations.push(installation_repos_expectation());
    let (app, cookie, calls) =
        build_signed_in_admin_app_with_recording_commands(expectations).await;

    let body = "description=AI+coding+workshop&permission=pull&max_uses=+3+&expires_in_days=+10+&repo_ids=11";
    let before = Utc::now();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/console/accounts/acme/links")
                .header("cookie", cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let after = Utc::now();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let RecordedCommand::CreateInvitationLink {
        max_uses,
        expires_at,
        repo_ids,
        ..
    } = &calls[0];
    assert_eq!(*max_uses, Some(3));
    assert_expires_days_from(*expires_at, 10, before, after);
    assert_eq!(*repo_ids, vec![11]);
}

#[tokio::test]
async fn create_link_blank_numeric_guardrails_mean_unlimited_and_no_expiration() {
    let mut expectations = oauth_expectations();
    expectations.push(installation_repos_expectation());
    let (app, cookie, calls) =
        build_signed_in_admin_app_with_recording_commands(expectations).await;

    let body =
        "description=AI+coding+workshop&permission=pull&max_uses=&expires_in_days=&repo_ids=10";
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/console/accounts/acme/links")
                .header("cookie", cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let RecordedCommand::CreateInvitationLink {
        max_uses,
        expires_at,
        ..
    } = &calls[0];
    assert_eq!(*max_uses, None);
    assert_eq!(*expires_at, None);
}

const REPO_SCOPE_REQUIRED: &str = "Repository scope is required. Select at least one available repository for this invitation link.";

/// POST the new invitation link form as a signed-in admin whose installation
/// exposes `acme/api` (10) and `acme/web` (11), returning the response and the
/// recorded command calls.
async fn post_create_link(
    body: &'static str,
) -> (axum::response::Response, Arc<Mutex<Vec<RecordedCommand>>>) {
    let mut expectations = oauth_expectations();
    expectations.push(installation_repos_expectation());
    let (app, cookie, calls) =
        build_signed_in_admin_app_with_recording_commands(expectations).await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/console/accounts/acme/links")
                .header("cookie", cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    (resp, calls)
}

#[tokio::test]
async fn create_link_without_repositories_rerenders_form_with_repository_scope_error() {
    let (resp, calls) = post_create_link(
        "description=AI+coding+workshop&permission=push&approval_required=true&max_uses=7&expires_in_days=45&internal_note=Keep+this+note",
    )
    .await;

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(calls.lock().unwrap().is_empty());
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("New invitation link"));
    assert!(text.contains("Fix the highlighted fields before creating this invitation link."));
    assert!(text.contains(REPO_SCOPE_REQUIRED));
    assert!(text.contains("id=\"repo_ids-error\""));
    assert!(text.contains("role=\"group\""));
    assert!(text.contains("aria-describedby=\"repo_ids-help repo_ids-error\""));
    assert_eq!(text.matches("aria-invalid=\"true\"").count(), 1);
    assert!(!text.contains("description-error"));
    assert!(!text.contains("Bad Request"));
    assert!(!text.contains("select at least one repository"));
    assert!(text.contains("name=\"description\" value=\"AI coding workshop\""));
    assert!(text.contains("value=\"push\" selected"));
    assert!(text.contains("name=\"approval_required\" value=\"true\" checked"));
    assert!(text.contains("name=\"max_uses\" value=\"7\""));
    assert!(text.contains("name=\"expires_in_days\" value=\"45\""));
    assert!(text.contains("Keep this note"));
    assert!(text.contains("acme/api"));
    assert!(text.contains("acme/web"));
    assert!(!text.contains("value=\"10\" checked"));
    assert!(!text.contains("value=\"11\" checked"));
}

#[tokio::test]
async fn create_link_with_only_unknown_repositories_rerenders_form_with_repository_scope_error() {
    let (resp, calls) = post_create_link(
        "description=AI+coding+workshop&permission=push&repo_ids=999&repo_ids=1000",
    )
    .await;

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(calls.lock().unwrap().is_empty());
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains(REPO_SCOPE_REQUIRED));
    assert!(text.contains("id=\"repo_ids-error\""));
    assert!(!text.contains("Bad Request"));
    assert!(text.contains("acme/api"));
    assert!(!text.contains("value=\"999\""));
    assert!(!text.contains("value=\"1000\""));
    assert!(!text.contains("value=\"10\" checked"));
    assert!(!text.contains("value=\"11\" checked"));
}

#[tokio::test]
async fn create_link_with_valid_and_unknown_repositories_scopes_only_the_available_ones() {
    let (resp, calls) = post_create_link(
        "description=AI+coding+workshop&permission=push&repo_ids=999&repo_ids=11&repo_ids=10",
    )
    .await;

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert!(location.starts_with("/console/accounts/acme/links/"));
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let RecordedCommand::CreateInvitationLink { repo_ids, .. } = &calls[0];
    assert_eq!(
        *repo_ids,
        vec![10, 11],
        "available order, unknown 999 dropped"
    );
}

#[tokio::test]
async fn create_link_validation_failure_keeps_available_repositories_checked_and_drops_unknown() {
    let (resp, calls) =
        post_create_link("description=&permission=push&repo_ids=10&repo_ids=999").await;

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(calls.lock().unwrap().is_empty());
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("id=\"description-error\""));
    assert!(
        !text.contains("repo_ids-error"),
        "a valid available repository was selected, so repository scope passes"
    );
    assert!(text.contains("value=\"10\" checked"));
    assert!(text.contains("value=\"11\""));
    assert!(!text.contains("value=\"11\" checked"));
    assert!(!text.contains("value=\"999\""));
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
