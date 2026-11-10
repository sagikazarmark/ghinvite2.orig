//! Old-generation reissue barrier. This never rewrites the terminal settlement.
use crate::{
    AppState,
    cancellation_protocol::{self as protocol, Observation, WriteOutcome},
};
use ghinvite_core::{
    InvitationState,
    budget::{BudgetKind, Reservation, Reserve, SlotId},
    delivery::{CreateOutcome, SettlementReason},
    reissue::{InvitationBarrier as Barrier, RepositoryReissueBarrierQuery as Query},
    storage::delivery_attempts::CancelCommand,
};
use restate_sdk::{
    context::{
        ContextClient, ContextReadState, ContextSideEffects, ContextWriteState, ObjectContext,
        RunFuture,
    },
    errors::{HandlerError, TerminalError},
    serde::Json,
};
use serde::{Deserialize, Serialize};
#[derive(Clone, Serialize, Deserialize)]
struct Completion {
    input: CancelCommand,
    slot: SlotId,
    result: protocol::WriteResult,
}
fn conflict() -> TerminalError {
    TerminalError::new_with_code(409, "reissue barrier identity conflict")
}
pub(crate) async fn prepare(
    ctx: &ObjectContext<'_>,
    app: &AppState,
    input: Query,
    #[cfg(feature = "integration")] faults: Option<&crate::delivery::DeliveryFaults>,
) -> Result<Barrier, TerminalError> {
    crate::delivery_cancellation::drain_completion(ctx, app).await?;
    flush_limit(ctx).await?;
    if ctx.key() != input.command.delivery_key() {
        return Err(conflict());
    }
    let key = format!(
        "reissue_barrier/{}",
        String::from(input.operation_id.clone())
    );
    if let Some(Json(old)) = ctx.get::<Json<Query>>(&key).await? {
        if old != input {
            return Err(conflict());
        }
    } else {
        ctx.set(&key, Json(input.clone()));
    }
    let Some(snapshot) = crate::repository_audit::snapshot(ctx).await? else {
        return Ok(Barrier::Pending);
    };
    if snapshot.create.command != input.command {
        return Err(conflict());
    }
    if !snapshot.settlement.as_ref().is_some_and(|s| {
        matches!(
            s.state,
            InvitationState::Expired | InvitationState::Declined | InvitationState::Cancelled
        )
    }) {
        return Ok(Barrier::Pending);
    }
    if ctx
        .get::<bool>("reissue_barrier_clear")
        .await?
        .unwrap_or(false)
    {
        return Ok(Barrier::Clear);
    }
    if let Some(Json(completion)) = ctx
        .get::<Json<Completion>>("reissue_cancel_completion")
        .await?
    {
        return finish(ctx, app, completion).await;
    }
    if ctx
        .get::<bool>("cancellation_unknown")
        .await?
        .unwrap_or(false)
        && snapshot
            .settlement
            .as_ref()
            .is_none_or(|s| s.state != InvitationState::Cancelled)
    {
        return Ok(Barrier::MayHaveWritten);
    }
    let (upstream_id, owned) = match snapshot.create.outcome {
        CreateOutcome::Created { upstream_id } => (upstream_id, true),
        CreateOutcome::AlreadyPending { upstream_id, .. } => (upstream_id, false),
        _ => return Ok(Barrier::Pending),
    };
    let Some(account) = ctx
        .object_client::<crate::availability::AccountInstallationClient>(format!(
            "repo:{}",
            input.command.account_id
        ))
        .current_installation()
        .call()
        .await?
        .0
    else {
        return Ok(Barrier::Pending);
    };
    let Some(apps) = app.github_apps.as_ref() else {
        return Ok(Barrier::Pending);
    };
    let cancel = CancelCommand {
        create: input.command,
        upstream_id,
        reason: SettlementReason::AdminReissue,
    };
    if let Some(Json(reserve)) = ctx
        .get::<Json<Reserve>>("reissue_reservation_started")
        .await?
    {
        let fenced = ctx
            .run(|| async {
                Ok::<_, HandlerError>(
                    app.storage
                        .cancellation_attempt_exists(&cancel)
                        .await
                        .unwrap_or(true),
                )
            })
            .name("reissue_unused_fence")
            .await?;
        if fenced {
            return Ok(Barrier::MayHaveWritten);
        }
        let budget = ctx.object_client::<crate::invite_budget::InviteBudgetClient>(format!(
            "repo:{}",
            cancel.create.account_id
        ));
        if let Reservation::Granted { slot_id } = budget.reserve(Json(reserve)).call().await?.0 {
            budget.release(Json(slot_id)).call().await?;
        }
        ctx.clear("reissue_reservation_started");
        ctx.clear("reissue_cancel_uncertain");
        let n = ctx.get::<u64>("reissue_cancel_ordinal").await?.unwrap_or(1);
        ctx.set(
            "reissue_cancel_ordinal",
            n.checked_add(1).ok_or_else(conflict)?,
        );
    }
    if ctx
        .get::<bool>("reissue_cancel_uncertain")
        .await?
        .unwrap_or(false)
    {
        return Ok(Barrier::MayHaveWritten);
    }
    let observed = ctx
        .run(|| async {
            Ok::<_, HandlerError>(Json(protocol::observe(apps, &cancel, &account).await))
        })
        .name("reissue_invitation_read")
        .await?
        .0;
    match observed {
        Observation::Gone | Observation::Accepted { .. } => {
            ctx.set("reissue_barrier_clear", true);
            return Ok(Barrier::Clear);
        }
        Observation::Limited { retry_after } => {
            if app.invite_budget.is_some() {
                let at = crate::clock::now(ctx, "reissue_limit_time").await?;
                let mut limit = protocol::content_limit(at, retry_after);
                crate::delivery_budget::bind_limit(ctx, "repository-reissue-read", &mut limit);
                ctx.set(
                    "reissue_read_limit",
                    Json((cancel.create.account_id, limit)),
                );
                flush_limit(ctx).await?;
            }
            return Ok(Barrier::Pending);
        }
        Observation::Unavailable => return Ok(Barrier::Pending),
        Observation::Pending { .. } => {}
    }
    if !owned || !app.cancellation_enabled || app.invite_budget.is_none() {
        return Ok(Barrier::Pending);
    }
    let fenced = ctx
        .run(|| async {
            Ok::<_, HandlerError>(
                app.storage
                    .cancellation_attempt_exists(&cancel)
                    .await
                    .unwrap_or(true),
            )
        })
        .name("reissue_cancel_fence")
        .await?;
    if fenced {
        return Ok(Barrier::MayHaveWritten);
    }
    let ordinal = ctx.get::<u64>("reissue_cancel_ordinal").await?.unwrap_or(1);
    let budget = ctx.object_client::<crate::invite_budget::InviteBudgetClient>(format!(
        "repo:{}",
        cancel.create.account_id
    ));
    let reserve = Reserve {
        request_key: format!("reissue-cancel:{}:{ordinal}", ctx.key()),
        kind: BudgetKind::OtherWrite,
        link_id: None,
    };
    ctx.set("reissue_reservation_started", Json(reserve.clone()));
    let slot = match budget.reserve(Json(reserve)).call().await?.0 {
        Reservation::Granted { slot_id } => slot_id,
        Reservation::RetryAt { .. } => {
            ctx.clear("reissue_reservation_started");
            ctx.set(
                "reissue_cancel_ordinal",
                ordinal.checked_add(1).ok_or_else(conflict)?,
            );
            return Ok(Barrier::Pending);
        }
    };
    // Persist the exact reservation before any I/O; repeated reserve after a
    // lost acknowledgement returns the same slot. Never refund unknown writes.
    ctx.set("reissue_cancel_slot", Json(slot));
    ctx.set("reissue_cancel_uncertain", true);
    #[cfg(feature = "integration")]
    if let Some(faults) =
        faults.filter(|f| f.stage.lock().unwrap().as_deref() == Some("reissue-before-fence"))
    {
        ctx.run(|| async {
            faults
                .interruptions
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::future::pending::<()>().await;
            Ok::<_, HandlerError>(())
        })
        .name("reissue-before-fence")
        .await?;
    }
    let at = crate::clock::now(ctx, "reissue_cancel_time").await?;
    let mut result = ctx
        .run(|| async {
            Ok::<_, HandlerError>(Json(
                protocol::write(apps, app.storage.as_ref(), &cancel, &account, at).await,
            ))
        })
        .name("reissue_guarded_cancel")
        .await?
        .0;
    if let Some(limit) = &mut result.limit {
        crate::delivery_budget::bind_limit(ctx, "repository-reissue-write", limit);
    }
    let completion = Completion {
        input: cancel,
        slot,
        result,
    };
    ctx.set("reissue_cancel_completion", Json(completion.clone()));
    finish(ctx, app, completion).await
}
async fn finish(
    ctx: &ObjectContext<'_>,
    app: &AppState,
    completion: Completion,
) -> Result<Barrier, TerminalError> {
    let budget = ctx.object_client::<crate::invite_budget::InviteBudgetClient>(format!(
        "repo:{}",
        completion.input.create.account_id
    ));
    if let Some(limit) = completion.result.limit {
        budget.record_limit(Json(limit)).call().await?;
    }
    if let Some(generation) = completion.result.rejected_generation {
        ctx.run(|| async {
            app.storage
                .reject_cancellation_attempt(&completion.input, generation)
                .await
                .map_err(HandlerError::from)
        })
        .name("reissue_reject_cancel")
        .await?;
    }
    let result = match completion.result.outcome {
        WriteOutcome::Cancelled { .. } => Barrier::Clear,
        WriteOutcome::Accepted { .. } => {
            budget.release(Json(completion.slot)).call().await?;
            Barrier::Clear
        }
        WriteOutcome::NoEffect => {
            budget.release(Json(completion.slot)).call().await?;
            ctx.clear("reissue_cancel_uncertain");
            let n = ctx.get::<u64>("reissue_cancel_ordinal").await?.unwrap_or(1);
            ctx.set(
                "reissue_cancel_ordinal",
                n.checked_add(1).ok_or_else(conflict)?,
            );
            Barrier::Pending
        }
        WriteOutcome::Unknown => Barrier::MayHaveWritten,
    };
    if result == Barrier::Clear {
        ctx.set("reissue_barrier_clear", true);
    }
    ctx.clear("reissue_reservation_started");
    ctx.clear("reissue_cancel_completion");
    Ok(result)
}

async fn flush_limit(ctx: &ObjectContext<'_>) -> Result<(), TerminalError> {
    if let Some(Json((account_id, limit))) = ctx
        .get::<Json<(u64, ghinvite_core::budget::RecordLimit)>>("reissue_read_limit")
        .await?
    {
        ctx.object_client::<crate::invite_budget::InviteBudgetClient>(format!("repo:{account_id}"))
            .record_limit(Json(limit))
            .call()
            .await?;
        ctx.clear("reissue_read_limit");
    }
    Ok(())
}
