//! Real Restate smoke: run `bash scripts/test-restate.sh` from the repository root.
//! The integration feature opts in; missing infrastructure is a failure, never a skip.

#![cfg(feature = "integration")]

use std::sync::Arc;
use std::time::Duration;

use ghinvite_core::audit::{ActorKind, EventType, TargetKind};
use ghinvite_core::storage::{AuditPosition, Storage};
use ghinvite_core::{RequestId, RequestState};
use ghinvite_github::InstallationClient;
use ghinvite_github::jwt::AppJwtSigner;
use ghinvite_github::mocks::MockTransport;
use ghinvite_storage_sqlx::SqlxStorage;
use ghinvite_workflows::invitation_link::CreateLinkOutput;
use reqwest::{Client, Response};
use restate_sdk::http_server::HttpServer;
use serde_json::json;
use tokio::net::TcpListener;
use tokio::time::{Instant, sleep, timeout};

const TEST_KEY_PEM: &str = include_str!("../../ghinvite-github/src/jwt_test_key.pem");

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
async fn real_restate_persists_request_expiration() {
    timeout(Duration::from_secs(120), scenario())
        .await
        .expect("Restate smoke exceeded its 120s scenario deadline");
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
    // Expiration must not contact GitHub. An empty script rejects any API call.
    let transport = Arc::new(MockTransport::scripted(vec![]));
    let signer = AppJwtSigner::from_pem(123, TEST_KEY_PEM).unwrap();
    let github_client =
        Arc::new(InstallationClient::new(transport, signer).with_base("https://api.github.test"));
    let state = ghinvite_workflows::AppState::new(storage.clone(), github_client);
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
        "Installation::onboard",
        client
            .post(format!("{ingress}/Installation/{run_id}/onboard"))
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
        4,
        "installation, link, request creation and expiration"
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
    server.abort();
}
