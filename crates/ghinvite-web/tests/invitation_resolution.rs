use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, TimeZone, Utc};
use ghinvite_core::storage::Storage;
use ghinvite_core::{
    Account, AccountType, InvitationLink, InvitationLinkId, InvitationLinkRepo, InvitationRequest,
    Permission, RequestId, RequestState, SelectedRepos, Slug, User,
};
use ghinvite_github::mocks::{Expectation, MockTransport};
use ghinvite_github::transport::{Method, Response};
use ghinvite_web::commands::{
    CreateInvitationLink, CreateInvitationLinkOutput, DecideInvitationRequest, GhinviteCommands,
    OnboardInstallation, RecordInstallationUninstalled, RecordRepositorySelectionChange,
    RevokeInvitationLink, RouteGithubInvitationWebhook, SubmitInvitationRequest,
    UpdateInvitationLinkMetadata,
};
use ghinvite_web::{AppState, WebConfig, build_app};
use http_body_util::BodyExt;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

const ACTIVE_SLUG: &str = "abcdEFGH01234567";
const UNKNOWN_SLUG: &str = "ZZZZZZZZZZZZZZZZ";
const CREATOR_ID: u64 = 701;
const REQUESTER_ID: u64 = 802;

fn encoded_return_to(path: &str) -> String {
    url::form_urlencoded::byte_serialize(path.as_bytes()).collect()
}

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
        command: SubmitInvitationRequest,
    ) -> ghinvite_web::Result<()> {
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
    build_test_app_with_requests(link, pending.into_iter().collect(), mock).await
}

async fn build_test_app_with_requests(
    link: InvitationLink,
    requests: Vec<InvitationRequest>,
    mock: MockTransport,
) -> (axum::Router, Arc<Mutex<Vec<RecordedCommand>>>) {
    let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
        .await
        .unwrap();
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
    for request in requests {
        storage
            .insert_invitation_request_and_increment_uses(&request)
            .await
            .unwrap();
    }

    let storage: Arc<dyn ghinvite_core::storage::Storage> = Arc::new(storage);
    let transport: Arc<dyn ghinvite_github::HttpTransport> = Arc::new(mock);
    let commands = Arc::new(RecordingCommands::default());
    let calls = commands.calls.clone();
    let state = AppState::new(storage, transport, commands, WebConfig::for_local_dev());
    let session_store = tower_sessions::MemoryStore::default();
    (build_app(state, session_store), calls)
}

fn request_with_state_at(
    id: RequestId,
    link: InvitationLinkId,
    requester_id: u64,
    state: RequestState,
    created_at: &str,
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
        created_at: dt(created_at),
    }
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

fn assert_notice_above_form(text: &str, notice: &str) {
    let notice_pos = text.find(notice).expect("retry notice rendered");
    let form_pos = text.find("Justification").expect("request form rendered");
    assert!(
        notice_pos < form_pos,
        "retry notice should appear above form"
    );
}

#[tokio::test]
async fn landing_unauthenticated_redirects_to_login_for_active_slug() {
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

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(
        location,
        format!(
            "/login?return_to={}",
            encoded_return_to(&format!("/i/{ACTIVE_SLUG}"))
        )
    );
}

