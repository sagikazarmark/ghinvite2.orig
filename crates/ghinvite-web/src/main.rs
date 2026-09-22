//! Native binary: runs the axum app on a local hyper server.
//! On Cloudflare Workers, `ghinvite-web-worker` serves `build_app()` instead.

use ghinvite_web::session_store::{ProtectedStore, SqliteBackend};
use ghinvite_web::{AppState, RestateClient, WebConfig, build_app};
use std::sync::Arc;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let config = WebConfig::for_local_dev()?;

    let storage: Arc<dyn ghinvite_web::WebStorage> = match std::env::var("GHINVITE_DATABASE_PATH") {
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

    // GitHub transport: real reqwest. Local-dev OAuth only works if you
    // configure GHINVITE_GITHUB_CLIENT_ID / SECRET to point at a real GitHub
    // App; otherwise /login redirects to a 404.
    let transport: Arc<dyn ghinvite_github::HttpTransport> =
        Arc::new(ghinvite_github::transport::ReqwestTransport::new()?);

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
    let session_store = ProtectedStore::new(backend, config.session_secret);

    let state = AppState::new(storage, transport, restate, config);
    let app = build_app(state, session_store);

    let addr = "127.0.0.1:8787".parse::<std::net::SocketAddr>()?;
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
