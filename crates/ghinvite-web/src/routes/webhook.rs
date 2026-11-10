//! Per-app HMAC verification; unsigned delivery headers never establish identity.
use crate::{
    installed_apps,
    state::AppState,
    webhook_events::{
        self, Delivery, InstallationAction, InstallationEvent, InstallationRepositoriesEvent,
        Rejection,
    },
};
use axum::{
    Router,
    body::to_bytes,
    extract::{Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use ghinvite_core::account::{
    App, InstallationRef, InstallationWebhookAction, InstallationWebhookInput,
};
use octoevents::EventKind;
use serde::Deserialize;
#[path = "webhook_organization.rs"]
mod organization;

/// The largest body read; a larger one is answered 413.
const BODY_LIMIT: usize = 2 * 1024 * 1024;

pub fn router() -> Router<AppState> {
    Router::new().route("/webhooks/{app}", post(handle_webhook))
}

/// The status each authentication failure is answered with (unchanged since
/// gh-toolkit's `WebhookRouter`).
fn status(rejection: Rejection) -> StatusCode {
    match rejection {
        Rejection::UnknownApp => StatusCode::NOT_FOUND,
        Rejection::MissingSignature | Rejection::BadSignature | Rejection::MissingHeader => {
            StatusCode::UNAUTHORIZED
        }
        Rejection::Envelope => StatusCode::BAD_REQUEST,
    }
}

async fn handle_webhook(
    State(state): State<AppState>,
    Path(name): Path<String>,
    request: Request,
) -> Response {
    let receive = async move {
        let Some(app) = installed_apps::app(&name) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let Some(webhooks) = &state.config.webhooks else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        let Some(configured) = webhooks.get(installed_apps::slot(app)) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let app_id = configured.app_id;
        if !request.headers().contains_key("x-hub-signature-256") {
            webhook_events::signature_failure(app.as_str(), "missing");
            return StatusCode::UNAUTHORIZED.into_response();
        }
        if ["x-github-event", "x-github-delivery"]
            .iter()
            .any(|name| !request.headers().contains_key(*name))
            || request
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.split(';').next().unwrap_or("").trim())
                != Some("application/json")
        {
            return StatusCode::BAD_REQUEST.into_response();
        }
        let (parts, body) = request.into_parts();
        let Ok(body) = to_bytes(body, BODY_LIMIT).await else {
            return StatusCode::PAYLOAD_TOO_LARGE.into_response();
        };
        let delivery = match webhook_events::authenticate(
            app.as_str(),
            Some(&configured.secrets),
            &parts.headers,
            body,
        ) {
            Ok(delivery) => delivery,
            Err(rejection) => return status(rejection).into_response(),
        };
        if app == App::Org
            && matches!(
                delivery.envelope.meta.kind,
                EventKind::Organization | EventKind::Membership
            )
        {
            return organization::receive(&state, &delivery, app_id).await;
        }
        // Repo access settles through reconciliation. Neither the obsolete member
        // event nor repository_invitation headers can dispatch a Delivery command.
        let kind = &delivery.envelope.meta.kind;
        if *kind != EventKind::Installation && *kind != EventKind::InstallationRepositories {
            return StatusCode::ACCEPTED.into_response();
        }
        installation(&state, app, app_id, &delivery).await
    };
    #[cfg(target_arch = "wasm32")]
    let receive = send_wrapper::SendWrapper::new(receive);
    receive.await
}

/// An `installation` or `installation_repositories` delivery.
async fn installation(state: &AppState, app: App, app_id: u64, delivery: &Delivery) -> Response {
    // The installation as `installed_apps::facts` takes it.
    type Installation = ghinvite_github::payloads::GhAppInstallation;
    #[derive(Deserialize)]
    struct Wire {
        installation: Installation,
    }
    #[derive(Deserialize)]
    struct Action {
        action: String,
    }
    let kind = &delivery.envelope.meta.kind;
    let payload = &delivery.envelope.raw_payload;
    let Ok(action) = serde_json::from_slice::<Action>(payload) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let known = if *kind == EventKind::Installation {
        matches!(
            action.action.as_str(),
            "created" | "deleted" | "suspend" | "unsuspend" | "new_permissions_accepted"
        )
    } else {
        matches!(action.action.as_str(), "added" | "removed")
    };
    if !known {
        return StatusCode::ACCEPTED.into_response();
    }
    let Ok(wire) = serde_json::from_slice::<Wire>(payload) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let updated_at = wire.installation.updated_at;
    let verified = match installed_apps::facts(app, app_id, wire.installation.id, wire.installation)
    {
        Ok(facts) => facts,
        Err(status) => return status.into_response(),
    };
    let result = if *kind == EventKind::Installation {
        let Ok(event) = InstallationEvent::decode(delivery) else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        let action = match event.action {
            InstallationAction::Created => InstallationWebhookAction::Created,
            InstallationAction::Deleted => InstallationWebhookAction::Deleted,
            InstallationAction::Suspend => InstallationWebhookAction::Suspend,
            InstallationAction::Unsuspend => InstallationWebhookAction::Unsuspend,
            InstallationAction::NewPermissionsAccepted => {
                InstallationWebhookAction::NewPermissionsAccepted
            }
        };
        state
            .commands
            .installation_webhook(InstallationWebhookInput {
                // Payload is authenticated; X-GitHub-Delivery is only a hint.
                delivery_id: hex::encode(delivery.payload_sha256),
                action,
                updated_at,
                verified,
                sender_id: event.sender.id,
                sender_login: event.sender.login,
            })
            .await
    } else {
        if InstallationRepositoriesEvent::decode(delivery).is_err() {
            return StatusCode::BAD_REQUEST.into_response();
        }
        state
            .commands
            .refresh_installation(InstallationRef {
                app,
                installation_id: verified.account.installation_id,
            })
            .await
    };
    match result {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(error) => error.into_response(),
    }
}
