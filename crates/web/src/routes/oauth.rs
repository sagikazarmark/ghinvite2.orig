use axum::Router;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new() // populated by Tasks 8–13 + 19–22 + 23–25
}
