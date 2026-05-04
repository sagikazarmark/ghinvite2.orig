//! `GET /health` — uptime check.

use crate::state::AppState;
use axum::Router;
use axum::routing::get;

pub fn router() -> Router<AppState> {
    Router::new().route("/health", get(health))
}

async fn health() -> &'static str {
    "ok"
}
