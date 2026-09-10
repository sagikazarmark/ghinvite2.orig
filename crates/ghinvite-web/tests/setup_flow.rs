use axum::body::Body;
use axum::http::{Request, StatusCode};
use ghinvite_core::{AccountType, SelectedRepos};
use ghinvite_github::mocks::{Expectation, MockTransport};
use ghinvite_github::transport::{Method, Response};
use ghinvite_web::commands::{
    CreateInvitationLink, CreateInvitationLinkOutput, DecideInvitationRequest, GhinviteCommands,
    OnboardInstallation, RecordInstallationUninstalled, RecordRepositorySelectionChange,
    RepositorySelectionChangeSource, RevokeInvitationLink, RouteGithubInvitationWebhook,
    SubmitInvitationRequest, UpdateInvitationLinkMetadata,
};
use ghinvite_web::{AppState, WebConfig, build_app};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

#[derive(Clone, Debug, PartialEq)]
enum RecordedCommand {
    OnboardInstallation {
        installation_id: u64,
        actor_user_id: u64,
        account_id: u64,
        account_login: String,
        account_type: AccountType,
        selected_repos: SelectedRepos,
    },
    RecordRepositorySelectionChange {
        installation_id: u64,
        selected_repos: SelectedRepos,
        source: RepositorySelectionChangeSource,
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
        _command: CreateInvitationLink,
    ) -> ghinvite_web::Result<CreateInvitationLinkOutput> {
        panic!("unexpected create_invitation_link command")
    }

    async fn update_invitation_link_metadata(
        &self,
        _command: UpdateInvitationLinkMetadata,
    ) -> ghinvite_web::Result<()> {
        panic!("unexpected update_invitation_link_metadata command")
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

    async fn onboard_installation(&self, command: OnboardInstallation) -> ghinvite_web::Result<()> {
        self.calls
            .lock()
            .unwrap()
            .push(RecordedCommand::OnboardInstallation {
                installation_id: command.installation_id,
                actor_user_id: command.actor_user_id,
                account_id: command.account_id,
                account_login: command.account_login,
                account_type: command.account_type,
                selected_repos: command.selected_repos,
            });
        Ok(())
    }

    async fn record_repository_selection_change(
        &self,
        command: RecordRepositorySelectionChange,
    ) -> ghinvite_web::Result<()> {
        self.calls
            .lock()
            .unwrap()
            .push(RecordedCommand::RecordRepositorySelectionChange {
                installation_id: command.installation_id,
                selected_repos: command.selected_repos,
                source: command.source,
            });
        Ok(())
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

async fn build_app_with(mock: MockTransport) -> (axum::Router, Arc<Mutex<Vec<RecordedCommand>>>) {
    let storage: Arc<dyn ghinvite_core::storage::Storage> = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let transport: Arc<dyn ghinvite_github::HttpTransport> = Arc::new(mock);
    let commands = Arc::new(RecordingCommands::default());
    let calls = commands.calls.clone();
    let state = AppState::new(storage, transport, commands, WebConfig::for_local_dev());
    let session_store = tower_sessions::MemoryStore::default();
    (build_app(state, session_store), calls)
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
    let (app, _calls) = build_app_with(MockTransport::scripted(vec![])).await;
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
    let (app, calls) = build_app_with(MockTransport::scripted(expectations)).await;
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
    assert!(calls.lock().unwrap().is_empty());
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
    let (app, calls) = build_app_with(MockTransport::scripted(expectations)).await;
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

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0],
        RecordedCommand::OnboardInstallation {
            installation_id: 77,
            actor_user_id: 42,
            account_id: 9001,
            account_login: "acme".into(),
            account_type: AccountType::Organization,
            selected_repos: SelectedRepos::Subset(vec![10, 11]),
        }
    );
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
    let (app, calls) = build_app_with(MockTransport::scripted(expectations)).await;
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

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0],
        RecordedCommand::OnboardInstallation {
            installation_id: 88,
            actor_user_id: 42,
            account_id: 42,
            account_login: "octocat".into(),
            account_type: AccountType::User,
            selected_repos: SelectedRepos::All,
        }
    );
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
    let (app, calls) = build_app_with(MockTransport::scripted(expectations)).await;
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

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0],
        RecordedCommand::RecordRepositorySelectionChange {
            installation_id: 77,
            selected_repos: SelectedRepos::Subset(vec![12]),
            source: RepositorySelectionChangeSource::SetupReturn,
        }
    );
}
