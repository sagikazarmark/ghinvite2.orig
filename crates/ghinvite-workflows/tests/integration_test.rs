//! Real Restate acceptance gate: run `bash scripts/test-restate.sh` from the repository root.
//! The integration feature opts in; missing infrastructure is a failure, never a skip.

#![cfg(feature = "integration")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ghinvite_core::audit::{ActorKind, EventType, TargetKind};
use ghinvite_core::storage::{AuditPosition, Storage};
use ghinvite_core::{
    GithubInvitation, InvitationLinkRepo, InvitationState, Permission, RequestId, RequestState,
};
use ghinvite_github::InstallationClient;
use ghinvite_github::jwt::AppJwtSigner;
use ghinvite_github::transport::ReqwestTransport;
use ghinvite_storage_sqlx::SqlxStorage;
use ghinvite_web::commands::{
    CreateInvitationLink, DecideInvitationRequest, GhinviteCommands, RestateCommands,
    SubmitInvitationRequest,
};
use ghinvite_web::restate_client::RestateClient;
use ghinvite_workflows::invitation_link::CreateLinkOutput;
use reqwest::{Client, Response};
use restate_sdk::http_server::HttpServer;
use serde_json::json;
use tokio::net::TcpListener;
use tokio::time::{Instant, sleep, timeout};

const TEST_KEY_PEM: &str = include_str!("../../ghinvite-github/src/jwt_test_key.pem");

// A transparent HTTP observer: forward the real command bytes to real ingress
// and relay its response unchanged. Capture only send receipts, never credentials.
// This lets the test correlate duplicate commands without changing the production
// fire-and-forget command API or substituting an echo/mock response.
#[derive(Clone)]
struct IngressObserver {
    client: Client,
    ingress: String,
    receipts: Arc<Mutex<Vec<serde_json::Value>>>,
}

async fn forward_ingress(
    axum::extract::State(observer): axum::extract::State<IngressObserver>,
    request: axum::extract::Request,
) -> axum::response::Response {
    let path = request.uri().path().to_owned();
    let method = request.method().clone();
    let body = axum::body::to_bytes(request.into_body(), 64 * 1024)
        .await
        .unwrap();
    let response = observer
        .client
        .request(method, format!("{}{path}", observer.ingress))
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.bytes().await.unwrap();
    if path.ends_with("/send") && status.is_success() {
        observer
            .receipts
            .lock()
            .unwrap()
            .push(serde_json::from_slice(&body).expect("real Restate send receipt"));
    }
    let mut response = axum::response::Response::new(axum::body::Body::from(body));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response
}

fn runtime_var(name: &str) -> String {
    std::env::var(name)
        .unwrap_or_else(|_| panic!("{name} is required; run: bash scripts/test-restate.sh"))
}

// Report phase/status and actionable topology hints, never arbitrary response
// bodies, headers or URLs (which may contain credentials).
async fn success(phase: &str, request: reqwest::RequestBuilder) -> Response {
    let response = request.send().await.unwrap_or_else(|error| {
        panic!(
            "{phase}: HTTP transport failed (timeout={}, connect={}); check the disposable runtime and host.docker.internal reachability",
            error.is_timeout(),
            error.is_connect()
        )
    });
    assert!(
        response.status().is_success(),
        "{phase}: HTTP {}; registration requires Restate to reach the test endpoint via host.docker.internal; response body/headers omitted",
        response.status()
    );
    response
}

