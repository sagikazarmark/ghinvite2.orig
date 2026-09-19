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

mod common;

#[path = "console_flow/read_recovery.rs"]
mod read_recovery;

#[path = "console_flow/mutation_recovery.rs"]
mod mutation_recovery;

#[path = "console_flow/request_history.rs"]
mod request_history;

async fn deadline_queue_app(
    expires_at: Option<&str>,
) -> (
    axum::Router,
    String,
    Arc<ghinvite_storage_sqlx::SqlxStorage>,
    ghinvite_core::storage::projection::ProjectionEnvelope,
) {
    use ghinvite_core::storage::projection::ProjectionStorage;
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    storage
        .insert_installation(&identity_account(42, "octocat", AccountType::User))
        .await
        .unwrap();
    for (user_id, login) in [(42, "octocat"), (99, "requester")] {
        storage
            .upsert_user(&ghinvite_core::User {
                user_id,
                login: login.into(),
                avatar_url: None,
                last_seen_at: Utc::now(),
            })
            .await
            .unwrap();
    }
    let link = ghinvite_core::InvitationLinkId::new();
    let request = ghinvite_core::RequestId::new();
    // A historical/custom deadline, deliberately not today's seven-day policy.
    let envelope = serde_json::from_value(serde_json::json!({
        "version":1, "transition_id":format!("v1/link/{link}/2"),
        "link":{"link_id":link,"revision":2,"uses":1,"invitation_code":"DeadlineQueue001",
            "created_at":"2026-01-01T00:00:00Z", "revoked_at":null,"revoked_by":null,
            "creation":{"version":1,"link_id":link,"admin":{"account_id":42,"user_id":42},
                "account_id":42,"installation_id":77,"description":"Deadline fixture",
                "internal_note":null,"expires_at":expires_at,"max_uses":null,"permission":"pull",
                "approval_required":true,"repos":[{"repo_id":10,"repo_full_name":"octocat/api"}]}},
        "requests":[{"request_id":request,"link_id":link,"account_id":42,"requester_id":99,
            "state":"pending","admitted_at":"2026-01-01T01:00:00Z",
            "decision_deadline":"2026-01-03T12:34:56Z","revision":1}],"events":[]
    }))
    .unwrap();
    storage.apply_transition(&envelope).await.unwrap();
    let state = AppState::new(
        storage.clone(),
        Arc::new(MockTransport::scripted(oauth_sign_in_expectations())),
        Arc::new(RecordingCommands::default()),
        WebConfig::for_local_dev_with_secret([7; 32]),
    );
    let (app, cookie) = sign_in(build_app(state, tower_sessions::MemoryStore::default())).await;
    (app, cookie, storage, envelope)
}

