use super::*;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};

async fn recovery_app(ingress: &MockServer) -> (axum::Router, String) {
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
        Arc::new(RecordingCommands::default()),
        WebConfig::for_local_dev_with_secret([7; 32]),
    )
    .with_admission(Arc::new(RestateClient::new(ingress.uri()).unwrap()));
    sign_in(build_app(state, tower_sessions::MemoryStore::default())).await
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

fn snapshot(id: ghinvite_core::InvitationLinkId, revoked: bool) -> serde_json::Value {
    serde_json::json!({"link_id":id,"creation":{
        "version":1,"link_id":id,"admin":{"account_id":42,"user_id":42},"account_id":42,
        "installation_id":77,"description":"Recovery fixture","internal_note":null,
        "expires_at":null,"max_uses":null,"permission":"pull","approval_required":true,
        "repos":[{"repo_id":10,"repo_full_name":"octocat/api"}]},
        "invitation_code":"RecoveryCode0001","created_at":"2026-09-14T12:00:00Z",
        "uses":0,"revision":if revoked {2} else {1},
        "revoked_at":if revoked {Some("2026-09-15T12:00:00Z")} else {None},
        "revoked_by":if revoked {Some(42)} else {None}})
}

#[tokio::test]
async fn uncertain_revocation_has_navigation_safe_status_and_csrf_protected_retry() {
    let ingress = MockServer::start().await;
    let id = ghinvite_core::InvitationLinkId::new();
    Mock::given(path(format!("/InvitationLinkV1/{id}/revoke")))
        .respond_with(ResponseTemplate::new(503))
        .mount(&ingress)
        .await;
    let (app, cookie) = recovery_app(&ingress).await;
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
    ingress.reset().await;
    Mock::given(path(format!("/InvitationLinkV1/{id}/link_status")))
        .respond_with(ResponseTemplate::new(200).set_body_json(snapshot(id, true)))
        .mount(&ingress)
        .await;
    let html = response_html(identity_request(&app, &cookie, "GET", &url).await).await;
    assert!(html.contains("Invitation link stopped accepting new invitation requests."));
    assert!(!html.contains("Retry original attempt"));
    assert_eq!(
        ingress.received_requests().await.unwrap().len(),
        1,
        "status must not mutate"
    );
}

