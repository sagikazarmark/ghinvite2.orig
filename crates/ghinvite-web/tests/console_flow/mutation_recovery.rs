use super::*;
use common::link_authority::snapshot;
use ghinvite_core::request_lifecycle::{DecideRequest, DecisionOutcome, DecisionReceipt};
use ghinvite_core::storage::projection::{CreateLink, RequestSnapshot};
use ghinvite_core::storage::{ContinuationStorage, InstallationStorage, RecordStorage};

async fn recovery_app(authority: &FakeLinkAuthority) -> (axum::Router, String) {
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
        Arc::new(BrowserGithub),
        authority.client(),
        WebConfig::for_local_dev_with_secret([7; 32]),
    );
    let app = build_app(state, tower_sessions::MemoryStore::default());
    let cookie = sign_in(&app).await;
    (app, cookie)
}

async fn post(
    app: &axum::Router,
    cookie: &str,
    uri: &str,
    fields: &str,
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("cookie", cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(fields.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// The octocat link the recovery tests act on.
fn recovery_link(id: ghinvite_core::InvitationLinkId) -> ghinvite_core::InvitationLink {
    ghinvite_core::InvitationLink {
        id,
        installation_id: 77,
        account_id: 42,
        created_by: 42,
        created_at: "2026-09-14T12:00:00Z".parse().unwrap(),
        expires_at: None,
        max_uses: None,
        uses_count: 0,
        permission: ghinvite_core::Permission::Pull,
        approval_required: true,
        description: "Recovery fixture".into(),
        internal_note: None,
        revoked_at: None,
        revoked_by: None,
        repos: vec![ghinvite_core::InvitationLinkRepo {
            repo_id: 10,
            repo_full_name: "octocat/api".into(),
        }],
    }
}

#[tokio::test]
async fn uncertain_revocation_has_navigation_safe_status_and_csrf_protected_retry() {
    let authority = FakeLinkAuthority::start().await;
    let id = ghinvite_core::InvitationLinkId::new();
    authority.seed_link(&recovery_link(id));
    authority.lose_acknowledgements("revoke");
    let (app, cookie) = recovery_app(&authority).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let response = post(
        &app,
        &cookie,
        &format!("/console/accounts/octocat/links/{id}/revoke"),
        &format!("csrf_token={csrf}"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let html = response_html(response).await;
    assert!(html.contains("Retry original attempt"));
    let url = format!("/console/accounts/octocat/attempts/revoke-{id}");
    assert!(html.contains(&url));
    let response =
        identity_request(&app, &cookie, "GET", "/console/accounts/octocat/attempts").await;
    assert!(response_html(response).await.contains(&url));
    assert_eq!(
        post(&app, &cookie, &url, "").await.status(),
        StatusCode::FORBIDDEN
    );
    let before = authority.calls().len();
    let html = response_html(identity_request(&app, &cookie, "GET", &url).await).await;
    assert!(html.contains("Invitation link stopped accepting new invitation requests."));
    assert!(!html.contains("Retry original attempt"));
    assert_eq!(
        authority.calls()[before..],
        ["link_status"],
        "status must not mutate"
    );
    assert_eq!(authority.applied(), ["revoke"]);
}

#[tokio::test]
async fn retries_reclaim_expired_continuations_in_bounded_batches_and_preserve_live_input() {
    let authority = FakeLinkAuthority::start().await;
    authority.fail("revoke", 503);
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
        storage.clone(),
        Arc::new(BrowserGithub),
        authority.client(),
        WebConfig::for_local_dev_with_secret([7; 32]),
    );
    let app = build_app(state, protected_store().await);
    let cookie = sign_in(&app).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let link = ghinvite_core::InvitationLinkId::new();
    post(
        &app,
        &cookie,
        &format!("/console/accounts/octocat/links/{link}/revoke"),
        &format!("csrf_token={csrf}"),
    )
    .await;
    let original = authority.received::<serde_json::Value>("revoke")[0].clone();
    // Seed expired records after the initial submission so the retry drives cleanup.
    for i in 0..205 {
        let id = format!("expired-{i}");
        // Retained before its deadline, so the write reads back.
        storage
            .retain_attempt_continuation("expired-session", &id, &id, "ciphertext", 1, 0)
            .await
            .unwrap();
    }
    for remaining in [105, 5, 0] {
        let response = post(
            &app,
            &cookie,
            &format!("/console/accounts/octocat/attempts/revoke-{link}"),
            &format!("csrf_token={csrf}"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        // Read as of before the deadline: what cleanup has not yet reclaimed.
        let unreclaimed = storage
            .list_attempt_continuations("expired-session", 0)
            .await
            .unwrap();
        assert_eq!(unreclaimed.len(), remaining);
        assert_eq!(authority.calls().last().unwrap(), "revoke");
        assert_eq!(
            authority.received::<serde_json::Value>("revoke").last(),
            Some(&original)
        );
    }
    let html = response_html(
        identity_request(&app, &cookie, "GET", "/console/accounts/octocat/attempts").await,
    )
    .await;
    assert!(html.contains(&format!("revoke-{link}")));
}

#[tokio::test]
async fn recovery_lists_every_attempt_and_opposite_intent_is_not_reported_as_success() {
    let authority = FakeLinkAuthority::start().await;
    authority.fail("decide", 503);
    authority.fail("revoke", 503);
    let (app, cookie) = recovery_app(&authority).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let link = ghinvite_core::InvitationLinkId::new();
    let request = ghinvite_core::RequestId::new();
    let operation = ghinvite_core::RequestId::new();
    let original = format!("/console/accounts/octocat/attempts/decision-{request}-{operation}");
    post(
        &app,
        &cookie,
        &format!("/console/accounts/octocat/requests/{request}/approve"),
        &format!("csrf_token={csrf}&link_id={link}&operation_id={operation}"),
    )
    .await;
    let response = post(
        &app,
        &cookie,
        &format!("/console/accounts/octocat/requests/{request}/decline"),
        &format!(
            "csrf_token={csrf}&link_id={link}&operation_id={}",
            ghinvite_core::RequestId::new()
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let html = response_html(response).await;
    assert!(html.contains("requested decision was not applied"));
    assert!(html.contains(&original));
    assert_eq!(authority.calls().len(), 1);
    post(
        &app,
        &cookie,
        &format!("/console/accounts/octocat/links/{link}/revoke"),
        &format!("csrf_token={csrf}"),
    )
    .await;
    let html = response_html(
        identity_request(&app, &cookie, "GET", "/console/accounts/octocat/attempts").await,
    )
    .await;
    assert!(html.contains(&original));
    assert!(html.contains(&format!("/console/accounts/octocat/attempts/revoke-{link}")));
}

#[tokio::test]
async fn concurrent_decision_submissions_bind_one_input_and_keep_the_winner_recoverable() {
    let authority = FakeLinkAuthority::start().await;
    authority.fail("decide", 503);
    let (app, cookie) = recovery_app(&authority).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let link = ghinvite_core::InvitationLinkId::new();
    let request = ghinvite_core::RequestId::new();
    let first = ghinvite_core::RequestId::new();
    let second = ghinvite_core::RequestId::new();
    let uri = format!("/console/accounts/octocat/requests/{request}/approve");
    let a = format!("csrf_token={csrf}&link_id={link}&operation_id={first}");
    let b = format!("csrf_token={csrf}&link_id={link}&operation_id={second}");
    let (a, b) = tokio::join!(post(&app, &cookie, &uri, &a), post(&app, &cookie, &uri, &b));
    assert!(matches!(
        (a.status(), b.status()),
        (StatusCode::BAD_GATEWAY, StatusCode::SEE_OTHER)
            | (StatusCode::SEE_OTHER, StatusCode::BAD_GATEWAY)
    ));
    assert_eq!(authority.calls(), ["decide"]);
    let winner = String::from(
        authority.received::<DecideRequest>("decide")[0]
            .operation_id
            .clone(),
    );
    let winner = winner.as_str();
    let html = response_html(
        identity_request(&app, &cookie, "GET", "/console/accounts/octocat/attempts").await,
    )
    .await;
    assert!(html.contains(winner));
    let loser = if winner == first.to_string() {
        second.to_string()
    } else {
        first.to_string()
    };
    assert!(!html.contains(&loser));
}

#[tokio::test]
async fn operation_identity_on_another_request_does_not_override_request_recovery() {
    let authority = FakeLinkAuthority::start().await;
    authority.fail("decide", 503);
    let (app, cookie) = recovery_app(&authority).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let link = ghinvite_core::InvitationLinkId::new();
    let first_request = ghinvite_core::RequestId::new();
    let second_request = ghinvite_core::RequestId::new();
    let first_operation = "01ARZ3NDEKTSV4RRFFQ69G5FAZ";
    let second_operation = "01ARZ3NDEKTSV4RRFFQ69G5FAA";
    for (request, operation) in [
        (first_request, first_operation),
        (second_request, second_operation),
    ] {
        post(
            &app,
            &cookie,
            &format!("/console/accounts/octocat/requests/{request}/approve"),
            &format!("csrf_token={csrf}&link_id={link}&operation_id={operation}"),
        )
        .await;
    }
    let response = post(
        &app,
        &cookie,
        &format!("/console/accounts/octocat/requests/{second_request}/approve"),
        &format!("csrf_token={csrf}&link_id={link}&operation_id={first_operation}"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let location = response.headers()["location"].to_str().unwrap();
    assert!(location.contains(&format!("decision-{second_request}-{second_operation}")));
    assert_eq!(authority.calls().len(), 2);
}

#[tokio::test]
async fn creation_recovery_retains_canonical_input_before_eligibility_and_projection() {
    let authority = FakeLinkAuthority::start().await;
    let id = ghinvite_core::InvitationLinkId::new();
    authority.fail("create", 503);
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    storage
        .insert_installation(&identity_account(42, "octocat", AccountType::User))
        .await
        .unwrap();
    let mut expectations = oauth_expectations(OCTOCAT);
    let mut repositories = installation_repos_expectation();
    let mut body: serde_json::Value = serde_json::from_slice(&repositories.response.body).unwrap();
    body["repositories"].as_array_mut().unwrap().reverse();
    repositories.response.body = serde_json::to_vec(&body).unwrap();
    expectations.push(repositories); // Only the first submission may consult GitHub.
    let state = AppState::new(
        storage,
        Arc::new(MockTransport::scripted(expectations)),
        authority.client(),
        WebConfig::for_local_dev_with_secret([7; 32]),
    );
    let app = build_app(state, tower_sessions::MemoryStore::default());
    let cookie = sign_in(&app).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let action = format!(
        "/console/accounts/octocat/links?link_id={id}&anchor={}",
        Utc::now().timestamp()
    );
    let fields = format!(
        "csrf_token={csrf}&description=Recovery+fixture&permission=pull&repo_ids=10&repo_ids=11&expires_in_days=2"
    );
    let response = post(&app, &cookie, &action, &fields).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let html = response_html(response).await;
    let url = format!("/console/accounts/octocat/attempts/create-{id}");
    assert!(html.contains(&url));
    assert!(html.contains(&format!("/console/accounts/octocat/links/{id}")));
    let original = authority.received::<CreateLink>("create")[0].clone();
    let response = identity_request(
        &app,
        &cookie,
        "GET",
        &format!("/console/accounts/octocat/links/{id}"),
    )
    .await;
    assert_ne!(
        response.status(),
        StatusCode::NOT_FOUND,
        "uncertain does not mean absent"
    );
    // The authority confirms it created the link from the canonical input.
    let mut confirmed = snapshot(&recovery_link(id));
    confirmed.creation = original.clone();
    confirmed.creation.repos.sort_by_key(|repo| repo.repo_id);
    authority.seed(confirmed);
    assert_eq!(
        identity_request(&app, &cookie, "GET", &url.to_lowercase())
            .await
            .status(),
        StatusCode::OK
    );
    let response = post(
        &app,
        &cookie,
        &action,
        &fields.replace("Recovery+fixture", "Changed"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(response_html(response).await.contains(&url));
    assert_eq!(authority.calls().len(), 3);
    authority.recover("create");
    assert_eq!(
        post(
            &app,
            &cookie,
            &url.to_lowercase(),
            &format!("csrf_token={csrf}")
        )
        .await
        .status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(authority.calls()[3..], ["create"]);
    let retry = authority.received::<CreateLink>("create").pop().unwrap();
    assert_eq!(
        original, retry,
        "absolute expiry and repository identities stay fixed"
    );
    let detail = format!("/console/accounts/octocat/links/{id}");
    let response = identity_request(&app, &cookie, "GET", &detail).await;
    assert_eq!(response.status(), StatusCode::OK);
    // The completed retry flashes once; the uncertain first submission and
    // the status read did not.
    let html = response_html(response).await;
    assert_eq!(html.matches("Invitation link created.").count(), 1);
    let html = response_html(identity_request(&app, &cookie, "GET", &detail).await).await;
    assert!(!html.contains("Invitation link created."));
}

#[tokio::test]
async fn recovered_revocation_retry_flashes_once_on_the_link_detail_page() {
    let authority = FakeLinkAuthority::start().await;
    let id = ghinvite_core::InvitationLinkId::new();
    authority.seed_link(&recovery_link(id));
    authority.lose_acknowledgements("revoke");
    let (app, cookie) = recovery_app(&authority).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let detail = format!("/console/accounts/octocat/links/{id}");
    let response = post(
        &app,
        &cookie,
        &format!("{detail}/revoke"),
        &format!("csrf_token={csrf}"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let url = format!("/console/accounts/octocat/attempts/revoke-{id}");
    let html = response_html(identity_request(&app, &cookie, "GET", &url).await).await;
    assert!(html.contains("Invitation link stopped accepting new invitation requests."));
    let response = post(&app, &cookie, &url, &format!("csrf_token={csrf}")).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()["location"], detail);
    let html = response_html(identity_request(&app, &cookie, "GET", &detail).await).await;
    assert_eq!(
        html.matches("Invitation link stopped accepting new invitation requests.")
            .count(),
        1
    );
    let html = response_html(identity_request(&app, &cookie, "GET", &detail).await).await;
    assert!(!html.contains("stopped accepting new invitation requests."));
}

#[tokio::test]
async fn rejected_creation_retry_starts_a_fresh_form_with_an_error() {
    let authority = FakeLinkAuthority::start().await;
    let id = ghinvite_core::InvitationLinkId::new();
    authority.fail("create", 503);
    let (app, cookie) = recovery_app(&authority).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let response = post(
        &app,
        &cookie,
        &format!(
            "/console/accounts/octocat/links?link_id={id}&anchor={}",
            Utc::now().timestamp()
        ),
        &format!("csrf_token={csrf}&description=Recovery+fixture&permission=pull&repo_ids=10"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    // The authority rejects the retried input.
    authority.fail("create", 400);
    let url = format!("/console/accounts/octocat/attempts/create-{id}");
    let response = post(&app, &cookie, &url, &format!("csrf_token={csrf}")).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()["location"],
        "/console/accounts/octocat/links/new"
    );
    let html = response_html(
        identity_request(&app, &cookie, "GET", "/console/accounts/octocat/links/new").await,
    )
    .await;
    assert!(html.contains(
        "The invitation link could not be created with these values. Review them and try again."
    ));
    assert!(
        !html.contains("invalid command"),
        "the authority's wording stays internal"
    );
    // The rejected input is released rather than left as an unknown outcome.
    assert_eq!(
        identity_request(&app, &cookie, "GET", &url).await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn decision_recovery_reads_original_receipt_and_retries_identical_input() {
    use ghinvite_core::RequestState::{Approved, Declined, Expired};
    for action in ["approve", "decline"] {
        let authority = FakeLinkAuthority::start().await;
        let link = ghinvite_core::InvitationLinkId::new();
        let request = ghinvite_core::RequestId::new();
        let operation = ghinvite_core::RequestId::new();
        authority.seed_link(&recovery_link(link));
        authority.fail("decide", 503);
        let (app, cookie) = recovery_app(&authority).await;
        let csrf = common::csrf_token(&app, &cookie).await;
        let response = post(
            &app,
            &cookie,
            &format!("/console/accounts/octocat/requests/{request}/{action}"),
            &format!("csrf_token={csrf}&link_id={link}&operation_id={operation}"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let url = format!("/console/accounts/octocat/attempts/decision-{request}-{operation}");
        assert!(response_html(response).await.contains(&url));
        let original = authority.received::<DecideRequest>("decide")[0].clone();
        let response = post(
            &app,
            &cookie,
            &format!("/console/accounts/octocat/requests/{request}/{action}"),
            &format!(
                "csrf_token={csrf}&link_id={link}&operation_id={}",
                ghinvite_core::RequestId::new()
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers()["location"], url);
        assert_eq!(authority.calls().len(), 1);
        let latest = response_html(
            identity_request(&app, &cookie, "GET", "/console/accounts/octocat/attempts").await,
        )
        .await;
        assert!(latest.contains(&url));
        // The authority retained no receipt for the operation.
        assert!(
            response_html(identity_request(&app, &cookie, "GET", &url).await)
                .await
                .contains("Outcome unknown")
        );
        assert_eq!(
            post(
                &app,
                &cookie,
                &url,
                &format!(
                    "csrf_token={csrf}&operation_id={}",
                    ghinvite_core::RequestId::new()
                )
            )
            .await
            .status(),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(authority.calls()[1..], ["decision_status", "decide"]);
        assert_eq!(authority.received::<DecideRequest>("decide")[1], original);
        let (same, opposite) = if action == "approve" {
            (Approved, Declined)
        } else {
            (Declined, Approved)
        };
        for (outcome, state, expected) in [
            (DecisionOutcome::Applied, same, "Request"),
            (DecisionOutcome::AlreadyCompleted, same, "already"),
            (DecisionOutcome::Incompatible, opposite, "not applied"),
            (DecisionOutcome::Incompatible, Expired, "decision deadline"),
        ] {
            let receipt = DecisionReceipt {
                outcome,
                request: RequestSnapshot {
                    request_id: request,
                    link_id: link,
                    account_id: 42,
                    requester_id: 99,
                    justification: None,
                    state,
                    admitted_at: "2026-09-14T12:00:00Z".parse().unwrap(),
                    decision_deadline: Some("2026-09-21T12:00:00Z".parse().unwrap()),
                    revision: 2,
                    decision: None,
                },
            };
            authority.seed_decision(original.clone(), receipt);
            let before = authority.calls().len();
            let html = response_html(identity_request(&app, &cookie, "GET", &url).await).await;
            assert!(html.contains(expected));
            assert!(html.contains(&state.to_string()));
            assert!(!html.contains("Retry original attempt"));
            assert_eq!(authority.calls()[before..], ["decision_status"]);
        }
    }
}

#[tokio::test]
async fn recovery_requires_current_account_authority_session_ownership_and_csrf() {
    let authority = FakeLinkAuthority::start().await;
    let id = ghinvite_core::InvitationLinkId::new();
    authority.fail("revoke", 503);
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let account = identity_account(42, "octocat", AccountType::User);
    storage.insert_installation(&account).await.unwrap();
    let state = AppState::new(
        storage.clone(),
        Arc::new(BrowserGithub),
        authority.client(),
        WebConfig::for_local_dev_with_secret([7; 32]),
    );
    let app = build_app(state, protected_store().await);
    let cookie = sign_in(&app).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let url = format!("/console/accounts/octocat/attempts/revoke-{id}");
    assert_eq!(
        post(
            &app,
            &cookie,
            &format!("/console/accounts/octocat/links/{id}/revoke"),
            &format!("csrf_token={csrf}")
        )
        .await
        .status(),
        StatusCode::BAD_GATEWAY
    );
    assert_eq!(
        identity_request(&app, "", "GET", &url).await.status(),
        StatusCode::SEE_OTHER
    );
    let other_cookie = sign_in(&app).await;
    assert_eq!(
        identity_request(&app, &other_cookie, "GET", &url)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        post(&app, &cookie, &url, "csrf_token=bad").await.status(),
        StatusCode::FORBIDDEN
    );
    storage
        .mark_installation_uninstalled(account.installation_id, Utc::now())
        .await
        .unwrap();
    assert!(
        response_html(identity_request(&app, &cookie, "GET", &url).await)
            .await
            .contains("Retry original attempt")
    );
    let mut replacement = identity_account(999, "octocat", AccountType::User);
    replacement.installation_id = 999;
    storage.insert_installation(&replacement).await.unwrap();
    assert_eq!(
        identity_request(&app, &cookie, "GET", &url).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        post(&app, &cookie, &url, &format!("csrf_token={csrf}"))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        authority.calls().iter().filter(|m| *m == "revoke").count(),
        1
    );
}

async fn protected_store()
-> ghinvite_web::session_store::ProtectedStore<ghinvite_web_server::SqliteBackend> {
    let backend = ghinvite_web_server::SqliteBackend::new(
        sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap(),
    );
    backend.migrate().await.unwrap();
    ghinvite_web::session_store::ProtectedStore::new(backend, [7; 32])
}

struct BrowserGithub;
#[async_trait::async_trait]
impl ghinvite_github::HttpTransport for BrowserGithub {
    async fn send(
        &self,
        request: ghinvite_github::transport::Request,
    ) -> ghinvite_github::Result<Response> {
        if request.url.contains("/repositories?") {
            return Ok(installation_repos_expectation().response);
        }
        Ok(oauth_expectations(OCTOCAT)
            .into_iter()
            .find(|e| e.url == request.url)
            .expect("known GitHub fixture request")
            .response)
    }
}

#[tokio::test]
#[ignore = "long-running Playwright fixture"]
async fn mutation_recovery_browser_server() {
    use axum::response::IntoResponse;
    use ghinvite_core::storage::projection::{ProjectionEnvelope, ProjectionStorage};
    // Every mutation applies, then loses its acknowledgement. SQL lags.
    let authority = FakeLinkAuthority::start().await;
    for method in ["create", "revoke", "decide"] {
        authority.lose_acknowledgements(method);
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
    for (id, login) in [(42, "octocat"), (99, "requester")] {
        storage
            .upsert_user(&ghinvite_core::User {
                user_id: id,
                login: login.into(),
                avatar_url: None,
                last_seen_at: Utc::now(),
            })
            .await
            .unwrap();
    }
    let link = snapshot(&recovery_link(ghinvite_core::InvitationLinkId::new()));
    authority.seed(link.clone());
    let requests = (0..2)
        .map(|_| RequestSnapshot {
            request_id: ghinvite_core::RequestId::new(),
            link_id: link.link_id,
            account_id: 42,
            requester_id: 99,
            justification: None,
            state: ghinvite_core::RequestState::Pending,
            admitted_at: Utc::now(),
            decision_deadline: Some(Utc::now() + Duration::days(7)),
            revision: 1,
            decision: None,
        })
        .collect::<Vec<_>>();
    for request in &requests {
        authority.seed_request(request.clone());
    }
    storage
        .apply_transition(&ProjectionEnvelope {
            transition_id: format!("link/{}/1", link.link_id),
            link,
            requests,
            events: vec![],
        })
        .await
        .unwrap();
    let app = build_app(
        AppState::new(
            storage,
            Arc::new(BrowserGithub),
            authority.client(),
            WebConfig::for_local_dev_with_secret([7; 32]),
        ),
        protected_store().await,
    );
    let login_app = app.clone();
    let fixture = axum::Router::new()
        .route(
            "/fixture-login",
            axum::routing::get(move || {
                let app = login_app.clone();
                async move {
                    let cookie = sign_in(&app).await;
                    (
                        [(
                            axum::http::header::SET_COOKIE,
                            format!("{cookie}; Path=/; HttpOnly; SameSite=Lax"),
                        )],
                        axum::response::Redirect::to("/console/accounts/octocat/links/new"),
                    )
                        .into_response()
                }
            }),
        )
        .route(
            "/fixture-effects",
            axum::routing::get(move || {
                let authority = authority.clone();
                async move {
                    // Per mutation: how many took effect, and how many calls
                    // (first attempts and retries) reached the authority.
                    let count = |methods: Vec<String>| {
                        let mut counts = BTreeMap::<String, usize>::new();
                        for method in methods {
                            if matches!(method.as_str(), "create" | "revoke" | "decide") {
                                *counts.entry(method).or_default() += 1;
                            }
                        }
                        counts
                    };
                    axum::Json(serde_json::json!({
                        "effects": count(authority.applied()),
                        "calls": count(authority.calls()),
                    }))
                }
            }),
        )
        .route("/health", axum::routing::get(|| async { StatusCode::OK }))
        .fallback(move |request: Request<Body>| {
            let app = app.clone();
            async move { app.oneshot(request).await.unwrap() }
        });
    axum::serve(
        tokio::net::TcpListener::bind("127.0.0.1:4175")
            .await
            .unwrap(),
        fixture,
    )
    .await
    .unwrap();
}
