//! Native binary: runs the axum app on a local hyper server.
//! Plan 7 wraps `web::build_app()` in a Workers `#[event(fetch)]` instead.

use std::sync::Arc;
use tokio::net::TcpListener;
use tower_sessions_sqlx_store::SqliteStore;
use web::{AppState, RestateClient, WebConfig, build_app};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let config = WebConfig::for_local_dev();

    let storage: Arc<dyn storage::Storage> = match std::env::var("GHINVITE_DATABASE_PATH") {
        Ok(path) => {
            let s = storage::SqlxStorage::at_path(std::path::Path::new(&path)).await?;
            s.run_migrations().await?;
            Arc::new(s)
        }
        Err(_) => {
            tracing::warn!("GHINVITE_DATABASE_PATH not set — using in-memory SQLite (state resets on restart)");
            Arc::new(storage::SqlxStorage::in_memory().await?)
        }
    };

    // GitHub transport: real reqwest. Local-dev OAuth only works if you
    // configure GHINVITE_GITHUB_CLIENT_ID / SECRET to point at a real GitHub
    // App; otherwise /login redirects to a 404.
    let transport: Arc<dyn github::HttpTransport> =
        Arc::new(github::transport::ReqwestTransport::new()?);

    // Restate client: ingress at config.restate_ingress (defaults to
    // 127.0.0.1:8080 from `docker compose up -d restate`).
    let restate = Arc::new(RestateClient::new(&config.restate_ingress)?);

    // Session store: a local sqlite database.
    let session_pool =
        tower_sessions_sqlx_store::sqlx::SqlitePool::connect("sqlite::memory:").await?;
    let session_store = SqliteStore::new(session_pool);
    session_store.migrate().await?;

    let state = AppState::new(storage, transport, restate, config);
    let app = build_app(state, session_store);

    let addr = "127.0.0.1:8787".parse::<std::net::SocketAddr>()?;
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
