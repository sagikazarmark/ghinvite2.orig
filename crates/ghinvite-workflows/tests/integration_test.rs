//! Restate handler integration tests.
//!
//! Requires:
//! - Restate running on localhost:9070 (admin) and localhost:8080 (ingress)
//!   Start with: docker compose up -d
//!   Wait with:  until curl -sf http://localhost:9070/health; do sleep 2; done
//! - github-stub running on localhost:3001:
//!   cargo run -p ghinvite-github --example stub -- --port 3001 &
//!
//! Run with:
//!   cargo test -p ghinvite-workflows --features integration -- --ignored

#![cfg(feature = "integration")]

use std::net::SocketAddr;
use std::sync::Arc;

use ghinvite_core::RequestId;
use ghinvite_core::storage::{SqlxStorage, Storage};
use ghinvite_github::InstallationClient;
use ghinvite_github::jwt::AppJwtSigner;
use ghinvite_github::transport::ReqwestTransport;
use restate_sdk::http_server::HttpServer;
use tokio::net::TcpListener;

const TEST_KEY_PEM: &str = include_str!("../../ghinvite-github/src/jwt_test_key.pem");

async fn setup_restate() -> SocketAddr {
    let storage = SqlxStorage::in_memory().await.unwrap();
    let last_seen_at = chrono::DateTime::parse_from_rfc3339("2026-05-04T12:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    // InvitationLink and InvitationRequest enforce user FKs; this smoke only exercises ingress.
    for (user_id, login) in [(7, "creator"), (8, "requester")] {
        storage
            .upsert_user(&ghinvite_core::User {
                user_id,
                login: login.to_string(),
                avatar_url: None,
                last_seen_at: last_seen_at.clone(),
            })
            .await
            .unwrap();
    }
    let storage = Arc::new(storage);
    let transport = Arc::new(ReqwestTransport::new().unwrap());
    let signer = AppJwtSigner::from_pem(123, TEST_KEY_PEM).unwrap();
    let github_client =
        Arc::new(InstallationClient::new(transport, signer).with_base("http://localhost:3001"));
    let state = ghinvite_workflows::AppState::new(storage, github_client);
    let endpoint = ghinvite_workflows::build_endpoint(state, None).unwrap();

    let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let endpoint_host =
        std::env::var("RESTATE_ENDPOINT_HOST").unwrap_or_else(|_| "172.18.0.1".to_string());
    tokio::spawn(async move {
        HttpServer::new(endpoint)
            .serve_with_cancel(listener, std::future::pending::<()>())
            .await;
    });

    // Register with Restate admin API
    reqwest::Client::new()
        .post("http://localhost:9070/deployments")
        .json(&serde_json::json!({
            "uri": format!("http://{}:{}", endpoint_host, addr.port())
        }))
        .send()
        .await
        .expect("Restate admin API not reachable — run: docker compose up -d");

    addr
}

async fn reset_github_stub() {
    reqwest::Client::new()
        .delete("http://localhost:3001/reset")
        .send()
        .await
        .expect("github-stub not running on :3001 — run: cargo run -p ghinvite-github --example stub -- --port 3001");
}

#[tokio::test]
#[ignore]
async fn raw_json_ingress_accepts_object_and_workflow_payloads() {
    reset_github_stub().await;
    let _addr = setup_restate().await;

    let client = reqwest::Client::new();

    let onboard_resp = client
        .post("http://localhost:8080/Installation/1/onboard")
        .json(&serde_json::json!({
            "installation_id": 1,
            "actor_user_id": 7,
            "account_id": 42,
            "account_login": "test-org",
            "account_type": "Organization",
            "selected_repos": "all",
            "installed_at": "2026-05-04T12:00:00Z"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(onboard_resp.status(), 200, "Installation::onboard failed");

    let create_link_resp = client
        .post("http://localhost:8080/InvitationLink/raw-json-smoke/create")
        .json(&serde_json::json!({
            "installation_id": 1,
            "account_id": 42,
            "created_by": 7,
            "created_at": "2026-05-04T12:00:00Z",
            "expires_at": "2026-05-04T12:00:01Z",
            "max_uses": null,
            "permission": "pull",
            "approval_required": true,
            "internal_note": null,
            "repos": []
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        create_link_resp.status(),
        200,
        "InvitationLink::create failed"
    );
    let create_link_body: serde_json::Value = create_link_resp.json().await.unwrap();
    let link_id = create_link_body
        .get("link_id")
        .and_then(serde_json::Value::as_str)
        .expect("InvitationLink::create response should contain link_id")
        .to_string();
    assert!(
        create_link_body
            .get("slug")
            .and_then(serde_json::Value::as_str)
            .is_some(),
        "InvitationLink::create response should contain slug"
    );

    let request_id = RequestId::new().to_string();
    let submit_resp = client
        .post(format!(
            "http://localhost:8080/InvitationRequest/{request_id}/submit"
        ))
        .json(&serde_json::json!({
            "request_id": request_id,
            "invitation_link_id": link_id,
            "requester_id": 8,
            "justification": null,
            "created_at": "2026-05-04T12:00:00Z"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        submit_resp.status(),
        200,
        "InvitationRequest::submit failed"
    );
    let final_state: String = submit_resp.json().await.unwrap();
    assert_eq!(final_state, "expired");
}
