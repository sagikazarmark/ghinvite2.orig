//! `/accounts/{login}/...` routes. **Plan 4: 501 placeholders.**
//! Plan 5 fills in the dashboard, link CRUD, approval queue, and settings.

use crate::state::AppState;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/accounts/{login}", get(stub))
        .route("/accounts/{login}/links/new", get(stub))
        .route("/accounts/{login}/links/{link_id}", get(stub))
        .route("/accounts/{login}/requests", get(stub))
        .route("/accounts/{login}/audit", get(stub))
        .route("/accounts/{login}/settings", get(stub))
}

async fn stub() -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        "Plan 5 will fill in the dashboard routes.",
    )
}
