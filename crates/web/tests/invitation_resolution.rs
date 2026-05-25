use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, TimeZone, Utc};
use domain::{
    Account, AccountType, InvitationLink, InvitationLinkId, InvitationLinkRepo, InvitationRequest,
    Permission, RequestId, RequestState, SelectedRepos, Slug, User,
};
use github::mocks::{Expectation, MockTransport};
use github::transport::{Method, Response};
use http_body_util::BodyExt;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use storage::Storage;
use tower::ServiceExt;
use web::commands::{
    CreateInvitationLink, CreateInvitationLinkOutput, DecideInvitationRequest, GhinviteCommands,
    OnboardInstallation, RecordInstallationUninstalled, RecordRepositorySelectionChange,
    RevokeInvitationLink, RouteGithubInvitationWebhook, SubmitInvitationRequest,
};
use web::{AppState, WebConfig, build_app};

const ACTIVE_SLUG: &str = "abcdEFGH01234567";
const UNKNOWN_SLUG: &str = "ZZZZZZZZZZZZZZZZ";
const CREATOR_ID: u64 = 701;
const REQUESTER_ID: u64 = 802;

#[derive(Clone, Debug, PartialEq, Eq)]
enum RecordedCommand {
    SubmitInvitationRequest {
        invitation_link_id: InvitationLinkId,
        requester_id: u64,
        justification: Option<String>,
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
    ) -> web::Result<CreateInvitationLinkOutput> {
        panic!("unexpected create_invitation_link command")
    }

    async fn revoke_invitation_link(&self, _command: RevokeInvitationLink) -> web::Result<()> {
        panic!("unexpected revoke_invitation_link command")
    }

    async fn submit_invitation_request(&self, command: SubmitInvitationRequest) -> web::Result<()> {
        self.calls
            .lock()
            .unwrap()
            .push(RecordedCommand::SubmitInvitationRequest {
                invitation_link_id: command.invitation_link_id,
                requester_id: command.requester_id,
                justification: command.justification,
            });
        Ok(())
    }

    async fn decide_invitation_request(
        &self,
        _command: DecideInvitationRequest,
    ) -> web::Result<()> {
        panic!("unexpected decide_invitation_request command")
    }

    async fn onboard_installation(&self, _command: OnboardInstallation) -> web::Result<()> {
        panic!("unexpected onboard_installation command")
    }

    async fn record_repository_selection_change(
        &self,
        _command: RecordRepositorySelectionChange,
    ) -> web::Result<()> {
        panic!("unexpected record_repository_selection_change command")
    }

    async fn record_installation_uninstalled(
        &self,
        _command: RecordInstallationUninstalled,
    ) -> web::Result<()> {
        panic!("unexpected record_installation_uninstalled command")
    }

    async fn route_github_invitation_webhook(
        &self,
        _command: RouteGithubInvitationWebhook,
    ) -> web::Result<()> {
        panic!("unexpected route_github_invitation_webhook command")
    }
}

