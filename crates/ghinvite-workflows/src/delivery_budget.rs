//! S3/S5 budget calls belong to the delivery owner, outside external-I/O runs.
use crate::{
    invite_budget::InviteBudgetClient,
    repository_protocol::{BudgetEffect, BudgetEvidence, Plan},
};
use ghinvite_core::{
    budget::{BudgetKind, RefundInvitationAfterPut204, Reservation, Reserve, SlotId},
    delivery::CreateCommand,
};
use restate_sdk::{
    context::{
        ContextClient, ContextReadState, ContextSideEffects, ContextWriteState, ObjectContext,
        RunFuture,
    },
    errors::{HandlerError, TerminalError},
    serde::Json,
};

const REQUEST: &str = "create_budget_request";
const SLOT: &str = "create_budget_slot";
const EVIDENCE: &str = "create_budget_evidence";

/// Bind before retaining evidence. Recovery sends the original identifier;
/// another actual HTTP observation gets a new invocation/phase identity.
pub(crate) fn bind_limit(
    ctx: &ObjectContext<'_>,
    phase: &str,
    limit: &mut ghinvite_core::budget::RecordLimit,
) {
    use sha2::{Digest, Sha256};
    if let Some(observation) = &mut limit.observation
        && observation.id.is_none()
    {
        let digest = Sha256::digest(format!(
            "ghinvite/budget-limit/v1/{}/{}/{phase}",
            ctx.key(),
            ctx.invocation_id()
        ));
        observation.id = Some(digest.iter().map(|byte| format!("{byte:02x}")).collect());
    }
}

pub(crate) async fn reserve(
    ctx: &ObjectContext<'_>,
    command: &CreateCommand,
    plan: &Plan,
) -> Result<Option<Reservation>, TerminalError> {
    // The immutable approval manifest supplies policy even while SQL projection lags.
    // Register before the first charge; replay cannot change another link or the cap.
    if let Some(link_id) = command.budget_link_id() {
        ctx.object_client::<InviteBudgetClient>(format!("repo:{}", command.account_id))
            .set_link_share(Json(ghinvite_core::budget::LinkShare {
                link_id,
                percent: command.budget_share_pct,
            }))
            .call()
            .await?;
    }
    let kind = match plan {
        Plan::Put => BudgetKind::RepoInvite {
            repo_id: command.repo_id,
        },
        Plan::Patch { .. } => BudgetKind::OtherWrite,
        Plan::Complete(_) => {
            return Err(TerminalError::new("completed delivery cannot reserve"));
        }
    };

    let input = match ctx.get::<Json<Reserve>>(REQUEST).await? {
        Some(input) => input.0,
        None => {
            let ordinal = ctx
                .get::<u64>("create_budget_ordinal")
                .await?
                .unwrap_or(0)
                .checked_add(1)
                .filter(|n| *n <= ghinvite_core::storage::integer::MAX_EXACT)
                .ok_or_else(|| TerminalError::new("budget attempt ordinal exhausted"))?;
            let input = Reserve {
                request_key: format!("create:{}:{ordinal}", command.delivery_key()),
                kind: kind.clone(),
                link_id: command.budget_link_id(),
            };
            // Retain the input before the cross-object call. A cancelled caller
            // can recover a granted slot without allocating another ordinal.
            ctx.set("create_budget_ordinal", ordinal);
            ctx.set(REQUEST, Json(input.clone()));
            input
        }
    };
    // An uncertain retained PATCH reservation cannot authorize a new PUT.
    if input.kind != kind {
        return Ok(None);
    }
    if let Some(slot) = ctx.get::<Json<SlotId>>(SLOT).await? {
        return Ok(Some(Reservation::Granted { slot_id: slot.0 }));
    }
    let Json(result) = ctx
        .object_client::<InviteBudgetClient>(format!("repo:{}", command.account_id))
        .reserve(Json(input))
        .call()
        .await?;
    match result {
        Reservation::Granted { slot_id } => ctx.set(SLOT, Json(slot_id)),
        Reservation::RetryAt { .. } => ctx.clear(REQUEST),
    }
    Ok(Some(result))
}

pub(crate) async fn finish(
    ctx: &ObjectContext<'_>,
    command: &CreateCommand,
    evidence: &BudgetEvidence,
) -> Result<(), TerminalError> {
    if evidence.effect == BudgetEffect::Retain && evidence.limit.is_none() {
        return Ok(());
    }
    // Retain observed evidence before the first cross-object call. Cancellation
    // of this caller cannot discard a rate closure or a direct PUT204 refund.
    let mut evidence = evidence.clone();
    if let Some(limit) = &mut evidence.limit {
        bind_limit(ctx, "repository-create", limit);
    }
    ctx.set(EVIDENCE, Json(evidence.clone()));
    let client = ctx.object_client::<InviteBudgetClient>(format!("repo:{}", command.account_id));
    if let Some(limit) = &evidence.limit {
        client.record_limit(Json(limit.clone())).call().await?;
    }
    let Some(Json(slot_id)) = ctx.get::<Json<SlotId>>(SLOT).await? else {
        ctx.clear(EVIDENCE);
        return Ok(());
    };
    match evidence.effect {
        BudgetEffect::Retain => {
            ctx.clear(EVIDENCE);
            return Ok(());
        }
        BudgetEffect::ReleaseUnused => client.release(Json(slot_id)).call().await?,
        BudgetEffect::RefundInvitation => {
            let Json(reservation) = ctx
                .get::<Json<Reserve>>(REQUEST)
                .await?
                .ok_or_else(|| TerminalError::new("missing budget reservation input"))?;
            client
                .refund_invitation_after_put204(Json(RefundInvitationAfterPut204 {
                    slot_id,
                    reservation,
                }))
                .call()
                .await?;
        }
    }
    ctx.clear(SLOT);
    ctx.clear(REQUEST);
    ctx.clear(EVIDENCE);
    Ok(())
}

/// Recover interrupted S3 before S0/S1 can change the plan or abort. Both the
/// durable marker and the actual fence must prove no write could have occurred.
pub(crate) async fn recover_unused(
    ctx: &ObjectContext<'_>,
    storage: &dyn ghinvite_core::storage::DeliveryStorage,
    command: &CreateCommand,
) -> Result<(), TerminalError> {
    if let Some(Json(evidence)) = ctx.get::<Json<BudgetEvidence>>(EVIDENCE).await? {
        finish(ctx, command, &evidence).await?;
    }
    let Some(Json(input)) = ctx.get::<Json<Reserve>>(REQUEST).await? else {
        return Ok(());
    };
    if ctx
        .get::<bool>(crate::delivery_control::MAY_HAVE_WRITTEN)
        .await?
        .unwrap_or(false)
    {
        return Ok(());
    }
    let unused = ctx
        .run(|| async {
            Ok::<_, HandlerError>(matches!(
                storage.delivery_attempt_exists(command).await,
                Ok(false)
            ))
        })
        .name("recover_unused_create_budget")
        .await?;
    if !unused {
        return Ok(());
    }
    let client = ctx.object_client::<InviteBudgetClient>(format!("repo:{}", command.account_id));
    let slot = match ctx.get::<Json<SlotId>>(SLOT).await? {
        Some(Json(slot)) => Some(slot),
        None => match client.reserve(Json(input)).call().await?.0 {
            Reservation::Granted { slot_id } => Some(slot_id),
            Reservation::RetryAt { .. } => None,
        },
    };
    if let Some(slot) = slot {
        client.release(Json(slot)).call().await?;
    }
    ctx.clear(SLOT);
    ctx.clear(REQUEST);
    Ok(())
}
