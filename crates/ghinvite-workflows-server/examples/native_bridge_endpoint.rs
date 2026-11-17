//! Fixture-only signed production endpoint composition with loopback transport.
//! The Workers entry point in `crates/ghinvite-workflows-worker` wraps the same
//! `build_endpoint` call for production.
//!
//! Required app configuration: P1_HOST, GH_REPO_APP_ID, GH_REPO_CLIENT_ID,
//! GH_REPO_PRIVATE_KEYS (`kid:base64(PKCS#8 PEM);...`, first key signs), and
//! GITHUB_API_BASE: a fixture never reaches GitHub, so its GitHub stand-in
//! (the octoemu emulator) is required, through the same seam as the
//! development roots (`ghinvite_config::github`).
//! Missing/invalid app configuration fails before storage or network startup.
//! Optional local configuration: GHINVITE_LISTEN_ADDR (127.0.0.1:9080),
//! GHINVITE_DATABASE_PATH (in-memory). RESTATE_IDENTITY_KEYS is required unless
//! --local-unsigned is explicitly selected for local development.

use std::net::SocketAddr;
use std::sync::Arc;

use ghinvite_config::apps::{AppStage, Deployment, organization_delivery_enabled, parse_mint_apps};
use ghinvite_workflows::{
    AppState, RepositoryGithub, build_endpoint_with_identity, endpoint_identity::EndpointIdentity,
};
use restate_sdk::http_server::HttpServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let stage = AppStage::from_environment()?;
    let organization_setting = match std::env::var("GH_ORG_DELIVERY_ENABLED") {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(_) => return Err("GH_ORG_DELIVERY_ENABLED invalid".into()),
    };
    let organization_enabled =
        organization_delivery_enabled(stage, organization_setting.as_deref())?;
    let reissue_setting = match std::env::var("GH_REISSUE_ENABLED") {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(_) => return Err("GH_REISSUE_ENABLED invalid".into()),
    };
    let reissue_enabled = ghinvite_config::reissue::enabled(reissue_setting.as_deref())?;
    let bulk_setting = match std::env::var("GH_BULK_ENABLED") {
        Ok(v) => Some(v),
        Err(std::env::VarError::NotPresent) => None,
        Err(_) => return Err("GH_BULK_ENABLED invalid".into()),
    };
    let bulk_enabled = ghinvite_config::bulk::enabled(bulk_setting.as_deref())?;
    let email_setting = match std::env::var("P1_EMAIL_KEYRING") {
        Ok(v) => Some(v),
        Err(std::env::VarError::NotPresent) => None,
        Err(_) => return Err("P1_EMAIL_KEYRING invalid".into()),
    };
    let email_ring =
        ghinvite_config::bulk::email_keyring(bulk_enabled && organization_enabled, email_setting)?;
    // A fixture deployment: GitHub's bases may (and the API base must) point
    // at a stand-in. Tokens are created there too.
    for field in ghinvite_config::github::OVERRIDES {
        if let Err(std::env::VarError::NotUnicode(_)) = std::env::var(field) {
            return Err(format!("{field} invalid").into());
        }
    }
    if std::env::var("GITHUB_API_BASE").is_err() {
        return Err("GITHUB_API_BASE required: the fixture never reaches GitHub".into());
    }
    let apps = parse_mint_apps(stage, Deployment::Development, |field| {
        std::env::var(field).ok()
    })?;
    let local_unsigned = match std::env::args().skip(1).collect::<Vec<_>>().as_slice() {
        [] => false,
        [flag] if flag == "--local-unsigned" => true,
        _ => return Err("expected no arguments or --local-unsigned".into()),
    };
    let addr: SocketAddr = std::env::var("GHINVITE_LISTEN_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:9080".into())
        .parse()?;
    let keys = std::env::var("RESTATE_IDENTITY_KEYS").ok();
    let identity = if local_unsigned {
        if keys.is_some() {
            return Err("--local-unsigned cannot be combined with RESTATE_IDENTITY_KEYS".into());
        }
        EndpointIdentity::local_unsigned()
    } else {
        EndpointIdentity::required(keys.as_deref())?
    };
    let github_transport = Arc::new(ghinvite_github::transport::ReqwestTransport::new()?);
    let github_apps = Arc::new(RepositoryGithub::new(
        github_transport,
        apps.github.api(),
        apps.installed,
    )?);

    // The macro shares the exact endpoint composition across both concrete
    // storage types. No second handler graph or blanket dynamic-storage facade.
    macro_rules! endpoint_with {
        ($storage:expr) => {{
            let storage = $storage;
            let state = AppState::new(storage.clone()).with_github_apps(github_apps);
            let state = if organization_enabled {
                state
                    .with_organization_delivery(storage.clone())
                    .with_pre_activation_cancellation()
            } else {
                state
            };
            let access_setting = match std::env::var("GH_ACCESS_REMOVAL_ENABLED") {
                Ok(value) => Some(value),
                Err(std::env::VarError::NotPresent) => None,
                Err(_) => return Err("GH_ACCESS_REMOVAL_ENABLED invalid".into()),
            };
            let state = if ghinvite_config::apps::access_removal_enabled(access_setting.as_deref())?
            {
                state.with_access_grants(storage.clone())
            } else {
                state
            };
            let state = if reissue_enabled {
                state.with_reissues(storage.clone())
            } else {
                state
            };
            let state = if bulk_enabled {
                state.with_bulk_invites(storage.clone())
            } else {
                state
            };
            let state = if bulk_enabled && organization_enabled {
                let ring = email_ring.ok_or("P1_EMAIL_KEYRING invalid")?;
                state.with_email_deliveries(
                    storage.clone(),
                    Arc::new(
                        ghinvite_email_vault::EmailVault::new(storage.clone(), ring)
                            .with_review_storage(storage.clone()),
                    ),
                )
            } else {
                state
            };
            build_endpoint_with_identity(state, storage, &identity)?
        }};
    }
    let storage_mode = match std::env::var("GHINVITE_STORAGE") {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => "local-sqlite".into(),
        Err(_) => return Err("GHINVITE_STORAGE invalid".into()),
    };
    let endpoint = match storage_mode.as_str() {
        "d1-bridge" => {
            if local_unsigned || std::env::var_os("GHINVITE_DATABASE_PATH").is_some() {
                return Err(
                    "D1 bridge forbids local unsigned mode and SQLite configuration".into(),
                );
            }
            let url = std::env::var("GHINVITE_D1_BRIDGE_URL")
                .map_err(|_| "D1 bridge configuration missing")?;
            let token = std::env::var("GHINVITE_D1_BRIDGE_TOKEN")
                .map_err(|_| "D1 bridge configuration missing")?;
            let path = std::env::var("GHINVITE_D1_AUTHORITY_MANIFEST")
                .map_err(|_| "D1 bridge configuration missing")?;
            let bytes = std::fs::read(path).map_err(|_| "D1 authority manifest unavailable")?;
            if bytes.len() > 1024 * 1024 {
                return Err("D1 authority manifest invalid".into());
            }
            let manifest =
                serde_json::from_slice(&bytes).map_err(|_| "D1 authority manifest invalid")?;
            let config =
                ghinvite_storage_d1::native::BridgeConfig::local_for_test(&url, token, manifest)?;
            let db = ghinvite_storage_d1::native::D1Database::connect(config).await?;
            endpoint_with!(Arc::new(ghinvite_storage_d1::D1Storage::new(db)))
        }
        "local-sqlite" => {
            if [
                "GHINVITE_D1_BRIDGE_URL",
                "GHINVITE_D1_BRIDGE_TOKEN",
                "GHINVITE_D1_AUTHORITY_MANIFEST",
            ]
            .iter()
            .any(|name| std::env::var_os(name).is_some())
            {
                return Err(
                    "D1 configuration requires explicit d1-bridge storage selection".into(),
                );
            }
            let storage = match std::env::var("GHINVITE_DATABASE_PATH") {
                Ok(path) => {
                    let s =
                        ghinvite_storage_sqlx::SqlxStorage::at_path(std::path::Path::new(&path))
                            .await?;
                    s.run_migrations().await?;
                    Arc::new(s)
                }
                Err(_) => {
                    tracing::warn!(
                        "GHINVITE_DATABASE_PATH not set — using in-memory SQLite (state resets on restart)"
                    );
                    Arc::new(ghinvite_storage_sqlx::SqlxStorage::in_memory().await?)
                }
            };

            endpoint_with!(storage)
        }
        _ => return Err("GHINVITE_STORAGE invalid".into()),
    };

    tracing::info!("ghinvite-workflows listening on http://{addr}");
    tracing::info!(
        "register with: curl -X POST http://localhost:9070/restate/v1/deployments -H 'Content-Type: application/json' -d '{{\"uri\":\"http://host.docker.internal:{}\"}}'",
        addr.port()
    );

    HttpServer::new(endpoint).listen_and_serve(addr).await;
    Ok(())
}
