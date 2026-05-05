//! Restate handler integration tests.
//!
//! Requires:
//! - Restate running on localhost:9070 (admin) and localhost:8080 (ingress)
//!   Start with: docker compose up -d
//!   Wait with:  until curl -sf http://localhost:9070/health; do sleep 2; done
//! - github-stub running on localhost:3001:
//!   cargo run -p github-stub -- --port 3001 &
//!
//! Run with:
//!   cargo test -p restate-svc --features integration -- --ignored

#![cfg(feature = "integration")]

use std::net::SocketAddr;
use std::sync::Arc;

use github::jwt::AppJwtSigner;
use github::transport::ReqwestTransport;
use github::InstallationClient;
use restate_sdk::http_server::HttpServer;
use storage::SqlxStorage;
use tokio::net::TcpListener;

const TEST_KEY_PEM: &str = include_str!("../../github/src/jwt_test_key.pem");

async fn setup_restate() -> SocketAddr {
    let storage = Arc::new(SqlxStorage::in_memory().await.unwrap());
    let transport = Arc::new(ReqwestTransport::new().unwrap());
    let signer = AppJwtSigner::from_pkcs8_pem(123, TEST_KEY_PEM).unwrap();
    let github_client = Arc::new(
        InstallationClient::new(transport, signer)
            .with_base("http://localhost:3001"),
    );
    let state = restate_svc::AppState::new(storage, github_client);
    let endpoint = restate_svc::build_endpoint(state, None).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        HttpServer::new(endpoint)
            .serve_with_cancel(listener, std::future::pending::<()>())
            .await;
    });

    // Register with Restate admin API
    reqwest::Client::new()
        .post("http://localhost:9070/restate/v1/deployments")
        .json(&serde_json::json!({
            "uri": format!("http://127.0.0.1:{}", addr.port())
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
        .expect("github-stub not running on :3001 — run: cargo run -p github-stub -- --port 3001");
}

#[tokio::test]
#[ignore]
async fn installation_install_and_query() {
    reset_github_stub().await;
    let _addr = setup_restate().await;

    let client = reqwest::Client::new();
    let resp = client
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
    assert_eq!(resp.status(), 200, "Installation::onboard failed");
}