#[tokio::test]
async fn queue_pages_navigate_without_offset_drift_and_use_description_first() {
    use ghinvite_core::storage::projection::ProjectionStorage;
    let (app, cookie, storage, mut envelope) = deadline_queue_app(None).await;
    let template = envelope.requests[0].clone();
    envelope.link.revision += 1;
    envelope.link.uses = 54;
    envelope.transition_id = format!("v1/link/{}/3", envelope.link.link_id);
    envelope.requests = (1..=53)
        .map(|n| {
            let mut request = template.clone();
            request.request_id = format!("01ARZ3NDEKTSV4RRFFQ69G5{n:03}").parse().unwrap();
            request.justification = Some(format!("Queue item {n:03}"));
            request
        })
        .collect();
    for request in &envelope.requests {
        let mut transition = envelope.clone();
        transition.requests = vec![request.clone()];
        storage.apply_transition(&transition).await.unwrap();
    }
    let path = "/console/accounts/octocat/requests";
    let html = response_html(identity_request(&app, &cookie, "GET", path).await).await;
    assert_eq!(html.matches("Approve request").count(), 25);
    assert!(html.contains("Queue item 001"));
    assert!(!html.contains("Queue item 026"));
    assert!(html.contains(">Deadline fixture</a>"));
    let next = html
        .split("rel=\"next\" href=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .replace("&amp;", "&");
    for request in &envelope.requests[..26] {
        storage
            .record_request_decision(&ghinvite_core::storage::RequestDecision {
                request_id: request.request_id,
                state: ghinvite_core::RequestState::Declined,
                decided_by: Some(42),
                decided_at: Utc::now(),
                decline_reason: None,
            })
            .await
            .unwrap();
    }
    let html = response_html(identity_request(&app, &cookie, "GET", &next).await).await;
    assert!(html.contains("Queue item 027"));
    assert!(!html.contains("Queue item 025"));
    assert!(html.contains("Back to oldest requests"));
    assert_eq!(
        identity_request(&app, &cookie, "GET", &format!("{path}?after=broken"))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        identity_request(&app, &cookie, "GET", &next.replace("octocat", "unknown"))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn queue_shows_recorded_decision_deadline_for_non_expiring_link() {
    let (app, cookie, _, _) = deadline_queue_app(None).await;
    let response =
        identity_request(&app, &cookie, "GET", "/console/accounts/octocat/requests").await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = response_html(response).await;
    assert!(html.contains("Decision deadline: 2026-01-03 12:34:56 UTC"));
    assert!(html.contains("Invitation link expiration: No expiration"));
    assert!(
        html.contains("The decision deadline shown is the recorded deadline for this request.")
    );
    assert!(!html.contains("Link expiration only stops new requests."));
    assert!(!html.contains("Expires:"));
}

#[tokio::test]
async fn queue_keeps_historical_deadline_independent_of_earlier_or_later_link_expiration() {
    for (expiration, label) in [
        ("2026-01-02T00:00:00Z", "2026-01-02 00:00:00 UTC"),
        ("2026-02-01T00:00:00Z", "2026-02-01 00:00:00 UTC"),
    ] {
        let (app, cookie, _, _) = deadline_queue_app(Some(expiration)).await;
        let response =
            identity_request(&app, &cookie, "GET", "/console/accounts/octocat/requests").await;
        assert_eq!(response.status(), StatusCode::OK);
        let html = response_html(response).await;
        assert!(html.contains("Decision deadline: 2026-01-03 12:34:56 UTC"));
        assert!(html.contains(&format!("Invitation link expiration: {label}")));
    }
}

#[tokio::test]
async fn queue_does_not_invent_a_deadline_for_missing_historical_data() {
    let (app, cookie, storage, envelope) = deadline_queue_app(None).await;
    // Legacy/migrated rows may have no recorded deadline. The old insert path
    // represents those rows without requiring a fabricated projection snapshot.
    let mut historical = storage
        .get_invitation_request(envelope.requests[0].request_id)
        .await
        .unwrap()
        .unwrap();
    historical.id = ghinvite_core::RequestId::new();
    historical.requester_id = 42;
    historical.decision_deadline = None;
    storage
        .insert_invitation_request_and_increment_uses(&historical)
        .await
        .unwrap();
    let response =
        identity_request(&app, &cookie, "GET", "/console/accounts/octocat/requests").await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = response_html(response).await;
    assert!(html.contains("Decision deadline: Unavailable"));
    assert!(!html.contains("Decision deadline: No expiration"));
    assert!(
        !html.contains("2026-01-08"),
        "must not recompute from current seven-day policy"
    );
}

#[tokio::test]
async fn queue_excludes_auto_approved_requests_without_a_decision_deadline() {
    use ghinvite_core::storage::projection::ProjectionStorage;
    let (app, cookie, storage, mut envelope) = deadline_queue_app(None).await;
    let link = ghinvite_core::InvitationLinkId::new();
    envelope.link.link_id = link;
    envelope.link.creation.link_id = link;
    envelope.link.creation.approval_required = false;
    envelope.link.invitation_code = "AutoDeadline0001".into();
    envelope.transition_id = format!("v1/link/{link}/2");
    envelope.requests[0].link_id = link;
    envelope.requests[0].request_id = ghinvite_core::RequestId::new();
    envelope.requests[0].state = ghinvite_core::RequestState::Approved;
    envelope.requests[0].decision_deadline = None;
    envelope.requests[0].justification = Some("Auto-approved request fixture".into());
    storage.apply_transition(&envelope).await.unwrap();
    let response =
        identity_request(&app, &cookie, "GET", "/console/accounts/octocat/requests").await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = response_html(response).await;
    assert!(!html.contains("Auto-approved request fixture"));
    assert!(!html.contains("AutoDeadline0001"));
    assert!(html.contains("Decision deadline: 2026-01-03 12:34:56 UTC"));
}

#[tokio::test]
async fn overdue_queue_row_warns_about_projection_lag_without_claiming_a_decision() {
    let (app, cookie, _, _) = deadline_queue_app(None).await;
    let response =
        identity_request(&app, &cookie, "GET", "/console/accounts/octocat/requests").await;
    let html = response_html(response).await;
    assert!(html.contains("Decision deadline passed. Queue updates may be delayed."));
    assert!(!html.contains("decisions are checked against the authoritative request state"));
    assert!(html.contains("Approve request"));
    assert!(html.contains("Decline request"));
}

#[tokio::test]
async fn isolated_admin_metadata_and_revoke_use_authority_before_projection() {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};
    let ingress = MockServer::start().await;
    let id = ghinvite_core::InvitationLinkId::new();
    let snapshot = serde_json::json!({"link_id": id, "creation": {
        "version": 1, "link_id": id, "admin": {"account_id": 42, "user_id": 42}, "account_id": 42,
        "installation_id": 1, "description": "Original", "internal_note": null, "expires_at": null,
        "max_uses": null, "permission": "pull", "approval_required": true,
        "repos": [{"repo_id": 1, "repo_full_name": "octocat/api"}]},
        "metadata": {"description": "Authoritative details", "internal_note": null},
        "invitation_code": "abcdEFGH01234567", "created_at": "2026-09-14T12:00:00Z",
        "uses": 0, "revision": 2, "revoked_at": null, "revoked_by": null});
    for method in ["link_status", "update_metadata", "revoke"] {
        Mock::given(path(format!("/InvitationLinkV1/{id}/{method}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(snapshot.clone()))
            .mount(&ingress)
            .await;
    }
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    storage
        .insert_installation(&identity_account(42, "octocat", AccountType::User))
        .await
        .unwrap();
    let state = AppState::new(
        storage,
        Arc::new(MockTransport::scripted(oauth_sign_in_expectations())),
        Arc::new(RecordingCommands::default()),
        WebConfig::for_local_dev_with_secret([7; 32]),
    )
    .with_admission(Arc::new(RestateClient::new(ingress.uri()).unwrap()));
    let (app, cookie) = sign_in(build_app(state, tower_sessions::MemoryStore::default())).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/console/accounts/octocat/links/{id}"))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response_html(response)
            .await
            .contains("Authoritative details")
    );
    for action in ["edit", "revoke"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/console/accounts/octocat/links/{id}/{action}"))
                    .header("cookie", &cookie)
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(format!(
                        "csrf_token={csrf}&description=Changed&account_id=666&user_id=666"
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
    }
    for request in ingress.received_requests().await.unwrap() {
        assert!(!request.url.path().ends_with("/send"));
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(
            body["admin"],
            serde_json::json!({"account_id": 42, "user_id": 42})
        );
    }
}

#[tokio::test]
async fn pending_decision_remains_accessible_after_uninstall() {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};
    let ingress = MockServer::start().await;
    let link = ghinvite_core::InvitationLinkId::new();
    let request = ghinvite_core::RequestId::new();
    Mock::given(path(format!("/InvitationLinkV1/{link}/decide")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"outcome":"applied","request":{
            "request_id":request,"link_id":link,"account_id":42,"requester_id":99,"state":"approved",
            "admitted_at":"2026-09-14T12:00:00Z","decision_deadline":"2026-09-21T12:00:00Z","revision":2}})))
        .expect(1).mount(&ingress).await;
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let account = identity_account(42, "octocat", AccountType::User);
    storage.insert_installation(&account).await.unwrap();
    storage
        .mark_installation_uninstalled(account.installation_id, Utc::now())
        .await
        .unwrap();
    let state = AppState::new(
        storage,
        Arc::new(MockTransport::scripted(oauth_sign_in_expectations())),
        Arc::new(RecordingCommands::default()),
        WebConfig::for_local_dev_with_secret([7; 32]),
    )
    .with_admission(Arc::new(RestateClient::new(ingress.uri()).unwrap()));
    let (app, cookie) = sign_in(build_app(state, tower_sessions::MemoryStore::default())).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/console/accounts/octocat/requests/{request}/approve"
                ))
                .header("cookie", &cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!(
                    "csrf_token={csrf}&link_id={link}&operation_id={}",
                    ghinvite_core::RequestId::new()
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let response =
        identity_request(&app, &cookie, "GET", "/console/accounts/octocat/requests").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response_html(response)
            .await
            .contains("Unavailable repositories may block delivery")
    );
}

#[tokio::test]
async fn isolated_lifecycle_decision_uses_authorized_identity_and_reports_expiry() {
    use ghinvite_core::request_lifecycle::*;
    use ghinvite_core::storage::projection::RequestSnapshot;
    struct Lifecycle(Arc<Mutex<Vec<DecideRequest>>>);
    #[async_trait::async_trait]
    impl ghinvite_web::lifecycle::RequestLifecycle for Lifecycle {
        async fn decide(&self, command: DecideRequest) -> ghinvite_web::Result<DecisionReceipt> {
            self.0.lock().unwrap().push(command.clone());
            Ok(DecisionReceipt { outcome: DecisionOutcome::Incompatible,
                request: serde_json::from_value(serde_json::json!({"request_id": command.request_id,
                    "link_id": command.link_id, "account_id": 42, "requester_id": 99,
                    "justification": null, "state": "expired", "admitted_at": "2026-01-01T00:00:00Z",
                    "decision_deadline": "2026-01-08T00:00:00Z", "revision": 2})).unwrap() })
        }
        async fn status(&self, _: RequestStatus) -> ghinvite_web::Result<RequestSnapshot> {
            panic!("unexpected status")
        }
    }
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    storage
        .insert_installation(&identity_account(42, "octocat", AccountType::User))
        .await
        .unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let state = AppState::new(
        storage,
        Arc::new(MockTransport::scripted(oauth_sign_in_expectations())),
        Arc::new(RecordingCommands::default()),
        WebConfig::for_local_dev_with_secret([7; 32]),
    )
    .with_request_lifecycle(Arc::new(Lifecycle(calls.clone())));
    let (app, cookie) = sign_in(build_app(state, tower_sessions::MemoryStore::default())).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let request = ghinvite_core::RequestId::new();
    let link = ghinvite_core::InvitationLinkId::new();
    let operation = ghinvite_core::RequestId::new();
    // Missing projection is not evidence that the acknowledged request is absent.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/console/accounts/octocat/requests/{request}/approve"
                ))
                .header("cookie", &cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!(
                    "csrf_token={csrf}&link_id={link}&operation_id={operation}&user_id=666"
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let html = response_html(response).await;
    assert!(html.contains("expired"));
    assert!(!html.contains("Request approved"));
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].admin.user_id, 42);
    assert_eq!(calls[0].admin.account_id, 42);
    assert_eq!(calls[0].link_id, link);
    assert_eq!(
        String::from(calls[0].operation_id.clone()),
        operation.to_string()
    );
}

