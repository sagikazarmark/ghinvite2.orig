//! Console route integration tests.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, Duration, Utc};
use common::build_app;
use common::sign_in::{
    ConsoleInstallationFixture, ConsoleTransport as MockTransport, console_config, repo_admin,
    repo_identity, seed_org_authority,
};
use ghinvite_core::storage::projection::fixture::Seed;
use ghinvite_core::storage::{AuditStorage, ConsoleStorage, RecordStorage};
use ghinvite_core::{Account, AccountType, SelectedRepos};
use ghinvite_github::mocks::Expectation;
use ghinvite_github::{Method, Response};
use ghinvite_web::RestateClient;
use http_body_util::BodyExt;
use std::collections::BTreeMap;
use std::sync::Arc;
use tower::ServiceExt;

mod common;

#[path = "console_flow/daily_digest.rs"]
mod daily_digest;

use common::authority_http_fixture::FakeLinkAuthority;
use common::sign_in::{
    ACME_ADMIN, GithubUser, OCTOCAT, acme_installation, oauth_expectations, session_cookie,
    sign_in, sign_in_with_cookie, signed_in_app, unreachable_restate,
};

#[path = "console_flow/read_recovery.rs"]
mod read_recovery;

#[path = "console_flow/mutation_recovery.rs"]
mod mutation_recovery;

#[path = "console_flow/request_history.rs"]
mod request_history;

#[path = "console_flow/approval_preview.rs"]
mod approval_preview;

#[derive(Clone, serde::Deserialize)]
struct QueueFixture {
    transition_id: String,
    link: ghinvite_core::invitation_link::LinkSnapshot,
    requests: Vec<ghinvite_core::request_lifecycle::RequestSnapshot>,
}
impl QueueFixture {
    async fn project(&self, storage: &ghinvite_storage_sqlx::SqlxStorage) {
        use ghinvite_core::storage::projection::{
            ProjectionEnvelope, ProjectionStorage, RequestProjectionEnvelope,
        };
        storage
            .apply_transition(&ProjectionEnvelope {
                transition_id: self.transition_id.clone(),
                link: self.link.clone(),
                events: vec![],
            })
            .await
            .unwrap();
        for request in &self.requests {
            storage
                .apply_request(&RequestProjectionEnvelope {
                    approval: None,
                    request: request.clone(),
                    events: vec![],
                })
                .await
                .unwrap();
        }
    }
}

async fn deadline_queue_app(
    expires_at: Option<&str>,
) -> (
    axum::Router,
    String,
    Arc<ghinvite_storage_sqlx::SqlxStorage>,
    QueueFixture,
) {
    let (app, cookie, storage, envelope, _) = deadline_queue_app_with_authority(expires_at).await;
    (app, cookie, storage, envelope)
}

async fn deadline_queue_app_with_authority(
    expires_at: Option<&str>,
) -> (
    axum::Router,
    String,
    Arc<ghinvite_storage_sqlx::SqlxStorage>,
    QueueFixture,
    FakeLinkAuthority,
) {
    deadline_queue_app_with_transport(expires_at, None).await
}

