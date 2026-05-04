//! `/i/{slug}...` routes. **Plan 4: 501 placeholders.**
//! Plan 6 fills in the recipient flow.

use crate::state::AppState;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/i/{slug}", get(stub))
        .route("/i/{slug}/request", post(stub))
        .route("/i/{slug}/pending/{request_id}", get(stub))
}

async fn stub() -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        "Plan 6 will fill in the recipient routes.",
    )
}
