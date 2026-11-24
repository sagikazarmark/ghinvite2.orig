//! Native binary: runs the axum app on a local hyper server.
//! On Cloudflare Workers, `ghinvite-web-worker` serves `build_app()` instead.

use ghinvite_web::session_store::ProtectedStore;
use ghinvite_web::{AppState, RestateClient, WebConfig, WebGithub, build_app_with};
use ghinvite_web_server::{SqliteBackend, island_assets};
use std::sync::Arc;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let arguments: Vec<_> = std::env::args().skip(1).collect();
    let mut config = match arguments.as_slice() {
        [] => {
            // An invalid non-Unicode stage is not an absent optional value.
            ghinvite_web::config::apps::AppStage::from_environment()?;
            WebConfig::for_production(|field| std::env::var(field).ok())?
        }
        [flag] if flag == "--local-dev" => WebConfig::for_local_dev()?,
        _ => return Err("usage: ghinvite-web-server [--local-dev]".into()),
    };

    config.island_assets_dir = Some(
        std::env::var("GHINVITE_ISLAND_ASSETS_DIR")
            .unwrap_or_else(|_| "dist/public/assets".into())
            .into(),
    );

    let storage: Arc<ghinvite_storage_sqlx::SqlxStorage> = match std::env::var(
        "GHINVITE_DATABASE_PATH",
    ) {
        Ok(path) => {
            let s =
                ghinvite_storage_sqlx::SqlxStorage::at_path(std::path::Path::new(&path)).await?;
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

    // GitHub: real reqwest, at `config.github`'s bases (GitHub itself in
    // production; `--local-dev` accepts GITHUB_API_BASE, GITHUB_WEB_BASE and
    // GITHUB_AVATAR_BASE, e.g. an octoemu emulator). The installed apps
    // (setup verification, Console authority reads) and avatars go through
    // ghinvite-github, sign-in through octologin (its own client); the apps'
    // installation-token caches live in `github` for the process's life.
    let github = WebGithub::new(
        Arc::new(ghinvite_github::transport::ReqwestTransport::new()?),
        &config.github,
        config.github_apps.clone(),
    )?;

    // Restate client: ingress at config.restate_ingress (defaults to
    // 127.0.0.1:8080 from `docker compose up -d restate`).
    let restate = Arc::new(RestateClient::with_auth(
        &config.restate_ingress,
        config.restate_auth.clone(),
    )?);

    // Session store: a local sqlite database.
    let session_pool = sqlx::SqlitePool::connect("sqlite::memory:").await?;
    let backend = SqliteBackend::new(session_pool);
    backend.migrate().await?;
    let session_store = ProtectedStore::new(backend, config.keyring.clone());

    let email_setting = match std::env::var("P1_EMAIL_KEYRING") {
        Ok(v) => Some(v),
        Err(std::env::VarError::NotPresent) => None,
        Err(_) => return Err("P1_EMAIL_KEYRING invalid".into()),
    };
    let email_enabled = config.bulk_enabled && config.organization_delivery_enabled;
    let email_ring = ghinvite_config::bulk::email_keyring(email_enabled, email_setting)?;
    let email_vault = if email_enabled {
        Some(Arc::new(
            ghinvite_email_vault::EmailVault::new(
                storage.clone(),
                email_ring.ok_or("P1_EMAIL_KEYRING invalid")?,
            )
            .with_review_storage(storage.clone()),
        ))
    } else {
        None
    };
    let digest_storage = storage.clone();
    let watchdog =
        config
            .sweep_watchdog_enabled
            .then(|| ghinvite_web_server::daily_digest::SweepWatchdog {
                restate: restate.clone(),
                organization_enabled: config.organization_delivery_enabled,
            });
    let email_storage = storage.clone();
    let state = AppState::new(storage, github, restate, config)
        .with_authority_cache(Arc::new(
            ghinvite_web::authority_cache::MemoryAuthorityCache::default(),
        ))
        .with_edge_limits(Arc::new(
            ghinvite_web::rate_limit::MemoryRateLimiter::default(),
        ));
    let state = if let Some(vault) = email_vault {
        state.with_bulk_email_vault(vault)
    } else {
        state
    };
    let assets = island_assets(&state.config);
    let app = build_app_with(state, session_store, assets).layer(axum::middleware::from_fn(
        |axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
         mut request: axum::extract::Request,
         next: axum::middleware::Next| async move {
            request
                .extensions_mut()
                .insert(ghinvite_web::rate_limit::TrustedClientIp::new(peer.ip()));
            next.run(request).await
        },
    ));

    let addr = "127.0.0.1:8787".parse::<std::net::SocketAddr>()?;
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("listening on http://{addr}");
    let digest_task = tokio::spawn(ghinvite_web_server::daily_digest::run(
        digest_storage,
        email_storage,
        watchdog,
    ));
    let result = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await;
    digest_task.abort();
    result?;
    Ok(())
}
