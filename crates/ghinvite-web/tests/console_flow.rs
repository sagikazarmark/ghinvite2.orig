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
    UpdateInvitationLinkMetadata,
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
    edit_storage: Option<Arc<ghinvite_storage_sqlx::SqlxStorage>>,
    fail_edits: bool,
}

#[async_trait::async_trait]
impl GhinviteCommands for RecordingCommands {
    async fn update_invitation_link_metadata(
        &self,
        command: UpdateInvitationLinkMetadata,
    ) -> ghinvite_web::Result<()> {
        if self.fail_edits {
            return Err(ghinvite_web::WebError::Restate(
                "service unavailable".into(),
            ));
        }
        assert_eq!(command.by_user, 42);
        self.edit_storage
            .as_ref()
            .expect("unexpected metadata update")
            .update_invitation_link_metadata(
                command.account_id,
                command.link_id,
                &command.description,
                command.internal_note.as_deref(),
            )
            .await?;
        Ok(())
    }

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
async fn links_collection_renders_account_scoped_rows_and_native_controls() {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let (app, cookie) = links_app(storage.clone(), "admin").await;
    let mut link = list_link(1);
    storage.insert_invitation_link(&link).await.unwrap();
    link.id = ghinvite_core::InvitationLinkId::new();
    link.slug = ghinvite_core::Slug::from_string("foreigncode00001".into()).unwrap();
    link.account_id = 9002;
    link.installation_id = 78;
    link.description = "Other account secret".into();
    storage.insert_invitation_link(&link).await.unwrap();

    let (status, html) = get_links(&app, &cookie, "?account_id=9002&login=other").await;
    assert_eq!(status, StatusCode::OK);
    assert_links_navigation_current(&html);
    assert!(html.contains("Workshop 001</a>"));
    assert!(!html.contains("Other account secret"));
    assert!(html.contains("<form method=\"get\" action=\"/console/accounts/acme/links\""));
    assert!(html.contains("name=\"page\" value=\"1\""));
    for column in ["Description", "Status", "Uses", "Expiration", "Created"] {
        assert!(html.contains(&format!("<th scope=\"col\">{column}</th>")));
    }
    assert!(html.contains("0 / unlimited"));
    assert!(html.contains("No expiration"));
    assert!(html.contains("href=\"/console/accounts/acme/links/new\""));
}

#[tokio::test]
async fn edit_link_lookup_failure_preserves_submitted_input() {
    let path = std::env::temp_dir().join(format!(
        "ghinvite-edit-{}.sqlite",
        ghinvite_core::InvitationLinkId::new()
    ));
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::at_path(&path)
            .await
            .unwrap(),
    );
    let (app, cookie) = links_app(storage.clone(), "admin").await;
    let link = list_link(1);
    storage.insert_invitation_link(&link).await.unwrap();
    let pool =
        sqlx::SqlitePool::connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
    sqlx::query("DROP TABLE invitation_link_repos")
        .execute(&pool)
        .await
        .unwrap();
    let response = post_edit(
        &app,
        &cookie,
        &link.id.to_string(),
        "description=%20Keep+my+description%20&internal_note=Keep%0Amy+note",
    )
    .await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains("value=\" Keep my description \""));
    assert!(html.contains(">Keep\nmy note</textarea>"));
    assert!(html.contains("Failed to load invitation link details. Please try again."));
    assert_links_navigation_current(&html);
    pool.close().await;
    drop(app);
    drop(storage);
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn edit_link_errors_preserve_input_and_do_not_change_details() {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let (app, cookie) = links_app(storage.clone(), "admin").await;
    let link = list_link(1);
    storage.insert_invitation_link(&link).await.unwrap();
    for description in [
        "".to_string(),
        "%20%20".into(),
        "Workshop%0Acohort".into(),
        "x".repeat(121),
    ] {
        // The command fake panics if invalid input reaches the save boundary.
        let response = post_edit(
            &app,
            &cookie,
            &link.id.to_string(),
            &format!("description={description}&internal_note=%20Keep%0Athis%20"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert_links_navigation_current(&html);
        assert!(html.contains("aria-invalid=\"true\""));
        assert!(html.contains("description-help description-error"));
        assert!(html.contains("> Keep\nthis </textarea>"));
    }
    let commands = Arc::new(RecordingCommands {
        fail_edits: true,
        ..Default::default()
    });
    let mut expectations = oauth_expectations();
    // The session layer does not persist the authorization cache on a 502.
    expectations.push(Expectation::ok_json(
        Method::Get,
        "https://api.github.com/user/memberships/orgs/acme",
        serde_json::json!({"role": "admin", "state": "active"}),
    ));
    let state = AppState::new(
        storage.clone(),
        Arc::new(MockTransport::scripted(expectations)),
        commands,
        WebConfig::for_local_dev(),
    );
    let (app, cookie) = sign_in(build_app(state, tower_sessions::MemoryStore::default())).await;
    let response = post_edit(
        &app,
        &cookie,
        &link.id.to_string(),
        "description=%20New+details%20&internal_note=%20Keep%0Athis%20",
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert_links_navigation_current(&html);
    assert!(html.contains("Failed to save invitation link details. Please try again."));
    assert!(html.contains("value=\" New details \""));
    assert!(html.contains("> Keep\nthis </textarea>"));
    assert!(!html.contains("aria-invalid"));
    let (_, detail) = get_links(&app, &cookie, &format!("/{}", link.id)).await;
    assert!(detail.contains("Workshop 001</h1>"));
    assert!(!detail.contains("New details"));
}

#[tokio::test]
async fn edit_link_get_and_post_require_account_admin_and_hide_foreign_links() {
    let app = build_test_app().await;
    for method in ["GET", "POST"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri("/console/accounts/acme/links/missing/edit")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("description=New"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert!(
            response.headers()["location"]
                .to_str()
                .unwrap()
                .starts_with("/login?return_to=")
        );
    }
    for role in ["admin", "member"] {
        let storage = Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::in_memory()
                .await
                .unwrap(),
        );
        let (app, cookie) = links_app(storage.clone(), role).await;
        let mut foreign = list_link(1);
        foreign.account_id = 9002;
        foreign.installation_id = 78;
        foreign.description = "Foreign private context".into();
        storage.insert_invitation_link(&foreign).await.unwrap();
        let own = list_link(2);
        storage.insert_invitation_link(&own).await.unwrap();
        let mut ids = vec![
            foreign.id.to_string(),
            "invalid".into(),
            ghinvite_core::InvitationLinkId::new().to_string(),
        ];
        if role == "member" {
            ids.push(own.id.to_string());
        }
        for id in ids {
            let (status, html) = get_links(&app, &cookie, &format!("/{id}/edit")).await;
            assert_eq!(status, StatusCode::NOT_FOUND);
            assert!(!html.contains("Foreign private context"));
            assert!(!html.contains("aria-current=\"page\""));
            let response =
                post_edit(&app, &cookie, &id, "description=Changed&account_id=9002").await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
    }
}

#[tokio::test]
async fn edit_link_save_normalizes_metadata_and_keeps_guardrails() {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let commands = Arc::new(RecordingCommands {
        edit_storage: Some(storage.clone()),
        ..Default::default()
    });
    let (app, cookie) = links_app_with_commands(storage.clone(), "admin", commands).await;
    let mut link = list_link(1);
    link.max_uses = Some(8);
    link.uses_count = 3;
    link.revoked_at = Some(link.created_at);
    link.internal_note = Some("Old private note".into());
    storage.insert_invitation_link(&link).await.unwrap();
    for (note, expected) in [("%20New%0Anote%20", Some("New\nnote")), ("%20%20", None)] {
        let response = post_edit(&app, &cookie, &link.id.to_string(), &format!("description=%20Updated+workshop%20&internal_note={note}&permission=admin&max_uses=999&uses_count=0&revoked_at=&account_id=9002&by_user=999")).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response.headers()["location"],
            format!("/console/accounts/acme/links/{}", link.id)
        );
        let (status, detail) = get_links(&app, &cookie, &format!("/{}", link.id)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(detail.contains("Updated workshop</h1>"));
        assert!(detail.contains("Invitation link details updated."));
        assert!(detail.contains("3 / 8"));
        assert!(detail.contains("inactive"));
        assert!(!detail.contains("Old private note"));
        if let Some(expected) = expected {
            assert!(detail.contains(expected));
        } else {
            assert!(!detail.contains("New\nnote"));
        }
    }
    let (_, list) = get_links(&app, &cookie, "?filter=all").await;
    assert!(list.contains("Updated workshop</a>"));
}

async fn post_edit(
    app: &axum::Router,
    cookie: &str,
    id: &str,
    body: &str,
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/console/accounts/acme/links/{id}/edit"))
                .header("cookie", cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn edit_link_form_prefills_metadata_for_inactive_links() {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let (app, cookie) = links_app(storage.clone(), "admin").await;
    let mut link = list_link(1);
    link.internal_note = Some("Private note\nSecond line".into());
    link.revoked_at = Some(link.created_at);
    storage.insert_invitation_link(&link).await.unwrap();
    let (status, html) = get_links(&app, &cookie, &format!("/{}/edit", link.id)).await;
    assert_eq!(status, StatusCode::OK);
    assert_links_navigation_current(&html);
    assert!(html.contains("value=\"Workshop 001\""));
    assert!(html.contains(">Private note\nSecond line</textarea>"));
    assert!(html.contains(&format!(
        "action=\"/console/accounts/acme/links/{}/edit\"",
        link.id
    )));
    assert!(html.contains(&format!(
        "href=\"/console/accounts/acme/links/{}\">Cancel</a>",
        link.id
    )));
}

#[tokio::test]
async fn link_detail_selects_links_but_missing_links_have_no_current_section() {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let (app, cookie) = links_app(storage.clone(), "admin").await;
    let link = list_link(1);
    storage.insert_invitation_link(&link).await.unwrap();

    let (status, html) = get_links(&app, &cookie, &format!("/{}", link.id)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("Workshop 001</h1>"));
    assert_links_navigation_current(&html);

    for id in [
        "not-an-id".to_string(),
        ghinvite_core::InvitationLinkId::new().to_string(),
    ] {
        let (status, html) = get_links(&app, &cookie, &format!("/{id}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(html.contains("console-frame"));
        assert!(!html.contains("aria-current=\"page\""));
        assert!(!html.contains("app-nav-row-active"));
    }
}

#[tokio::test]
async fn links_collection_preserves_auth_and_concealment() {
    let app = build_test_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/console/accounts/acme/links?filter=all&page=2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()["location"],
        "/login?return_to=%2Fconsole%2Faccounts%2Facme%2Flinks%3Ffilter%3Dall%26page%3D2"
    );

    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let (app, cookie) = links_app(storage.clone(), "member").await;
    storage.insert_invitation_link(&list_link(1)).await.unwrap();
    let (status, html) = get_links(&app, &cookie, "?filter=all&account_id=9001").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(html.contains("Page not found"));
    assert!(!html.contains("Workshop 001"));
    assert!(!html.contains("console-frame"));
}

#[tokio::test]
async fn links_collection_url_state_filters_sorts_then_paginates() {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let (app, cookie) = links_app(storage.clone(), "admin").await;
    for n in 1..=60 {
        let mut link = list_link(n);
        if n % 2 == 0 {
            link.revoked_at = Some(Utc::now());
        }
        storage.insert_invitation_link(&link).await.unwrap();
    }
    let (status, first) = get_links(&app, &cookie, "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first.matches("Invitation code:").count(), 25);
    assert!(first.find("Workshop 059</a>").unwrap() < first.find("Workshop 011</a>").unwrap());
    assert!(!first.contains("Workshop 060</a>"));
    assert!(!first.contains(">Previous</a>"));
    assert!(first.contains("filter=active&#38;sort=created&#38;direction=desc&#38;page=2"));

    let query = "?filter=inactive&sort=description&direction=asc&page=2";
    let (status, second) = get_links(&app, &cookie, query).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second.matches("Invitation code:").count(), 5);
    assert!(second.find("Workshop 052</a>").unwrap() < second.find("Workshop 060</a>").unwrap());
    assert!(second.contains("Page 2 of 2"));
    assert!(second.contains("filter=inactive&#38;sort=description&#38;direction=asc&#38;page=1"));
    assert!(!second.contains(">Next</a>"));
    assert!(second.contains("name=\"page\" value=\"1\""));
    assert_eq!(
        get_links(&app, &cookie, query).await.1,
        second,
        "refresh restores the selected page"
    );
    assert_eq!(
        get_links(&app, &cookie, "").await.1,
        first,
        "Back restores the previous URL"
    );

    let (_, last) = get_links(
        &app,
        &cookie,
        "?filter=inactive&sort=description&direction=asc&page=999999999999999999999999999",
    )
    .await;
    assert_eq!(last, second);
    for query in [
        "?filter=wrong&sort=wrong&direction=wrong&page=-1",
        "?page=0",
        "?page=abc",
        "?filter=%FF&page=%00",
    ] {
        assert_eq!(get_links(&app, &cookie, query).await.1, first, "{query}");
    }
    // Duplicate recognized keys consistently use the last value.
    assert_eq!(
        get_links(&app, &cookie, "?filter=all&filter=active")
            .await
            .1,
        first
    );
}

#[tokio::test]
async fn links_collection_distinguishes_empty_account_from_empty_filter() {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let (app, cookie) = links_app(storage.clone(), "admin").await;
    let (status, empty) = get_links(&app, &cookie, "?page=5").await;
    assert_eq!(status, StatusCode::OK);
    assert!(empty.contains("No invitation links yet"));
    assert_links_navigation_current(&empty);
    assert!(!empty.contains("No invitation links match"));
    assert!(!empty.contains(">Previous</a>"));

    let mut link = list_link(1);
    link.revoked_at = Some(Utc::now());
    storage.insert_invitation_link(&link).await.unwrap();
    let (_, filtered) = get_links(&app, &cookie, "").await;
    assert!(filtered.contains("No invitation links match this filter"));
    assert_links_navigation_current(&filtered);
    assert!(!filtered.contains("No invitation links yet"));
    assert!(filtered.contains("filter=all&#38;sort=created&#38;direction=desc&#38;page=1"));
    assert!(filtered.contains("href=\"/console/accounts/acme/links/new\""));
}

#[tokio::test]
async fn links_collection_load_failure_is_not_an_empty_account() {
    let path = std::env::temp_dir().join(format!(
        "ghinvite-links-{}.sqlite",
        ghinvite_core::InvitationLinkId::new()
    ));
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::at_path(&path)
            .await
            .unwrap(),
    );
    let (app, cookie) = links_app(storage.clone(), "admin").await;
    // Break only the collection read, leaving account authorization available.
    let pool =
        sqlx::SqlitePool::connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
    sqlx::query("DROP TABLE invitation_link_repos")
        .execute(&pool)
        .await
        .unwrap();
    let (status, html) =
        get_links(&app, &cookie, "?filter=all&sort=uses&direction=asc&page=2").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(html.contains("Invitation links could not be loaded"));
    assert_links_navigation_current(&html);
    assert!(html.contains("Try again"));
    assert!(html.contains("filter=all&#38;sort=uses&#38;direction=asc&#38;page=2"));
    assert!(!html.contains("No invitation links yet"));
    assert!(!html.contains("No invitation links match"));
    pool.close().await;
    drop(app);
    drop(storage);
    std::fs::remove_file(path).unwrap();
}

fn assert_links_navigation_current(html: &str) {
    for marker in ["console-sidebar", "mobile-console-nav"] {
        let nav = html
            .split_once(marker)
            .unwrap()
            .1
            .split_once("</nav>")
            .unwrap()
            .0;
        let current: Vec<_> = nav
            .split("<a ")
            .skip(1)
            .map(|anchor| anchor.split_once("</a>").unwrap().0)
            .filter(|anchor| anchor.contains("aria-current=\"page\""))
            .collect();
        assert_eq!(current.len(), 1, "{marker} must have one current section");
        assert!(current[0].contains("href=\"/console/accounts/acme/links\""));
        assert!(current[0].contains("app-nav-row-active"));
        assert!(current[0].ends_with(">Links"));
        assert!(!nav.contains("/links/new"));
    }
}

fn list_link(n: u32) -> ghinvite_core::InvitationLink {
    ghinvite_core::InvitationLink {
        id: ghinvite_core::InvitationLinkId::new(),
        slug: ghinvite_core::Slug::from_string(format!("code{n:012}")).unwrap(),
        installation_id: 77,
        account_id: 9001,
        created_by: 42,
        created_at: "2026-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
            + Duration::seconds(n.into()),
        expires_at: None,
        max_uses: None,
        uses_count: 0,
        permission: ghinvite_core::Permission::Pull,
        approval_required: false,
        description: format!("Workshop {n:03}"),
        internal_note: None,
        revoked_at: None,
        revoked_by: None,
        repos: vec![],
    }
}

async fn links_app(
    storage: Arc<ghinvite_storage_sqlx::SqlxStorage>,
    role: &str,
) -> (axum::Router, String) {
    links_app_with_commands(storage, role, Arc::new(RecordingCommands::default())).await
}

async fn links_app_with_commands(
    storage: Arc<ghinvite_storage_sqlx::SqlxStorage>,
    role: &str,
    commands: Arc<dyn GhinviteCommands>,
) -> (axum::Router, String) {
    for (installation_id, account_id, login) in [(77, 9001, "acme"), (78, 9002, "other")] {
        storage
            .insert_installation(&Account {
                installation_id,
                account_id,
                account_login: login.into(),
                account_type: AccountType::Organization,
                installed_at: Utc::now(),
                uninstalled_at: None,
                selected_repos: SelectedRepos::All,
            })
            .await
            .unwrap();
    }
    let mut expectations = oauth_sign_in_expectations();
    // Rejected membership checks are not saved in the authorization cache.
    for _ in 0..if role == "admin" { 1 } else { 8 } {
        expectations.push(Expectation::ok_json(
            Method::Get,
            "https://api.github.com/user/memberships/orgs/acme",
            serde_json::json!({"role": role, "state": "active"}),
        ));
    }
    let state = AppState::new(
        storage,
        Arc::new(MockTransport::scripted(expectations)),
        commands,
        WebConfig::for_local_dev(),
    );
    sign_in(build_app(state, tower_sessions::MemoryStore::default())).await
}

async fn get_links(app: &axum::Router, cookie: &str, query: &str) -> (StatusCode, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/console/accounts/acme/links{query}"))
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(body.to_vec()).unwrap())
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
async fn console_overview_keeps_five_recent_links_and_opens_filtered_collections() {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let (app, cookie) = links_app(storage.clone(), "admin").await;
    for n in 1..=6 {
        let mut link = list_link(n);
        if n % 2 == 0 {
            link.revoked_at = Some(link.created_at);
        }
        storage.insert_invitation_link(&link).await.unwrap();
    }
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/console/accounts/acme")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert_eq!(html.matches("Workshop ").count(), 5);
    assert!(!html.contains("Workshop 001"));
    assert!(html.find("Workshop 006").unwrap() < html.find("Workshop 002").unwrap());
    assert!(html.contains("3 active invitation links can accept invitation requests."));
    assert!(html.contains("href=\"/console/accounts/acme/links?filter=active\""));
    assert!(html.contains("href=\"/console/accounts/acme/links?filter=all\">View all</a>"));
    assert!(html.contains("href=\"/console/accounts/acme/links/new\">New invitation link</a>"));
    assert_eq!(
        html.matches("href=\"/console/accounts/acme\" aria-current=\"page\"")
            .count(),
        2
    );

    let (status, active) = get_links(&app, &cookie, "?filter=active").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(active.matches("Workshop ").count(), 3);
    assert!(!active.contains("Workshop 006"));
    let (status, all) = get_links(&app, &cookie, "?filter=all").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(all.matches("Workshop ").count(), 6);
    assert!(all.contains("Workshop 001"));
    assert!(all.contains("Workshop 006"));
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
async fn new_link_form_mounts_the_island_under_csp_without_inline_executable_script() {
    let mut expectations = oauth_expectations();
    expectations.push(installation_repos_expectation());
    let (app, cookie, _calls) =
        build_signed_in_admin_app_with_recording_commands(expectations).await;

    let before = Utc::now();
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/console/accounts/acme/links/new")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let after = Utc::now();

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("content-security-policy")
            .map(|v| v.to_str().unwrap()),
        Some(ghinvite_web::middleware::csp::CONTENT_SECURITY_POLICY)
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);

    assert_links_navigation_current(&text);

    // Island root wraps the form.
    assert!(text.contains(
        "<div id=\"link-form-island\"><form method=\"post\" action=\"/console/accounts/acme/links\""
    ));
    assert!(text.contains("</form></div>"));

    // The props blob is a data block carrying action, values, repos and the
    // server's instant.
    let props_open = "<script type=\"application/json\" id=\"link-form-props\">";
    let blob_at = text.find(props_open).expect("props blob present");
    let blob = &text[blob_at + props_open.len()..];
    let blob = &blob[..blob.find("</script>").unwrap()];
    let props: ghinvite_web::views::link_form::LinkFormIslandProps =
        serde_json::from_str(blob).unwrap();
    assert_eq!(props.action, "/console/accounts/acme/links");
    assert_eq!(
        props.values,
        ghinvite_web::views::links::LinkFormValues::default()
    );
    assert_eq!(
        props.repos,
        vec![
            ghinvite_web::views::link_form::RepositoryChoice {
                id: 10,
                full_name: "acme/api".to_string(),
            },
            ghinvite_web::views::link_form::RepositoryChoice {
                id: 11,
                full_name: "acme/web".to_string(),
            },
        ]
    );
    assert!(
        before <= props.now && props.now <= after,
        "now is the server's instant"
    );

    // The module script follows, with a stable src under /assets.
    let module = "<script type=\"module\" src=\"/assets/ghinvite-island.js\"></script>";
    assert!(text.contains(module));
    assert!(blob_at < text.find(module).unwrap());

    // Exactly three script elements — app script, data block, module — and
    // therefore nothing inline and executable.
    let external = "<script src=\"/static/app.js\"></script>";
    assert_eq!(text.matches(external).count(), 1);
    assert_eq!(
        text.matches("<script").count(),
        text.matches(external).count()
            + text.matches(props_open).count()
            + text.matches(module).count(),
        "new-link HTML contains an inline executable <script> block"
    );
}

#[tokio::test]
async fn create_link_failed_post_seeds_island_props_with_errors_and_preserved_values() {
    let mut expectations = oauth_expectations();
    expectations.push(installation_repos_expectation());
    let (app, cookie, _calls) =
        build_signed_in_admin_app_with_recording_commands(expectations).await;

    let body = "description=%3C%2Fscript%3E%3Cscript%3Ealert(1)%3C%2Fscript%3E&permission=owner&max_uses=7&expires_in_days=45&repo_ids=999&repo_ids=10";
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
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);

    let props_open = "<script type=\"application/json\" id=\"link-form-props\">";
    let blob_at = text.find(props_open).expect("props blob present");
    let blob = &text[blob_at + props_open.len()..];
    let blob = &blob[..blob.find("</script>").unwrap()];
    let props: ghinvite_web::views::link_form::LinkFormIslandProps =
        serde_json::from_str(blob).unwrap();

    // Values verbatim, errors as the server attached them.
    assert_eq!(
        props.values.description,
        "</script><script>alert(1)</script>"
    );
    assert_eq!(props.values.permission, "owner");
    assert_eq!(props.values.max_uses, "7");
    assert_eq!(props.values.expires_in_days, "45");
    assert_eq!(props.values.selected_repo_ids, vec![999, 10]);
    assert_eq!(
        props.values.errors.permission.as_deref(),
        Some("Choose a supported permission level: pull, triage, push, maintain, or admin.")
    );
    assert!(props.values.errors.description.is_none());
    assert!(props.values.errors.repo_scope.is_none());
    assert!(!props.values.errors.summary.is_empty());

    // The description cannot break out of the data block: the raw bytes hold
    // no `<`, and the page has exactly three script elements.
    assert!(!blob.contains('<'));
    assert_eq!(text.matches("</script>").count(), 3);
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
    assert_links_navigation_current(&text);
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

fn assert_preserved_description_input(html: &str) {
    let form = &html[html.find("<form").unwrap()..html.find("</form>").unwrap()];
    let mut inputs = form
        .split("<input ")
        .skip(1)
        .map(|input| input.split_once('>').unwrap().0)
        .filter(|input| input.contains("name=\"description\""));
    let input = inputs.next().expect("form renders the description input");
    assert!(inputs.next().is_none(), "description input is unique");
    assert!(input.contains("value=\"AI coding workshop\""));
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
    assert_preserved_description_input(&text);
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
    body: impl Into<Body>,
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
                .body(body.into())
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
    // aria-invalid is not permitted on role="group"; no field is invalid here.
    assert_eq!(text.matches("aria-invalid=\"true\"").count(), 0);
    assert!(!text.contains("description-error"));
    assert!(!text.contains("Bad Request"));
    assert!(!text.contains("select at least one repository"));
    assert_preserved_description_input(&text);
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

const PERMISSION_UNSUPPORTED: &str =
    "Choose a supported permission level: pull, triage, push, maintain, or admin.";

#[tokio::test]
async fn create_link_missing_permission_key_rerenders_form_with_permission_error() {
    // The select always submits a value, so a POST without the key is tampering.
    // It must get the same inline error path as a wrong value, not a bare 400
    // from form deserialization.
    let (resp, calls) = post_create_link(
        "description=AI+coding+workshop&approval_required=true&max_uses=7&expires_in_days=45&repo_ids=10",
    )
    .await;

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(calls.lock().unwrap().is_empty());
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains(PERMISSION_UNSUPPORTED));
    assert!(text.contains("id=\"permission-error\""));
    assert!(!text.contains("Bad Request"));
    assert_preserved_description_input(&text);
    assert!(text.contains("value=\"10\" checked"));
}

#[tokio::test]
async fn create_link_tampered_permission_rerenders_form_with_permission_error() {
    let (resp, calls) = post_create_link(
        "description=AI+coding+workshop&permission=owner&approval_required=true&max_uses=7&expires_in_days=45&internal_note=Keep+this+note&repo_ids=10",
    )
    .await;

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        calls.lock().unwrap().is_empty(),
        "a tampered permission level must not reach the command facade"
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("New invitation link"));
    assert!(text.contains("Fix the highlighted fields before creating this invitation link."));
    assert!(text.contains(PERMISSION_UNSUPPORTED));
    assert!(text.contains("id=\"permission-error\""));
    assert!(text.contains("aria-describedby=\"permission-help permission-error\""));
    assert_eq!(text.matches("aria-invalid=\"true\"").count(), 1);
    assert!(text.contains("select-error"));
    assert!(!text.contains("Bad Request"));
    assert!(!text.contains("invalid permission"));
    // The tampered value is not echoed into the form (the island props blob
    // after it carries the submitted values verbatim, JSON-escaped); the
    // select offers only supported levels.
    let form_markup = &text[text.find("<form").unwrap()..text.find("</form>").unwrap()];
    assert!(!form_markup.contains("owner"));
    assert_eq!(text.matches("<option").count(), 5);
    // Every other submitted value and selection survives the re-render.
    assert_preserved_description_input(&text);
    assert!(text.contains("name=\"approval_required\" value=\"true\" checked"));
    assert!(text.contains("name=\"max_uses\" value=\"7\""));
    assert!(text.contains("name=\"expires_in_days\" value=\"45\""));
    assert!(text.contains("Keep this note"));
    assert!(text.contains("value=\"10\" checked"));
    assert!(text.contains("acme/api"));
    assert!(!text.contains("description-error"));
    assert!(!text.contains("repo_ids-error"));
}

#[tokio::test]
async fn create_link_tampered_permission_is_reported_with_other_field_errors() {
    let (resp, calls) =
        post_create_link("description=&permission=Push&max_uses=0&repo_ids=10").await;

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(calls.lock().unwrap().is_empty());
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("id=\"permission-error\""));
    assert!(text.contains("id=\"description-error\""));
    assert!(text.contains("id=\"max_uses-error\""));
    assert_eq!(text.matches("aria-invalid=\"true\"").count(), 3);
    assert!(!text.contains("Bad Request"));
}

#[tokio::test]
async fn create_link_every_supported_permission_level_reaches_command() {
    for (raw, expected) in [
        ("pull", ghinvite_core::Permission::Pull),
        ("triage", ghinvite_core::Permission::Triage),
        ("push", ghinvite_core::Permission::Push),
        ("maintain", ghinvite_core::Permission::Maintain),
        ("admin", ghinvite_core::Permission::Admin),
    ] {
        let body = format!("description=AI+coding+workshop&permission={raw}&repo_ids=10");
        let (resp, calls) = post_create_link(body).await;

        assert_eq!(resp.status(), StatusCode::SEE_OTHER, "permission={raw:?}");
        let location = resp.headers().get("location").unwrap().to_str().unwrap();
        assert!(location.starts_with("/console/accounts/acme/links/"));
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "permission={raw:?}");
        let RecordedCommand::CreateInvitationLink { permission, .. } = &calls[0];
        assert_eq!(*permission, expected, "permission={raw:?}");
    }
}

#[tokio::test]
async fn create_link_approval_policy_checkbox_behaviour_is_unchanged() {
    // Checked means account admin approval is required.
    let (resp, calls) = post_create_link(
        "description=AI+coding+workshop&permission=pull&approval_required=true&repo_ids=10",
    )
    .await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    {
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let RecordedCommand::CreateInvitationLink {
            approval_required, ..
        } = &calls[0];
        assert!(*approval_required, "checked box requires admin approval");
    }

    // Unchecked (the key is absent from a native form POST) means auto-approve.
    let (resp, calls) =
        post_create_link("description=AI+coding+workshop&permission=pull&repo_ids=10").await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let RecordedCommand::CreateInvitationLink {
        approval_required, ..
    } = &calls[0];
    assert!(!*approval_required, "unchecked box auto-approves");
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