#[tokio::test]
async fn recovery_lists_every_attempt_and_opposite_intent_is_not_reported_as_success() {
    let ingress = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&ingress)
        .await;
    let (app, cookie) = recovery_app(&ingress).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let link = ghinvite_core::InvitationLinkId::new();
    let request = ghinvite_core::RequestId::new();
    let operation = ghinvite_core::RequestId::new();
    let original = format!("/console/accounts/octocat/attempts/decision-{link}-{operation}");
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
    assert_eq!(ingress.received_requests().await.unwrap().len(), 1);
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
    let ingress = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&ingress)
        .await;
    let (app, cookie) = recovery_app(&ingress).await;
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
    let calls = ingress.received_requests().await.unwrap();
    assert_eq!(calls.len(), 1);
    let input: serde_json::Value = serde_json::from_slice(&calls[0].body).unwrap();
    let winner = input["operation_id"].as_str().unwrap();
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
async fn exact_operation_conflict_takes_precedence_over_request_recovery() {
    let ingress = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&ingress)
        .await;
    let (app, cookie) = recovery_app(&ingress).await;
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
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let html = response_html(response).await;
    assert!(html.contains("Operation conflict"));
    assert!(html.contains(first_operation));
    assert_eq!(ingress.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn creation_recovery_retains_canonical_input_before_eligibility_and_projection() {
    let ingress = MockServer::start().await;
    let id = ghinvite_core::InvitationLinkId::new();
    Mock::given(path(format!("/InvitationLinkV1/{id}/create")))
        .respond_with(ResponseTemplate::new(503))
        .mount(&ingress)
        .await;
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    storage
        .insert_installation(&identity_account(42, "octocat", AccountType::User))
        .await
        .unwrap();
    let mut expectations = oauth_sign_in_expectations();
    let mut repositories = installation_repos_expectation();
    let mut body: serde_json::Value = serde_json::from_slice(&repositories.response.body).unwrap();
    body["repositories"].as_array_mut().unwrap().reverse();
    repositories.response.body = serde_json::to_vec(&body).unwrap();
    expectations.push(repositories); // Only the first submission may consult GitHub.
    let state = AppState::new(
        storage,
        Arc::new(MockTransport::scripted(expectations)),
        Arc::new(RecordingCommands::default()),
        WebConfig::for_local_dev_with_secret([7; 32]),
    )
    .with_admission(Arc::new(RestateClient::new(ingress.uri()).unwrap()));
    let (app, cookie) = sign_in(build_app(state, tower_sessions::MemoryStore::default())).await;
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
    let original: serde_json::Value =
        serde_json::from_slice(&ingress.received_requests().await.unwrap()[0].body).unwrap();
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
    let mut confirmed = snapshot(id, false);
    confirmed["creation"] = original.clone();
    confirmed["creation"]["repos"]
        .as_array_mut()
        .unwrap()
        .sort_by_key(|r| r["repo_id"].as_u64().unwrap());
    Mock::given(path(format!("/InvitationLinkV1/{id}/link_status")))
        .respond_with(ResponseTemplate::new(200).set_body_json(confirmed))
        .mount(&ingress)
        .await;
    assert_eq!(
        identity_request(&app, &cookie, "GET", &url).await.status(),
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
    assert_eq!(ingress.received_requests().await.unwrap().len(), 3);
    ingress.reset().await;
    Mock::given(path(format!("/InvitationLinkV1/{id}/create")))
        .respond_with(ResponseTemplate::new(200).set_body_json(snapshot(id, false)))
        .mount(&ingress)
        .await;
    assert_eq!(
        post(&app, &cookie, &url, &format!("csrf_token={csrf}"))
            .await
            .status(),
        StatusCode::SEE_OTHER
    );
    let retry: serde_json::Value =
        serde_json::from_slice(&ingress.received_requests().await.unwrap()[0].body).unwrap();
    assert_eq!(
        original, retry,
        "absolute expiry and repository identities stay fixed"
    );
    Mock::given(path(format!("/InvitationLinkV1/{id}/link_status")))
        .respond_with(ResponseTemplate::new(200).set_body_json(snapshot(id, false)))
        .mount(&ingress)
        .await;
    assert_eq!(
        identity_request(
            &app,
            &cookie,
            "GET",
            &format!("/console/accounts/octocat/links/{id}")
        )
        .await
        .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn decision_recovery_reads_original_receipt_and_retries_identical_input() {
    for action in ["approve", "decline"] {
        let ingress = MockServer::start().await;
        let link = ghinvite_core::InvitationLinkId::new();
        let request = ghinvite_core::RequestId::new();
        let operation = ghinvite_core::RequestId::new();
        Mock::given(path(format!("/InvitationLinkV1/{link}/decide")))
            .respond_with(ResponseTemplate::new(503))
            .mount(&ingress)
            .await;
        let (app, cookie) = recovery_app(&ingress).await;
        let csrf = common::csrf_token(&app, &cookie).await;
        let response = post(
            &app,
            &cookie,
            &format!("/console/accounts/octocat/requests/{request}/{action}"),
            &format!("csrf_token={csrf}&link_id={link}&operation_id={operation}"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let url = format!("/console/accounts/octocat/attempts/decision-{link}-{operation}");
        assert!(response_html(response).await.contains(&url));
        let original = ingress.received_requests().await.unwrap()[0].body.clone();
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
        assert_eq!(ingress.received_requests().await.unwrap().len(), 1);
        let latest = response_html(
            identity_request(&app, &cookie, "GET", "/console/accounts/octocat/attempts").await,
        )
        .await;
        assert!(latest.contains(&url));
        ingress.reset().await;
        Mock::given(path(format!("/InvitationLinkV1/{link}/decision_status")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::Value::Null))
            .mount(&ingress)
            .await;
        assert!(
            response_html(identity_request(&app, &cookie, "GET", &url).await)
                .await
                .contains("Outcome unknown")
        );
        Mock::given(path(format!("/InvitationLinkV1/{link}/decide")))
            .respond_with(ResponseTemplate::new(503))
            .mount(&ingress)
            .await;
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
        assert_eq!(ingress.received_requests().await.unwrap()[1].body, original);
        for (outcome, state, expected) in [
            (
                "applied",
                if action == "approve" {
                    "approved"
                } else {
                    "declined"
                },
                "Request",
            ),
            (
                "already_completed",
                if action == "approve" {
                    "approved"
                } else {
                    "declined"
                },
                "already",
            ),
            (
                "incompatible",
                if action == "approve" {
                    "declined"
                } else {
                    "approved"
                },
                "not applied",
            ),
            ("incompatible", "expired", "decision deadline"),
        ] {
            ingress.reset().await;
            Mock::given(path(format!("/InvitationLinkV1/{link}/decision_status")))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"outcome":outcome,"request":{
                    "request_id":request,"link_id":link,"account_id":42,"requester_id":99,"state":state,
                    "admitted_at":"2026-09-14T12:00:00Z","decision_deadline":"2026-09-21T12:00:00Z","revision":2}})))
                .mount(&ingress).await;
            let html = response_html(identity_request(&app, &cookie, "GET", &url).await).await;
            assert!(html.contains(expected));
            assert!(html.contains(state));
            assert!(!html.contains("Retry original attempt"));
            assert_eq!(ingress.received_requests().await.unwrap().len(), 1);
        }
    }
}

#[tokio::test]
async fn recovery_requires_current_account_authority_session_ownership_and_csrf() {
    let ingress = MockServer::start().await;
    let id = ghinvite_core::InvitationLinkId::new();
    Mock::given(path(format!("/InvitationLinkV1/{id}/revoke")))
        .respond_with(ResponseTemplate::new(503))
        .mount(&ingress)
        .await;
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
        Arc::new(RecordingCommands::default()),
        WebConfig::for_local_dev_with_secret([7; 32]),
    )
    .with_admission(Arc::new(RestateClient::new(ingress.uri()).unwrap()));
    let (app, cookie) = sign_in(build_app(state, protected_store().await)).await;
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
    let (_, other_cookie) = sign_in(app.clone()).await;
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
        ingress
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path().ends_with("/revoke"))
            .count(),
        1
    );
}