async fn ready(client: &Client, url: &str, phase: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut last_status = None;
    loop {
        if let Ok(Ok(response)) = timeout(Duration::from_secs(2), client.get(url).send()).await {
            last_status = Some(response.status());
            if response.status().is_success() {
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "{phase}: not ready within 30s; last HTTP status: {last_status:?}; run bash scripts/test-restate.sh"
        );
        sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test]
async fn real_restate_approval_delivery_and_expiration() {
    timeout(Duration::from_secs(120), scenario())
        .await
        .expect("Restate acceptance gate exceeded its 120s scenario deadline");
}

async fn scenario() {
    let admin = runtime_var("RESTATE_ADMIN_URL");
    let ingress = runtime_var("RESTATE_INGRESS_URL");
    let endpoint_host = runtime_var("RESTATE_ENDPOINT_HOST");
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .unwrap();
    ready(&client, &format!("{admin}/health"), "Restate readiness").await;

    let storage: Arc<dyn Storage> = Arc::new(SqlxStorage::in_memory().await.unwrap());
    let created_at = chrono::Utc::now();
    let expires_at = created_at + chrono::Duration::seconds(2);
    for (user_id, login) in [(7, "creator"), (8, "requester")] {
        storage
            .upsert_user(&ghinvite_core::User {
                user_id,
                login: login.to_string(),
                avatar_url: None,
                last_seen_at: created_at,
            })
            .await
            .unwrap();
    }
    let stub_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stub_base = format!("http://{}", stub_listener.local_addr().unwrap());
    let stub_server = tokio::spawn(async move {
        axum::serve(stub_listener, ghinvite_github::stub::router())
            .await
            .unwrap();
    });
    let transport = Arc::new(ReqwestTransport::with_client(client.clone()));
    let signer = AppJwtSigner::from_pem(123, TEST_KEY_PEM).unwrap();
    let github_client = Arc::new(InstallationClient::new(transport, signer).with_base(&stub_base));
    let state = ghinvite_workflows::AppState::new(storage.clone(), github_client.clone());
    let endpoint = ghinvite_workflows::build_endpoint(state, None).unwrap();
    let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    // The Tokio test runtime drops the endpoint task even if an assertion fails.
    let server = tokio::spawn(async move {
        HttpServer::new(endpoint)
            .serve_with_cancel(listener, std::future::pending::<()>())
            .await;
    });
    // The SDK's native server speaks cleartext HTTP/2 only.
    let endpoint_client = Client::builder()
        .http2_prior_knowledge()
        .default_headers(reqwest::header::HeaderMap::from_iter([(
            reqwest::header::ACCEPT,
            reqwest::header::HeaderValue::from_static(
                "application/vnd.restate.endpointmanifest.v4+json",
            ),
        )]))
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    ready(
        &endpoint_client,
        &format!("http://127.0.0.1:{port}/discover"),
        "Endpoint discovery",
    )
    .await;

    success(
        "Deployment registration",
        client.post(format!("{admin}/deployments")).json(&json!({
            "uri": format!("http://{endpoint_host}:{port}")
        })),
    )
    .await;

    // The runner provides fresh Restate state, and workflow/object keys are unique.
    let run_id = RequestId::new();
    success(
        "configure installation identity",
        client
            .post(format!("{stub_base}/installation-identity"))
            .json(&json!({"id":42,"login":"test-org","type":"Organization"})),
    )
    .await;
    success(
        "Installation::onboard",
        client
            .post(format!("{ingress}/Installation/1/onboard"))
            .json(&json!({
                "installation_id": 1,
                "actor_user_id": 7,
                "account_id": 42,
                "account_login": "test-org",
                "account_type": "Organization",
                "selected_repos": "all",
                "installed_at": created_at
            })),
    )
    .await;
    let installation = storage
        .get_installation(1)
        .await
        .unwrap()
        .expect("installation persisted");
    assert_eq!(installation.account_id, 42);
    assert_eq!(installation.account_login, "test-org");
    let onboarding_calls: serde_json::Value = client
        .get(format!("{stub_base}/calls"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let response = success(
        "InvitationLink::create",
        client
            .post(format!("{ingress}/InvitationLink/{run_id}/create"))
            .json(&json!({
                "installation_id": 1,
                "account_id": 42,
                "created_by": 7,
                "created_at": created_at,
                "expires_at": expires_at,
                "max_uses": 1,
                "permission": "pull",
                "approval_required": true,
                "description": "Real Restate expiration smoke",
                "internal_note": null,
                "repos": [{"repo_id": 10, "repo_full_name": "test-org/smoke"}]
            })),
    )
    .await;
    let link: CreateLinkOutput = response
        .json()
        .await
        .expect("create-link response contract");
    assert!(!link.slug.is_empty());

    let request_id = RequestId::new();
    let response = success(
        "InvitationRequest::submit (including durable expiration)",
        client
            .post(format!("{ingress}/InvitationRequest/{request_id}/submit"))
            .json(&json!({
                "request_id": request_id,
                "invitation_link_id": link.link_id,
                "requester_id": 8,
                "justification": null,
                "created_at": created_at
            })),
    )
    .await;
    let final_state: RequestState = response.json().await.expect("submit response contract");
    assert_eq!(final_state, RequestState::Expired);
    let request = storage
        .get_invitation_request(request_id)
        .await
        .unwrap()
        .expect("request persisted");
    assert_eq!(request.state, RequestState::Expired);
    assert_eq!(request.invitation_link_id, link.link_id);
    assert_eq!(request.requester_id, 8);
    assert_eq!(request.decided_at, Some(expires_at));
    assert_eq!(request.decided_by, None);
    let stored_link = storage
        .get_invitation_link_by_id(link.link_id)
        .await
        .unwrap()
        .expect("link persisted");
    assert_eq!(stored_link.description, "Real Restate expiration smoke");
    assert_eq!(stored_link.slug.as_str(), link.slug);
    assert_eq!(stored_link.uses_count, 1);
    assert_eq!(stored_link.repos.len(), 1);
    assert_eq!(stored_link.repos[0].repo_full_name, "test-org/smoke");

    let audit = storage
        .list_audit_events(42, None, AuditPosition::Latest)
        .await
        .unwrap();
    assert_eq!(
        audit.events.len(),
        5,
        "installation, repository refresh, link, request creation and expiration"
    );
    for expected in [
        EventType::InstallationCreated,
        EventType::InvitationLinkCreated,
        EventType::RequestCreated,
        EventType::RequestExpired,
    ] {
        assert_eq!(
            audit
                .events
                .iter()
                .filter(|event| event.event_type == expected)
                .count(),
            1
        );
    }
    let expiration = audit
        .events
        .iter()
        .find(|event| event.event_type == EventType::RequestExpired)
        .unwrap();
    assert_eq!(expiration.target_kind, TargetKind::InvitationRequest);
    assert_eq!(expiration.target_id, request_id.to_string());
    assert_eq!(expiration.actor_kind, ActorKind::System);
    assert_eq!(expiration.metadata, json!({"reason": "timeout"}));
    assert!(
        expiration
            .request_id
            .as_ref()
            .is_some_and(|id| !id.is_empty())
    );
    assert_eq!(
        stub_calls(&client, &stub_base).await,
        onboarding_calls,
        "expiration must not contact GitHub"
    );
    let observer = IngressObserver {
        client: client.clone(),
        ingress,
        receipts: Arc::default(),
    };
    let observer_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let restate = Arc::new(
        RestateClient::new(format!(
            "http://{}",
            observer_listener.local_addr().unwrap()
        ))
        .unwrap(),
    );
    let observer_app = axum::Router::new()
        .fallback(forward_ingress)
        .with_state(observer.clone());
    let observer_server = tokio::spawn(async move {
        axum::serve(observer_listener, observer_app).await.unwrap();
    });
    let commands = RestateCommands::new(restate);
    approval_fanout(
        &storage,
        &commands,
        &client,
        &stub_base,
        &github_client,
        &observer,
        false,
    )
    .await;
    approval_fanout(
        &storage,
        &commands,
        &client,
        &stub_base,
        &github_client,
        &observer,
        true,
    )
    .await;
    server.abort();
    stub_server.abort();
    observer_server.abort();
}

async fn stub_calls(client: &Client, base: &str) -> serde_json::Value {
    success(
        "GitHub stub call ledger",
        client.get(format!("{base}/calls")),
    )
    .await
    .json()
    .await
    .unwrap()
}

async fn wait_for_request(storage: &Arc<dyn Storage>, id: RequestId, expected: RequestState) {
    timeout(Duration::from_secs(15), async {
        loop {
            if storage
                .get_invitation_request(id)
                .await
                .unwrap()
                .is_some_and(|r| r.state == expected)
            {
                return;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("request did not reach {expected:?} within 15s"));
}

// Read through the account audit/public Storage API, including terminal invitations
// that the pending-invitation query intentionally excludes. No SQL/debug helpers.
async fn delivered_invitations(
    storage: &Arc<dyn Storage>,
    request_id: RequestId,
) -> Vec<GithubInvitation> {
    let audit = storage
        .list_audit_events(42, None, AuditPosition::Latest)
        .await
        .unwrap();
    let mut rows = Vec::new();
    for event in audit
        .events
        .iter()
        .filter(|e| e.target_kind == TargetKind::GithubInvitation)
    {
        let row = storage
            .get_github_invitation(event.target_id.parse().unwrap())
            .await
            .unwrap()
            .expect("audited invitation persisted");
        if row.invitation_request_id == request_id
            && !rows.iter().any(|r: &GithubInvitation| r.id == row.id)
        {
            rows.push(row);
        }
    }
    rows.sort_by_key(|r| r.repo_id);
    rows
}

async fn approval_fanout(
    storage: &Arc<dyn Storage>,
    commands: &RestateCommands,
    http: &Client,
    stub: &str,
    github: &InstallationClient,
    observer: &IngressObserver,
    manual: bool,
) {
    // Literal expectations for each repository; keep the wire and persisted
    // outcomes together rather than encoding meaning in repository positions.
    struct ExpectedDelivery {
        name: &'static str,
        repo_id: u64,
        configured_outcome: Option<&'static str>,
        state: InvitationState,
        statuses: &'static [u16],
        event: EventType,
        actor: ActorKind,
        detail: Option<(&'static str, &'static str)>,
    }
    let cases = [
        ExpectedDelivery {
            name: "sent",
            repo_id: 20,
            configured_outcome: None,
            state: InvitationState::Sent,
            statuses: &[201],
            event: EventType::InvitationSent,
            actor: ActorKind::System,
            detail: None,
        },
        ExpectedDelivery {
            name: "member",
            repo_id: 21,
            configured_outcome: Some("already_collaborator"),
            state: InvitationState::Accepted,
            statuses: &[204],
            event: EventType::InvitationAccepted,
            actor: ActorKind::Github,
            detail: Some(("reason", "already_collaborator")),
        },
        ExpectedDelivery {
            name: "denied",
            repo_id: 22,
            configured_outcome: Some("terminal_failure"),
            state: InvitationState::Failed,
            statuses: &[422],
            event: EventType::InvitationSendFailed,
            actor: ActorKind::System,
            detail: Some((
                // The audit detail is the sanitized diagnostic now. No upstream
                // text survives at all: the status classifies the failure and
                // the envelope's size is all that is said about its message.
                "error",
                "github returned status 422: message withheld (23 bytes)",
            )),
        },
        ExpectedDelivery {
            name: "retry",
            repo_id: 23,
            configured_outcome: Some("transient_once"),
            state: InvitationState::Sent,
            statuses: &[502, 201],
            event: EventType::InvitationSent,
            actor: ActorKind::System,
            detail: None,
        },
    ];
    let now = chrono::Utc::now();
    let prefix = if manual { "manual" } else { "auto" };
    let repos = cases
        .each_ref()
        .map(|case| format!("{prefix}-{}", case.name));
    for (case, repo) in cases.iter().zip(&repos) {
        let Some(outcome) = case.configured_outcome else {
            continue;
        };
        success(
            "Configure GitHub outcome",
            http.post(format!("{stub}/outcomes")).json(&json!({
                "owner":"test-org", "repo":repo, "user":"requester", "outcome":outcome
            })),
        )
        .await;
    }
    let link = commands
        .create_invitation_link(CreateInvitationLink {
            installation_id: 1,
            account_id: 42,
            created_by: 7,
            created_at: now,
            expires_at: Some(now + chrono::Duration::minutes(5)),
            max_uses: Some(2),
            permission: Permission::Push,
            approval_required: manual,
            description: format!("{prefix} acceptance gate"),
            internal_note: Some("private-note-canary".into()),
            repos: cases
                .iter()
                .zip(&repos)
                .map(|(case, name)| InvitationLinkRepo {
                    repo_id: case.repo_id,
                    repo_full_name: format!("test-org/{name}"),
                })
                .collect(),
        })
        .await
        .expect("web create-link command over real Restate");
    let id = RequestId::new();
    let submit = SubmitInvitationRequest::new(
        id,
        link.link_id,
        8,
        Some("private-justification-canary".into()),
        now,
    );
    commands
        .submit_invitation_request(submit.clone())
        .await
        .expect("web submit command over real Restate");
    let first_receipt = observer.receipts.lock().unwrap().last().unwrap().clone();
    assert!(
        first_receipt["invocationId"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
    if manual {
        wait_for_request(storage, id, RequestState::Pending).await;
        assert!(delivered_invitations(storage, id).await.is_empty());
        let calls = stub_calls(http, stub).await;
        assert!(
            !calls["requests"]
                .as_array()
                .unwrap()
                .iter()
                .any(|call| call["path"].as_str().unwrap().contains("/manual-")),
            "pending request must not dispatch GitHub work"
        );
        commands
            .decide_invitation_request(DecideInvitationRequest::approve(id, 7, now))
            .await
            .expect("web approval resolves real durable promise");
    }
    wait_for_request(storage, id, RequestState::Approved).await;
    let rows = timeout(Duration::from_secs(30), async {
        loop {
            let rows = delivered_invitations(storage, id).await;
            if rows.len() == 4 {
                break rows;
            }
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("four audited repository outcomes within 30s, including runtime retry");
    for ((case, repo), row) in cases.iter().zip(&repos).zip(&rows) {
        assert_eq!(row.repo_id, case.repo_id);
        assert_eq!(row.state, case.state);
        assert_eq!(row.invitation_request_id, id);
        if case.state == InvitationState::Sent {
            let pending = github.list_invitations(1, "test-org", repo).await.unwrap();
            assert_eq!(pending.len(), 1);
            assert_eq!(row.github_invitation_id, Some(pending[0].id));
            assert_eq!(pending[0].invitee.login, "requester");
            assert_eq!(pending[0].permissions, "write");
            assert_eq!(row.error_message, None);
        } else {
            assert_eq!(row.github_invitation_id, None);
            assert_eq!(
                row.error_message.as_deref(),
                case.detail
                    .filter(|(key, _)| *key == "error")
                    .map(|(_, value)| value)
            );
        }
    }
    let request = storage.get_invitation_request(id).await.unwrap().unwrap();
    assert_eq!(
        request.state,
        RequestState::Approved,
        "approval is distinct from delivery: one repo failed"
    );
    assert_eq!(request.requester_id, 8);
    assert_eq!(request.invitation_link_id, link.link_id);
    assert_eq!(request.decided_by, if manual { Some(7) } else { None });
    assert_eq!(request.decided_at, Some(now));

    let calls = stub_calls(http, stub).await;
    assert_eq!(
        calls["requests"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["method"] == "PUT")
            .count(),
        if manual { 10 } else { 5 },
        "no extra fanout to unexpected repositories or requesters"
    );
    for (case, repo) in cases.iter().zip(&repos) {
        let path = format!("/repos/test-org/{repo}/collaborators/requester");
        let puts: Vec<_> = calls["requests"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["method"] == "PUT" && c["path"] == path)
            .collect();
        assert_eq!(
            puts.len(),
            case.statuses.len(),
            "exact outbound attempts for {repo}"
        );
        for (call, status) in puts.iter().zip(case.statuses) {
            assert_eq!(call["permission"], "push");
            assert_eq!(call["status"], *status);
        }
    }
    let audit = storage
        .list_audit_events(42, None, AuditPosition::Latest)
        .await
        .unwrap();
    let request_events: Vec<_> = audit
        .events
        .iter()
        .filter(|e| e.target_id == id.to_string())
        .collect();
    assert_eq!(request_events.len(), 2);
    let created = request_events
        .iter()
        .find(|e| e.event_type == EventType::RequestCreated)
        .unwrap();
    assert_eq!(created.target_kind, TargetKind::InvitationRequest);
    assert_eq!(created.actor_kind, ActorKind::User);
    assert_eq!(created.actor_id, Some(8));
    assert_eq!(
        created.metadata,
        json!({"invitation_link_id":link.link_id.to_string(), "auto_approve":!manual})
    );
    let approved = request_events
        .iter()
        .find(|e| e.event_type == EventType::RequestApproved)
        .unwrap();
    assert_eq!(
        approved.actor_kind,
        if manual {
            ActorKind::User
        } else {
            ActorKind::System
        }
    );
    assert_eq!(approved.actor_id, if manual { Some(7) } else { None });
    assert_eq!(approved.target_kind, TargetKind::InvitationRequest);
    assert_eq!(
        approved.metadata,
        if manual {
            json!({})
        } else {
            json!({"reason":"auto_approve"})
        }
    );
    for ((case, repo), row) in cases.iter().zip(&repos).zip(&rows) {
        let events: Vec<_> = audit
            .events
            .iter()
            .filter(|e| e.target_id == row.id.to_string())
            .collect();
        assert_eq!(
            events.len(),
            1,
            "one audit per logical delivery, including retry"
        );
        let event = events[0];
        assert_eq!(event.event_type, case.event);
        assert_eq!(event.actor_kind, case.actor);
        assert_eq!(event.actor_id, None);
        let mut metadata =
            json!({"repo_full_name":format!("test-org/{repo}"), "recipient":"requester"});
        if let Some((key, value)) = case.detail {
            metadata[key] = json!(value);
        } else {
            metadata["github_invitation_id"] = json!(row.github_invitation_id.unwrap());
        }
        assert_eq!(
            event.metadata, metadata,
            "audit contains only expected integration metadata"
        );
    }
    for event in &audit.events {
        assert_eq!(event.account_id, 42);
        assert!(event.request_id.as_ref().is_some_and(|id| !id.is_empty()));
        let metadata = event.metadata.to_string();
        for forbidden in [
            "private-note-canary",
            "private-justification-canary",
            "test-token",
            "Bearer",
            "PRIVATE KEY",
        ] {
            assert!(
                !metadata.contains(forbidden),
                "audit must not expose {forbidden}"
            );
        }
    }
    // Complete the workflow before submitting the exact stable operation again.
    // Workflow output is approval, not the asynchronous per-repo delivery result.
    let output_url = format!(
        "{}/restate/workflow/InvitationRequest/{id}/output",
        runtime_var("RESTATE_INGRESS_URL")
    );
    let output = success("Completed workflow output", http.get(&output_url)).await;
    assert_eq!(
        output.json::<RequestState>().await.unwrap(),
        RequestState::Approved
    );
    commands.submit_invitation_request(submit).await.unwrap();
    let duplicate_receipt = observer.receipts.lock().unwrap().last().unwrap().clone();
    assert_eq!(
        duplicate_receipt["invocationId"], first_receipt["invocationId"],
        "real ingress must deduplicate the stable command to the completed invocation"
    );
    // The receipt establishes identity; output establishes completion for that
    // same workflow. A duplicate routed to any new invocation fails above.
    let response = success(
        "Completed workflow output after duplicate",
        http.get(&output_url),
    )
    .await;
    assert_eq!(
        response.json::<RequestState>().await.unwrap(),
        RequestState::Approved
    );
    assert_eq!(
        stub_calls(http, stub).await,
        calls,
        "duplicate stable operation must not dispatch more GitHub work"
    );
    assert_eq!(
        storage
            .get_invitation_link_by_id(link.link_id)
            .await
            .unwrap()
            .unwrap()
            .uses_count,
        1
    );
    assert_eq!(
        storage
            .list_requests_for_link(link.link_id)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        delivered_invitations(storage, id)
            .await
            .iter()
            .map(|r| r.id)
            .collect::<Vec<_>>(),
        rows.iter().map(|r| r.id).collect::<Vec<_>>()
    );
    assert_eq!(
        storage
            .list_audit_events(42, None, AuditPosition::Latest)
            .await
            .unwrap()
            .events
            .len(),
        audit.events.len()
    );
    eprintln!(
        "{prefix} approval: four persisted repository outcomes, bounded retry, stable-operation deduplication verified"
    );
}
