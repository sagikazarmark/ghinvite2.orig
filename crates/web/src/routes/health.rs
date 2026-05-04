//! `GET /health` — uptime check.

use crate::state::AppState;
use axum::routing::get;
use axum::Router;

pub fn router() -> Router<AppState> {
    Router::new().route("/health", get(health))
}

async fn health() -> &'static str {
    "ok"
}