async fn protected_store()
-> ghinvite_web::session_store::ProtectedStore<ghinvite_web::session_store::SqliteBackend> {
    let backend = ghinvite_web::session_store::SqliteBackend::new(
        sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap(),
    );
    backend.migrate().await.unwrap();
    ghinvite_web::session_store::ProtectedStore::new(backend, [7; 32])
}

/// Retain the first command/result, then lose its acknowledgement. SQL lags.
#[derive(Default)]
struct LostAcknowledgements {
    links: BTreeMap<String, serde_json::Value>,
    receipts: BTreeMap<String, (serde_json::Value, serde_json::Value)>,
    effects: BTreeMap<String, usize>,
    calls: BTreeMap<String, usize>,
}
struct LostAcknowledgementTransport(Arc<Mutex<LostAcknowledgements>>);
impl wiremock::Respond for LostAcknowledgementTransport {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let parts = request.url.path().split('/').collect::<Vec<_>>();
        let id = parts[2];
        let method = parts[3];
        let input: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        let mut state = self.0.lock().unwrap();
        if method == "link_status" {
            return state
                .links
                .get(id)
                .map(|s| ResponseTemplate::new(200).set_body_json(s))
                .unwrap_or(ResponseTemplate::new(404));
        }
        let key = if matches!(method, "decide" | "decision_status") {
            format!("{id}/decide/{}", input["operation_id"].as_str().unwrap())
        } else {
            format!("{id}/{method}")
        };
        if method == "decision_status" {
            return ResponseTemplate::new(200)
                .set_body_json(state.receipts.get(&key).map(|(_, receipt)| receipt));
        }
        *state.calls.entry(method.into()).or_default() += 1;
        if let Some((old, receipt)) = state.receipts.get(&key) {
            return if *old == input {
                ResponseTemplate::new(200).set_body_json(receipt)
            } else {
                ResponseTemplate::new(409)
            };
        }
        let receipt = match method {
            "create" => {
                let mut link = snapshot(id.parse().unwrap(), false);
                link["creation"] = input.clone();
                state.links.insert(id.into(), link.clone());
                link
            }
            "revoke" => {
                let link = state.links.get_mut(id).unwrap();
                link["revoked_at"] = serde_json::json!("2026-09-18T12:00:00Z");
                link["revoked_by"] = serde_json::json!(42);
                link["revision"] = serde_json::json!(2);
                link.clone()
            }
            "decide" => serde_json::json!({"outcome":"applied", "request":{
                "request_id":input["request_id"],"link_id":id,"account_id":42,"requester_id":99,
                "state":if input["action"]["kind"] == "approve" {"approved"} else {"declined"},
                "admitted_at":"2026-09-14T12:00:00Z","decision_deadline":"2026-09-21T12:00:00Z","revision":2}}),
            _ => panic!("unexpected method {method}"),
        };
        *state.effects.entry(method.into()).or_default() += 1;
        state.receipts.insert(key, (input, receipt));
        ResponseTemplate::new(503).set_body_string("fixture: committed, acknowledgement lost")
    }
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
        Ok(oauth_sign_in_expectations()
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
    use ghinvite_core::storage::projection::ProjectionStorage;
    let ingress = MockServer::start().await;
    let effects = Arc::new(Mutex::new(LostAcknowledgements::default()));
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(LostAcknowledgementTransport(effects.clone()))
        .mount(&ingress)
        .await;
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
    let link = ghinvite_core::InvitationLinkId::new();
    let link_snapshot = snapshot(link, false);
    effects
        .lock()
        .unwrap()
        .links
        .insert(link.to_string(), link_snapshot.clone());
    let requests = (0..2).map(|_| serde_json::json!({"request_id":ghinvite_core::RequestId::new(),"link_id":link,"account_id":42,
        "requester_id":99,"state":"pending","admitted_at":"2026-09-14T12:00:00Z","decision_deadline":"2026-09-21T12:00:00Z","revision":1})).collect::<Vec<_>>();
    storage
        .apply_transition(
            &serde_json::from_value(
                serde_json::json!({"version":1,"transition_id":format!("v1/link/{link}/1"),
        "link":link_snapshot,"requests":requests,"events":[]}),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let app = build_app(
        AppState::new(
            storage,
            Arc::new(BrowserGithub),
            Arc::new(RecordingCommands::default()),
            WebConfig::for_local_dev_with_secret([7; 32]),
        )
        .with_admission(Arc::new(RestateClient::new(ingress.uri()).unwrap())),
        protected_store().await,
    );
    let login_app = app.clone();
    let fixture = axum::Router::new()
        .route(
            "/fixture-login",
            axum::routing::get(move || {
                let app = login_app.clone();
                async move {
                    let (_, cookie) = sign_in(app).await;
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
                let effects = effects.clone();
                async move {
                    let e = effects.lock().unwrap();
                    axum::Json(serde_json::json!({"effects":e.effects,"calls":e.calls}))
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
