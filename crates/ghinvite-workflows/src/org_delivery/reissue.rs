//! Positive old-generation invitation barrier; terminal receipts are immutable.
use super::{cancel_protocol as io, *};
use ghinvite_core::{
    InvitationState,
    budget::{BudgetKind, Reservation},
    delivery::SettlementReason,
    reissue::{InvitationBarrier as Barrier, OrganizationReissueBarrierQuery as Query},
};

#[derive(Clone, Serialize, Deserialize)]
struct Work {
    input: OrganizationAttempt,
    reserve: Reserve,
    slot: Option<SlotId>,
    may_have_written: bool,
    completion: Option<io::Result>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
struct State {
    clear: bool,
    ordinal: u64,
    work: Option<Work>,
    read_limit: Option<ghinvite_core::budget::RecordLimit>,
}
fn store(ctx: &ObjectContext<'_>, state: &State) {
    ctx.set("org_reissue_barrier", Json(state.clone()));
}
fn budget_key(command: &OrganizationCreateCommand) -> String {
    format!("org:{}", command.account_id)
}

pub(super) async fn prepare(
    owner: &OrganizationDelivery,
    ctx: &ObjectContext<'_>,
    query: Query,
) -> Result<Barrier, TerminalError> {
    query.command.validate().map_err(|_| conflict())?;
    if ctx.key() != query.command.delivery_key() {
        return Err(conflict());
    }
    let operation_key = format!(
        "org_reissue_operation/{}",
        String::from(query.operation_id.clone())
    );
    if let Some(Json(previous)) = ctx.get::<Json<Query>>(&operation_key).await? {
        if previous != query {
            return Err(conflict());
        }
    } else {
        ctx.set(&operation_key, Json(query.clone()));
    }
    let Some(Json(mut retained)) = ctx.get::<Json<Retained>>("organization").await? else {
        return Ok(Barrier::Pending);
    };
    retained.snapshot.validate().map_err(|_| conflict())?;
    if retained.snapshot.create.command != query.command {
        return Err(conflict());
    }
    if !retained
        .snapshot
        .settlement
        .as_ref()
        .is_some_and(|settlement| {
            matches!(
                settlement.state,
                InvitationState::Expired | InvitationState::Declined | InvitationState::Cancelled
            )
        })
    {
        return Ok(Barrier::Pending);
    }
    // Finish old cancellation accounting without changing the already terminal
    // settlement. A retained uncertain write remains an independent barrier.
    if let Some(barrier) = cancellation::prepare_reissue(owner, ctx, &mut retained).await? {
        return Ok(barrier);
    }
    let mut state = ctx
        .get::<Json<State>>("org_reissue_barrier")
        .await?
        .map(|v| v.0)
        .unwrap_or_default();
    if state.clear {
        return Ok(Barrier::Clear);
    }
    if state
        .work
        .as_ref()
        .is_some_and(|work| work.completion.is_some())
    {
        return finish(owner, ctx, &mut state).await;
    }
    let (upstream_id, created_at, owned) = match retained.snapshot.create.outcome {
        OrganizationCreateOutcome::Created {
            upstream_id,
            created_at,
        } => (upstream_id, created_at, true),
        OrganizationCreateOutcome::AlreadyPending { upstream_id, .. } => (
            upstream_id,
            retained.snapshot.create.command.approved_at,
            false,
        ),
        _ => return Ok(Barrier::Pending),
    };
    let input = OrganizationAttempt {
        create: query.command,
        action: OrganizationWrite::Cancel {
            upstream_id,
            reason: SettlementReason::AdminReissue,
        },
    };
    let budget =
        ctx.object_client::<crate::invite_budget::InviteBudgetClient>(budget_key(&input.create));
    if let Some(limit) = state.read_limit.clone() {
        budget.record_limit(Json(limit)).call().await?;
        state.read_limit = None;
        store(ctx, &state);
    }
    if let Some(work) = state.work.as_ref() {
        if work.input != input {
            return Err(conflict());
        }
        // A process may stop after recording MAY but before claiming the SQL
        // fence. Only an acknowledged absent fence permits refund/re-reserve.
        let fenced = ctx
            .run(|| async {
                Ok::<_, HandlerError>(
                    owner
                        .storage
                        .organization_attempt_exists(&input)
                        .await
                        .unwrap_or(true),
                )
            })
            .name("organization_reissue_unused_fence")
            .await?;
        if fenced {
            return Ok(Barrier::MayHaveWritten);
        }
        let slot = match work.slot {
            Some(slot) => Some(slot),
            None => match budget.reserve(Json(work.reserve.clone())).call().await?.0 {
                Reservation::Granted { slot_id } => Some(slot_id),
                Reservation::RetryAt { .. } => None,
            },
        };
        if let Some(slot) = slot {
            budget.release(Json(slot)).call().await?;
        }
        state.work = None;
        store(ctx, &state);
    }
    let Some(account) = ctx
        .object_client::<crate::availability::AccountInstallationClient>(budget_key(&input.create))
        .current_installation()
        .call()
        .await?
        .0
    else {
        return Ok(Barrier::Pending);
    };
    let Some(apps) = owner.state.github_apps.as_ref() else {
        return Ok(Barrier::Pending);
    };
    let now = crate::clock::now(ctx, "organization_reissue_read_time").await?;
    let observation = ctx
        .run(|| async {
            Ok::<_, HandlerError>(Json(
                io::observe(apps, &input, &account, created_at, now).await,
            ))
        })
        .name("organization_reissue_read")
        .await?
        .0;
    match observation {
        io::Observation::Absent | io::Observation::Accepted => {
            state.clear = true;
            store(ctx, &state);
            return Ok(Barrier::Clear);
        }
        io::Observation::Limited(mut limit) => {
            if owner.state.invite_budget.is_some() {
                crate::delivery_budget::bind_limit(ctx, "organization-reissue-read", &mut limit);
                state.read_limit = Some(limit.clone());
                store(ctx, &state);
                budget.record_limit(Json(limit)).call().await?;
                state.read_limit = None;
                store(ctx, &state);
            }
            return Ok(Barrier::Pending);
        }
        io::Observation::Unavailable => return Ok(Barrier::Pending),
        io::Observation::Pending { .. } => {}
    }
    if !owned || !owner.state.cancellation_enabled || owner.state.invite_budget.is_none() {
        return Ok(Barrier::Pending);
    }
    let fenced = ctx
        .run(|| async {
            Ok::<_, HandlerError>(
                owner
                    .storage
                    .organization_attempt_exists(&input)
                    .await
                    .unwrap_or(true),
            )
        })
        .name("organization_reissue_cancel_fence")
        .await?;
    if fenced {
        return Ok(Barrier::MayHaveWritten);
    }
    state.ordinal = increment(state.ordinal)?;
    let reserve = Reserve {
        request_key: format!("org-reissue-cancel:{}:{}", ctx.key(), state.ordinal),
        kind: BudgetKind::OtherWrite,
        link_id: None,
    };
    state.work = Some(Work {
        input: input.clone(),
        reserve: reserve.clone(),
        slot: None,
        may_have_written: false,
        completion: None,
    });
    store(ctx, &state);
    let slot = match budget.reserve(Json(reserve)).call().await?.0 {
        Reservation::Granted { slot_id } => slot_id,
        Reservation::RetryAt { .. } => {
            state.work = None;
            store(ctx, &state);
            return Ok(Barrier::Pending);
        }
    };
    let work = state.work.as_mut().expect("retained work");
    work.slot = Some(slot);
    work.may_have_written = true;
    store(ctx, &state);
    let now = crate::clock::now(ctx, "organization_reissue_cancel_time").await?;
    let mut result = ctx
        .run(|| async {
            Ok::<_, HandlerError>(Json(
                io::write(
                    apps,
                    owner.storage.as_ref(),
                    &input,
                    &account,
                    created_at,
                    now,
                )
                .await,
            ))
        })
        .name("organization_reissue_guarded_cancel")
        .await?
        .0;
    if let Some(limit) = &mut result.limit {
        crate::delivery_budget::bind_limit(ctx, "organization-reissue-write", limit);
    }
    state.work.as_mut().expect("retained work").completion = Some(result);
    store(ctx, &state);
    finish(owner, ctx, &mut state).await
}
async fn finish(
    owner: &OrganizationDelivery,
    ctx: &ObjectContext<'_>,
    state: &mut State,
) -> Result<Barrier, TerminalError> {
    let work = state.work.as_ref().ok_or_else(conflict)?;
    let result = work.completion.clone().ok_or_else(conflict)?;
    let budget = ctx
        .object_client::<crate::invite_budget::InviteBudgetClient>(budget_key(&work.input.create));
    if let Some(limit) = result.limit {
        budget.record_limit(Json(limit)).call().await?;
    }
    if let Some(generation) = result.rejected_generation {
        ctx.run(|| async {
            owner
                .storage
                .reject_organization_attempt(&work.input, generation)
                .await
                .map_err(HandlerError::from)
        })
        .name("organization_reissue_reject_cancel")
        .await?;
    }
    if result.unused {
        budget
            .release(Json(work.slot.ok_or_else(conflict)?))
            .call()
            .await?;
    }
    let barrier = match result.outcome {
        io::Outcome::Cancelled | io::Outcome::Accepted => {
            state.clear = true;
            state.work = None;
            Barrier::Clear
        }
        io::Outcome::NoEffect => {
            state.work = None;
            Barrier::Pending
        }
        io::Outcome::Unknown => {
            state.work.as_mut().expect("retained work").completion = None;
            Barrier::MayHaveWritten
        }
    };
    store(ctx, state);
    Ok(barrier)
}
