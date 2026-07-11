//! `POST /webhooks/github` — HMAC-verified webhook receiver. Plan 6.

use crate::commands::dispatch_github_webhook;
use crate::state::AppState;
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use chrono::Utc;

pub fn router() -> Router<AppState> {
    Router::new().route("/webhooks/github", post(handle_webhook))
}

async fn handle_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // HMAC verification must happen on raw bytes before parsing.
    let signature = match headers
        .get("x-hub-signature-256")
        .and_then(|v| v.to_str().ok())
    {
        Some(s) => s.to_owned(),
        None => {
            tracing::warn!("webhook: missing X-Hub-Signature-256 header");
            return StatusCode::UNAUTHORIZED.into_response();
        }
    };

    if !ghinvite_github::hmac::verify_signature_256(&state.config.webhook_secret, &body, &signature)
    {
        tracing::warn!("webhook: HMAC verification failed");
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Parse after HMAC verification.
    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = ?e, "webhook: body is not valid JSON");
            return StatusCode::OK.into_response(); // don't ask GitHub to retry malformed payloads
        }
    };

    let event_type = match headers.get("x-github-event").and_then(|v| v.to_str().ok()) {
        Some(e) => e.to_owned(),
        None => {
            tracing::warn!("webhook: missing X-GitHub-Event header");
            return StatusCode::OK.into_response();
        }
    };

    let delivery_id = headers
        .get("x-github-delivery")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    let result = dispatch_github_webhook(
        state.storage.as_ref(),
        state.commands.as_ref(),
        event_type.as_str(),
        &payload,
        Utc::now(),
    )
    .await;

    match result {
        Ok(_) => StatusCode::OK.into_response(),
        Err(e) => {
            tracing::warn!(error = %e, event = %event_type, delivery = %delivery_id, "webhook: routing to Restate failed");
            // Return 5xx so GitHub retries (72h retry window). Don't return 200 on failure.
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
