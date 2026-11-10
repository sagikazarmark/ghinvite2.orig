//! Link-owned full control snapshots and one-way fanout. Retained admission
//! metadata, never SQL projections, determines the requests and hold windows.
use crate::request_owner::InvitationRequestClient;
use chrono::{DateTime, Utc};
use ghinvite_core::{
    delivery_control::{HoldWindow, LinkControl},
    invitation_link::LinkSnapshot,
    link_lifecycle::{PauseReason, control_operation_id},
    request_control::RequestControl,
    request_lifecycle::InitializeRequest,
};
use restate_sdk::{
    context::{ContextClient, ContextReadState, ObjectContext},
    errors::TerminalError,
    serde::Json,
};

pub(crate) fn snapshot(link: &LinkSnapshot) -> LinkControl {
    let window = link.pause.as_ref().and_then(|pause| match pause.reason {
        PauseReason::AbuseSuspected { window_start } => Some(HoldWindow {
            epoch: pause.epoch,
            window_start,
            paused_at: pause.at,
        }),
        PauseReason::Manual => None,
    });
    if link.revoked_at.is_some() {
        LinkControl::Revoked {
            cancel_pending_invitations: false,
            decline_pending_requests: false,
            abort_hold: window,
        }
    } else if let Some(window) = window {
        LinkControl::AutoPause { window }
    } else if link.pause.is_some() {
        LinkControl::ManualPause
    } else {
        LinkControl::Active
    }
}

fn command(
    link: &LinkSnapshot,
    initialization: &InitializeRequest,
    at: DateTime<Utc>,
    hold_epoch: u64,
) -> RequestControl {
    RequestControl {
        link_id: link.link_id,
        request_id: initialization.request.request_id,
        operation_id: control_operation_id(link.link_id, link.revision),
        revision: link.revision,
        hold_epoch,
        at,
        control: snapshot(link),
        admitted_at: initialization.request.admitted_at,
        invitation_lifetime_seconds: initialization.invitation_lifetime_seconds,
    }
}

/// Initial policy is acknowledged by the request inbox before initialization
/// can dispatch. This call does not wait for any delivery or call back to the link.
pub(crate) async fn initialize(
    ctx: &ObjectContext<'_>,
    link: &LinkSnapshot,
    initialization: &InitializeRequest,
    hold_epoch: u64,
) -> Result<(), TerminalError> {
    let mut input = command(
        link,
        initialization,
        initialization.request.admitted_at,
        hold_epoch,
    );
    apply_options(&mut input, crate::link_revocation::options(ctx).await?);
    ctx.object_client::<InvitationRequestClient>(initialization.request.request_id.to_string())
        .apply_link_control(Json(input))
        .call()
        .await?;
    Ok(())
}

pub(crate) async fn broadcast(
    ctx: &ObjectContext<'_>,
    link: &LinkSnapshot,
    at: DateTime<Utc>,
) -> Result<(), TerminalError> {
    let hold_epoch = ctx.get::<u64>("hold_epoch").await?.unwrap_or(0);
    let options = crate::link_revocation::options(ctx).await?;
    let mut keys = ctx.get_keys().await?;
    keys.sort();
    for key in keys.into_iter().filter(|key| key.starts_with("admission/")) {
        if let Some(Json(initialization)) = ctx.get::<Json<InitializeRequest>>(&key).await? {
            let mut input = command(link, &initialization, at, hold_epoch);
            apply_options(&mut input, options);
            ctx.object_client::<InvitationRequestClient>(
                initialization.request.request_id.to_string(),
            )
            .apply_link_control(Json(input))
            .send()
            .await?;
        }
    }
    Ok(())
}

fn apply_options(input: &mut RequestControl, options: ghinvite_core::revocation::RevokeCascade) {
    if let LinkControl::Revoked {
        cancel_pending_invitations,
        decline_pending_requests,
        ..
    } = &mut input.control
    {
        *cancel_pending_invitations = options.cancel_pending_invitations;
        *decline_pending_requests = options.decline_pending_requests;
    }
}
