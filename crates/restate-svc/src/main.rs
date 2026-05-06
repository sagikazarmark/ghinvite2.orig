//! Native binary: runs the Restate endpoint on a local HTTP server.
//! The Workers entry point in `crates/restate-svc-worker` wraps the same
//! `build_endpoint` call for production.
//!
//! Environment variables (all optional — defaults work for `docker compose up`):
//!
//! - `GHINVITE_GITHUB_APP_ID`          — GitHub App numeric id (default: 0, disables real API calls)
//! - `GHINVITE_GITHUB_APP_PRIVATE_KEY`      — PEM private key as a plain string (not base64)
//! - `GHINVITE_GITHUB_APP_PRIVATE_KEY_FILE` — path to the PEM file (alternative to the above)
//! - `GHINVITE_LISTEN_ADDR`            — bind address (default: 127.0.0.1:9080); use 0.0.0.0:9080
//!                                       when Restate must reach this binary from inside Docker
//! - `GHINVITE_DATABASE_PATH`          — path to SQLite file (default: in-memory, resets on restart)
//! - `RESTATE_IDENTITY_KEY`            — Restate Cloud signing key (`publickeyv1_...`); optional for
//!                                       local dev, required in production

use std::net::SocketAddr;
use std::sync::Arc;

use github::{InstallationClient, jwt::AppJwtSigner, transport::ReqwestTransport};
use restate_sdk::http_server::HttpServer;
use restate_svc::{AppState, build_endpoint};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let storage: Arc<dyn storage::Storage> = match std::env::var("GHINVITE_DATABASE_PATH") {
        Ok(path) => {
            let s = storage::SqlxStorage::at_path(std::path::Path::new(&path)).await?;
            s.run_migrations().await?;
            Arc::new(s)
        }
        Err(_) => {
            tracing::warn!(
                "GHINVITE_DATABASE_PATH not set — using in-memory SQLite (state resets on restart)"
            );
            Arc::new(storage::SqlxStorage::in_memory().await?)
        }
    };

    let app_id: u64 = std::env::var("GHINVITE_GITHUB_APP_ID")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let pem: Option<String> = if let Ok(pem) = std::env::var("GHINVITE_GITHUB_APP_PRIVATE_KEY") {
        Some(pem)
    } else if let Ok(path) = std::env::var("GHINVITE_GITHUB_APP_PRIVATE_KEY_FILE") {
        Some(std::fs::read_to_string(&path).map_err(|e| format!("reading {path}: {e}"))?)
    } else {
        None
    };

    let transport = Arc::new(ReqwestTransport::new()?);
    let github_client = if let Some(pem) = pem {
        let signer = AppJwtSigner::from_pem(app_id, &pem)?;
        Arc::new(InstallationClient::new(transport, signer))
    } else {
        tracing::warn!(
            "neither GHINVITE_GITHUB_APP_PRIVATE_KEY nor GHINVITE_GITHUB_APP_PRIVATE_KEY_FILE set \
             — GitHub API calls will fail at runtime"
        );
        let dummy_pem = include_str!("../../github/src/jwt_test_key.pem");
        let signer = AppJwtSigner::from_pem(app_id, dummy_pem)?;
        Arc::new(InstallationClient::new(transport, signer))
    };

    let state = AppState::new(storage, github_client);
    // Identity verification is optional for local dev (docker compose Restate
    // does not sign requests). In production the Worker reads
    // RESTATE_IDENTITY_KEY from wrangler secrets.
    let identity_key = std::env::var("RESTATE_IDENTITY_KEY")
        .ok()
        .map(|s| s.trim().to_string());
    if identity_key.is_none() {
        tracing::warn!(
            "RESTATE_IDENTITY_KEY not set — endpoint will accept unsigned requests \
             (safe for local dev only)"
        );
    }
    let endpoint = build_endpoint(state, identity_key.as_deref())?;

    let addr: SocketAddr = std::env::var("GHINVITE_LISTEN_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:9080".into())
        .parse()?;

    tracing::info!("restate-svc listening on http://{addr}");
    tracing::info!(
        "register with: curl -X POST http://localhost:9070/restate/v1/deployments -H 'Content-Type: application/json' -d '{{\"uri\":\"http://host.docker.internal:{}\"}}'",
        addr.port()
    );

    HttpServer::new(endpoint).listen_and_serve(addr).await;
    Ok(())
}