async fn deadline_queue_app_with_transport(
    expires_at: Option<&str>,
    transport: Option<Arc<dyn ghinvite_github::HttpTransportDyn>>,
) -> (
    axum::Router,
    String,
    Arc<ghinvite_storage_sqlx::SqlxStorage>,
    QueueFixture,
    FakeLinkAuthority,
) {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    storage
        .seed_console_installation(&identity_account(42, "octocat", AccountType::User))
        .await
        .unwrap();
    for (user_id, login) in [(42, "octocat"), (99, "requester")] {
        storage
            .upsert_user(&ghinvite_core::User {
                user_id,
                login: login.into(),
                avatar_url: None,
                github_created_at: None,
                last_seen_at: Utc::now(),
            })
            .await
            .unwrap();
    }
    let link = ghinvite_core::InvitationLinkId::new();
    let request = ghinvite_core::RequestId::new();
    // A historical/custom deadline, deliberately not today's seven-day policy.
    let envelope: QueueFixture = serde_json::from_value(serde_json::json!({
        "transition_id":format!("link/{link}/2"),
        "link":{"link_id":link,"revision":2,"uses":1,
            "secret":common::capability::state(),"created_at":"2026-01-01T00:00:00Z", "revoked_at":null,"revoked_by":null,
            "creation":{"link_id":link,"admin":{"account_id":42,"user_id":42},
                "account_id":42,"installation_id":77,"description":"Deadline fixture",
                "internal_note":null,"expires_at":expires_at,"max_uses":null,"secret_sha256":common::capability::state().sha256,
                "approval_required":true,"targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"octocat/api","grant":"read"}]}}},
        "requests":[{"request_id":request,"link_id":link,"account_id":42,"requester_id":99,
            "state":"pending","admitted_at":"2026-01-01T01:00:00Z",
            "decision_deadline":"2026-01-03T12:34:56Z","revision":1}],"events":[]
    }))
    .unwrap();
    envelope.project(&storage).await;
    // The authority holds the link the projection was written from.
    let authority = FakeLinkAuthority::start().await;
    authority.seed(envelope.link.clone());
    let state = crate::common::sign_in::app_state(
        storage.clone(),
        transport.unwrap_or_else(|| Arc::new(MockTransport::scripted(oauth_expectations(OCTOCAT)))),
        authority.client(),
        console_config(),
    );
    let app = build_app(state, tower_sessions::MemoryStore::default());
    let cookie = sign_in(&app).await;
    (app, cookie, storage, envelope, authority)
}

#[tokio::test]
async fn queue_pages_navigate_without_offset_drift_and_use_description_first() {
    let (app, cookie, storage, mut envelope) = deadline_queue_app(None).await;
    let template = envelope.requests[0].clone();
    envelope.link.revision += 1;
    envelope.link.uses = 54;
    envelope.transition_id = format!("link/{}/3", envelope.link.link_id);
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
        transition.project(&storage).await;
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
        {
            let mut decided = storage
                .get_invitation_request(request.request_id)
                .await
                .unwrap()
                .unwrap();
            decided.state = ghinvite_core::RequestState::Declined;
            decided.decided_by = Some(42);
            decided.decided_at = Some(Utc::now());
            decided.decline_reason = None;
            storage.seed_decision(&decided).await.unwrap();
        }
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
async fn queue_excludes_auto_approved_requests_without_a_decision_deadline() {
    let (app, cookie, storage, mut envelope) = deadline_queue_app(None).await;
    let link = ghinvite_core::InvitationLinkId::new();
    envelope.link.link_id = link;
    envelope.link.creation.link_id = link;
    envelope.link.creation.approval_required = false;
    envelope.link.creation.max_uses = Some(1000);
    envelope.transition_id = format!("link/{link}/2");
    envelope.requests[0].link_id = Some(link);
    envelope.requests[0].request_id = ghinvite_core::RequestId::new();
    envelope.requests[0].state = ghinvite_core::RequestState::Approved;
    envelope.requests[0].decision_deadline = None;
    envelope.requests[0].justification = Some("Auto-approved request fixture".into());
    envelope.requests[0].decision = Some(ghinvite_core::request_lifecycle::TerminalDecision {
        decision_id: "fixture-auto".into(),
        decided_by: None,
        effective_at: envelope.requests[0].admitted_at,
        evaluated_at: envelope.requests[0].admitted_at,
        cancellation_reason: None,
        decline_reason: None,
        requester_message: None,
    });
    envelope.project(&storage).await;
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

/// A link of the `octocat` user account, which `sign_in` administers.
fn octocat_link() -> ghinvite_core::InvitationLink {
    let mut link = list_link(1);
    link.account_id = 42;
    link.targets.repos[0].repo_full_name = "octocat/api".into();
    link
}

/// A request the authority admitted for `link`, pending until `deadline`.
fn pending_request(
    link: ghinvite_core::InvitationLinkId,
    deadline: DateTime<Utc>,
) -> ghinvite_core::request_lifecycle::RequestSnapshot {
    ghinvite_core::request_lifecycle::RequestSnapshot {
        batch: None,
        domain_evidence: None,
        request_id: ghinvite_core::RequestId::new(),
        link_id: Some(link),
        account_id: 42,
        requester_id: 99,
        justification: None,
        state: ghinvite_core::RequestState::Pending,
        admitted_at: deadline - Duration::days(7),
        decision_deadline: Some(deadline),
        revision: 1,
        decision: None,
    }
}

/// An app signed in as `octocat`, with the account installed unless
/// `uninstalled`, and no projected links.
async fn octocat_app(authority: &FakeLinkAuthority, uninstalled: bool) -> (axum::Router, String) {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let account = identity_account(42, "octocat", AccountType::User);
    storage.seed_console_installation(&account).await.unwrap();
    if uninstalled {
        storage
            .uninstall_console_installation(account.installation_id, Utc::now())
            .await
            .unwrap();
    }
    let state = crate::common::sign_in::app_state(
        storage,
        Arc::new(MockTransport::scripted(
            oauth_expectations(OCTOCAT)
                .into_iter()
                .chain([
                    repos_for(42, "octocat"),
                    repo_identity(10, "octocat/api", 42),
                ])
                .collect(),
        )),
        authority.client(),
        console_config(),
    );
    let app = build_app(state, tower_sessions::MemoryStore::default());
    let cookie = sign_in(&app).await;
    (app, cookie)
}

#[tokio::test]
async fn isolated_admin_metadata_and_revoke_use_authority_before_projection() {
    let authority = FakeLinkAuthority::start().await;
    let link = octocat_link();
    let id = link.id;
    let mut authoritative = common::authority_http_fixture::snapshot(&link);
    authoritative.metadata = Some(ghinvite_core::invitation_link::LinkMetadata {
        description: "Authoritative details".into(),
        internal_note: None,
    });
    authority.seed(authoritative);
    let (app, cookie) = octocat_app(&authority, false).await;
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
                        "csrf_token={csrf}&operation_id={id}&description=Changed&account_id=666&user_id=666"
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
    }
    // Each is a call awaiting the authority's answer, never a one-way send.
    assert_eq!(
        authority.calls(),
        [
            "link_status",
            "link_status",
            "update_metadata",
            "link_status",
            "revoke"
        ]
    );
    for method in ["link_status", "update_metadata", "revoke"] {
        for body in authority.received::<serde_json::Value>(method) {
            assert_eq!(
                body["admin"],
                serde_json::json!({"account_id": 42, "user_id": 42})
            );
        }
    }
}

#[tokio::test]
async fn pending_decision_remains_accessible_after_uninstall() {
    let authority = FakeLinkAuthority::start().await;
    let link = octocat_link();
    authority.seed_link(&link);
    let request = pending_request(link.id, Utc::now() + Duration::days(7));
    authority.seed_request(request.clone());
    let (app, cookie) = octocat_app(&authority, true).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/console/accounts/octocat/requests/{}/approve",
                    request.request_id
                ))
                .header("cookie", &cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!(
                    "csrf_token={csrf}&link_id={}&operation_id={}",
                    link.id,
                    ghinvite_core::RequestId::new()
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(authority.calls(), ["link_status", "decide"]);
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
    let authority = FakeLinkAuthority::start().await;
    let link = octocat_link();
    authority.seed_link(&link);
    // Past its deadline, so the authority expires it rather than deciding.
    let request = pending_request(link.id, "2026-01-08T00:00:00Z".parse().unwrap());
    authority.seed_request(request.clone());
    let operation = ghinvite_core::RequestId::new();
    let (app, cookie) = octocat_app(&authority, false).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    // Missing projection is not evidence that the acknowledged request is absent.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/console/accounts/octocat/requests/{}/approve",
                    request.request_id
                ))
                .header("cookie", &cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!(
                    "csrf_token={csrf}&link_id={}&operation_id={operation}&user_id=666",
                    link.id
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let html = response_html(response).await;
    assert!(html.contains("expired"));
    assert!(!html.contains("Request approved"));
    assert_eq!(authority.calls(), ["link_status", "decide"]);
    let calls = authority.received::<DecideRequest>("decide");
    assert_eq!(calls[0].admin.user_id, 42);
    assert_eq!(calls[0].admin.account_id, 42);
    assert_eq!(calls[0].link_id, link.id);
    assert_eq!(
        String::from(calls[0].operation_id.clone()),
        operation.to_string()
    );
}

#[tokio::test]
async fn console_mutations_reject_missing_invalid_and_sibling_origin_tokens() {
    let (app, cookie, authority) =
        build_signed_in_admin_app_with_authority(oauth_expectations(ACME_ADMIN)).await;
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
    assert!(authority.calls().is_empty());
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
    storage.seed_console_installation(account).await.unwrap();
    if account.account_type == AccountType::Organization {
        seed_org_authority(&storage, account).await;
    }
    let app = build_app(
        crate::common::sign_in::app_state(
            storage,
            Arc::new(MockTransport::scripted(expectations)),
            unreachable_restate(),
            console_config(),
        ),
        tower_sessions::MemoryStore::default(),
    );
    let cookie = sign_in(&app).await;
    (app, cookie)
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
    let (app, cookie) = identity_app(&account, oauth_expectations(OCTOCAT)).await;
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

#[tokio::test]
async fn personal_console_discovery_requires_identity_even_after_rename_or_name_reuse() {
    for (id, stored_login, authorized) in [
        (42, "octocat", true),
        (42, "old-octocat", true),
        (999, "octocat", false),
    ] {
        let account = identity_account(id, stored_login, AccountType::User);
        let expectations = oauth_expectations(OCTOCAT);
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
        format!("https://api.github.com/orgs/{login}/memberships/octocat"),
        serde_json::json!({
            "role": "admin", "state": "active",
            "organization": {"id": organization_id, "login": organization_login}, "user":{"id":42,"login":"octocat"}
        }),
    )
}

#[tokio::test]
async fn organization_name_reuse_cannot_authorize_discovery_reads_or_mutations() {
    let account = identity_account(9001, "acme", AccountType::Organization);
    let mut expectations = oauth_expectations(OCTOCAT);
    expectations.push(org_membership("acme", 9999, "acme"));
    expectations.push(org_membership("acme", 9999, "acme"));
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
async fn oauth_signin_keeps_user_bound_installation_evidence_but_revokes_old_session() {
    for (user_id, login) in [(42, "octocat"), (43, "another-user")] {
        let account = identity_account(9001, "acme", AccountType::Organization);
        let mut expectations = oauth_expectations(OCTOCAT);
        expectations.push(org_membership("acme", 9001, "acme"));
        expectations.extend(oauth_expectations(
            GithubUser::new(login, user_id).with_token("u_second"),
        ));
        expectations.push(
            Expectation::status(
                Method::Get,
                format!("https://api.github.com/orgs/acme/memberships/{login}"),
                404,
            )
            // The organization app's installation token.
            .require_header("authorization", "Bearer ghs_fixed_456_77_1"),
        );
        let (app, cookie) = identity_app(&account, expectations).await;
        let response = identity_request(&app, &cookie, "GET", "/console/accounts/acme").await;
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = session_cookie(&response, Some(cookie));
        let old_cookie = cookie.clone();
        let cookie = sign_in_with_cookie(&app, Some(cookie)).await;
        assert_ne!(old_cookie, cookie);
        let response = identity_request(&app, &cookie, "GET", "/console/accounts/acme").await;
        assert_eq!(
            response.status(),
            if user_id == 42 {
                StatusCode::OK
            } else {
                StatusCode::NOT_FOUND
            }
        );
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
    storage.seed_console_installation(&account).await.unwrap();
    seed_org_authority(&storage, &account).await;
    let mut expectations = oauth_expectations(OCTOCAT);
    expectations.push(org_membership("acme", 9001, "acme"));
    expectations.push(org_membership("acme", 9001, "renamed-acme"));
    let state = crate::common::sign_in::app_state(
        storage.clone(),
        Arc::new(MockTransport::scripted(expectations)),
        std::sync::Arc::new(ghinvite_web::RestateClient::new("http://127.0.0.1:9").unwrap()),
        console_config(),
    );
    let app = build_app(state, tower_sessions::MemoryStore::default());
    let cookie = sign_in(&app).await;
    let response = identity_request(&app, &cookie, "GET", "/console/accounts/acme").await;
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = session_cookie(&response, Some(cookie));

    {
        use ghinvite_core::storage::{AppInstallationReadStorage, AppInstallationStorage};
        let mut old = storage
            .app_installation(ghinvite_core::account::App::Org, 77)
            .await
            .unwrap()
            .unwrap();
        old.revision += 1;
        old.account.uninstalled_at = Some(Utc::now());
        storage.put_app_installation(&old).await.unwrap();
        storage
            .uninstall_console_installation(77, Utc::now())
            .await
            .unwrap();
    }
    account.installation_id = 78;
    account.account_id = 9002;
    storage.seed_console_installation(&account).await.unwrap();
    seed_org_authority(&storage, &account).await;
    let response = identity_request(&app, &cookie, "GET", "/console/accounts/acme").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn organization_rename_keeps_identity_authority_and_cache_across_console_routes() {
    let account = identity_account(9001, "acme", AccountType::Organization);
    let mut expectations = oauth_expectations(OCTOCAT);
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
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (
            serde_json::json!({"role": "admin", "state": "active", "organization": {"id": "9001"}}),
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (
            serde_json::json!({"role": "admin", "state": "pending", "organization": {"id": 9001,"login":"acme"},"user":{"id":42,"login":"octocat"}}),
            StatusCode::NOT_FOUND,
        ),
        (
            serde_json::json!({"role": "member", "state": "active", "organization": {"id": 9001,"login":"acme"},"user":{"id":42,"login":"octocat"}}),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let account = identity_account(9001, "acme", AccountType::Organization);
        let mut expectations = oauth_expectations(OCTOCAT);
        expectations.push(Expectation::ok_json(
            Method::Get,
            "https://api.github.com/orgs/acme/memberships/octocat",
            membership,
        ));
        expectations.push(org_membership("acme", 9001, "acme"));
        let (app, cookie) = identity_app(&account, expectations).await;
        let response =
            identity_request(&app, &cookie, "POST", "/console/accounts/acme/links").await;
        assert_eq!(response.status(), denied_status);
        let response = identity_request(&app, &cookie, "GET", "/console/accounts/acme").await;
        assert_eq!(
            response.status(),
            if denied_status == StatusCode::NOT_FOUND {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::OK
            }
        );
    }
}

/// KV can retain expired or future-dated records; service freshness remains authoritative.
struct RetainedAuthorityEvidence(std::sync::Mutex<ghinvite_web::authority_cache::CacheEntry>);
#[async_trait::async_trait]
impl ghinvite_web::authority_cache::AuthorityCache for RetainedAuthorityEvidence {
    async fn get(
        &self,
        key: &str,
    ) -> Result<
        Option<ghinvite_web::authority_cache::CacheEntry>,
        ghinvite_web::authority_cache::CacheUnavailable,
    > {
        assert_eq!(key, "authz:org:77:42:membership");
        Ok(Some(self.0.lock().unwrap().clone()))
    }
    async fn put(
        &self,
        key: &str,
        value: ghinvite_web::authority_cache::CacheEntry,
    ) -> Result<(), ghinvite_web::authority_cache::CacheUnavailable> {
        assert_eq!(key, "authz:org:77:42:membership");
        *self.0.lock().unwrap() = value;
        Ok(())
    }
}
#[tokio::test]
async fn expired_or_future_organization_authority_is_reverified_and_errors_never_grant_access() {
    for observed_at in [Utc::now().timestamp() - 301, Utc::now().timestamp() + 300] {
        let storage = Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::in_memory()
                .await
                .unwrap(),
        );
        let account = acme_installation();
        storage.seed_console_installation(&account).await.unwrap();
        seed_org_authority(&storage, &account).await;
        let mut expectations = oauth_expectations(OCTOCAT);
        expectations.extend([
            Expectation::status(
                Method::Get,
                "https://api.github.com/orgs/acme/memberships/octocat",
                503,
            ),
            Expectation::status(
                Method::Get,
                "https://api.github.com/orgs/acme/memberships/octocat",
                404,
            ),
        ]);
        let cache = Arc::new(RetainedAuthorityEvidence(std::sync::Mutex::new(
            ghinvite_web::authority_cache::CacheEntry {
                observed_at,
                value: serde_json::json!({"role":"admin","state":"active","organization":{"id":9001,"login":"acme"},"user":{"id":42,"login":"octocat"}}),
            },
        )));
        let state = crate::common::sign_in::app_state(
            storage,
            Arc::new(MockTransport::scripted(expectations)),
            unreachable_restate(),
            console_config(),
        )
        .with_authority_cache(cache)
        .with_edge_limits(Arc::new(
            ghinvite_web::rate_limit::MemoryRateLimiter::default(),
        ));
        let app = ghinvite_web::build_app(
            crate::common::sign_in::with_signin_bridge(state),
            tower_sessions::MemoryStore::default(),
        )
        .layer(axum::Extension(
            ghinvite_web::rate_limit::TrustedClientIp::new(std::net::Ipv4Addr::LOCALHOST.into()),
        ));
        let cookie = sign_in(&app).await;
        let response = identity_request(&app, &cookie, "GET", "/console/accounts/acme").await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let response = identity_request(&app, &cookie, "GET", "/console/accounts/acme").await;
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
            .seed_console_installation(&identity_account(42, stored_login, AccountType::User))
            .await
            .unwrap();
        let mut link = list_link(1);
        link.account_id = 42;
        link.targets.repos[0].repo_full_name = format!("{stored_login}/api");
        let authority = FakeLinkAuthority::start().await;
        authority.seed_link(&link);
        let state = crate::common::sign_in::app_state(
            storage.clone(),
            Arc::new(MockTransport::scripted(
                oauth_expectations(OCTOCAT)
                    .into_iter()
                    .chain([
                        repos_for(42, stored_login),
                        repo_identity(10, &format!("{stored_login}/api"), 42),
                    ])
                    .collect(),
            )),
            authority.client(),
            console_config(),
        );
        let app = build_app(state, tower_sessions::MemoryStore::default());
        let cookie = sign_in(&app).await;
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

async fn build_test_app() -> axum::Router {
    use ghinvite_github::mocks::MockTransport;
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let transport: Arc<dyn ghinvite_github::HttpTransportDyn> =
        Arc::new(MockTransport::scripted(vec![]));
    let state = crate::common::sign_in::app_state(
        storage,
        transport,
        std::sync::Arc::new(ghinvite_web::RestateClient::new("http://127.0.0.1:9").unwrap()),
        console_config(),
    );
    let session_store = tower_sessions::MemoryStore::default();
    build_app(state, session_store)
}

/// Signed in as an admin of the installed `acme` organization.
async fn build_signed_in_admin_app() -> (axum::Router, String) {
    signed_in_app(
        &[acme_installation()],
        oauth_expectations(ACME_ADMIN),
        unreachable_restate(),
    )
    .await
}

/// Signed in over the installed `acme` organization with GitHub scripted by
/// `expectations`, and invitation link commands answered by the returned
/// authority.
async fn build_signed_in_admin_app_with_authority(
    mut expectations: Vec<Expectation>,
) -> (axum::Router, String, FakeLinkAuthority) {
    let authority = FakeLinkAuthority::start().await;
    expectations.extend([
        installation_repos_expectation(),
        repo_identity(10, "acme/api", 9001),
        repo_admin("acme/api", 42, "octocat"),
        repo_identity(11, "acme/web", 9001),
        repo_admin("acme/web", 42, "octocat"),
    ]);
    let (app, cookie) =
        signed_in_app(&[acme_installation()], expectations, authority.client()).await;
    (app, cookie, authority)
}

fn repos_for(account_id: u64, login: &str) -> Expectation {
    Expectation::ok_json(
        Method::Get,
        "https://api.github.com/installation/repositories?per_page=100",
        serde_json::json!({"total_count":2,"repositories":[{"id":10,"full_name":format!("{login}/api"),"owner":{"id":account_id,"login":login}},{"id":11,"full_name":format!("{login}/web"),"owner":{"id":account_id,"login":login}}]}),
    )
}

fn installation_repos_expectation() -> Expectation {
    Expectation::ok_json(
        Method::Get,
        "https://api.github.com/installation/repositories?per_page=100",
        serde_json::json!({
            "total_count": 2,
            "repositories": [
                {"id": 10, "full_name": "acme/api", "owner":{"id":9001,"login":"acme"}, "private": true},
                {"id": 11, "full_name": "acme/web", "owner":{"id":9001,"login":"acme"}, "private": true}
            ]
        }),
    )
}

async fn build_signed_in_admin_app_with_console_installations(
    logins: Vec<&str>,
) -> (axum::Router, String) {
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

    let expectations = oauth_expectations(OCTOCAT);
    signed_in_app(&accounts, expectations, unreachable_restate()).await
}

async fn build_signed_in_app_with_failed_installation_discovery() -> (axum::Router, String) {
    let mut expectations = oauth_expectations(OCTOCAT);
    expectations.push(Expectation::status(
        Method::Get,
        "https://api.github.com/orgs/acme/memberships/octocat",
        503,
    ));
    identity_app(&acme_installation(), expectations).await
}

async fn build_signed_in_app_without_installation() -> (axum::Router, String) {
    signed_in_app(&[], oauth_expectations(OCTOCAT), unreachable_restate()).await
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
    storage.seed_link(&link).await.unwrap();
    link.id = ghinvite_core::InvitationLinkId::new();
    link.account_id = 9002;
    link.installation_id = 78;
    link.description = "Other account secret".into();
    storage.seed_link(&link).await.unwrap();

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
    assert!(html.contains("0 / 1000"));
    assert!(html.contains("No expiration"));
    assert!(html.contains("href=\"/console/accounts/acme/links/new\""));
}

/// Saving details reads authoritative targets and sends exactly one mutation.
/// Missing links stop after the authorization read.
#[tokio::test]
async fn saving_link_details_authorizes_targets_before_one_mutation() {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let authority = FakeLinkAuthority::start().await;
    let (app, cookie) = links_app_with_authority(storage, "admin", &authority).await;
    let link = list_link(1);
    authority.seed_link(&link);

    let saved = post_edit(
        &app,
        &cookie,
        &link.id.to_string(),
        "description=Updated&internal_note=",
    )
    .await;
    assert_eq!(saved.status(), StatusCode::SEE_OTHER);
    assert_eq!(authority.calls(), ["link_status", "update_metadata"]);

    let missing = ghinvite_core::InvitationLinkId::new().to_string();
    let response = post_edit(
        &app,
        &cookie,
        &missing,
        "description=Updated&internal_note=",
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let html = response_html(response).await;
    assert!(
        html.contains("Not Found") || html.contains("This console page is not available."),
        "{html}"
    );
    assert_eq!(
        authority.calls(),
        ["link_status", "update_metadata", "link_status"]
    );
}

#[tokio::test]
async fn edit_link_errors_preserve_input_and_do_not_change_details() {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let authority = FakeLinkAuthority::start().await;
    let (app, cookie) = links_app_with_authority(storage, "admin", &authority).await;
    let link = list_link(1);
    authority.seed_link(&link);
    for description in [
        "".to_string(),
        "%20%20".into(),
        "Workshop%0Acohort".into(),
        "x".repeat(121),
    ] {
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
    // Invalid input never reaches the authority.
    assert!(authority.metadata_updates().is_empty());

    authority.fail("update_metadata", 503);
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
    assert!(html.contains("Save outcome unknown"));
    assert!(html.contains("value=\" New details \""));
    assert!(html.contains("> Keep\nthis </textarea>"));
    assert!(!html.contains("aria-invalid=\"true\""));
    let (_, detail) = get_links(&app, &cookie, &format!("/{}", link.id)).await;
    assert!(detail.contains("Workshop 001</h1>"));
    assert!(!detail.contains("New details"));
}

/// A refused ingress call and one whose outcome is unknown must not read the
/// same. Restate may have applied the command before the response went wrong,
/// so "please try again" on an unknown outcome invites applying the same
/// mutation twice.
#[tokio::test]
async fn an_unknown_save_outcome_does_not_invite_a_blind_retry() {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let authority = FakeLinkAuthority::start().await;
    let (app, cookie) = links_app_with_authority(storage, "admin", &authority).await;
    let link = list_link(1);
    authority.seed_link(&link);
    authority.fail("update_metadata", 503);

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
    assert!(!html.contains("ingress"), "{html}");
    assert!(!html.contains("503"), "{html}");
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
        let authority = FakeLinkAuthority::start().await;
        let (app, cookie) = links_app_with_authority(storage, role, &authority).await;
        let mut foreign = list_link(1);
        foreign.account_id = 9002;
        foreign.installation_id = 78;
        foreign.description = "Foreign private context".into();
        authority.seed_link(&foreign);
        let own = list_link(2);
        authority.seed_link(&own);
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
        assert!(authority.metadata_updates().is_empty());
        assert_eq!(
            authority.link(foreign.id).unwrap().description(),
            "Foreign private context"
        );
    }
}

#[tokio::test]
async fn edit_link_save_normalizes_metadata_and_keeps_guardrails() {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let authority = FakeLinkAuthority::start().await;
    let (app, cookie) = links_app_with_authority(storage, "admin", &authority).await;
    let mut link = list_link(1);
    link.max_uses = Some(8);
    link.uses_count = 3;
    link.revoked_at = Some(link.created_at);
    link.revoked_by = Some(link.created_by);
    link.internal_note = Some("Old private note".into());
    authority.seed_link(&link);
    for (note, expected) in [("%20New%0Anote%20", Some("New\nnote")), ("%20%20", None)] {
        let response = post_edit(&app, &cookie, &link.id.to_string(), &format!("description=%20Updated+workshop%20&internal_note={note}&permission=admin&max_uses=999&uses_count=0&revoked_at=&account_id=9002&by_user=999")).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response.headers()["location"],
            format!("/console/accounts/acme/links/{}", link.id)
        );
        // Only the normalized metadata reaches the authority, as the signed-in
        // admin of the routed account; tampered guardrails are not commands.
        let update = authority.metadata_updates().pop().unwrap();
        assert_eq!(update.link_id, link.id);
        assert_eq!(update.description, "Updated workshop");
        assert_eq!(update.internal_note.as_deref(), expected);
        assert_eq!(update.admin.account_id, 9001);
        assert_eq!(update.admin.user_id, 42);
        let (status, detail) = get_links(&app, &cookie, &format!("/{}", link.id)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(detail.contains("Updated workshop</h1>"));
        assert!(detail.contains("3 / 8"));
        assert!(detail.contains("inactive"));
        assert!(!detail.contains("Old private note"));
        if let Some(expected) = expected {
            assert!(detail.contains(expected));
        } else {
            assert!(!detail.contains("New\nnote"));
        }
    }
    let current = authority.link(link.id).unwrap();
    assert_eq!(current.creation.max_uses, Some(8));
    assert_eq!(
        current.creation.targets.repos[0].grant,
        ghinvite_core::targets::RepoRole::Read
    );
    assert_eq!(current.uses, 3);
    assert!(current.revoked_at.is_some());
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
    let authority = FakeLinkAuthority::start().await;
    let (app, cookie) = links_app_with_authority(storage, "admin", &authority).await;
    let mut link = list_link(1);
    link.internal_note = Some("Private note\nSecond line".into());
    link.revoked_at = Some(link.created_at);
    link.revoked_by = Some(link.created_by);
    authority.seed_link(&link);
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
    let authority = FakeLinkAuthority::start().await;
    let (app, cookie) = links_app_with_authority(storage, "admin", &authority).await;
    let link = list_link(1);
    authority.seed_link(&link);

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
    storage.seed_link(&list_link(1)).await.unwrap();
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
            link.revoked_by = Some(link.created_by);
        }
        storage.seed_link(&link).await.unwrap();
    }
    let (status, first) = get_links(&app, &cookie, "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first.matches("Link ID:").count(), 25);
    assert!(first.find("Workshop 059</a>").unwrap() < first.find("Workshop 011</a>").unwrap());
    assert!(!first.contains("Workshop 060</a>"));
    assert!(!first.contains(">Previous</a>"));
    assert!(first.contains("filter=active&#38;sort=created&#38;direction=desc&#38;page=2"));

    let query = "?filter=inactive&sort=description&direction=asc&page=2";
    let (status, second) = get_links(&app, &cookie, query).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second.matches("Link ID:").count(), 5);
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
    link.revoked_by = Some(link.created_by);
    storage.seed_link(&link).await.unwrap();
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
    sqlx::query("DROP TABLE invitation_link_targets")
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
        batch_source: None,
        access_lifetime_seconds: None,
        email_domains: vec![],
        id: ghinvite_core::InvitationLinkId::new(),
        organization_installation_id: None,
        installation_id: 77,
        account_id: 9001,
        created_by: 42,
        created_at: "2026-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
            + Duration::seconds(n.into()),
        expires_at: None,
        max_uses: Some(1000),
        invitation_lifetime_seconds: 604800,
        abuse_threshold_per_hour: None,
        budget_share_pct: 50,
        uses_count: 0,
        secret: common::capability::state(),
        approval_required: false,
        description: format!("Workshop {n:03}"),
        internal_note: None,
        revoked_at: None,
        revoked_by: None,
        pause: None,
        targets: ghinvite_core::targets::Targets {
            repos: vec![ghinvite_core::InvitationLinkRepo {
                repo_id: 10,
                repo_full_name: "acme/api".into(),
            }]
            .into_iter()
            .map(|repo| ghinvite_core::targets::RepoTarget {
                repo_id: repo.repo_id,
                repo_full_name: repo.repo_full_name,
                grant: ghinvite_core::Permission::Pull.into(),
            })
            .collect(),
            ..Default::default()
        },
    }
}

async fn links_app(
    storage: Arc<ghinvite_storage_sqlx::SqlxStorage>,
    role: &str,
) -> (axum::Router, String) {
    links_app_with_authority(storage, role, &FakeLinkAuthority::start().await).await
}

/// Sign in to the `acme` Console as `role`, with invitation link commands and
/// reads answered by `authority`.
async fn links_app_with_authority(
    storage: Arc<ghinvite_storage_sqlx::SqlxStorage>,
    role: &str,
    authority: &FakeLinkAuthority,
) -> (axum::Router, String) {
    for (installation_id, account_id, login) in [(77, 9001, "acme"), (78, 9002, "other")] {
        storage
            .seed_console_installation(&Account {
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
    use ghinvite_core::storage::{AppInstallationReadStorage, AppInstallationStorage};
    if role != "admin" {
        let mut installation = storage
            .app_installation(ghinvite_core::account::App::Repo, 77)
            .await
            .unwrap()
            .unwrap();
        installation.revision += 1;
        installation.installed_by_user_id = Some(43);
        installation.installed_by_login = Some("other-installer".into());
        storage.put_app_installation(&installation).await.unwrap();
    }
    let mut expectations = oauth_expectations(OCTOCAT);
    expectations.extend([
        installation_repos_expectation(),
        repo_identity(10, "acme/api", 9001),
        repo_admin("acme/api", 42, "octocat"),
        repo_identity(11, "acme/web", 9001),
        repo_admin("acme/web", 42, "octocat"),
    ]);
    let state = crate::common::sign_in::app_state(
        storage,
        Arc::new(MockTransport::scripted(expectations)),
        authority.client(),
        console_config(),
    );
    let app = build_app(state, tower_sessions::MemoryStore::default());
    let cookie = sign_in(&app).await;
    (app, cookie)
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

/// Every Console response is private to the signed-in admin, whatever its
/// shape: a page, the redirect after a form, or a refusal.
#[tokio::test]
async fn console_responses_are_never_stored() {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let authority = FakeLinkAuthority::start().await;
    let (app, cookie) = links_app_with_authority(storage, "admin", &authority).await;
    let link = list_link(1);
    authority.seed_link(&link);

    let saved = post_edit(
        &app,
        &cookie,
        &link.id.to_string(),
        "description=Updated&internal_note=",
    )
    .await;
    assert_eq!(saved.status(), StatusCode::SEE_OTHER);
    let mut responses = vec![("POST edit", saved)];
    for (label, method, path) in [
        (
            "links page",
            "GET",
            "/console/accounts/acme/links".to_owned(),
        ),
        (
            "link page",
            "GET",
            format!("/console/accounts/acme/links/{}", link.id),
        ),
        (
            "attempts page",
            "GET",
            "/console/accounts/acme/attempts".into(),
        ),
        (
            "not-found page",
            "GET",
            "/console/accounts/acme/missing".into(),
        ),
        (
            "plain refusal",
            "POST",
            "/console/accounts/acme/missing".into(),
        ),
    ] {
        responses.push((label, identity_request(&app, &cookie, method, &path).await));
    }
    let signed_out = build_test_app()
        .await
        .oneshot(
            Request::builder()
                .uri("/console/accounts/acme/links")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(signed_out.status(), StatusCode::SEE_OTHER);
    responses.push(("sign-in redirect", signed_out));

    for (label, response) in responses {
        assert_eq!(
            response
                .headers()
                .get("cache-control")
                .map(|value| value.to_str().unwrap()),
            Some("private, no-store"),
            "{label} ({})",
            response.status()
        );
    }
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
            link.revoked_by = Some(link.created_by);
        }
        storage.seed_link(&link).await.unwrap();
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
    storage.seed_link(&link).await.unwrap();
    let mut foreign_link = list_link(2);
    foreign_link.account_id = 9002;
    foreign_link.installation_id = 78;
    storage.seed_link(&foreign_link).await.unwrap();
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
            serde_json::json!({"permission": "private-invalid-permission", "approval_required": "true", "max_uses": -1, "repo_count": "2"}),
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
            serde_json::json!({"repo_full_name":"private-repo","requester_id":8}),
            "—",
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
        "private-invalid-permission",
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
    storage.seed_link(&link).await.unwrap();
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
        let (app, cookie) = if role == "admin" {
            let account = acme_installation();
            storage.seed_console_installation(&account).await.unwrap();
            seed_org_authority(&storage, &account).await;
            let mut expectations = oauth_expectations(OCTOCAT);
            expectations.push(org_membership("acme", 9001, "acme"));
            let app = build_app(
                crate::common::sign_in::app_state(
                    storage.clone(),
                    Arc::new(MockTransport::scripted(expectations)),
                    unreachable_restate(),
                    console_config(),
                ),
                tower_sessions::MemoryStore::default(),
            );
            let cookie = sign_in(&app).await;
            (app, cookie)
        } else {
            links_app(storage.clone(), role).await
        };
        if role == "member" {
            assert_eq!(
                audit_get(&app, &cookie, base).await.0,
                StatusCode::NOT_FOUND
            );
        } else {
            let event = audit_event(1, ghinvite_core::audit::EventType::InstallationCreated);
            storage.audit(&event).await.unwrap();
            storage
                .uninstall_console_installation(77, Utc::now())
                .await
                .unwrap();
            // An uninstalled account keeps its history readable to its admins.
            let (status, html) = audit_get(&app, &cookie, "/console/accounts/acme/audit").await;
            assert_eq!(status, StatusCode::OK);
            assert!(html.contains("resource-001"));
            storage
                .seed_console_installation(&Account {
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
        .seed_console_installation(&Account {
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
    let state = crate::common::sign_in::app_state(
        storage,
        Arc::new(MockTransport::scripted(oauth_expectations(OCTOCAT))),
        std::sync::Arc::new(ghinvite_web::RestateClient::new("http://127.0.0.1:9").unwrap()),
        console_config(),
    );
    let app = build_app(state, tower_sessions::MemoryStore::default());
    let cookie = sign_in(&app).await;
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
    let authority = FakeLinkAuthority::start().await;
    let (app, cookie) = links_app_with_authority(storage.clone(), "admin", &authority).await;
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
    assert!(authority.calls().is_empty());
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
    let mut expectations = oauth_expectations(ACME_ADMIN);
    expectations.push(installation_repos_expectation());
    let (app, cookie, _authority) = build_signed_in_admin_app_with_authority(expectations).await;

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

    // Island root wraps the form, which posts to the creation identity.
    let island = "<div id=\"link-form-island\"><form method=\"post\" action=\"";
    let action = text
        .split_once(island)
        .expect("island root wraps the form")
        .1
        .split_once('"')
        .unwrap()
        .0
        .replace("&amp;", "&")
        .replace("&#38;", "&");
    assert!(text.contains("</form></div>"));

    // The props blob is a data block carrying action, values, repos and the
    // server's instant.
    let props_open = "<script type=\"application/json\" id=\"link-form-props\">";
    let blob_at = text.find(props_open).expect("props blob present");
    let blob = &text[blob_at + props_open.len()..];
    let blob = &blob[..blob.find("</script>").unwrap()];
    let props: ghinvite_ui::link_form::LinkFormIslandProps = serde_json::from_str(blob).unwrap();
    assert_eq!(props.action, action);
    assert_eq!(props.values.repo_grants, vec!["10:pull", "11:pull"]);
    let expected = ghinvite_ui::links::LinkFormValues {
        repo_grants: props.values.repo_grants.clone(),
        ..Default::default()
    };
    assert_eq!(props.values, expected);
    assert_eq!(
        props.repos,
        vec![
            ghinvite_ui::link_form::RepositoryChoice {
                id: 10,
                full_name: "acme/api".to_string(),
            },
            ghinvite_ui::link_form::RepositoryChoice {
                id: 11,
                full_name: "acme/web".to_string(),
            },
        ]
    );
    assert!(
        before <= props.now && props.now <= after,
        "now is the server's instant"
    );
    // The creation identity: a fresh link ID and the whole-second instant the
    // server validates a submission against, the one the island sees.
    let query = action
        .strip_prefix("/console/accounts/acme/links?")
        .expect("the form posts to the links collection");
    let query: BTreeMap<_, _> = url::form_urlencoded::parse(query.as_bytes()).collect();
    assert!(
        query["link_id"]
            .parse::<ghinvite_core::InvitationLinkId>()
            .is_ok()
    );
    assert_eq!(query["anchor"], props.now.timestamp().to_string());

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
    let created = post_create_link(
        "description=%3C%2Fscript%3E%3Cscript%3Ealert(1)%3C%2Fscript%3E&permission=owner&max_uses=7&expires_in_days=45&repo_ids=999&repo_grants=999:owner&repo_ids=10&repo_grants=10:owner",
    )
    .await;

    assert_eq!(created.response.status(), StatusCode::OK);
    let uri = created.uri();
    let text = response_html(created.response).await;

    let props_open = "<script type=\"application/json\" id=\"link-form-props\">";
    let blob_at = text.find(props_open).expect("props blob present");
    let blob = &text[blob_at + props_open.len()..];
    let blob = &blob[..blob.find("</script>").unwrap()];
    let props: ghinvite_ui::link_form::LinkFormIslandProps = serde_json::from_str(blob).unwrap();

    // Values verbatim, errors as the server attached them.
    assert_eq!(
        props.values.description,
        "</script><script>alert(1)</script>"
    );
    assert_eq!(props.values.permission, "owner");
    assert_eq!(props.values.max_uses, "7");
    assert_eq!(props.values.expires_in_days, "45");
    assert_eq!(props.values.selected_repo_ids, vec![10]);
    assert!(props.values.errors.permission.is_none());
    // Submitted grants verbatim; the unselected acme/web starts at pull.
    assert_eq!(
        props.values.repo_grants,
        vec!["999:owner", "10:owner", "11:pull"]
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
    // The re-rendered form keeps the creation identity it was submitted with,
    // and the island validates against the same instant the server did.
    assert_eq!(props.action, uri);
    assert_eq!(props.now, created.anchor);

    // The description cannot break out of the data block: the raw bytes hold
    // no `<`, and the page has exactly three script elements.
    assert!(!blob.contains('<'));
    assert_eq!(text.matches("</script>").count(), 3);
    assert!(created.authority.created().is_empty());
}

#[tokio::test]
async fn create_link_invalid_description_rerenders_form_with_errors_and_preserved_values() {
    let created = post_create_link(
        "description=%20%20%20&permission=push&approval_required=true&max_uses=7&expires_in_days=45&internal_note=Keep+this+note&repo_ids=10&repo_grants=10:push",
    )
    .await;

    assert_eq!(created.response.status(), StatusCode::OK);
    assert!(created.authority.created().is_empty());
    let text = response_html(created.response).await;
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
    let created = post_create_link(
        "description=%20AI+coding+workshop%20&permission=push&approval_required=true&max_uses=7&expires_in_days=45&internal_note=Keep+this+note&repo_ids=10&repo_grants=10:push",
    )
    .await;

    assert_eq!(created.response.status(), StatusCode::OK);
    assert!(created.response.headers().get("location").is_none());
    let command = created.command();
    assert_eq!(command.link_id, created.link_id);
    assert_eq!(command.account_id, 9001);
    assert_eq!(command.installation_id, 77);
    assert_eq!(command.admin.account_id, 9001);
    assert_eq!(command.admin.user_id, 42);
    assert_eq!(command.description, "AI coding workshop");
    assert_eq!(command.internal_note.as_deref(), Some("Keep this note"));
    assert_eq!(
        command.targets.repos[0].grant,
        ghinvite_core::targets::RepoRole::Write
    );
    assert!(command.approval_required);
    assert_eq!(command.max_uses, Some(7));
    // Expiry is exactly `days` after the creation anchor, so a retry of the
    // same identity yields the same command.
    assert_eq!(
        command.expires_at,
        Some(created.anchor + Duration::days(45))
    );
    assert_eq!(created.repo_ids(), vec![10]);
    assert_eq!(command.targets.repos[0].repo_full_name, "acme/api");
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
    let created = post_create_link(
        "description=AI+coding+workshop&permission=push&max_uses=abc&expires_in_days=0&internal_note=Keep+this+note&repo_ids=10&repo_grants=10:push",
    )
    .await;

    assert_eq!(created.response.status(), StatusCode::OK);
    assert!(created.authority.created().is_empty());
    let text = response_html(created.response).await;
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
    let created = post_create_link(
        "description=AI+coding+workshop&permission=pull&max_uses=+3+&expires_in_days=+10+&repo_ids=11&repo_grants=11:pull",
    )
    .await;

    assert_eq!(created.response.status(), StatusCode::OK);
    let command = created.command();
    assert_eq!(command.max_uses, Some(3));
    assert_eq!(
        command.expires_at,
        Some(created.anchor + Duration::days(10))
    );
    assert_eq!(created.repo_ids(), vec![11]);
}

#[tokio::test]
async fn create_link_blank_numeric_guardrails_default_to_fourteen_days() {
    let created = post_create_link(
        "description=AI+coding+workshop&permission=pull&approval_required=true&max_uses=&expires_in_days=&repo_ids=10&repo_grants=10:pull",
    )
    .await;

    assert_eq!(created.response.status(), StatusCode::OK);
    let command = created.command();
    assert_eq!(command.max_uses, None);
    assert_eq!(
        command.expires_at,
        Some(created.anchor + Duration::days(14))
    );
}

#[tokio::test]
async fn create_link_requires_a_creation_identity() {
    let mut expectations = oauth_expectations(ACME_ADMIN);
    expectations.push(installation_repos_expectation());
    let (app, cookie, authority) = build_signed_in_admin_app_with_authority(expectations).await;
    let token = common::csrf_token(&app, &cookie).await;
    let link_id = ghinvite_core::InvitationLinkId::new();
    for query in [
        String::new(),
        format!("?link_id={link_id}"),
        format!("?anchor={}", Utc::now().timestamp()),
        format!("?link_id={link_id}&anchor={}", i64::MAX),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/console/accounts/acme/links{query}"))
                    .header("cookie", &cookie)
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(format!(
                        "csrf_token={token}&description=AI+coding+workshop&permission=pull&repo_ids=10&repo_grants=10:pull"
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{query}");
    }
    assert!(authority.calls().is_empty());
}

const REPO_SCOPE_REQUIRED: &str = "Repository scope is required. Select at least one available repository for this invitation link.";

/// A new invitation link form POST and what reached the link authority.
struct CreatePost {
    response: axum::response::Response,
    authority: FakeLinkAuthority,
    /// The creation identity the form was posted with.
    link_id: ghinvite_core::InvitationLinkId,
    anchor: DateTime<Utc>,
}

impl CreatePost {
    /// The form action that carries this creation identity.
    fn uri(&self) -> String {
        format!(
            "/console/accounts/acme/links?link_id={}&anchor={}",
            self.link_id,
            self.anchor.timestamp()
        )
    }

    /// The one creation command the authority received.
    fn command(&self) -> ghinvite_core::invitation_link::CreateLink {
        let mut created = self.authority.created();
        assert_eq!(created.len(), 1, "exactly one creation command");
        created.pop().unwrap()
    }

    fn repo_ids(&self) -> Vec<u64> {
        self.command()
            .targets
            .repos
            .iter()
            .map(|r| r.repo_id)
            .collect()
    }
}

/// POST the new invitation link form, with a fresh creation identity, as a
/// signed-in admin whose installation exposes `acme/api` (10) and `acme/web`
/// (11).
async fn post_create_link(body: impl AsRef<str>) -> CreatePost {
    let mut expectations = oauth_expectations(ACME_ADMIN);
    expectations.push(installation_repos_expectation());
    let (app, cookie, authority) = build_signed_in_admin_app_with_authority(expectations).await;
    let token = common::csrf_token(&app, &cookie).await;
    let mut created = CreatePost {
        response: axum::response::Response::default(),
        authority,
        link_id: ghinvite_core::InvitationLinkId::new(),
        // The form renders its anchor in whole seconds.
        anchor: DateTime::from_timestamp(Utc::now().timestamp(), 0).unwrap(),
    };
    created.response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(created.uri())
                .header("cookie", cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!("csrf_token={token}&{}", body.as_ref())))
                .unwrap(),
        )
        .await
        .unwrap();
    created
}

#[tokio::test]
async fn create_link_without_repositories_rerenders_form_with_repository_scope_error() {
    let created = post_create_link(
        "description=AI+coding+workshop&permission=push&approval_required=true&max_uses=7&expires_in_days=45&internal_note=Keep+this+note",
    )
    .await;

    assert_eq!(created.response.status(), StatusCode::OK);
    assert!(created.authority.created().is_empty());
    let text = response_html(created.response).await;
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
    let created = post_create_link(
        "description=AI+coding+workshop&permission=push&repo_ids=999&repo_grants=999:push&repo_ids=1000&repo_grants=1000:push",
    )
    .await;

    assert_eq!(created.response.status(), StatusCode::OK);
    assert!(created.authority.created().is_empty());
    let text = response_html(created.response).await;
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
    let created = post_create_link(
        "description=AI+coding+workshop&permission=push&repo_ids=999&repo_grants=999:push&repo_ids=11&repo_grants=11:push&repo_ids=10&repo_grants=10:push",
    )
    .await;

    assert_eq!(created.response.status(), StatusCode::OK);
    assert!(created.authority.created().is_empty());
    let html = response_html(created.response).await;
    assert!(html.contains("Selected repositories are no longer available: 999"));
    assert!(html.contains("Review the remaining scope before creating"));
    assert!(html.contains("value=\"10\" checked"));
    assert!(html.contains("value=\"11\" checked"));
}

#[tokio::test]
async fn create_link_validation_failure_keeps_available_repositories_checked_and_drops_unknown() {
    let created = post_create_link("description=&permission=push&repo_ids=10&repo_grants=10:push&repo_ids=999&repo_grants=999:push").await;

    assert_eq!(created.response.status(), StatusCode::OK);
    assert!(created.authority.created().is_empty());
    let text = response_html(created.response).await;
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

const REPO_GRANT_REQUIRED: &str = ghinvite_ui::link_form::REPO_GRANT_REQUIRED;

#[tokio::test]
async fn create_link_missing_target_grant_rerenders_form_with_grant_error() {
    // Every selected repository needs its own grant; the set-all helper is optional.
    // It must get the same inline error path as a wrong value, not a bare 400
    // from form deserialization.
    let created = post_create_link(
        "description=AI+coding+workshop&approval_required=true&max_uses=7&expires_in_days=45&repo_ids=10",
    )
    .await;

    assert_eq!(created.response.status(), StatusCode::OK);
    assert!(created.authority.created().is_empty());
    let text = response_html(created.response).await;
    assert!(text.contains(REPO_GRANT_REQUIRED));
    assert!(text.contains("id=\"repo_grants-error\""));
    assert!(!text.contains("Bad Request"));
    assert_preserved_description_input(&text);
    assert!(text.contains("value=\"10\" checked"));
}

#[tokio::test]
async fn create_link_malformed_target_grants_keep_raw_props_and_never_reach_commands() {
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
            body.append_pair("permission", "pull")
                .append_pair("repo_grants", &format!("10:{raw}"));
        }
        let created = post_create_link(body.finish()).await;
        assert_eq!(
            created.response.status(),
            StatusCode::OK,
            "permission={raw:?}"
        );
        assert!(created.authority.created().is_empty());
        let html = response_html(created.response).await;
        let props = html
            .split_once("<script type=\"application/json\" id=\"link-form-props\">")
            .unwrap()
            .1
            .split_once("</script>")
            .unwrap()
            .0;
        let props: ghinvite_ui::link_form::LinkFormIslandProps =
            serde_json::from_str(props).unwrap();
        // The selected repository keeps exactly what was submitted; the
        // unselected acme/web starts at pull.
        assert_eq!(
            props.values.repo_grants,
            raw.map(|grant| format!("10:{grant}"))
                .into_iter()
                .chain(["11:pull".to_string()])
                .collect::<Vec<_>>()
        );
        assert_eq!(
            props.values.errors.repo_grants.as_deref(),
            Some(REPO_GRANT_REQUIRED)
        );
        let select = html
            .split_once("id=\"repo-grant-10\"")
            .unwrap()
            .1
            .split_once("</select>")
            .unwrap()
            .0;
        let (attributes, options) = select.split_once('>').unwrap();
        for attribute in [
            "name=\"repo_grants\"",
            "aria-invalid=\"true\"",
            "aria-labelledby=\"repo-grant-10-label\"",
            "aria-describedby=\"repo_grants-error\"",
            "aria-errormessage=\"repo_grants-error\"",
        ] {
            assert!(attributes.contains(attribute), "{attributes}");
        }
        assert!(attributes.contains("select-error"));
        assert_eq!(options.matches("<option ").count(), 5);
        assert_eq!(options.matches(" selected=true").count(), 1);
        assert!(options.contains("<option value=\"10:pull\" selected=true>pull</option>"));
        assert!(!options.contains("owner"));
        assert!(!options.contains("value=\"\""));
        let region = html.split_once("id=\"repo_grants-error\"").unwrap().1;
        let (attributes, content) = region.split_once('>').unwrap();
        assert!(attributes.contains("aria-live=\"polite\""));
        assert!(!attributes.contains("hidden"));
        assert!(content.contains(REPO_GRANT_REQUIRED));
        assert_eq!(html.matches("aria-invalid=\"true\"").count(), 1);
        assert_description_error_empty(&html);
    }
}

#[tokio::test]
async fn create_link_tampered_target_grant_rerenders_form_with_grant_error() {
    let created = post_create_link(
        "description=AI+coding+workshop&permission=owner&approval_required=true&max_uses=7&expires_in_days=45&internal_note=Keep+this+note&repo_ids=10&repo_grants=10:owner",
    )
    .await;

    assert_eq!(created.response.status(), StatusCode::OK);
    assert!(
        created.authority.created().is_empty(),
        "a tampered permission level must not reach the link authority"
    );
    let text = response_html(created.response).await;
    assert!(text.contains("New invitation link"));
    assert!(text.contains("Fix the highlighted fields before creating this invitation link."));
    assert!(text.contains(REPO_GRANT_REQUIRED));
    assert!(text.contains("id=\"repo_grants-error\""));
    assert!(text.contains("aria-describedby=\"repo_grants-error\""));
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
    // The permission helper plus one grant select per listed repository: the
    // failed form lists the picker's page again, not only the selection.
    assert_eq!(text.matches("<option").count(), 15);
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
async fn create_link_tampered_target_grant_is_reported_with_other_field_errors() {
    let created =
        post_create_link("description=&permission=Push&max_uses=0&repo_ids=10&repo_grants=10:Push")
            .await;

    assert_eq!(created.response.status(), StatusCode::OK);
    assert!(created.authority.created().is_empty());
    let text = response_html(created.response).await;
    assert!(text.contains("id=\"repo_grants-error\""));
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
        let body = format!(
            "description=AI+coding+workshop&permission={raw}&repo_ids=10&repo_grants=10:{raw}"
        );
        let created = post_create_link(body).await;

        assert_eq!(
            created.response.status(),
            StatusCode::OK,
            "permission={raw:?}"
        );
        assert!(created.response.headers().get("location").is_none());
        assert_eq!(
            ghinvite_core::Permission::from(created.command().targets.repos[0].grant),
            expected,
            "permission={raw:?}"
        );
    }
}

#[tokio::test]
async fn create_link_approval_policy_checkbox_behaviour_is_unchanged() {
    // Checked means account admin approval is required.
    let created = post_create_link(
        "description=AI+coding+workshop&permission=pull&approval_required=true&repo_ids=10&repo_grants=10:pull",
    )
    .await;
    assert_eq!(created.response.status(), StatusCode::OK);
    assert!(
        created.command().approval_required,
        "checked box requires admin approval"
    );

    // Unchecked (the key is absent from a native form POST) means auto-approve.
    let created = post_create_link(
        "description=AI+coding+workshop&permission=pull&repo_ids=10&repo_grants=10:pull",
    )
    .await;
    assert_eq!(created.response.status(), StatusCode::OK);
    assert!(
        !created.command().approval_required,
        "unchecked box auto-approves"
    );
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

/// A malformed id names nothing, so every Console route that takes one
/// answers the admin exactly as for a missing resource: the Console's own
/// not-found page, reads and mutations alike.
#[tokio::test]
async fn console_malformed_path_ids_render_the_console_404() {
    let (app, cookie) = build_signed_in_admin_app().await;
    let base = "/console/accounts/acme";
    let mut responses = Vec::new();
    for path in [
        "links/not-a-link-id",
        "links/not-a-link-id/edit",
        "links/not-a-link-id/requests",
        "requests/not-a-request-id",
    ] {
        responses.push((
            path,
            identity_request(&app, &cookie, "GET", &format!("{base}/{path}")).await,
        ));
    }
    for (path, body) in [
        ("links/not-a-link-id/edit", "description=Kept"),
        (
            "links/not-a-link-id/revoke",
            "operation_id=01ARZ3NDEKTSV4RRFFQ69G5FAV",
        ),
        ("requests/not-a-request-id/approve", ""),
        ("requests/not-a-request-id/decline", ""),
    ] {
        responses.push((
            path,
            post_form(&app, &cookie, &format!("{base}/{path}"), body).await,
        ));
    }

    for (path, response) in responses {
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        let html = response_html(response).await;
        assert!(
            html.contains("Not Found") || html.contains("This console page is not available."),
            "{path}: {html}"
        );
        assert!(html.contains("href=\"/console/accounts/acme\""), "{path}");
    }
}

const LINK_DETAILS_UPDATED: &str = "Invitation link details updated.";
const LINK_REVOKED: &str = "Invitation link stopped accepting new invitation requests.";
const LINK_CREATE_REJECTED: &str =
    "The invitation link could not be created with these values. Review them and try again.";
const LINK_REVOKE_REJECTED: &str =
    "This invitation link could not be stopped. Reload it and try again.";

async fn post_form(
    app: &axum::Router,
    cookie: &str,
    uri: &str,
    body: &str,
) -> axum::response::Response {
    let token = common::csrf_token(app, cookie).await;
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("cookie", cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!("csrf_token={token}&{body}")))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// Follow a redirect and return the page it lands on.
async fn follow(
    app: &axum::Router,
    cookie: &str,
    response: axum::response::Response,
) -> (StatusCode, String) {
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let location = response.headers()["location"].to_str().unwrap().to_owned();
    let response = identity_request(app, cookie, "GET", &location).await;
    (response.status(), response_html(response).await)
}

/// The action of the new invitation link form, unescaped.
fn creation_form_action(html: &str) -> String {
    html.split("action=\"/console/accounts/acme/links?")
        .nth(1)
        .expect("creation form action")
        .split('"')
        .next()
        .unwrap()
        .replace("&#38;", "&")
        .replace("&amp;", "&")
}

#[tokio::test]
async fn create_link_shows_secret_once_and_detail_cannot_redisplay_it() {
    let mut expectations = oauth_expectations(ACME_ADMIN);
    expectations.push(installation_repos_expectation());
    let (app, cookie, authority) = build_signed_in_admin_app_with_authority(expectations).await;
    let link_id = ghinvite_core::InvitationLinkId::new();
    let uri = format!(
        "/console/accounts/acme/links?link_id={link_id}&anchor={}",
        Utc::now().timestamp()
    );
    let response = post_form(
        &app,
        &cookie,
        &uri,
        "description=Flash+workshop&permission=pull&expires_in_days=5&repo_ids=10&repo_grants=10:pull",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = response_html(response).await;
    let secret = html
        .split("/i#")
        .nth(1)
        .expect("one-time fragment URL")
        .split('"')
        .next()
        .unwrap();
    assert_eq!(secret.len(), 43);
    assert!(html.contains("shown once"));
    assert_eq!(authority.created().len(), 1);
    let (_, again) = get_links(&app, &cookie, &format!("/{link_id}")).await;
    assert!(!again.contains(secret), "the secret cannot be redisplayed");
    assert!(!again.contains("/i#"));
    assert!(again.contains("Rotate invitation URL"));
}

#[tokio::test]
async fn create_link_authority_rejection_rerenders_form_with_values_and_identity() {
    let mut expectations = oauth_expectations(ACME_ADMIN);
    // The rejected submission and the corrected resubmission each read the
    // installation's repositories.
    expectations.push(installation_repos_expectation());
    expectations.push(installation_repos_expectation());
    let (app, cookie, authority) = build_signed_in_admin_app_with_authority(expectations).await;
    let link_id = ghinvite_core::InvitationLinkId::new();
    let anchor = Utc::now().timestamp();
    let uri = format!("/console/accounts/acme/links?link_id={link_id}&anchor={anchor}");
    authority.fail("create", 400);
    let response = post_form(
        &app,
        &cookie,
        &uri,
        "description=AI+coding+workshop&permission=push&approval_required=true&max_uses=7&expires_in_days=45&internal_note=Keep+this+note&repo_ids=10&repo_grants=10:push",
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let html = response_html(response).await;
    assert!(html.contains("New invitation link"));
    assert!(html.contains(LINK_CREATE_REJECTED));
    assert_preserved_description_input(&html);
    assert!(html.contains("value=\"push\" selected"));
    assert!(html.contains("name=\"approval_required\" value=\"true\" checked"));
    assert_numeric_input(&html, "max_uses", "7", None);
    assert_numeric_input(&html, "expires_in_days", "45", None);
    assert!(html.contains("Keep this note"));
    assert!(html.contains("value=\"10\" checked"));
    assert_eq!(
        creation_form_action(&html),
        format!("link_id={link_id}&anchor={anchor}"),
        "the same creation identity is resubmitted"
    );
    // The authority's rejection is not disclosed.
    assert!(!html.contains("Invalid command"));
    assert!(!html.contains("Bad Request"));
    assert_eq!(
        authority.calls().iter().filter(|m| *m == "create").count(),
        1
    );
    assert!(authority.link(link_id).is_none());
    // A definitive rejection is not a recoverable attempt.
    let attempts = response_html(
        identity_request(&app, &cookie, "GET", "/console/accounts/acme/attempts").await,
    )
    .await;
    assert!(!attempts.contains(&format!("create-{link_id}")));

    // Corrected values under the same identity create the link.
    authority.recover("create");
    let response = post_form(
        &app,
        &cookie,
        &uri,
        "description=Corrected+workshop&permission=push&expires_in_days=45&repo_ids=10&repo_grants=10:push",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response_html(response).await.contains("/i#"));
    assert_eq!(
        authority.link(link_id).unwrap().creation.description,
        "Corrected workshop"
    );
}

#[tokio::test]
async fn link_detail_mutations_flash_once_after_the_redirect() {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let authority = FakeLinkAuthority::start().await;
    let (app, cookie) = links_app_with_authority(storage, "admin", &authority).await;
    let link = list_link(1);
    authority.seed_link(&link);

    let response = post_edit(
        &app,
        &cookie,
        &link.id.to_string(),
        "description=Updated+workshop&internal_note=",
    )
    .await;
    let (status, html) = follow(&app, &cookie, response).await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("Updated workshop</h1>"));
    assert_eq!(html.matches(LINK_DETAILS_UPDATED).count(), 1);

    let revoke = format!("/console/accounts/acme/links/{}/revoke", link.id);
    let response = post_form(&app, &cookie, &revoke, &format!("operation_id={}", link.id)).await;
    let (status, html) = follow(&app, &cookie, response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(html.matches(LINK_REVOKED).count(), 1);
    assert!(!html.contains(LINK_DETAILS_UPDATED));
    assert!(authority.link(link.id).unwrap().revoked_at.is_some());
    let (_, again) = get_links(&app, &cookie, &format!("/{}", link.id)).await;
    assert!(!again.contains(LINK_REVOKED), "a flash is shown once");
}

#[tokio::test]
async fn revoke_authority_rejection_returns_to_link_detail_with_an_error() {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let authority = FakeLinkAuthority::start().await;
    let (app, cookie) = links_app_with_authority(storage, "admin", &authority).await;
    let link = list_link(1);
    authority.seed_link(&link);
    authority.fail("revoke", 400);

    let revoke = format!("/console/accounts/acme/links/{}/revoke", link.id);
    let response = post_form(&app, &cookie, &revoke, &format!("operation_id={}", link.id)).await;
    assert_eq!(
        response.headers()["location"],
        format!("/console/accounts/acme/links/{}", link.id)
    );
    let (status, html) = follow(&app, &cookie, response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(html.matches(LINK_REVOKE_REJECTED).count(), 1);
    assert!(html.contains("alert-error"));
    assert!(!html.contains(LINK_REVOKED));
    assert!(!html.contains("Invalid command"));
    assert!(authority.link(link.id).unwrap().revoked_at.is_none());
    // Nothing was applied, so no attempt is left reading as an unknown outcome.
    let attempts = response_html(
        identity_request(&app, &cookie, "GET", "/console/accounts/acme/attempts").await,
    )
    .await;
    assert!(!attempts.contains(&format!("revoke-{}", link.id)));

    authority.recover("revoke");
    let response = post_form(&app, &cookie, &revoke, &format!("operation_id={}", link.id)).await;
    let (_, html) = follow(&app, &cookie, response).await;
    assert!(html.contains(LINK_REVOKED));
}

#[tokio::test]
async fn create_link_explicit_never_requires_approval_or_max_uses() {
    let created = post_create_link("description=Never+expires&permission=pull&approval_required=true&never_expires=true&expires_in_days=14&repo_ids=10&repo_grants=10:pull").await;
    assert_eq!(created.response.status(), StatusCode::OK);
    assert_eq!(created.command().expires_at, None);
    let refused = post_create_link("description=Never+expires&permission=pull&never_expires=true&expires_in_days=14&repo_ids=10&repo_grants=10:pull").await;
    assert_eq!(refused.response.status(), StatusCode::OK);
    assert!(refused.authority.created().is_empty());
    let html = response_html(refused.response).await;
    assert!(html.contains(ghinvite_ui::link_form::NO_GUARDRAIL));
    assert!(html.contains("This invitation link will never expire."));
}

#[tokio::test]
async fn typed_creation_refusal_preserves_values_but_allocates_fresh_identity() {
    let mut expectations = oauth_expectations(ACME_ADMIN);
    expectations.push(installation_repos_expectation());
    expectations.push(installation_repos_expectation());
    let (app, cookie, authority) = build_signed_in_admin_app_with_authority(expectations).await;
    let link_id = ghinvite_core::InvitationLinkId::new();
    let anchor = (Utc::now() - Duration::days(2)).timestamp();
    let uri = format!("/console/accounts/acme/links?link_id={link_id}&anchor={anchor}");
    let body = "description=Preserved+workshop&permission=push&expires_in_days=1&repo_ids=10&repo_grants=10:push";
    let response = post_form(&app, &cookie, &uri, body).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let html = response_html(response).await;
    assert!(html.contains("Choose an expiration in the future."));
    assert!(html.contains("Preserved workshop"));
    let next = creation_form_action(&html);
    assert!(
        !next.contains(&link_id.to_string()),
        "a refusal retains the original identity"
    );
    assert!(authority.link(link_id).is_none());
    let response = post_form(
        &app,
        &cookie,
        &format!("/console/accounts/acme/links?{next}"),
        body,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response_html(response).await.contains("/i#"));
}

#[tokio::test]
async fn create_link_heterogeneous_targets_ignore_global_helper_and_unselected_grants() {
    let created = post_create_link("description=Mixed+grants&permission=owner&repo_ids=10&repo_grants=10:triage&repo_ids=11&repo_grants=11:maintain&repo_grants=12:admin").await;
    assert_eq!(created.response.status(), StatusCode::OK);
    let targets = &created.command().targets.repos;
    assert_eq!(
        targets
            .iter()
            .map(|repo| (repo.repo_id, repo.grant))
            .collect::<Vec<_>>(),
        vec![
            (10, ghinvite_core::targets::RepoRole::Triage),
            (11, ghinvite_core::targets::RepoRole::Maintain)
        ]
    );
}

#[tokio::test]
async fn native_set_all_rerenders_explicit_grants_without_creating_or_selecting_targets() {
    let created = post_create_link("description=Unfinished&permission=maintain&apply_all=true&repo_ids=10&repo_grants=10:pull&repo_grants=11:push").await;
    assert_eq!(created.response.status(), StatusCode::OK);
    assert!(created.authority.created().is_empty());
    let html = response_html(created.response).await;
    let props: ghinvite_ui::link_form::LinkFormIslandProps = serde_json::from_str(
        html.split_once("<script type=\"application/json\" id=\"link-form-props\">")
            .unwrap()
            .1
            .split_once("</script>")
            .unwrap()
            .0,
    )
    .unwrap();
    assert_eq!(props.values.selected_repo_ids, vec![10]);
    assert!(props.values.repo_grants.contains(&"10:maintain".into()));
    assert!(props.values.repo_grants.contains(&"11:maintain".into()));
    assert_eq!(props.values.description, "Unfinished");
}

#[path = "console_flow/organization_form.rs"]
mod organization_form;

#[tokio::test]
async fn navigation_pending_count_updates_on_links_and_queue_after_a_decision() {
    let (app, cookie, storage, envelope) = deadline_queue_app(None).await;
    for path in [
        "/console/accounts/octocat/links",
        "/console/accounts/octocat/requests",
    ] {
        let response = identity_request(&app, &cookie, "GET", path).await;
        assert_eq!(response.status(), StatusCode::OK);
        let html = response_html(response).await;
        assert_eq!(
            html.matches("aria-label=\"Pending requests: 1\"").count(),
            2
        );
    }
    let mut request = storage
        .get_invitation_request(envelope.requests[0].request_id)
        .await
        .unwrap()
        .unwrap();
    request.state = ghinvite_core::RequestState::Declined;
    request.decided_by = Some(42);
    request.decided_at = Some(Utc::now());
    storage.seed_decision(&request).await.unwrap();
    for path in [
        "/console/accounts/octocat/links",
        "/console/accounts/octocat/requests",
    ] {
        let response = identity_request(&app, &cookie, "GET", path).await;
        assert_eq!(response.status(), StatusCode::OK);
        let html = response_html(response).await;
        assert_eq!(
            html.matches("aria-label=\"Pending requests: 0\"").count(),
            2
        );
        assert!(!html.contains("aria-label=\"Pending requests: 1\""));
    }
}

#[path = "console_flow/requester_message.rs"]
mod requester_message;

#[path = "console_flow/requester_profile.rs"]
mod requester_profile;

#[path = "console_flow/repository_picker.rs"]
mod repository_picker;