fn dt(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

fn sample_account() -> Account {
    Account {
        installation_id: 1,
        account_id: 9001,
        account_login: "acme".into(),
        account_type: AccountType::Organization,
        installed_at: dt("2026-05-04T12:00:00Z"),
        uninstalled_at: None,
        selected_repos: SelectedRepos::All,
    }
}

fn sample_user(user_id: u64, login: &str) -> User {
    User {
        user_id,
        login: login.into(),
        avatar_url: None,
        last_seen_at: dt("2026-05-04T12:00:00Z"),
    }
}

fn active_link(slug: &str) -> InvitationLink {
    InvitationLink {
        id: InvitationLinkId::new(),
        slug: Slug::from_string(slug.to_string()).unwrap(),
        installation_id: 1,
        account_id: 9001,
        created_by: CREATOR_ID,
        created_at: dt("2026-05-04T12:00:00Z"),
        expires_at: None,
        max_uses: None,
        uses_count: 0,
        permission: Permission::Pull,
        approval_required: true,
        description: "AI coding workshop".into(),
        internal_note: None,
        revoked_at: None,
        revoked_by: None,
        repos: vec![InvitationLinkRepo {
            repo_id: 10,
            repo_full_name: "acme/api".into(),
        }],
    }
}

fn pending_request(id: RequestId, link: InvitationLinkId, requester_id: u64) -> InvitationRequest {
    request_with_state(id, link, requester_id, RequestState::Pending)
}

fn request_with_state(
    id: RequestId,
    link: InvitationLinkId,
    requester_id: u64,
    state: RequestState,
) -> InvitationRequest {
    InvitationRequest {
        id,
        invitation_link_id: link,
        requester_id,
        justification: None,
        state,
        decided_by: None,
        decided_at: None,
        decline_reason: None,
        created_at: dt("2026-05-04T12:30:00Z"),
    }
}

async fn build_test_app(
    link: InvitationLink,
    pending: Option<InvitationRequest>,
    mock: MockTransport,
) -> (axum::Router, Arc<Mutex<Vec<RecordedCommand>>>) {
    let storage = storage::SqlxStorage::in_memory().await.unwrap();
    storage
        .insert_installation(&sample_account())
        .await
        .unwrap();
    storage
        .upsert_user(&sample_user(CREATOR_ID, "creator"))
        .await
        .unwrap();
    storage
        .upsert_user(&sample_user(REQUESTER_ID, "octocat"))
        .await
        .unwrap();
    storage.insert_invitation_link(&link).await.unwrap();
    if let Some(request) = pending {
        storage
            .insert_invitation_request_and_increment_uses(&request)
            .await
            .unwrap();
    }

    let storage: Arc<dyn storage::Storage> = Arc::new(storage);
    let transport: Arc<dyn github::HttpTransport> = Arc::new(mock);
    let commands = Arc::new(RecordingCommands::default());
    let calls = commands.calls.clone();
    let state = AppState::new(storage, transport, commands, WebConfig::for_local_dev());
    let session_store = tower_sessions::MemoryStore::default();
    (build_app(state, session_store), calls)
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
                body: br#"{"access_token":"u_xxx","token_type":"bearer","scope":"read:user"}"#
                    .to_vec(),
            },
        },
        Expectation::ok_json(
            Method::Get,
            "https://api.github.com/user",
            serde_json::json!({"id": user_id, "login": login}),
        ),
    ]
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

async fn sign_in(app: axum::Router) -> String {
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
    session_cookie(&resp2, Some(cookie1))
}

#[tokio::test]
async fn landing_returns_not_found_for_malformed_slug() {
    let (app, _calls) = build_test_app(
        active_link(ACTIVE_SLUG),
        None,
        MockTransport::scripted(vec![]),
    )
    .await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/i/not-a-valid-slug")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn landing_returns_not_found_for_unknown_slug() {
    let (app, _calls) = build_test_app(
        active_link(ACTIVE_SLUG),
        None,
        MockTransport::scripted(vec![]),
    )
    .await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/i/{UNKNOWN_SLUG}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn landing_renders_active_link() {
    let (app, _calls) = build_test_app(
        active_link(ACTIVE_SLUG),
        None,
        MockTransport::scripted(vec![]),
    )
    .await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/i/{ACTIVE_SLUG}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("acme/api"));
    assert!(text.contains("Repository access request"));
    assert!(text.contains("GitHub sign-in confirms your identity"));
}

#[tokio::test]
async fn landing_with_existing_pending_request_still_renders_preview() {
    let link = active_link(ACTIVE_SLUG);
    let link_id = link.id;
    let pending = pending_request(RequestId::new(), link_id, REQUESTER_ID);
    let (app, _calls) = build_test_app(
        link,
        Some(pending),
        MockTransport::scripted(oauth_expectations("octocat", REQUESTER_ID)),
    )
    .await;
    let cookie = sign_in(app.clone()).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/i/{ACTIVE_SLUG}"))
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("acme/api"));
}

