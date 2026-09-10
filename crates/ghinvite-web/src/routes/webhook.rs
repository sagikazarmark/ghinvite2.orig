//! `POST /webhooks/github`: authenticated GitHub deliveries via octoevents.

use crate::commands::github_webhook_dispatcher;
use crate::state::AppState;
use axum::Router;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use chrono::Utc;
use octoevents::{Verifier, WebhookReceiverBuilder, WebhookSecret};

pub fn router() -> Router<AppState> {
    Router::new().route("/webhooks/github", post(handle_webhook))
}

async fn handle_webhook(State(state): State<AppState>, request: Request) -> impl IntoResponse {
    let receive = async move {
        let secret = match WebhookSecret::try_from(state.config.webhook_secret.as_slice()) {
            Ok(secret) => secret,
            Err(error) => {
                tracing::error!(%error, "webhook receiver is not configured");
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
        };
        let dispatcher = github_webhook_dispatcher(state.storage, state.commands, Utc::now());
        WebhookReceiverBuilder::new(Verifier::new(secret))
            // Preserve the previous Axum Bytes extractor's 2 MiB limit.
            .body_limit(2 * 1024 * 1024)
            .build(dispatcher)
            .receive(request)
            .await
            .into_response()
    };
    // Workers poll on one JS thread; Axum still requires a Send future.
    #[cfg(target_arch = "wasm32")]
    let receive = send_wrapper::SendWrapper::new(receive);
    receive.await
}