#[tokio::test]
async fn console_mutations_reject_missing_invalid_and_sibling_origin_tokens() {
    let (app, cookie, calls) =
        build_signed_in_admin_app_with_recording_commands(oauth_expectations()).await;
    for path in [
        "/console/accounts/acme/links",
        "/console/accounts/acme/links/01ARZ3NDEKTSV4RRFFQ69G5FAV/edit",
        "/console/accounts/acme/links/01ARZ3NDEKTSV4RRFFQ69G5FAV/revoke",
        "/console/accounts/acme/requests/01ARZ3NDEKTSV4RRFFQ69G5FAV/approve",
        "/console/accounts/acme/requests/01ARZ3NDEKTSV4RRFFQ69G5FAV/decline",
    ] {
        for body in ["", "csrf_token=incorrect"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(path)
                        .header("cookie", &cookie)
                        .header("origin", "https://evil.ghinvite.test")
                        .header("sec-fetch-site", "same-site")
                        .header("content-type", "application/x-www-form-urlencoded")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
            assert_eq!(
                response_html(response).await,
                "Invalid CSRF token. Reload the page and try again."
            );
        }
    }
    assert!(calls.lock().unwrap().is_empty());
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
            serde_json::json!({"role": "admin", "state": "active", "organization": {"id": 9001}}),
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

fn identity_account(account_id: u64, login: &str, account_type: AccountType) -> Account {
    Account {
        installation_id: 77,
        account_id,
        account_login: login.into(),
        account_type,
        installed_at: Utc::now(),
        uninstalled_at: None,
        selected_repos: SelectedRepos::All,
    }
}

async fn identity_app(account: &Account, expectations: Vec<Expectation>) -> (axum::Router, String) {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    storage.insert_installation(account).await.unwrap();
    let state = AppState::new(
        storage,
        Arc::new(MockTransport::scripted(expectations)),
        Arc::new(RecordingCommands::default()),
        WebConfig::for_local_dev_with_secret([7; 32]),
    );
    sign_in(build_app(state, tower_sessions::MemoryStore::default())).await
}

async fn identity_request(
    app: &axum::Router,
    cookie: &str,
    method: &str,
    path: &str,
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("cookie", cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn response_html(response: axum::response::Response) -> String {
    String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap()
}

#[tokio::test]
async fn personal_account_reused_login_cannot_authorize_direct_reads_or_mutations() {
    let account = identity_account(999, "octocat", AccountType::User);
    let (app, cookie) = identity_app(&account, oauth_sign_in_expectations()).await;
    for (method, path) in [
        ("GET", "/console/accounts/octocat"),
        ("GET", "/console/accounts/octocat/links"),
        ("POST", "/console/accounts/octocat/links"),
        ("GET", "/console/accounts/octocat/audit"),
    ] {
        let response = identity_request(&app, &cookie, method, path).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {path}");
        let html = response_html(response).await;
        assert!(!html.contains("console-sidebar"));
    }
}

fn visible_account(account: &Account) -> Expectation {
    let account_type = match account.account_type {
        AccountType::User => "User",
        AccountType::Organization => "Organization",
    };
    Expectation::ok_json(
        Method::Get,
        "https://api.github.com/user/installations?per_page=100",
        serde_json::json!({
            "total_count": 1,
            "installations": [{
                "id": account.installation_id,
                "account": {"id": account.account_id, "login": account.account_login, "type": account_type},
                "repository_selection": "all",
                "target_type": account_type,
                "target_id": account.account_id
            }]
        }),
    )
}

#[tokio::test]
async fn personal_console_discovery_requires_identity_even_after_rename_or_name_reuse() {
    for (id, stored_login, authorized) in [
        (42, "octocat", true),
        (42, "old-octocat", true),
        (999, "octocat", false),
    ] {
        let account = identity_account(id, stored_login, AccountType::User);
        let mut expectations = oauth_sign_in_expectations();
        expectations.push(visible_account(&account));
        let (app, cookie) = identity_app(&account, expectations).await;
        let response = identity_request(&app, &cookie, "GET", "/console").await;
        if authorized {
            assert_eq!(response.status(), StatusCode::SEE_OTHER);
            assert_eq!(
                response.headers()["location"],
                format!("/console/accounts/{stored_login}")
            );
            let direct = identity_request(
                &app,
                &cookie,
                "GET",
                &format!("/console/accounts/{stored_login}"),
            )
            .await;
            assert_eq!(direct.status(), StatusCode::OK);
        } else {
            assert_eq!(response.status(), StatusCode::OK);
            let html = response_html(response).await;
            assert!(!html.contains("href=\"/console/accounts/octocat\""));
        }
    }
}

fn org_membership(login: &str, organization_id: u64, organization_login: &str) -> Expectation {
    Expectation::ok_json(
        Method::Get,
        format!("https://api.github.com/user/memberships/orgs/{login}"),
        serde_json::json!({
            "role": "admin", "state": "active",
            "organization": {"id": organization_id, "login": organization_login}
        }),
    )
}

#[tokio::test]
async fn organization_name_reuse_cannot_authorize_discovery_reads_or_mutations() {
    let account = identity_account(9001, "acme", AccountType::Organization);
    let mut expectations = oauth_sign_in_expectations();
    expectations.push(org_membership("acme", 9999, "acme"));
    expectations.push(org_membership("acme", 9999, "acme"));
    expectations.push(visible_account(&account));
    expectations.push(org_membership("acme", 9999, "acme"));
    let (app, cookie) = identity_app(&account, expectations).await;
    for (method, path) in [
        ("GET", "/console/accounts/acme"),
        ("POST", "/console/accounts/acme/links"),
    ] {
        let response = identity_request(&app, &cookie, method, path).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {path}");
    }
    let response = identity_request(&app, &cookie, "GET", "/console").await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = response_html(response).await;
    assert!(!html.contains("href=\"/console/accounts/acme\""));
}

#[tokio::test]
async fn oauth_signin_discards_organization_authority_for_same_or_different_user() {
    for (user_id, login) in [(42, "octocat"), (43, "another-user")] {
        let account = identity_account(9001, "acme", AccountType::Organization);
        let mut expectations = oauth_sign_in_expectations();
        expectations.push(org_membership("acme", 9001, "acme"));
        let mut second_login = oauth_sign_in_expectations();
        second_login[0] = Expectation::ok_json(
            Method::Post,
            "https://github.com/login/oauth/access_token",
            serde_json::json!({"access_token": "u_second", "token_type": "bearer", "scope": "read:user read:org"}),
        );
        second_login[1] = Expectation::ok_json(
            Method::Get,
            "https://api.github.com/user",
            serde_json::json!({"id": user_id, "login": login}),
        );
        expectations.extend(second_login);
        expectations.push(
            Expectation::status(
                Method::Get,
                "https://api.github.com/user/memberships/orgs/acme",
                404,
            )
            .require_header("authorization", "Bearer u_second"),
        );
        let (app, cookie) = identity_app(&account, expectations).await;
        let response = identity_request(&app, &cookie, "GET", "/console/accounts/acme").await;
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = session_cookie(&response, Some(cookie));
        let old_cookie = cookie.clone();
        let (app, cookie) = sign_in_with_cookie(app, Some(cookie)).await;
        assert_ne!(old_cookie, cookie);
        let response = identity_request(&app, &cookie, "GET", "/console/accounts/acme").await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let response = identity_request(&app, &old_cookie, "GET", "/console/accounts/acme").await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response.headers()["location"],
            "/login?return_to=%2Fconsole%2Faccounts%2Facme"
        );
    }
}

#[tokio::test]
async fn organization_authority_cache_cannot_follow_a_reused_account_name() {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let mut account = identity_account(9001, "acme", AccountType::Organization);
    storage.insert_installation(&account).await.unwrap();
    let mut expectations = oauth_sign_in_expectations();
    expectations.push(org_membership("acme", 9001, "acme"));
    expectations.push(org_membership("acme", 9001, "renamed-acme"));
    let state = AppState::new(
        storage.clone(),
        Arc::new(MockTransport::scripted(expectations)),
        Arc::new(RecordingCommands::default()),
        WebConfig::for_local_dev_with_secret([7; 32]),
    );
    let (app, cookie) = sign_in(build_app(state, tower_sessions::MemoryStore::default())).await;
    let response = identity_request(&app, &cookie, "GET", "/console/accounts/acme").await;
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = session_cookie(&response, Some(cookie));

    storage
        .mark_installation_uninstalled(77, Utc::now())
        .await
        .unwrap();
    account.installation_id = 78;
    account.account_id = 9002;
    storage.insert_installation(&account).await.unwrap();
    let response = identity_request(&app, &cookie, "GET", "/console/accounts/acme").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn organization_rename_keeps_identity_authority_and_cache_across_console_routes() {
    let account = identity_account(9001, "acme", AccountType::Organization);
    let mut expectations = oauth_sign_in_expectations();
    expectations.push(visible_account(&account));
    expectations.push(org_membership("acme", 9001, "renamed-acme"));
    let (app, cookie) = identity_app(&account, expectations).await;
    let response = identity_request(&app, &cookie, "GET", "/console").await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let cookie = session_cookie(&response, Some(cookie));
    for path in [
        "/console/accounts/acme",
        "/console/accounts/acme/links",
        "/console/accounts/acme/audit",
    ] {
        let response = identity_request(&app, &cookie, "GET", path).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
    }
}

#[tokio::test]
async fn unverified_organization_membership_fails_closed_and_can_be_retried() {
    for (membership, denied_status) in [
        (
            serde_json::json!({"role": "admin", "state": "active"}),
            StatusCode::BAD_GATEWAY,
        ),
        (
            serde_json::json!({"role": "admin", "state": "active", "organization": {"id": "9001"}}),
            StatusCode::BAD_GATEWAY,
        ),
        (
            serde_json::json!({"role": "admin", "state": "pending", "organization": {"id": 9001}}),
            StatusCode::NOT_FOUND,
        ),
        (
            serde_json::json!({"role": "member", "state": "active", "organization": {"id": 9001}}),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let account = identity_account(9001, "acme", AccountType::Organization);
        let mut expectations = oauth_sign_in_expectations();
        expectations.push(Expectation::ok_json(
            Method::Get,
            "https://api.github.com/user/memberships/orgs/acme",
            membership,
        ));
        expectations.push(org_membership("acme", 9001, "acme"));
        let (app, cookie) = identity_app(&account, expectations).await;
        let response =
            identity_request(&app, &cookie, "POST", "/console/accounts/acme/links").await;
        assert_eq!(response.status(), denied_status);
        let response = identity_request(&app, &cookie, "GET", "/console/accounts/acme").await;
        assert_eq!(response.status(), StatusCode::OK);
    }
}

#[tokio::test]
async fn expired_or_legacy_organization_authority_is_reverified_and_errors_never_grant_access() {
    for (cache_key, checked_at) in [
        ("acme", Utc::now()),
        ("42:9001", Utc::now() - Duration::seconds(61)),
        ("42:9001", Utc::now() + Duration::minutes(5)),
    ] {
        let storage = Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::in_memory()
                .await
                .unwrap(),
        );
        storage
            .insert_installation(&identity_account(9001, "acme", AccountType::Organization))
            .await
            .unwrap();
        let store = tower_sessions::MemoryStore::default();
        // Seed an old persisted session; exercise all verification via HTTP.
        let session = tower_sessions::Session::new(None, Arc::new(store.clone()), None);
        session
            .insert(
                "ghinvite",
                serde_json::json!({
                    "user_id": 42, "login": "octocat", "access_token": "u_xxx", "oauth_csrf": null,
                    "admin_checks": {cache_key: {"is_admin": true, "checked_at": checked_at}}
                }),
            )
            .await
            .unwrap();
        session.save().await.unwrap();
        let cookie = format!("id={}", session.id().unwrap());
        let state = AppState::new(
            storage,
            Arc::new(MockTransport::scripted(vec![
                Expectation::status(
                    Method::Get,
                    "https://api.github.com/user/memberships/orgs/acme",
                    503,
                ),
                Expectation::status(
                    Method::Get,
                    "https://api.github.com/user/memberships/orgs/acme",
                    404,
                ),
            ])),
            Arc::new(RecordingCommands::default()),
            WebConfig::for_local_dev_with_secret([7; 32]),
        );
        let app = build_app(state, store);
        let response = identity_request(&app, &cookie, "GET", "/console/accounts/acme").await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let response =
            identity_request(&app, &cookie, "POST", "/console/accounts/acme/links").await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}

#[tokio::test]
async fn personal_owner_can_edit_links_by_identity_after_rename() {
    for stored_login in ["octocat", "old-octocat"] {
        let storage = Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::in_memory()
                .await
                .unwrap(),
        );
        storage
            .insert_installation(&identity_account(42, stored_login, AccountType::User))
            .await
            .unwrap();
        let mut link = list_link(1);
        link.account_id = 42;
        let state = AppState::new(
            storage.clone(),
            Arc::new(MockTransport::scripted(oauth_sign_in_expectations())),
            Arc::new(RecordingCommands {
                edit_storage: Some(storage.clone()),
                ..Default::default()
            }),
            WebConfig::for_local_dev_with_secret([7; 32]),
        );
        let (app, cookie) = sign_in(build_app(state, tower_sessions::MemoryStore::default())).await;
        storage.insert_invitation_link(&link).await.unwrap();
        let path = format!("/console/accounts/{stored_login}/links/{}/edit", link.id);
        let token = common::csrf_token(&app, &cookie).await;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(&path)
                    .header("cookie", &cookie)
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(format!(
                        "csrf_token={token}&description=Updated+by+owner"
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let response = identity_request(&app, &cookie, "GET", &path).await;
        assert_eq!(response.status(), StatusCode::OK);
        let html = response_html(response).await;
        assert!(html.contains("Updated by owner"));
    }
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
    /// How the metadata command fails, when it is meant to.
    fail_edits: Option<ghinvite_web::IngressFailure>,
}

#[async_trait::async_trait]
impl GhinviteCommands for RecordingCommands {
    async fn update_invitation_link_metadata(
        &self,
        command: UpdateInvitationLinkMetadata,
    ) -> ghinvite_web::Result<()> {
        if let Some(failure) = self.fail_edits.clone() {
            return Err(ghinvite_web::WebError::Restate(failure));
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
    let state = AppState::new(
        storage,
        transport,
        commands,
        WebConfig::for_local_dev_with_secret([7; 32]),
    );
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
    let state = AppState::new(
        storage,
        transport,
        commands,
        WebConfig::for_local_dev_with_secret([7; 32]),
    );
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
    let state = AppState::new(
        storage,
        transport,
        commands,
        WebConfig::for_local_dev_with_secret([7; 32]),
    );
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
    sign_in_with_cookie(app, None).await
}

async fn sign_in_with_cookie(app: axum::Router, cookie: Option<String>) -> (axum::Router, String) {
    let mut request = Request::builder().uri("/login");
    if let Some(cookie) = &cookie {
        request = request.header("cookie", cookie);
    }
    let resp1 = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp1.status(), StatusCode::SEE_OTHER);
    let cookie1 = session_cookie(&resp1, cookie);
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
    let accounts: Vec<_> = logins
        .iter()
        .enumerate()
        .map(|(index, login)| Account {
            installation_id: 77 + index as u64,
            account_id: if *login == "octocat" {
                42
            } else {
                9001 + index as u64
            },
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
        .collect();
    for account in &accounts {
        storage.insert_installation(account).await.unwrap();
    }

    let mut expectations = oauth_sign_in_expectations();
    let installations: Vec<_> = accounts
        .iter()
        .map(|account| {
            let target_type = if account.account_type == AccountType::User {
                "User"
            } else {
                "Organization"
            };
            serde_json::json!({
                "id": account.installation_id,
                "account": {"id": account.account_id, "login": account.account_login, "type": target_type},
                "repository_selection": "all",
                "target_type": target_type,
                "target_id": account.account_id
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
    for account in accounts
        .iter()
        .filter(|account| account.account_type == AccountType::Organization)
    {
        expectations.push(org_membership(
            &account.account_login,
            account.account_id,
            &account.account_login,
        ));
    }

    let storage: Arc<dyn ghinvite_core::storage::Storage> = storage;
    let transport: Arc<dyn ghinvite_github::HttpTransport> =
        Arc::new(MockTransport::scripted(expectations));
    let restate = Arc::new(RestateClient::new("http://127.0.0.1:8080").unwrap());
    let commands = Arc::new(RestateCommands::new(restate));
    let state = AppState::new(
        storage,
        transport,
        commands,
        WebConfig::for_local_dev_with_secret([7; 32]),
    );
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
    let state = AppState::new(
        storage,
        transport,
        commands,
        WebConfig::for_local_dev_with_secret([7; 32]),
    );
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
    let state = AppState::new(
        storage,
        transport,
        commands,
        WebConfig::for_local_dev_with_secret([7; 32]),
    );
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
        fail_edits: Some(ghinvite_web::IngressFailure::Rejected { status: 503 }),
        ..Default::default()
    });
    let mut expectations = oauth_expectations();
    // The session layer does not persist the authorization cache on a 502.
    expectations.push(Expectation::ok_json(
        Method::Get,
        "https://api.github.com/user/memberships/orgs/acme",
        serde_json::json!({"role": "admin", "state": "active", "organization": {"id": 9001}}),
    ));
    let state = AppState::new(
        storage.clone(),
        Arc::new(MockTransport::scripted(expectations)),
        commands,
        WebConfig::for_local_dev_with_secret([7; 32]),
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

/// A refused ingress call and one whose outcome is unknown must not read the
/// same. Restate may have persisted the invocation before the response went
/// wrong, so "please try again" on an unknown outcome invites applying the same
/// mutation twice.
#[tokio::test]
async fn an_unknown_save_outcome_does_not_invite_a_blind_retry() {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    // `links_app` seeds the account the link hangs off.
    let _ = links_app(storage.clone(), "admin").await;
    let link = list_link(1);
    storage.insert_invitation_link(&link).await.unwrap();
    let commands = Arc::new(RecordingCommands {
        fail_edits: Some(ghinvite_web::IngressFailure::OutcomeUnknown {
            detail: "ingress send rejected",
            status: Some(503),
        }),
        ..Default::default()
    });
    let mut expectations = oauth_expectations();
    expectations.push(Expectation::ok_json(
        Method::Get,
        "https://api.github.com/user/memberships/orgs/acme",
        serde_json::json!({"role": "admin", "state": "active", "organization": {"id": 9001}}),
    ));
    let state = AppState::new(
        storage.clone(),
        Arc::new(MockTransport::scripted(expectations)),
        commands,
        WebConfig::for_local_dev_with_secret([7; 32]),
    );
    let (app, cookie) = sign_in(build_app(state, tower_sessions::MemoryStore::default())).await;

    let response = post_edit(
        &app,
        &cookie,
        &link.id.to_string(),
        "description=New+details&internal_note=",
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains("Save outcome unknown"), "{html}");
    assert!(!html.contains("Please try again"), "{html}");
    // The operational detail stays in the log, never in the page.
    assert!(!html.contains("ingress send rejected"), "{html}");
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
    let token = common::csrf_token(app, cookie).await;
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/console/accounts/acme/links/{id}/edit"))
                .header("cookie", cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!("csrf_token={token}&{body}")))
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
            serde_json::json!({"role": role, "state": "active", "organization": {"id": 9001}}),
        ));
    }
    let state = AppState::new(
        storage,
        Arc::new(MockTransport::scripted(expectations)),
        commands,
        WebConfig::for_local_dev_with_secret([7; 32]),
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
async fn console_audit_page_for_admin_renders_empty_history() {
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
    assert_eq!(resp.headers()["cache-control"], "private, no-store");
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("console-frame"));
    assert!(text.contains("No recorded events for this account."));
    assert!(text.contains("method=\"get\""));
    assert!(text.contains("All events"));
    assert!(text.contains("href=\"/console/accounts/acme/audit\""));
    assert!(text.contains("aria-current=\"page\""));
    assert!(text.contains("app-nav-row-active"));
    assert!(!text.contains("Audit log is not yet implemented."));
    assert!(!text.contains("<table"));
    assert!(text.contains("Event type"));
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

fn audit_event(n: u64, kind: ghinvite_core::audit::EventType) -> ghinvite_core::audit::AuditEvent {
    ghinvite_core::audit::AuditEvent {
        id: ghinvite_core::AuditEventId::new(),
        account_id: 9001,
        occurred_at: DateTime::parse_from_rfc3339("2026-05-04T12:00:00.123456789Z")
            .unwrap()
            .with_timezone(&Utc)
            + Duration::seconds(n as i64),
        event_type: kind,
        actor_kind: ghinvite_core::audit::ActorKind::User,
        actor_id: Some(42),
        target_kind: ghinvite_core::audit::TargetKind::Installation,
        target_id: format!("resource-{n:03}"),
        metadata: serde_json::Value::Null,
        request_id: None,
    }
}

#[tokio::test]
async fn audit_rows_allowlist_details_and_never_expose_private_payloads() {
    use ghinvite_core::audit::{ActorKind, EventType, TargetKind};
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let (app, cookie) = links_app(storage.clone(), "admin").await;
    let link = list_link(1);
    storage.insert_invitation_link(&link).await.unwrap();
    let mut foreign_link = list_link(2);
    foreign_link.account_id = 9002;
    foreign_link.installation_id = 78;
    storage.insert_invitation_link(&foreign_link).await.unwrap();
    let cases = [
        (
            EventType::InvitationLinkMetadataUpdated,
            serde_json::json!({"changed_fields": ["internal_note", "description", "internal_note", "secret-key"], "description": "private-description", "internal_note": "private-note"}),
            "Description; Internal note",
        ),
        (
            EventType::RequestApproved,
            serde_json::json!({"reason": "auto_approve"}),
            "Auto-approved",
        ),
        (
            EventType::InvitationAccepted,
            serde_json::json!({"reason": "already_collaborator"}),
            "Already a collaborator",
        ),
        (
            EventType::InvitationAccepted,
            serde_json::json!({"reconciled": true}),
            "Observed during reconciliation",
        ),
        (
            EventType::InvitationCancelled,
            serde_json::json!({"reconciled": true}),
            "Observed during reconciliation",
        ),
        (
            EventType::InvitationLinkCreated,
            serde_json::json!({"permission": "push", "approval_required": true, "max_uses": 7, "repo_count": 3}),
            "Permission level: push; Account-admin approval required; Max use: 7; Repositories: 3",
        ),
        (
            EventType::InvitationLinkCreated,
            serde_json::json!({"permission": "pull", "approval_required": false, "max_uses": null}),
            "Permission level: pull; Auto-approval; Max use: unlimited",
        ),
        (
            EventType::InstallationReposChanged,
            serde_json::json!({"selected_repos_kind": "all"}),
            "All available repositories",
        ),
        (
            EventType::InstallationReposChanged,
            serde_json::json!({"selected_repos_kind": "subset"}),
            "Selected repositories",
        ),
        (
            EventType::InvitationSendFailed,
            serde_json::json!({"error": "upstream-secret-error"}),
            "—",
        ),
        (
            EventType::RequestDeclined,
            serde_json::json!({"reason": "private-decline-reason"}),
            "—",
        ),
        (
            EventType::InvitationExpired,
            serde_json::json!({"reason": "private-expiration-reason"}),
            "—",
        ),
        (
            EventType::InvitationLinkCreated,
            serde_json::json!({"permission": "owner", "approval_required": "true", "max_uses": -1, "repo_count": "2"}),
            "—",
        ),
        (
            EventType::InvitationAccepted,
            serde_json::json!({"reason": "private-mechanism", "reconciled": "true"}),
            "—",
        ),
        (
            EventType::RequestApproved,
            serde_json::json!({"reason": true}),
            "—",
        ),
        (
            EventType::InstallationReposChanged,
            serde_json::json!({"selected_repos_kind": "selected"}),
            "—",
        ),
        (
            EventType::InvitationLinkMetadataUpdated,
            serde_json::json!({"changed_fields": "description"}),
            "—",
        ),
        (EventType::RequestCreated, serde_json::Value::Null, "—"),
        (
            EventType::InvitationSent,
            serde_json::json!({"recovered":true,"repo_full_name":"private-repo","requester_id":8}),
            "Confirmed outcome observed during recovery",
        ),
        (
            EventType::RequestCreated,
            serde_json::json!(["arbitrary-array"]),
            "—",
        ),
    ];
    for (n, (kind, metadata, _)) in cases.iter().enumerate() {
        let mut event = audit_event(n as u64, *kind);
        event.metadata = metadata.clone();
        if let Some(m) = event.metadata.as_object_mut() {
            m.insert(
                "unknown".into(),
                serde_json::json!("<img src=x onerror=private-script>"),
            );
            m.insert(
                "justification".into(),
                serde_json::json!("private-justification"),
            );
            m.insert(
                "recipient_login".into(),
                serde_json::json!("private-recipient"),
            );
            m.insert("repos".into(), serde_json::json!(["private-repository"]));
            m.insert("invitation_code".into(), serde_json::json!("private-code"));
        }
        event.request_id = Some("private-restate-invocation".into());
        match n {
            0 => {
                event.target_kind = TargetKind::InvitationLink;
                event.target_id = link.id.to_string();
            }
            1 => {
                event.target_kind = TargetKind::InvitationLink;
                event.target_id = foreign_link.id.to_string();
                event.actor_kind = ActorKind::System;
            }
            2 => {
                event.target_kind = TargetKind::InvitationLink;
                event.target_id = "missing-link".into();
                event.actor_kind = ActorKind::Github;
            }
            3 => {
                event.target_kind = TargetKind::GithubInvitation;
                event.actor_id = None;
            }
            4 => {
                event.target_kind = TargetKind::InvitationRequest;
                event.actor_id = Some(987);
            }
            _ => {}
        }
        storage.audit(&event).await.unwrap();
    }
    let (_, html) = audit_get(&app, &cookie, "/console/accounts/acme/audit").await;
    let body = html
        .split("<tbody>")
        .nth(1)
        .unwrap()
        .split("</tbody>")
        .next()
        .unwrap();
    let rows: Vec<_> = body.split("<tr>").skip(1).collect();
    assert_eq!(rows.len(), cases.len());
    for (row, (_, _, summary)) in rows.iter().rev().zip(&cases) {
        assert!(row.contains(&format!("<td>{summary}</td>")), "{row}");
    }
    for secret in [
        "private-",
        "secret-key",
        "arbitrary-array",
        "owner",
        link.slug.as_str(),
        foreign_link.slug.as_str(),
        &link.description,
    ] {
        assert!(!html.contains(secret), "leaked {secret}");
    }
    assert!(html.contains(&format!(
        "href=\"/console/accounts/acme/links/{}\"",
        link.id
    )));
    assert!(!html.contains(&format!(
        "href=\"/console/accounts/acme/links/{}\"",
        foreign_link.id
    )));
    assert!(html.contains(&format!(
        "Invitation link {} · Unavailable",
        foreign_link.id
    )));
    assert!(html.contains("Invitation link missing-link · Unavailable"));
    for actor in [
        "<td>System</td>",
        "<td>GitHub</td>",
        "Unknown GitHub user",
        "GitHub user ID 987",
    ] {
        assert!(body.contains(actor));
    }
    assert!(body.contains("GitHub invitation (internal ID)"));
    assert!(!body.contains("href=\"https://github.com/"));
    assert_eq!(html.matches("<script").count(), 1); // existing external theme script only
    // Every vocabulary member remains offered, including events with no producer.
    for label in [
        "Installation created",
        "Installation repositories changed",
        "Installation uninstalled",
        "Invitation link created",
        "Invitation link metadata updated",
        "Invitation link revoked",
        "Invitation link expired",
        "Invitation link exhausted",
        "Invitation request created",
        "Invitation request approved",
        "Invitation request declined",
        "Invitation request expired",
        "GitHub invitation sent",
        "GitHub invitation accepted",
        "GitHub invitation declined",
        "GitHub invitation expired",
        "GitHub invitation cancelled",
        "GitHub invitation send failed",
    ] {
        assert!(html.contains(label), "{label}");
    }
    for event in EventType::ALL {
        assert!(html.contains(&format!("value=\"{}\"", event.as_str())));
    }
}

#[tokio::test]
async fn audit_optional_enrichment_failures_fall_back_but_core_failures_are_500() {
    use ghinvite_core::audit::{EventType, TargetKind};
    let path = std::env::temp_dir().join(format!(
        "ghinvite-audit-{}.sqlite",
        ghinvite_core::AuditEventId::new()
    ));
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::at_path(&path)
            .await
            .unwrap(),
    );
    let (app, cookie) = links_app(storage.clone(), "admin").await;
    let link = list_link(1);
    storage.insert_invitation_link(&link).await.unwrap();
    let mut event = audit_event(1, EventType::InvitationLinkCreated);
    event.target_kind = TargetKind::InvitationLink;
    event.target_id = link.id.to_string();
    storage.audit(&event).await.unwrap();
    let pool =
        sqlx::SqlitePool::connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
    // Corrupt optional local lookup data; the recorded event must survive.
    sqlx::query("UPDATE users SET last_seen_at = 'broken'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE invitation_links RENAME TO unavailable_links")
        .execute(&pool)
        .await
        .unwrap();
    let base = "/console/accounts/acme/audit";
    let (status, html) = audit_get(&app, &cookie, base).await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("GitHub user ID 42"));
    assert!(!html.contains("@octocat · GitHub user ID"));
    assert!(html.contains(&format!("Invitation link {} · Unavailable", link.id)));
    for sql in [
        "UPDATE audit_events SET metadata = '{broken'",
        "DROP TABLE audit_events",
    ] {
        sqlx::query(sql).execute(&pool).await.unwrap();
        let (status, html) = audit_get(
            &app,
            &cookie,
            &format!("{base}?event=invitation_link.created&before=bad&account_id=9002"),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(html.contains("Could not load the audit log."));
        assert!(!html.contains("No recorded events"));
        assert!(html.contains("console-frame"));
        for marker in ["console-sidebar", "mobile-console-nav"] {
            let nav = html
                .split_once(marker)
                .unwrap()
                .1
                .split("</nav>")
                .next()
                .unwrap();
            assert!(nav.contains("aria-current=\"page\""));
            assert!(nav.contains("/console/accounts/acme/audit"));
        }
        assert_eq!(
            audit_anchor(&html, "Retry").unwrap(),
            format!("{base}?event=invitation_link.created")
        );
    }
    pool.close().await;
    drop(app);
    drop(storage);
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn audit_authorization_preserves_login_urls_and_conceals_unavailable_accounts() {
    let base = "/console/accounts/acme/audit?event=request.created&before=bad";
    let app = build_test_app().await;
    let response = app
        .oneshot(Request::builder().uri(base).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()["location"],
        "/login?return_to=%2Fconsole%2Faccounts%2Facme%2Faudit%3Fevent%3Drequest.created%26before%3Dbad"
    );
    for role in ["member", "admin"] {
        let storage = Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::in_memory()
                .await
                .unwrap(),
        );
        let (app, cookie) = links_app(storage.clone(), role).await;
        if role == "member" {
            assert_eq!(
                audit_get(&app, &cookie, base).await.0,
                StatusCode::NOT_FOUND
            );
        } else {
            let event = audit_event(1, ghinvite_core::audit::EventType::InstallationCreated);
            storage.audit(&event).await.unwrap();
            storage
                .mark_installation_uninstalled(77, Utc::now())
                .await
                .unwrap();
            assert_eq!(
                audit_get(&app, &cookie, base).await.0,
                StatusCode::NOT_FOUND
            );
            storage
                .insert_installation(&Account {
                    installation_id: 79,
                    account_id: 9001,
                    account_login: "acme".into(),
                    account_type: AccountType::Organization,
                    installed_at: Utc::now(),
                    uninstalled_at: None,
                    selected_repos: SelectedRepos::All,
                })
                .await
                .unwrap();
            let (status, html) = audit_get(&app, &cookie, "/console/accounts/acme/audit").await;
            assert_eq!(status, StatusCode::OK);
            assert!(html.contains("resource-001"));
        }
        assert_eq!(
            audit_get(&app, &cookie, "/console/accounts/missing/audit")
                .await
                .0,
            StatusCode::NOT_FOUND
        );
    }
    // Personal-account owners use the established authorization path, without an org call.
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    storage
        .insert_installation(&Account {
            installation_id: 77,
            account_id: 42,
            account_login: "octocat".into(),
            account_type: AccountType::User,
            installed_at: Utc::now(),
            uninstalled_at: None,
            selected_repos: SelectedRepos::All,
        })
        .await
        .unwrap();
    let state = AppState::new(
        storage,
        Arc::new(MockTransport::scripted(oauth_sign_in_expectations())),
        Arc::new(RecordingCommands::default()),
        WebConfig::for_local_dev_with_secret([7; 32]),
    );
    let (app, cookie) = sign_in(build_app(state, tower_sessions::MemoryStore::default())).await;
    assert_eq!(
        audit_get(&app, &cookie, "/console/accounts/octocat/audit")
            .await
            .0,
        StatusCode::OK
    );
}

async fn audit_get(app: &axum::Router, cookie: &str, uri: &str) -> (StatusCode, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    let html = String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    (status, html)
}

fn audit_anchor(html: &str, label: &str) -> Option<String> {
    html.split("<a ").skip(1).find_map(|tag| {
        let (attrs, content) = tag.split_once('>')?;
        if content.split("</a>").next()? != label {
            return None;
        }
        Some(
            attrs
                .split("href=\"")
                .nth(1)?
                .split('"')
                .next()?
                .replace("&amp;", "&")
                .replace("&#38;", "&"),
        )
    })
}

#[tokio::test]
async fn audit_history_urls_filter_seek_normalize_and_remain_read_only() {
    use ghinvite_core::audit::EventType;
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let commands = Arc::new(RecordingCommands::default());
    let (app, cookie) = links_app_with_commands(storage.clone(), "admin", commands.clone()).await;
    for n in 0..51 {
        storage
            .audit(&audit_event(n, EventType::RequestCreated))
            .await
            .unwrap();
    }
    let mut foreign = audit_event(999, EventType::RequestCreated);
    foreign.account_id = 9002;
    storage.audit(&foreign).await.unwrap();
    storage
        .audit(&audit_event(998, EventType::InvitationSent))
        .await
        .unwrap();
    let base = "/console/accounts/acme/audit";
    let first_uri = format!("{base}?event=request.created");
    let (status, first) = audit_get(&app, &cookie, &first_uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first.matches("<time ").count(), 25);
    assert!(first.contains("resource-050") && first.contains("resource-026"));
    assert!(!first.contains("resource-025") && !first.contains("resource-999"));
    assert!(first.contains("datetime=\"2026-05-04T12:00:50.123456789Z\""));
    assert!(first.contains("2026-05-04 12:00:50 UTC"));
    assert!(first.contains("@octocat · GitHub user ID 42"));
    assert!(first.contains("last known, not historical snapshots"));
    assert!(first.contains("overflow-x-auto") && first.contains("tabindex=\"0\""));
    assert_eq!(first.matches("scope=\"col\"").count(), 5);
    assert!(audit_anchor(&first, "Newer").is_none());
    let older_uri = audit_anchor(&first, "Older").unwrap();
    assert!(
        older_uri.contains("event=request.created&before="),
        "{older_uri}"
    );
    let (_, second) = audit_get(&app, &cookie, &older_uri).await;
    assert_eq!(second.matches("<time ").count(), 25);
    assert!(second.contains("resource-025") && second.contains("resource-001"));
    assert!(!second.contains("resource-026") && !second.contains("resource-000"));
    assert_eq!(audit_anchor(&second, "Back to latest").unwrap(), first_uri);
    let newer_uri = audit_anchor(&second, "Newer").unwrap();
    let (_, newer) = audit_get(&app, &cookie, &newer_uri).await;
    assert_eq!(
        newer
            .split("<tbody>")
            .nth(1)
            .unwrap()
            .split("</tbody>")
            .next(),
        first
            .split("<tbody>")
            .nth(1)
            .unwrap()
            .split("</tbody>")
            .next()
    );
    let last_uri = audit_anchor(&second, "Older").unwrap();
    let (_, last) = audit_get(&app, &cookie, &last_uri).await;
    assert_eq!(last.matches("<time ").count(), 1);
    assert!(audit_anchor(&last, "Older").is_none());
    // Refresh/bookmarks preserve the rows; a new front arrival cannot shift older pages.
    storage
        .audit(&audit_event(1000, EventType::RequestCreated))
        .await
        .unwrap();
    assert_eq!(audit_get(&app, &cookie, &older_uri).await.1, second);
    let form = second
        .split("<form")
        .find(|form| form.starts_with(" method=\"get\""))
        .unwrap()
        .split("</form>")
        .next()
        .unwrap();
    assert!(form.contains("method=\"get\"") && form.contains("name=\"event\""));
    assert!(!form.contains("before") && !form.contains("after"));
    let (_, latest) = audit_get(&app, &cookie, &first_uri).await;
    for query in [
        "event=request.created&before=bad".to_string(),
        format!("event=request.created&before={}", "x".repeat(513)),
        format!("{}&after=bad", older_uri.split_once('?').unwrap().1),
        "event=bad&event=request.created&before=bad&before=".into(),
        "event=request.created&account_id=9002&installation_id=78&unknown=ignored".into(),
    ] {
        assert_eq!(
            audit_get(&app, &cookie, &format!("{base}?{query}")).await.1,
            latest,
            "{query}"
        );
    }
    let (_, all) = audit_get(&app, &cookie, base).await;
    assert_eq!(
        audit_get(&app, &cookie, &format!("{base}?event=unknown"))
            .await
            .1,
        all
    );
    let changed = older_uri.replace("event=request.created", "event=invitation.sent");
    let (_, changed) = audit_get(&app, &cookie, &changed).await;
    assert_eq!(changed.matches("<time ").count(), 1);
    assert!(changed.contains("resource-998"));
    assert!(audit_anchor(&changed, "Back to latest").is_none());
    let (_, none) = audit_get(&app, &cookie, &format!("{base}?event=request.declined")).await;
    assert!(none.contains("No recorded events match this event type."));
    assert_eq!(audit_anchor(&none, "All events").unwrap(), base);
    // Forged, but syntactically valid, foreign-account boundary has no authority.
    use base64::Engine;
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(&serde_json::json!({
        "v": 1, "event": "request.created", "time": "2026-05-01T00:00:00.000000001Z", "id": foreign.id,
    })).unwrap());
    let (_, empty) = audit_get(&app, &cookie, &format!("{first_uri}&before={token}")).await;
    assert!(empty.contains("No recorded events in this range."));
    assert_eq!(audit_anchor(&empty, "Back to latest").unwrap(), first_uri);
    assert!(!empty.contains("resource-999"));
    assert!(commands.calls.lock().unwrap().is_empty());
    // Public read boundary also proves browsing did not append history.
    let page = storage
        .list_audit_events(9001, None, ghinvite_core::storage::AuditPosition::Latest)
        .await
        .unwrap();
    assert_eq!(page.events[0].target_id, "resource-1000");
    assert!(
        page.events
            .iter()
            .all(|e| e.event_type == EventType::RequestCreated
                || e.event_type == EventType::InvitationSent)
    );
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
    let token = common::csrf_token(&app, &cookie).await;
    let body = format!("csrf_token={token}&{body}");
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
    assert_eq!(props.values.selected_repo_ids, vec![10]);
    assert_eq!(
        props.values.errors.permission.as_deref(),
        Some("Choose a supported permission level: pull, triage, push, maintain, or admin.")
    );
    assert!(props.values.errors.description.is_none());
    assert!(
        props
            .values
            .errors
            .repo_scope
            .as_deref()
            .unwrap()
            .contains("no longer available")
    );
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
    let token = common::csrf_token(&app, &cookie).await;
    let body = format!("csrf_token={token}&{body}");
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
    assert_numeric_input(&text, "max_uses", "7", None);
    assert_numeric_input(&text, "expires_in_days", "45", None);
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
    let token = common::csrf_token(&app, &cookie).await;
    let body = format!("csrf_token={token}&{body}");
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
    let form = html
        .split("id=\"link-form-island\"")
        .nth(1)
        .unwrap()
        .split("</form>")
        .next()
        .unwrap();
    let mut inputs = form
        .split("<input ")
        .skip(1)
        .map(|input| input.split_once('>').unwrap().0)
        .filter(|input| input.contains("name=\"description\""));
    let input = inputs.next().expect("form renders the description input");
    assert!(inputs.next().is_none(), "description input is unique");
    assert!(input.contains("value=\"AI coding workshop\""));
}

fn assert_numeric_input(html: &str, name: &str, raw: &str, error: Option<&str>) {
    let form = html
        .split("id=\"link-form-island\"")
        .nth(1)
        .unwrap()
        .split("</form>")
        .next()
        .unwrap();
    let mut inputs = form
        .split("<input ")
        .skip(1)
        .map(|input| input.split_once('>').unwrap().0)
        .filter(|input| input.contains(&format!("name=\"{name}\"")));
    let input = inputs.next().expect("numeric input is present");
    assert!(inputs.next().is_none(), "numeric input is unique");
    for attribute in [
        format!("id=\"{name}\""),
        format!("value=\"{raw}\""),
        "type=\"number\"".into(),
        "inputmode=\"numeric\"".into(),
        "min=\"1\"".into(),
        format!("aria-labelledby=\"{name}-label\""),
        format!("aria-invalid=\"{}\"", error.is_some()),
    ] {
        assert!(input.contains(&attribute), "missing {attribute}: {input}");
    }
    assert!(!input.contains(" max="));
    assert!(!input.contains(" required="));
    let descriptions = if error.is_some() {
        format!("{name}-help {name}-error")
    } else {
        format!("{name}-help")
    };
    assert!(input.contains(&format!("aria-describedby=\"{descriptions}\"")));
    assert_eq!(
        input.contains(&format!("aria-errormessage=\"{name}-error\"")),
        error.is_some()
    );
    assert_eq!(input.contains("input-error"), error.is_some());
    let region = form.split_once(&format!("id=\"{name}-error\"")).unwrap().1;
    let (attributes, content) = region.split_once('>').unwrap();
    assert!(attributes.contains("aria-live=\"polite\""));
    assert!(!attributes.contains("hidden"));
    if let Some(error) = error {
        assert!(content.starts_with(&format!("<div>{error}</div></div>")));
    } else {
        assert!(content.starts_with("</div>"));
    }
}

#[tokio::test]
async fn create_link_invalid_numeric_guardrails_rerender_form_with_field_errors() {
    let mut expectations = oauth_expectations();
    expectations.push(installation_repos_expectation());
    let (app, cookie, calls) =
        build_signed_in_admin_app_with_recording_commands(expectations).await;

    let body = "description=AI+coding+workshop&permission=push&max_uses=abc&expires_in_days=0&internal_note=Keep+this+note&repo_ids=10";
    let token = common::csrf_token(&app, &cookie).await;
    let body = format!("csrf_token={token}&{body}");
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
    assert_description_error_empty(&text);
    assert_preserved_description_input(&text);
    assert_numeric_input(
        &text,
        "max_uses",
        "abc",
        Some(ghinvite_ui::link_form::MAX_USES_NOT_POSITIVE),
    );
    assert_numeric_input(
        &text,
        "expires_in_days",
        "0",
        Some(ghinvite_ui::link_form::EXPIRES_IN_DAYS_NOT_POSITIVE),
    );
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
    let token = common::csrf_token(&app, &cookie).await;
    let body = format!("csrf_token={token}&{body}");
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
    let token = common::csrf_token(&app, &cookie).await;
    let body = format!("csrf_token={token}&{body}");
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
    body: impl AsRef<str>,
) -> (axum::response::Response, Arc<Mutex<Vec<RecordedCommand>>>) {
    let mut expectations = oauth_expectations();
    expectations.push(installation_repos_expectation());
    let (app, cookie, calls) =
        build_signed_in_admin_app_with_recording_commands(expectations).await;
    let token = common::csrf_token(&app, &cookie).await;
    let body = format!("csrf_token={token}&{}", body.as_ref());
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
    // aria-invalid is not permitted on role="group"; no field is invalid here.
    assert_eq!(text.matches("aria-invalid=\"true\"").count(), 0);
    assert_description_error_empty(&text);
    assert!(!text.contains("Bad Request"));
    assert!(!text.contains("select at least one repository"));
    assert_preserved_description_input(&text);
    assert!(text.contains("value=\"push\" selected"));
    assert!(text.contains("name=\"approval_required\" value=\"true\" checked"));
    assert_numeric_input(&text, "max_uses", "7", None);
    assert_numeric_input(&text, "expires_in_days", "45", None);
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
async fn create_link_with_unavailable_selection_requires_review_before_reducing_scope() {
    let (resp, calls) = post_create_link(
        "description=AI+coding+workshop&permission=push&repo_ids=999&repo_ids=11&repo_ids=10",
    )
    .await;

    assert_eq!(resp.status(), StatusCode::OK);
    let html = response_html(resp).await;
    assert!(html.contains("Selected repositories are no longer available: 999"));
    assert!(html.contains("Review the remaining scope before creating"));
    assert!(html.contains("value=\"10\" checked"));
    assert!(html.contains("value=\"11\" checked"));
    assert!(calls.lock().unwrap().is_empty());
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
        text.contains("repo_ids-error"),
        "the unavailable selection must be reviewed"
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
async fn create_link_malformed_permissions_keep_raw_props_and_never_reach_commands() {
    for raw in [
        None,
        Some(""),
        Some("owner"),
        Some("Push"),
        Some(" pull "),
        Some("0"),
    ] {
        let mut body = url::form_urlencoded::Serializer::new(String::new());
        body.append_pair("description", "Workshop")
            .append_pair("repo_ids", "10");
        if let Some(raw) = raw {
            body.append_pair("permission", raw);
        }
        let (response, calls) = post_create_link(body.finish()).await;
        assert_eq!(response.status(), StatusCode::OK, "permission={raw:?}");
        assert!(calls.lock().unwrap().is_empty());
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8(body.to_vec()).unwrap();
        let props = html
            .split_once("<script type=\"application/json\" id=\"link-form-props\">")
            .unwrap()
            .1
            .split_once("</script>")
            .unwrap()
            .0;
        let props: ghinvite_ui::link_form::LinkFormIslandProps =
            serde_json::from_str(props).unwrap();
        assert_eq!(props.values.permission, raw.unwrap_or_default());
        assert_eq!(
            props.values.errors.permission.as_deref(),
            Some(PERMISSION_UNSUPPORTED)
        );
        let select = html
            .split_once("<select ")
            .unwrap()
            .1
            .split_once("</select>")
            .unwrap()
            .0;
        let (attributes, options) = select.split_once('>').unwrap();
        for attribute in [
            "id=\"permission\"",
            "name=\"permission\"",
            "aria-invalid=\"true\"",
            "aria-labelledby=\"permission-label\"",
            "aria-describedby=\"permission-help permission-error\"",
            "aria-errormessage=\"permission-error\"",
        ] {
            assert!(attributes.contains(attribute), "{attributes}");
        }
        assert!(attributes.contains("select-error"));
        assert_eq!(options.matches("<option ").count(), 5);
        assert_eq!(options.matches(" selected=true").count(), 1);
        assert!(options.contains("<option value=\"pull\" selected=true>pull</option>"));
        assert!(!options.contains("owner"));
        assert!(!options.contains("value=\"\""));
        let region = html.split_once("id=\"permission-error\"").unwrap().1;
        let (attributes, content) = region.split_once('>').unwrap();
        assert!(attributes.contains("aria-live=\"polite\""));
        assert!(!attributes.contains("hidden"));
        assert!(content.starts_with(&format!("<div>{PERMISSION_UNSUPPORTED}</div></div>")));
        assert_eq!(html.matches("aria-invalid=\"true\"").count(), 1);
        assert_description_error_empty(&html);
    }
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
    let form_markup = text
        .split("id=\"link-form-island\"")
        .nth(1)
        .unwrap()
        .split("</form>")
        .next()
        .unwrap();
    assert!(!form_markup.contains("owner"));
    assert_eq!(text.matches("<option").count(), 5);
    // Every other submitted value and selection survives the re-render.
    assert_preserved_description_input(&text);
    assert!(text.contains("name=\"approval_required\" value=\"true\" checked"));
    assert_numeric_input(&text, "max_uses", "7", None);
    assert_numeric_input(&text, "expires_in_days", "45", None);
    assert!(text.contains("Keep this note"));
    assert!(text.contains("value=\"10\" checked"));
    assert!(text.contains("acme/api"));
    assert_description_error_empty(&text);
    assert!(!text.contains("repo_ids-error"));
}

fn assert_description_error_empty(html: &str) {
    let region = html.split_once("id=\"description-error\"").unwrap().1;
    let (attributes, content) = region.split_once('>').unwrap();
    assert!(attributes.contains("aria-live=\"polite\""));
    assert!(!attributes.contains("hidden"));
    assert!(content.starts_with("</div>"));
    let input = html
        .split("<input")
        .find(|input| {
            input
                .split('>')
                .next()
                .unwrap()
                .contains("id=\"description\"")
        })
        .unwrap();
    let attributes = input.split('>').next().unwrap();
    assert!(attributes.contains("aria-invalid=\"false\""));
    assert!(!attributes.contains("aria-errormessage="));
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
    let token = common::csrf_token(&app, &cookie).await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/console/accounts/acme/links/not-a-link-id/revoke")
                .header("cookie", cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!("csrf_token={token}")))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"Not Found");
}