#[tokio::test]
async fn landing_unauthenticated_redirects_to_login_for_bad_or_inactive_slugs() {
    let mut revoked = active_link(ACTIVE_SLUG);
    revoked.revoked_at = Some(dt("2026-05-05T12:00:00Z"));
    revoked.revoked_by = Some(CREATOR_ID);

    let mut expired = active_link(ACTIVE_SLUG);
    expired.expires_at = Some(Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap());

    let mut exhausted = active_link(ACTIVE_SLUG);
    exhausted.max_uses = Some(1);
    exhausted.uses_count = 1;

    for (link, uri, expected_return_to) in [
        (
            active_link(ACTIVE_SLUG),
            "/i/not-a-valid-slug".to_string(),
            "/i/not-a-valid-slug".to_string(),
        ),
        (
            active_link(ACTIVE_SLUG),
            format!("/i/{UNKNOWN_SLUG}"),
            format!("/i/{UNKNOWN_SLUG}"),
        ),
        (
            revoked,
            format!("/i/{ACTIVE_SLUG}"),
            format!("/i/{ACTIVE_SLUG}"),
        ),
        (
            expired,
            format!("/i/{ACTIVE_SLUG}"),
            format!("/i/{ACTIVE_SLUG}"),
        ),
        (
            exhausted,
            format!("/i/{ACTIVE_SLUG}"),
            format!("/i/{ACTIVE_SLUG}"),
        ),
    ] {
        let (app, _calls) = build_test_app(link, None, MockTransport::scripted(vec![])).await;
        let resp = app
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        let location = resp.headers().get("location").unwrap().to_str().unwrap();
        assert_eq!(
            location,
            format!(
                "/login?return_to={}",
                encoded_return_to(&expected_return_to)
            )
        );
    }
}

#[tokio::test]
async fn submit_unauthenticated_redirects_to_login_for_canonical_page() {
    let (app, calls) = build_test_app(
        active_link(ACTIVE_SLUG),
        None,
        MockTransport::scripted(vec![]),
    )
    .await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/i/{ACTIVE_SLUG}"))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(
                    "request_id=01ARZ3NDEKTSV4RRFFQ69G5FAV&justification=ship",
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(
        location,
        format!(
            "/login?return_to={}",
            encoded_return_to(&format!("/i/{ACTIVE_SLUG}"))
        )
    );
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn unsupported_nested_invitation_routes_authenticate_before_404() {
    let (app, _calls) = build_test_app(
        active_link(ACTIVE_SLUG),
        None,
        MockTransport::scripted(oauth_expectations("octocat", REQUESTER_ID)),
    )
    .await;

    let resp = app
        .clone()
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
        format!(
            "/login?return_to={}",
            encoded_return_to(&format!("/i/{ACTIVE_SLUG}/request"))
        )
    );

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/i/{ACTIVE_SLUG}/request?foo=1&bar=2"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(
        location,
        format!(
            "/login?return_to={}",
            encoded_return_to(&format!("/i/{ACTIVE_SLUG}/request?foo=1&bar=2"))
        )
    );

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

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("Page not found"));
    assert!(text.contains("The link may be incorrect or no longer available."));
    assert!(!text.contains("Request repository access"));
    assert!(!text.contains("Submit request"));
}

