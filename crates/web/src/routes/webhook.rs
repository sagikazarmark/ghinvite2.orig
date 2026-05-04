//! `POST /webhooks/github`. **Plan 4: 501 placeholder.**
//! Plan 6 fills in the HMAC-verified webhook receiver.

use crate::state::AppState;
use axum::Router;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;

pub fn router() -> Router<AppState> {
    Router::new().route("/webhooks/github", post(stub))
}

async fn stub() -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        "Plan 6 will fill in the GitHub webhook receiver.",
    )
}
