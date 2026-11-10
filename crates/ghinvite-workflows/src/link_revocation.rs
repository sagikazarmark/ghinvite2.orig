//! Link-owned revocation receipt and distributed capture of a fixed cascade set.
use crate::{admission, request_owner::InvitationRequestClient};
use chrono::{DateTime, Utc};
use ghinvite_core::{
    RequestId, RequestState,
    admission::AdminLinkCommand,
    delivery_control::{DeliveryKind, Identity, LinkControl},
    invitation_link::LinkSnapshot,
    request_lifecycle::InitializeRequest,
    revocation::*,
};
use restate_sdk::{
    context::{ContextClient, ContextReadState, ContextWriteState, ObjectContext},
    errors::TerminalError,
    serde::Json,
};
use serde::{Deserialize, Serialize};

const CURRENT: &str = "revocation/current";
#[derive(Clone, Serialize, Deserialize)]
struct Receipt {
    input: RevokeCommand,
    outcome: RevokeOutcome,
}
#[derive(Clone, Serialize, Deserialize)]
struct Capture {
    requests: Vec<RequestId>,
    deliveries: Vec<Identity>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Work {
    input: RevokeCommand,
    link: LinkSnapshot,
    at: DateTime<Utc>,
    capture: Option<Capture>,
    #[serde(default)]
    access: Vec<ghinvite_core::access_grant::AccessGrantCommand>,
}
fn conflict() -> TerminalError {
    TerminalError::new_with_code(409, "revocation operation conflict")
}
fn key(input: &RevokeCommand) -> String {
    format!(
        "revocation/operation/{}",
        String::from(input.operation_id.clone())
    )
}

pub(crate) async fn receipt(
    ctx: &ObjectContext<'_>,
    input: &RevokeCommand,
) -> Result<Option<RevokeOutcome>, TerminalError> {
    match ctx.get::<Json<Receipt>>(&key(input)).await? {
        Some(Json(old)) if old.input == *input => Ok(Some(old.outcome)),
        Some(_) => Err(conflict()),
        None => Ok(None),
    }
}
fn record(ctx: &ObjectContext<'_>, input: RevokeCommand, outcome: RevokeOutcome) -> RevokeOutcome {
    ctx.set(&key(&input), Json(Receipt { input, outcome }));
    outcome
}
pub(crate) async fn options(ctx: &ObjectContext<'_>) -> Result<RevokeCascade, TerminalError> {
    Ok(ctx
        .get::<Json<Work>>(CURRENT)
        .await?
        .map(|j| j.0.input.cascade)
        .unwrap_or_default())
}

pub(crate) async fn execute(
    ctx: &ObjectContext<'_>,
    input: RevokeCommand,
    mut link: LinkSnapshot,
    at: DateTime<Utc>,
    cancellation_enabled: bool,
    access_enabled: bool,
) -> Result<RevokeOutcome, TerminalError> {
    if let Some(outcome) = receipt(ctx, &input).await? {
        return Ok(outcome);
    }
    let mut work = if let Some(Json(work)) = ctx.get::<Json<Work>>(CURRENT).await? {
        if work.input != input {
            if work.input.operation_id == input.operation_id {
                return Err(conflict());
            }
            return Ok(record(ctx, input, Err(RevokeRefusal::Revoked)));
        }
        work
    } else {
        if link.revoked_at.is_some() {
            return Ok(record(ctx, input, Err(RevokeRefusal::Revoked)));
        }
        if (input.cascade.remove_granted_access && !access_enabled)
            || (input.cascade.cancel_pending_invitations && !cancellation_enabled)
        {
            return Ok(record(ctx, input, Err(RevokeRefusal::UnsupportedCascade)));
        }
        let access = if input.cascade.remove_granted_access {
            crate::link_access_removal::retained(ctx, &input)
                .await?
                .grants
        } else {
            Vec::new()
        };
        link.revoked_at = Some(at);
        link.revoked_by = Some(input.admin.user_id);
        link.revision = link
            .revision
            .checked_add(1)
            .filter(|r| *r <= ghinvite_core::storage::integer::MAX_EXACT)
            .ok_or_else(conflict)?;
        let work = Work {
            input: input.clone(),
            link: link.clone(),
            at,
            capture: None,
            access,
        };
        // Persist the bound work before closing the link. A retry after a failed
        // manifest read resumes this exact command, rather than inferring success.
        ctx.set(CURRENT, Json(work.clone()));
        ctx.set("link", Json(link.clone()));
        admission::project(
            ctx,
            &link,
            vec![admission::event(
                ghinvite_core::audit::EventType::InvitationLinkRevoked,
                link.link_id,
                link.revoked_by,
                at,
            )],
        )
        .await?;
        work
    };
    if work.capture.is_none() {
        let mut capture = Capture {
            requests: vec![],
            deliveries: vec![],
        };
        let hold = match crate::link_control::snapshot(&work.link) {
            LinkControl::Revoked { abort_hold, .. } => abort_hold,
            _ => None,
        };
        let mut keys = ctx.get_keys().await?;
        keys.sort();
        for entry in keys.into_iter().filter(|k| k.starts_with("admission/")) {
            let Some(Json(initialized)) = ctx.get::<Json<InitializeRequest>>(&entry).await? else {
                continue;
            };
            let request_id = initialized.request.request_id;
            if input.cascade.decline_pending_requests {
                capture.requests.push(request_id);
            }
            let held = hold.as_ref().is_some_and(|w| {
                initialized.request.admitted_at >= w.window_start
                    && initialized.request.admitted_at <= w.paused_at
            });
            if input.cascade.cancel_pending_invitations || held {
                let Json(manifest) = ctx
                    .object_client::<InvitationRequestClient>(request_id.to_string())
                    .cascade_manifest(Json(RequestCascadeQuery {
                        link_id: link.link_id,
                        request_id,
                    }))
                    .call()
                    .await?;
                if manifest.request_id != request_id
                    || manifest
                        .deliveries
                        .iter()
                        .any(|d| d.link_id != Some(link.link_id) || d.request_id != request_id)
                {
                    return Err(conflict());
                }
                capture.deliveries.extend(manifest.deliveries);
            }
        }
        work.capture = Some(capture);
        ctx.set(CURRENT, Json(work.clone()));
    }
    let captured = work.capture.as_ref().ok_or_else(conflict)?;
    let accepted = RevokeAccepted {
        addressed_access_grants: work.access.len().try_into().map_err(|_| conflict())?,
        addressed_requests: captured.requests.len().try_into().map_err(|_| conflict())?,
        addressed_deliveries: captured
            .deliveries
            .len()
            .try_into()
            .map_err(|_| conflict())?,
    };
    crate::link_control::broadcast(ctx, &work.link, work.at).await?;
    for grant in &work.access {
        ctx.object_client::<crate::access_grant::AccessGrantClient>(grant.key())
            .remove_now(Json(ghinvite_core::access_grant::RemovalCommand {
                grant: grant.clone(),
                reason: ghinvite_core::access_grant::RemovalReason::LinkRevoked,
            }))
            .send()
            .await?;
    }
    ctx.object_client::<crate::projection::InvitationProjectionClient>(ctx.key())
        .apply_revocation(Json(RevocationProjection {
            command: input.clone(),
            accepted,
            revision: work.link.revision,
            at: work.at,
        }))
        .send()
        .await?;
    Ok(record(ctx, input, Ok(accepted)))
}

pub(crate) async fn progress(
    ctx: &ObjectContext<'_>,
    _input: &AdminLinkCommand,
) -> Result<Option<CascadeProgress>, TerminalError> {
    let Some(Json(work)) = ctx.get::<Json<Work>>(CURRENT).await? else {
        return Ok(None);
    };
    let Some(capture) = work.capture else {
        return Err(TerminalError::new_with_code(
            503,
            "cascade capture incomplete",
        ));
    };
    let mut result = CascadeProgress {
        addressed_access_grants: work.access.len().try_into().map_err(|_| conflict())?,
        done_access_grants: 0,
        addressed_requests: capture.requests.len().try_into().map_err(|_| conflict())?,
        addressed_deliveries: capture
            .deliveries
            .len()
            .try_into()
            .map_err(|_| conflict())?,
        done_requests: 0,
        done_deliveries: 0,
        waiting_on_github: 0,
        unavailable: 0,
    };
    for grant in &work.access {
        match ctx
            .object_client::<crate::access_grant::AccessGrantClient>(grant.key())
            .removal_progress(Json(grant.clone()))
            .call()
            .await
        {
            Ok(Json(CascadeEntryState::Done)) => result.done_access_grants += 1,
            Ok(Json(CascadeEntryState::WaitingOnGithub)) => result.waiting_on_github += 1,
            Ok(Json(CascadeEntryState::Pending)) => {}
            Err(_) => result.unavailable += 1,
        }
    }
    for request_id in capture.requests {
        match ctx
            .object_client::<InvitationRequestClient>(request_id.to_string())
            .cascade_manifest(Json(RequestCascadeQuery {
                link_id: work.input.link_id,
                request_id,
            }))
            .call()
            .await
        {
            Ok(Json(view)) if view.request_id == request_id => {
                if view.observed_control_revision >= work.link.revision
                    && view.state != RequestState::Pending
                {
                    result.done_requests += 1;
                }
            }
            _ => result.unavailable += 1,
        }
    }
    for identity in capture.deliveries {
        let delivery_key = ghinvite_core::delivery::delivery_key(
            identity.request_id,
            identity.target_id,
            identity.delivery_generation,
        );
        let kind = identity.kind;
        let query = Json(DeliveryCascadeQuery {
            identity,
            required_control_revision: work.link.revision,
        });
        let progress = match kind {
            DeliveryKind::Repository => {
                ctx.object_client::<crate::delivery::RepositoryDeliveryClient>(delivery_key)
                    .cascade_progress(query)
                    .call()
                    .await
            }
            DeliveryKind::Organization => {
                ctx.object_client::<crate::org_delivery::OrganizationDeliveryClient>(delivery_key)
                    .cascade_progress(query)
                    .call()
                    .await
            }
        };
        match progress {
            Ok(Json(view)) if view.observed_control_revision >= work.link.revision => {
                match view.state {
                    CascadeEntryState::Done => result.done_deliveries += 1,
                    CascadeEntryState::WaitingOnGithub => result.waiting_on_github += 1,
                    CascadeEntryState::Pending => {}
                }
            }
            Ok(_) => {}
            Err(_) => result.unavailable += 1,
        }
    }
    Ok(Some(result))
}