#[tokio::test]
async fn signed_in_landing_renders_merged_request_form() {
    let (app, _calls) = build_test_app(
        active_link(ACTIVE_SLUG),
        None,
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
    assert!(text.contains("Request repository access"));
    assert!(text.contains("Review the repositories and submit a request as @octocat."));
    assert!(text.contains("acme/api"));
    assert!(text.contains("Permission: Read (pull)"));
    assert!(text.contains("Justification"));
    assert!(text.contains("action=\"/i/abcdEFGH01234567\""));
    assert!(!text.contains("AI coding workshop"));
    assert!(!text.contains("http-equiv=\"refresh\""));
}

#[tokio::test]
async fn signed_in_landing_carries_csp_and_only_external_script() {
    let (app, _calls) = build_test_app(
        active_link(ACTIVE_SLUG),
        None,
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
    assert_eq!(
        resp.headers()
            .get("content-security-policy")
            .map(|v| v.to_str().unwrap()),
        Some(ghinvite_web::middleware::csp::CONTENT_SECURITY_POLICY)
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("Request repository access"));
    let external = "<script src=\"/static/app.js\"></script>";
    assert!(text.contains(external));
    assert_eq!(
        text.matches("<script").count(),
        text.matches(external).count(),
        "invitation HTML contains an inline <script> block"
    );
}

#[tokio::test]
async fn signed_in_landing_shows_pending_status_instead_of_form() {
    let link = active_link(ACTIVE_SLUG);
    let pending = pending_request(RequestId::new(), link.id, REQUESTER_ID);
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
    assert!(text.contains("Awaiting review"));
    assert!(text.contains("account admins have your request"));
    assert!(text.contains("<meta http-equiv=\"refresh\" content=\"20\""));
    assert!(text.contains("Check again"));
    assert!(!text.contains("Submit request"));
    assert!(!text.contains("textarea"));
}

#[tokio::test]
async fn signed_in_landing_shows_approved_status_instead_of_form() {
    let link = active_link(ACTIVE_SLUG);
    let approved = request_with_state(
        RequestId::new(),
        link.id,
        REQUESTER_ID,
        RequestState::Approved,
    );
    let (app, _calls) = build_test_app(
        link,
        Some(approved),
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
    assert!(text.contains("Approved"));
    assert!(text.contains("GitHub notifications and email"));
    assert!(!text.contains("Submit request"));
    assert!(!text.contains("http-equiv=\"refresh\""));
}

#[tokio::test]
async fn signed_in_landing_shows_newest_retry_notice_with_form() {
    let link = active_link(ACTIVE_SLUG);
    let requests = vec![
        request_with_state_at(
            RequestId::new(),
            link.id,
            REQUESTER_ID,
            RequestState::Cancelled,
            "2026-05-04T12:40:00Z",
        ),
        request_with_state_at(
            RequestId::new(),
            link.id,
            REQUESTER_ID,
            RequestState::Declined,
            "2026-05-04T12:30:00Z",
        ),
    ];
    let (app, _calls) = build_test_app_with_requests(
        link,
        requests,
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
    let notice = "Your previous request was cancelled.";
    assert!(text.contains(notice));
    assert!(!text.contains("Your previous request was declined."));
    assert_notice_above_form(&text, notice);
    assert!(text.contains("Submit request"));
}

#[tokio::test]
async fn signed_in_landing_shows_declined_retry_notice_above_form() {
    let link = active_link(ACTIVE_SLUG);
    let declined = request_with_state(
        RequestId::new(),
        link.id,
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
    let notice = "Your previous request was declined.";
    assert!(text.contains(notice));
    assert_notice_above_form(&text, notice);
    assert!(text.contains("Submit request"));
}

#[tokio::test]
async fn signed_in_landing_shows_expired_retry_notice_with_form() {
    let link = active_link(ACTIVE_SLUG);
    let expired = request_with_state(
        RequestId::new(),
        link.id,
        REQUESTER_ID,
        RequestState::Expired,
    );
    let (app, _calls) = build_test_app(
        link,
        Some(expired),
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
    let notice = "Your previous request expired.";
    assert!(text.contains(notice));
    assert_notice_above_form(&text, notice);
    assert!(text.contains("Submit request"));
    assert!(text.contains("Justification"));
}

#[tokio::test]
async fn inactive_link_with_existing_pending_request_shows_status() {
    let mut link = active_link(ACTIVE_SLUG);
    link.revoked_at = Some(dt("2026-05-05T12:00:00Z"));
    link.revoked_by = Some(CREATOR_ID);
    let pending = pending_request(RequestId::new(), link.id, REQUESTER_ID);
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
    assert!(text.contains("Awaiting review"));
    assert!(text.contains("<meta http-equiv=\"refresh\" content=\"20\""));
    assert!(!text.contains("revoked"));
}

#[tokio::test]
async fn inactive_link_with_existing_approved_request_shows_status() {
    let mut link = active_link(ACTIVE_SLUG);
    link.revoked_at = Some(dt("2026-05-05T12:00:00Z"));
    link.revoked_by = Some(CREATOR_ID);
    let approved = request_with_state(
        RequestId::new(),
        link.id,
        REQUESTER_ID,
        RequestState::Approved,
    );
    let (app, _calls) = build_test_app(
        link,
        Some(approved),
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
    assert!(text.contains("Approved"));
    assert!(text.contains("GitHub notifications and email"));
    assert!(!text.contains("revoked"));
    assert!(!text.contains("Submit request"));
}

#[tokio::test]
async fn inactive_link_without_non_repeatable_request_returns_generic_404() {
    let mut expired = active_link(ACTIVE_SLUG);
    expired.expires_at = Some(Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap());

    let mut revoked = active_link(ACTIVE_SLUG);
    revoked.revoked_at = Some(dt("2026-05-05T12:00:00Z"));
    revoked.revoked_by = Some(CREATOR_ID);

    let mut exhausted = active_link(ACTIVE_SLUG);
    exhausted.max_uses = Some(1);
    exhausted.uses_count = 1;

    for (link, hidden_detail) in [
        (expired, "expired"),
        (revoked, "revoked"),
        (exhausted, "exhausted"),
    ] {
        let declined = request_with_state(
            RequestId::new(),
            link.id,
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
                    .uri(format!("/i/{ACTIVE_SLUG}"))
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
        assert!(!text.contains(hidden_detail));
    }
}

#[tokio::test]
async fn submit_creates_request_and_redirects_to_canonical_page() {
    let link = active_link(ACTIVE_SLUG);
    let link_id = link.id;
    let (app, calls) = build_test_app(
        link,
        None,
        MockTransport::scripted(oauth_expectations("octocat", REQUESTER_ID)),
    )
    .await;
    let cookie = sign_in(app.clone()).await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/i/{ACTIVE_SLUG}"))
                .header("cookie", cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(
                    "request_id=01ARZ3NDEKTSV4RRFFQ69G5FAV&justification=ship-it",
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(location, format!("/i/{ACTIVE_SLUG}"));
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        &[RecordedCommand::SubmitInvitationRequest {
            invitation_link_id: link_id,
            requester_id: REQUESTER_ID,
            justification: Some("ship-it".into()),
        }]
    );
}

#[tokio::test]
async fn submit_with_existing_pending_request_redirects_without_command() {
    let link = active_link(ACTIVE_SLUG);
    let pending = pending_request(RequestId::new(), link.id, REQUESTER_ID);
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
                .uri(format!("/i/{ACTIVE_SLUG}"))
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
    assert_eq!(location, format!("/i/{ACTIVE_SLUG}"));
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn submit_with_existing_declined_request_sends_command() {
    let link = active_link(ACTIVE_SLUG);
    let link_id = link.id;
    let declined = request_with_state(
        RequestId::new(),
        link_id,
        REQUESTER_ID,
        RequestState::Declined,
    );
    let (app, calls) = build_test_app(
        link,
        Some(declined),
        MockTransport::scripted(oauth_expectations("octocat", REQUESTER_ID)),
    )
    .await;
    let cookie = sign_in(app.clone()).await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/i/{ACTIVE_SLUG}"))
                .header("cookie", cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(
                    "request_id=01ARZ3NDEKTSV4RRFFQ69G5FAV&justification=retry",
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(location, format!("/i/{ACTIVE_SLUG}"));
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        &[RecordedCommand::SubmitInvitationRequest {
            invitation_link_id: link_id,
            requester_id: REQUESTER_ID,
            justification: Some("retry".into()),
        }]
    );
}

#[tokio::test]
async fn submit_with_existing_approved_request_redirects_without_command() {
    let link = active_link(ACTIVE_SLUG);
    let approved = request_with_state(
        RequestId::new(),
        link.id,
        REQUESTER_ID,
        RequestState::Approved,
    );
    let (app, calls) = build_test_app(
        link,
        Some(approved),
        MockTransport::scripted(oauth_expectations("octocat", REQUESTER_ID)),
    )
    .await;
    let cookie = sign_in(app.clone()).await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/i/{ACTIVE_SLUG}"))
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
    assert_eq!(location, format!("/i/{ACTIVE_SLUG}"));
    assert!(calls.lock().unwrap().is_empty());
}
