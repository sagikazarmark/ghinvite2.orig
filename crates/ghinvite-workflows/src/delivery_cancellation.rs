//! Explicit preactivation execution of retained cancellation/own-expiry intents.
//! The delivery owner calls the real budget VO between journaled I/O stages.
use crate::{
    AppState,
    cancellation_protocol::{self as protocol, Observation, WriteOutcome},
    delivery::RepositoryDeliveryClient,
};
use chrono::{DateTime, Utc};
use ghinvite_core::{
    Account, InvitationState,
    audit::ActorKind,
    budget::{BudgetKind, Reservation, Reserve, SlotId},
    delivery::{CreateOutcome, DeliverySnapshot, SettlementReason},
    delivery_control::{CancelReason, Disposition, Event, ExpiryEvidence},
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
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExpiryTick {
    pub generation: u64,
    pub upstream_id: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Completion {
    input: CancelCommand,
    result: protocol::WriteResult,
    slot: SlotId,
    ordinal: u64,
    at: DateTime<Utc>,
}

async fn account(
    ctx: &ObjectContext<'_>,
    snapshot: &DeliverySnapshot,
) -> Result<Option<Account>, TerminalError> {
    Ok(ctx
        .object_client::<crate::availability::AccountInstallationClient>(format!(
            "repo:{}",
            snapshot.create.command.account_id
        ))
        .current_installation()
        .call()
        .await?
        .0)
}
async fn read(
    ctx: &ObjectContext<'_>,
    app: &AppState,
    input: &CancelCommand,
    account: &Account,
    observed_at: DateTime<Utc>,
) -> Result<Observation, TerminalError> {
    let apps = app
        .github_apps
        .as_ref()
        .ok_or_else(|| TerminalError::new("repository app unavailable"))?;
    let observation = ctx
        .run(|| async {
            Ok::<_, HandlerError>(Json(protocol::observe(apps, input, account).await))
        })
        .name("cancellation_read")
        .await?
        .0;
    if let Observation::Limited { retry_after } = observation {
        let mut limit = protocol::content_limit(observed_at, retry_after);
        crate::delivery_budget::bind_limit(ctx, "repository-cancel-read", &mut limit);
        ctx.set(
            "cancellation_read_limit",
            Json((input.create.account_id, limit)),
        );
        flush_read_limit(ctx).await?;
    }
    Ok(observation)
}
async fn later(ctx: &ObjectContext<'_>) -> Result<(), TerminalError> {
    let at =
        crate::clock::now(ctx, "cancellation_retry_time").await? + chrono::Duration::seconds(60);
    ctx.set("cancellation_retry_at", Json(at));
    ctx.object_client::<RepositoryDeliveryClient>(ctx.key())
        .drive_cancellation()
        .send_after(std::time::Duration::from_secs(60))
        .await?;
    Ok(())
}
fn command(snapshot: &DeliverySnapshot, reason: SettlementReason) -> Option<CancelCommand> {
    match snapshot.create.outcome {
        CreateOutcome::Created { upstream_id }
            if snapshot.settlement.is_none() && snapshot.tracking_stopped.is_none() =>
        {
            Some(CancelCommand {
                create: snapshot.create.command.clone(),
                upstream_id,
                reason,
            })
        }
        _ => None,
    }
}
async fn accepted(
    ctx: &ObjectContext<'_>,
    snapshot: DeliverySnapshot,
    role: ghinvite_core::targets::EffectiveRole,
    at: DateTime<Utc>,
) -> Result<(), TerminalError> {
    let observation = crate::settlement_policy::Observation::new(
        &snapshot,
        at,
        crate::settlement_policy::Evidence::Absent {
            availability_verified: true,
            role_name: Some(role),
            direct_collaborator: None,
            membership: None,
        },
    );
    crate::settlement::apply_observation(ctx, snapshot, &observation).await
}

/// Finish only already retained accounting; reissue must not leave an old
/// completion capable of changing budget or settlement after the barrier.
pub(crate) async fn drain_completion(
    ctx: &ObjectContext<'_>,
    app: &AppState,
) -> Result<(), TerminalError> {
    flush_read_limit(ctx).await?;
    if let Some(Json(completion)) = ctx
        .get::<Json<Completion>>("cancellation_completion")
        .await?
    {
        finish(ctx, app, completion).await?;
    }
    Ok(())
}

pub(crate) async fn drive(
    ctx: &ObjectContext<'_>,
    app: &AppState,
    #[cfg(feature = "integration")] faults: Option<&crate::delivery::DeliveryFaults>,
) -> Result<(), TerminalError> {
    if !app.cancellation_enabled {
        return Ok(());
    }
    flush_read_limit(ctx).await?;
    if let Some(Json(completion)) = ctx
        .get::<Json<Completion>>("cancellation_completion")
        .await?
    {
        return finish(ctx, app, completion).await;
    }
    let Some(Json(retained)) = ctx
        .get::<Json<crate::delivery_control::Retained>>(crate::delivery_control::STATE)
        .await?
    else {
        return Ok(());
    };
    let Some(Json(snapshot)) = crate::repository_audit::snapshot(ctx).await?.map(Json) else {
        return Ok(());
    };
    let now = crate::clock::now(ctx, "cancellation_drive_time").await?;
    if ctx
        .get::<Json<DateTime<Utc>>>("cancellation_retry_at")
        .await?
        .is_some_and(|at| at.0 > now)
    {
        return Ok(());
    }
    let reason = match retained.state.disposition() {
        Disposition::CancelOwned {
            reason: CancelReason::OwnExpiry,
            ..
        } => SettlementReason::OwnExpiry,
        _ => SettlementReason::LinkRevoked,
    };
    let Some(input) = command(&snapshot, reason) else {
        return Ok(());
    };
    recover_unused_reservation(ctx, app, &input).await?;
    let Some(account) = account(ctx, &snapshot).await? else {
        return later(ctx).await;
    };
    let observed_at = crate::clock::now(ctx, "cancellation_read_time").await?;
    let observation = read(ctx, app, &input, &account, observed_at).await?;
    if let Observation::Accepted { role } = observation {
        return accepted(ctx, snapshot, role, observed_at).await;
    }
    // Acquire the actual immutable GitHub creation time, never recovery time.
    if ctx
        .get::<Json<DateTime<Utc>>>("owned_created_at")
        .await?
        .is_none()
    {
        if let Observation::Pending { created_at } = observation {
            if created_at.timestamp() < input.create.approved_at.timestamp()
                || created_at > observed_at
            {
                return later(ctx).await;
            }
            ctx.set("owned_created_at", Json(created_at));
            crate::delivery_control::advance(ctx, app).await?;
        } else {
            return later(ctx).await;
        }
        // The next one-way invocation sees the fully advanced retained intent.
        return Ok(());
    }
    if !matches!(retained.state.disposition(), Disposition::CancelOwned { upstream_id, .. } if *upstream_id == input.upstream_id)
    {
        return Ok(());
    }
    if !matches!(observation, Observation::Pending { .. }) {
        return later(ctx).await;
    }
    if let Some(Json(old)) = ctx.get::<Json<CancelCommand>>("cancellation_input").await? {
        if old != input {
            return Err(TerminalError::new_with_code(
                409,
                "cancellation identity conflict",
            ));
        }
    } else {
        ctx.set("cancellation_input", Json(input.clone()));
    }
    // A lost HTTP/run acknowledgement keeps the slot and fence. Re-entry may
    // read acceptance, but does not acquire another slot or issue another DELETE.
    if ctx
        .get::<bool>("cancellation_unknown")
        .await?
        .unwrap_or(false)
    {
        return later(ctx).await;
    }
    let budget = ctx.object_client::<crate::invite_budget::InviteBudgetClient>(format!(
        "repo:{}",
        input.create.account_id
    ));
    let ordinal = ctx
        .get::<u64>("cancellation_reservation_generation")
        .await?
        .unwrap_or(1);
    let reservation = Reserve {
        request_key: format!("cancel:{}:{ordinal}", ctx.key()),
        kind: BudgetKind::OtherWrite,
        link_id: input.create.budget_link_id(),
    };
    let slot = match ctx.get::<Json<SlotId>>("cancellation_slot").await? {
        Some(Json(slot)) => slot,
        None => {
            ctx.set("cancellation_reservation_started", true);
            match budget.reserve(Json(reservation)).call().await?.0 {
                Reservation::Granted { slot_id } => {
                    ctx.set("cancellation_slot", Json(slot_id));
                    slot_id
                }
                Reservation::RetryAt { at, .. } => {
                    ctx.set("cancellation_retry_at", Json(at));
                    ctx.set(
                        "cancellation_reservation_generation",
                        ordinal.checked_add(1).ok_or_else(|| {
                            TerminalError::new("cancellation reservation exhausted")
                        })?,
                    );
                    ctx.object_client::<RepositoryDeliveryClient>(ctx.key())
                        .drive_cancellation()
                        .send_after((at - observed_at).to_std().unwrap_or_default())
                        .await?;
                    ctx.clear("cancellation_reservation_started");
                    return Ok(());
                }
            }
        }
    };
    let apps = app
        .github_apps
        .as_ref()
        .ok_or_else(|| TerminalError::new("repository app unavailable"))?;
    #[cfg(feature = "integration")]
    if let Some(faults) = faults
        && faults
            .cancel_after_reserve
            .swap(false, std::sync::atomic::Ordering::SeqCst)
    {
        return Err(TerminalError::new_with_code(
            409,
            "fixture: cancellation owner interrupted after budget",
        ));
    }
    // Retain may-have-written before entering the external run, exactly as for create.
    ctx.set("cancellation_unknown", true);
    let result = ctx
        .run(|| async {
            let result =
                protocol::write(apps, app.storage.as_ref(), &input, &account, observed_at).await;
            #[cfg(feature = "integration")]
            if let Some(faults) = faults
                && faults
                    .lose_cancel_results
                    .try_update(
                        std::sync::atomic::Ordering::SeqCst,
                        std::sync::atomic::Ordering::SeqCst,
                        |n| n.checked_sub(1),
                    )
                    .is_ok()
            {
                faults
                    .lost_cancel_results
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return Err(std::io::Error::other(
                    "fixture: cancellation run acknowledgement lost",
                )
                .into());
            }
            Ok::<_, HandlerError>(Json(result))
        })
        .name("guarded_github_cancel")
        .await?
        .0;
    let at = crate::clock::now(ctx, "cancellation_confirmed_time").await?;
    let mut result = result;
    if let Some(limit) = &mut result.limit {
        crate::delivery_budget::bind_limit(ctx, "repository-cancel-write", limit);
    }
    let completion = Completion {
        input,
        result,
        slot,
        ordinal,
        at,
    };
    ctx.set("cancellation_completion", Json(completion.clone()));
    #[cfg(feature = "integration")]
    if let Some(faults) = faults
        && faults
            .cancel_after_result
            .swap(false, std::sync::atomic::Ordering::SeqCst)
    {
        return Err(TerminalError::new_with_code(
            409,
            "fixture: cancellation result retained before completion",
        ));
    }
    finish(ctx, app, completion).await
}

async fn finish(
    ctx: &ObjectContext<'_>,
    app: &AppState,
    completion: Completion,
) -> Result<(), TerminalError> {
    let Completion {
        input,
        result,
        slot,
        ordinal,
        at,
    } = completion;
    let Some(Json(snapshot)) = crate::repository_audit::snapshot(ctx).await?.map(Json) else {
        return Err(TerminalError::new("cancellation receipt unavailable"));
    };
    if snapshot.create.command != input.create {
        return Err(TerminalError::new_with_code(
            409,
            "cancellation identity conflict",
        ));
    }
    let budget = ctx.object_client::<crate::invite_budget::InviteBudgetClient>(format!(
        "repo:{}",
        input.create.account_id
    ));
    if let Some(limit) = result.limit {
        budget.record_limit(Json(limit)).call().await?;
    }
    if let Some(generation) = result.rejected_generation {
        ctx.run(|| async {
            app.storage
                .reject_cancellation_attempt(&input, generation)
                .await
                .map_err(HandlerError::from)
        })
        .name("release_cancellation_attempt")
        .await?;
    }
    match result.outcome {
        WriteOutcome::Cancelled { evidence } => {
            crate::settlement::record(
                ctx,
                snapshot,
                InvitationState::Cancelled,
                at,
                ActorKind::System,
                None,
                serde_json::json!({"reason":input.reason,"evidence":evidence}),
            )
            .await?;
            ctx.clear("control_cancellation_pending");
        }
        WriteOutcome::Accepted { role } => {
            budget.release(Json(slot)).call().await?;
            accepted(ctx, snapshot, role, at).await?;
        }
        WriteOutcome::NoEffect => {
            budget.release(Json(slot)).call().await?;
            ctx.clear("cancellation_slot");
            ctx.clear("cancellation_reservation_started");
            ctx.clear("cancellation_unknown");
            ctx.set(
                "cancellation_reservation_generation",
                ordinal
                    .checked_add(1)
                    .filter(|n| *n <= ghinvite_core::storage::integer::MAX_EXACT)
                    .ok_or_else(|| TerminalError::new("cancellation reservation exhausted"))?,
            );
            later(ctx).await?;
        }
        WriteOutcome::Unknown => {
            later(ctx).await?;
        }
    }
    ctx.clear("cancellation_completion");
    Ok(())
}

pub(crate) async fn tick(
    ctx: &ObjectContext<'_>,
    app: &AppState,
    input: ExpiryTick,
) -> Result<(), TerminalError> {
    if !app.cancellation_enabled {
        return Ok(());
    }
    flush_read_limit(ctx).await?;
    let Some(Json(retained)) = ctx
        .get::<Json<crate::delivery_control::Retained>>(crate::delivery_control::STATE)
        .await?
    else {
        return Ok(());
    };
    let now = crate::clock::now(ctx, "own_expiry_tick_time").await?;
    if !retained
        .state
        .own_expiry_due(input.generation, input.upstream_id, now)
    {
        return Ok(());
    }
    let Some(Json(snapshot)) = crate::repository_audit::snapshot(ctx).await?.map(Json) else {
        return Ok(());
    };
    let Some(command) = command(&snapshot, SettlementReason::OwnExpiry) else {
        return Ok(());
    };
    crate::delivery_control::expiry_event(
        ctx,
        app,
        Event::TickOwnExpiry {
            generation: input.generation,
            upstream_id: input.upstream_id,
        },
        now,
    )
    .await?;
    let observation = match account(ctx, &snapshot).await? {
        Some(account) => read(ctx, app, &command, &account, now).await?,
        None => Observation::Unavailable,
    };
    let evidence = match &observation {
        Observation::Pending { .. } => ExpiryEvidence::StillPending,
        Observation::Accepted { .. } => ExpiryEvidence::Accepted,
        Observation::Gone => ExpiryEvidence::Absent,
        Observation::Unavailable | Observation::Limited { .. } => ExpiryEvidence::Unavailable,
    };
    crate::delivery_control::expiry_event(
        ctx,
        app,
        Event::OwnExpiryObserved {
            generation: input.generation,
            upstream_id: input.upstream_id,
            expected_revision: snapshot.revision,
            observed_at: now,
            evidence,
        },
        now,
    )
    .await?;
    if let Observation::Accepted { role } = observation {
        accepted(ctx, snapshot, role, now).await?;
    }
    Ok(())
}

/// Recover a possibly acknowledged budget RPC after caller interruption. Only
/// the durable no-write marker AND a readable absent fence prove the slot unused.
async fn recover_unused_reservation(
    ctx: &ObjectContext<'_>,
    app: &AppState,
    input: &CancelCommand,
) -> Result<(), TerminalError> {
    if !ctx
        .get::<bool>("cancellation_reservation_started")
        .await?
        .unwrap_or(false)
        || ctx
            .get::<bool>("cancellation_unknown")
            .await?
            .unwrap_or(false)
    {
        return Ok(());
    }
    let fenced = ctx
        .run(|| async {
            Ok::<_, HandlerError>(
                app.storage
                    .cancellation_attempt_exists(input)
                    .await
                    .unwrap_or(true),
            )
        })
        .name("cancellation_unused_fence")
        .await?;
    if fenced {
        ctx.set("cancellation_unknown", true);
        return Ok(());
    }
    let ordinal = ctx
        .get::<u64>("cancellation_reservation_generation")
        .await?
        .unwrap_or(1);
    let budget = ctx.object_client::<crate::invite_budget::InviteBudgetClient>(format!(
        "repo:{}",
        input.create.account_id
    ));
    let reserve = Reserve {
        request_key: format!("cancel:{}:{ordinal}", ctx.key()),
        kind: BudgetKind::OtherWrite,
        link_id: input.create.budget_link_id(),
    };
    if let Reservation::Granted { slot_id } = budget.reserve(Json(reserve)).call().await?.0 {
        budget.release(Json(slot_id)).call().await?;
    }
    ctx.clear("cancellation_slot");
    ctx.clear("cancellation_reservation_started");
    ctx.set(
        "cancellation_reservation_generation",
        ordinal
            .checked_add(1)
            .filter(|n| *n <= ghinvite_core::storage::integer::MAX_EXACT)
            .ok_or_else(|| TerminalError::new("cancellation reservation exhausted"))?,
    );
    Ok(())
}

// Retain positive read-side closure evidence before the cross-object call too.
// A caller interrupted after the observation resumes the same absolute deadline.
async fn flush_read_limit(ctx: &ObjectContext<'_>) -> Result<(), TerminalError> {
    if let Some(Json((account_id, limit))) = ctx
        .get::<Json<(u64, ghinvite_core::budget::RecordLimit)>>("cancellation_read_limit")
        .await?
    {
        ctx.object_client::<crate::invite_budget::InviteBudgetClient>(format!("repo:{account_id}"))
            .record_limit(Json(limit))
            .call()
            .await?;
        ctx.clear("cancellation_read_limit");
    }
    Ok(())
}
