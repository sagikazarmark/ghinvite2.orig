//! Numeric-operator-only rotation controls. No account administrator privilege.
use crate::{
    AppState, WebError,
    continuation_rotation::{self, Failure},
    middleware::csrf::CsrfForm,
    session,
};
use axum::{
    Json, Router,
    extract::{FromRequestParts, State},
    http::{StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
    routing::get,
};
use ghinvite_core::storage::continuation_rotation::Phase;
use serde::Deserialize;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/console/keyring/continuations", get(start).post(advance))
        .layer(axum::middleware::map_response(no_store))
}
async fn no_store(response: Response) -> Response {
    private(response)
}
// Run before the body extractor: unauthorized users learn neither CSRF nor
// progress-token validity and every continuation step rechecks operator status.
struct Operator {
    id: u64,
    csrf: String,
}
impl FromRequestParts<AppState> for Operator {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        let tower = parts
            .extensions
            .get::<tower_sessions::Session>()
            .ok_or_else(|| private(WebError::NotFound))?;
        let s = session::load(tower).await.map_err(|_| {
            private((
                StatusCode::SERVICE_UNAVAILABLE,
                "Operator session unavailable",
            ))
        })?;
        if !s.is_authenticated() || !state.config.operator_ids.contains(&s.user_id) {
            return Err(private(WebError::NotFound));
        }
        Ok(Self {
            id: s.user_id,
            csrf: s.csrf_token.ok_or_else(|| private(WebError::NotFound))?,
        })
    }
}
fn private(value: impl IntoResponse) -> Response {
    let mut r = value.into_response();
    r.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("private, no-store"),
    );
    r
}
async fn start(State(state): State<AppState>, operator: Operator) -> Response {
    private(Json(
        serde_json::json!({"csrf_token":operator.csrf,"keyring_fingerprint":state.config.keyring.fingerprint().to_string(),"current_version":state.config.keyring.current_version(),"phases":["rewrap","verify"],"method":"POST","progress_in_body_only":true,"page_size":ghinvite_core::storage::continuation_rotation::PAGE_SIZE}),
    ))
}
#[derive(Deserialize)]
struct Input {
    phase: Phase,
    #[serde(default)]
    progress: Option<String>,
}
async fn advance(
    State(state): State<AppState>,
    operator: Operator,
    CsrfForm(input): CsrfForm<Input>,
) -> Response {
    let token = input.progress.as_deref().filter(|s| !s.is_empty());
    match continuation_rotation::page(
        state.storage.as_ref(),
        &state.config.keyring,
        operator.id,
        input.phase,
        token,
        chrono::Utc::now().timestamp(),
    )
    .await
    {
        Ok(report) => private(Json(report)),
        Err(Failure::InvalidProgress) => private((
            StatusCode::BAD_REQUEST,
            "Invalid rotation progress. Start a new pass.",
        )),
        Err(Failure::RestartRequired) => private((
            StatusCode::CONFLICT,
            "Continuations changed. Start a new pass; completed rewraps remain safe.",
        )),
        Err(Failure::Unavailable) => private((
            StatusCode::SERVICE_UNAVAILABLE,
            "Continuation rotation unavailable. No completion is established.",
        )),
    }
}