#[tokio::test]
async fn landing_returns_not_found_for_revoked_link() {
    let mut link = active_link(ACTIVE_SLUG);
    link.revoked_at = Some(dt("2026-05-05T12:00:00Z"));
    link.revoked_by = Some(CREATOR_ID);
    let (app, _calls) = build_test_app(link, None, MockTransport::scripted(vec![])).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/i/{ACTIVE_SLUG}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn landing_returns_not_found_for_expired_link() {
    let mut link = active_link(ACTIVE_SLUG);
    link.expires_at = Some(Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap());
    let (app, _calls) = build_test_app(link, None, MockTransport::scripted(vec![])).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/i/{ACTIVE_SLUG}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn landing_returns_not_found_for_exhausted_link() {
    let mut link = active_link(ACTIVE_SLUG);
    link.max_uses = Some(1);
    link.uses_count = 1;
    let (app, _calls) = build_test_app(link, None, MockTransport::scripted(vec![])).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/i/{ACTIVE_SLUG}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn request_form_for_unauthenticated_recipient_redirects_to_login() {
    let (app, _calls) = build_test_app(
        active_link(ACTIVE_SLUG),
        None,
        MockTransport::scripted(vec![]),
    )
    .await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/i/{ACTIVE_SLUG}/request"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(
        location,
        format!("/login?return_to=/i/{ACTIVE_SLUG}/request")
    );
}

#[tokio::test]
async fn request_form_with_existing_pending_request_redirects_to_pending_page() {
    let link = active_link(ACTIVE_SLUG);
    let link_id = link.id;
    let request_id = RequestId::new();
    let pending = pending_request(request_id, link_id, REQUESTER_ID);
    let (app, _calls) = build_test_app(
        link,
        Some(pending),
        MockTransport::scripted(oauth_expectations("octocat", REQUESTER_ID)),
    )
    .await;
    let cookie = sign_in(app.clone()).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/i/{ACTIVE_SLUG}/request"))
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(location, format!("/i/{ACTIVE_SLUG}/pending/{request_id}"));
}

#[tokio::test]
async fn request_form_with_other_recipient_pending_request_renders_form() {
    let link = active_link(ACTIVE_SLUG);
    let link_id = link.id;
    let pending = pending_request(RequestId::new(), link_id, CREATOR_ID);
    let (app, _calls) = build_test_app(
        link,
        Some(pending),
        MockTransport::scripted(oauth_expectations("octocat", REQUESTER_ID)),
    )
    .await;
    let cookie = sign_in(app.clone()).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/i/{ACTIVE_SLUG}/request"))
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("Submit request"));
    assert!(text.contains("Justification"));
    assert!(text.contains("visible to account admins"));
    assert!(text.contains("href=\"/logout\""));
    assert!(!text.contains("return_to=/i/"));
}

#[tokio::test]
async fn request_form_with_same_recipient_declined_request_renders_form() {
    let link = active_link(ACTIVE_SLUG);
    let link_id = link.id;
    let declined = request_with_state(
        RequestId::new(),
        link_id,
        REQUESTER_ID,
        RequestState::Declined,
    );
    let (app, _calls) = build_test_app(
        link,
        Some(declined),
        MockTransport::scripted(oauth_expectations("octocat", REQUESTER_ID)),
    )
    .await;
    let cookie = sign_in(app.clone()).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/i/{ACTIVE_SLUG}/request"))
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("Submit request"));
    assert!(text.contains("Justification"));
}

#[tokio::test]
async fn submit_with_existing_pending_request_redirects_to_pending_page_without_command() {
    let link = active_link(ACTIVE_SLUG);
    let link_id = link.id;
    let request_id = RequestId::new();
    let pending = pending_request(request_id, link_id, REQUESTER_ID);
    let (app, calls) = build_test_app(
        link,
        Some(pending),
        MockTransport::scripted(oauth_expectations("octocat", REQUESTER_ID)),
    )
    .await;
    let cookie = sign_in(app.clone()).await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/i/{ACTIVE_SLUG}/request"))
                .header("cookie", cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(
                    "request_id=01ARZ3NDEKTSV4RRFFQ69G5FAV&justification=again",
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(location, format!("/i/{ACTIVE_SLUG}/pending/{request_id}"));
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn submit_with_other_recipient_pending_request_sends_command() {
    let link = active_link(ACTIVE_SLUG);
    let link_id = link.id;
    let pending = pending_request(RequestId::new(), link_id, CREATOR_ID);
    let (app, calls) = build_test_app(
        link,
        Some(pending),
        MockTransport::scripted(oauth_expectations("octocat", REQUESTER_ID)),
    )
    .await;
    let cookie = sign_in(app.clone()).await;
    let posted_request_id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/i/{ACTIVE_SLUG}/request"))
                .header("cookie", cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!(
                    "request_id={posted_request_id}&justification=ship-it"
                )))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(
        location,
        format!("/i/{ACTIVE_SLUG}/pending/{posted_request_id}")
    );
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        &[RecordedCommand::SubmitInvitationRequest {
            invitation_link_id: link_id,
            requester_id: REQUESTER_ID,
            justification: Some("ship-it".into()),
        }]
    );
}
