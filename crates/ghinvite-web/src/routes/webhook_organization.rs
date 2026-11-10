//! Signed numeric routing hints only; the owner performs fresh GitHub reads.
use crate::{state::AppState, webhook_events::Delivery};
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use ghinvite_core::{
    account::App, org_webhook::OrganizationWebhookSource,
    storage::org_webhooks::OrganizationWebhookEvent,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Id {
    id: u64,
}
#[derive(Deserialize)]
struct Installation {
    id: u64,
    app_id: Option<u64>,
}
#[derive(Deserialize)]
struct Membership {
    user: Id,
    organization: Option<Id>,
}
#[derive(Deserialize)]
struct Wire {
    action: String,
    installation: Installation,
    organization: Id,
    membership: Option<Membership>,
    member: Option<Id>,
    team: Option<Id>,
    scope: Option<String>,
}
fn decode(delivery: &Delivery, app_id: u64) -> Result<Option<OrganizationWebhookEvent>, ()> {
    #[derive(Deserialize)]
    struct Action {
        action: String,
    }
    let action: Action = serde_json::from_slice(&delivery.envelope.raw_payload).map_err(|_| ())?;
    let source = match (&delivery.envelope.meta.kind, action.action.as_str()) {
        (octoevents::EventKind::Organization, "member_added") => {
            OrganizationWebhookSource::OrganizationMemberAdded
        }
        (octoevents::EventKind::Membership, "added") => {
            OrganizationWebhookSource::TeamMembershipAdded
        }
        _ => return Ok(None),
    };
    let wire: Wire = serde_json::from_slice(&delivery.envelope.raw_payload).map_err(|_| ())?;
    if wire.action != action.action || wire.installation.app_id.is_some_and(|id| id != app_id) {
        return Err(());
    }
    let (requester_id, team_id) = match source {
        OrganizationWebhookSource::OrganizationMemberAdded => {
            let membership = wire.membership.ok_or(())?;
            if membership
                .organization
                .is_some_and(|org| org.id != wire.organization.id)
            {
                return Err(());
            }
            (membership.user.id, None)
        }
        OrganizationWebhookSource::TeamMembershipAdded => {
            if wire.scope.as_deref() != Some("team") {
                return Err(());
            }
            (wire.member.ok_or(())?.id, Some(wire.team.ok_or(())?.id))
        }
    };
    let event = OrganizationWebhookEvent {
        app: App::Org,
        account_id: wire.organization.id,
        installation_id: wire.installation.id,
        requester_id,
        team_id,
        source,
        payload_sha256: delivery.payload_sha256,
    };
    // Validate portable positive IDs before touching the routing index.
    ghinvite_core::storage::org_webhooks::encode(&event, chrono::Utc::now(), false)
        .map_err(|_| ())?;
    Ok(Some(event))
}
fn removed(
    delivery: &Delivery,
    app_id: u64,
) -> Result<Option<ghinvite_core::org_eligibility::MemberRemoved>, ()> {
    if delivery.envelope.meta.kind != octoevents::EventKind::Organization {
        return Ok(None);
    }
    #[derive(Deserialize)]
    struct Action {
        action: String,
    }
    let action: Action = serde_json::from_slice(&delivery.envelope.raw_payload).map_err(|_| ())?;
    if action.action != "member_removed" {
        return Ok(None);
    }
    let wire: Wire = serde_json::from_slice(&delivery.envelope.raw_payload).map_err(|_| ())?;
    if wire.installation.app_id.is_some_and(|id| id != app_id) {
        return Err(());
    }
    let membership = wire.membership.ok_or(())?;
    if membership
        .organization
        .is_some_and(|org| org.id != wire.organization.id)
    {
        return Err(());
    }
    let input = ghinvite_core::org_eligibility::MemberRemoved {
        account_id: wire.organization.id,
        installation_id: wire.installation.id,
        removed_user_id: membership.user.id,
        payload_sha256: delivery.payload_sha256,
    };
    input.validate().map_err(|_| ())?;
    Ok(Some(input))
}
pub(super) async fn receive(state: &AppState, delivery: &Delivery, app_id: u64) -> Response {
    match removed(delivery, app_id) {
        Ok(Some(input)) => {
            if !state.config.organization_delivery_enabled {
                return StatusCode::ACCEPTED.into_response();
            }
            return match state.commands.organization_member_removed(input).await {
                Ok(()) => StatusCode::ACCEPTED.into_response(),
                Err(error) => error.into_response(),
            };
        }
        Err(()) => return StatusCode::BAD_REQUEST.into_response(),
        Ok(None) => {}
    }
    let event = match decode(delivery, app_id) {
        Ok(Some(event)) => event,
        Ok(None) => return StatusCode::ACCEPTED.into_response(),
        Err(()) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let wake = match state
        .storage
        .bind_organization_webhook(
            &event,
            chrono::Utc::now(),
            state.config.organization_delivery_enabled,
        )
        .await
    {
        Ok(wake) => wake,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    // Honor disablement even when an earlier deployment retained a match.
    if let Some(wake) = wake
        && state.config.organization_delivery_enabled
        && let Err(error) = state.commands.organization_webhook(wake).await
    {
        return error.into_response();
    }
    StatusCode::ACCEPTED.into_response()
}
