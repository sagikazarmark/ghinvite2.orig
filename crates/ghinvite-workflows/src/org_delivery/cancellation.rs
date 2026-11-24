//! Separately fenced, budgeted organization cancellation and own-expiry timers.
use super::cancel_protocol::{self as io, Observation, Outcome};
use super::*;
use crate::invite_budget::InviteBudgetClient;
use ghinvite_core::{
    InvitationState,
    audit::{ActorKind, TargetKind},
    budget::{BudgetKind, Reservation},
    delivery::SettlementReason,
    delivery_control::{
        BudgetDisposition, CancelReason, Disposition, Event, ExpiryEvidence, Facts, Phase,
    },
};
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExpiryTick {
    pub generation: u64,
    pub upstream_id: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Work {
    input: OrganizationAttempt,
    ordinal: u64,
    reserve: Reserve,
    slot: Option<SlotId>,
    may_have_written: bool,
    completion: Option<(io::Result, DateTime<Utc>)>,
}
fn key(value: &Retained) -> String {
    format!("org:{}", value.snapshot.create.command.account_id)
}
fn owned(value: &Retained) -> Option<(u64, DateTime<Utc>)> {
    if value.snapshot.settlement.is_some() || value.snapshot.tracking_stopped.is_some() {
        return None;
    }
    match value.snapshot.create.outcome {
        OrganizationCreateOutcome::Created {
            upstream_id,
            created_at,
        } => Some((upstream_id, created_at)),
        _ => None,
    }
}
pub(super) async fn settle(
    ctx: &ObjectContext<'_>,
    value: &mut Retained,
    state: InvitationState,
    at: DateTime<Utc>,
    reason: Option<SettlementReason>,
) -> Result<(), TerminalError> {
    if value.snapshot.settlement.is_some() || value.snapshot.tracking_stopped.is_some() {
        return Ok(());
    }
    let command = &value.snapshot.create.command;
    let version = value.snapshot.create.audit_version;
    value.snapshot.settlement = Some(OrganizationSettlement {
        state,
        event: ghinvite_core::audit::AuditEvent {
            id: ghinvite_core::delivery_audit::organization_settlement_id(version, command),
            account_id: command.account_id,
            occurred_at: at,
            event_type: ghinvite_core::delivery_audit::organization_settlement_kind(version, state)
                .map_err(|_| conflict())?,
            actor_kind: if state == InvitationState::Cancelled {
                ActorKind::System
            } else {
                ActorKind::Github
            },
            actor_id: None,
            target_kind: TargetKind::OrganizationDelivery,
            target_id: command.storage_id(),
            metadata: serde_json::json!({"reason":reason}),
            request_id: Some(command.request_id.to_string()),
        },
    });
    for team in value.snapshot.teams.values_mut() {
        if matches!(
            team.create.outcome,
            TeamCreateOutcome::Created {
                via: TeamCreation::OrganizationInvitation { .. }
            }
        ) && team.settlement.is_none()
            && team.tracking_stopped.is_none()
        {
            team.settlement = Some(TeamSettlement {
                state,
                observed_at: at,
                reason: if version.is_legacy() { None } else { reason },
            });
            team.revision = increment(team.revision)?;
        }
    }
    value.snapshot.revision = increment(value.snapshot.revision)?;
    value.snapshot.validate().map_err(|_| conflict())?;
    save(ctx, value).await?;
    project(ctx, value).await?;
    Ok(())
}
pub(super) async fn flush(
    owner: &OrganizationDelivery,
    ctx: &ObjectContext<'_>,
    value: &mut Retained,
) -> Result<(), TerminalError> {
    let Some(Json(work)) = ctx.get::<Json<Work>>("org_cancel_work").await? else {
        return Ok(());
    };
    let Some((result, at)) = work.completion.clone() else {
        return Ok(());
    };
    let budget = ctx.object_client::<InviteBudgetClient>(key(value));
    if let Some(limit) = result.limit {
        budget.record_limit(Json(limit)).call().await?;
    }
    if result.unused
        && let Some(slot) = work.slot
    {
        budget.release(Json(slot)).call().await?;
    }
    if let Some(generation) = result.rejected_generation {
        ctx.run(|| async {
            owner
                .storage
                .reject_organization_attempt(&work.input, generation)
                .await
                .map_err(HandlerError::from)
        })
        .name("organization_cancel_release")
        .await?;
    }
    match result.outcome {
        Outcome::Cancelled => {
            let OrganizationWrite::Cancel { reason, .. } = work.input.action else {
                return Err(conflict());
            };
            settle(ctx, value, InvitationState::Cancelled, at, Some(reason)).await?;
            ctx.clear("org_cancel_work");
        }
        Outcome::Accepted => {
            settle(ctx, value, InvitationState::Accepted, at, None).await?;
            ctx.clear("org_cancel_work");
        }
        Outcome::NoEffect => {
            ctx.clear("org_cancel_work");
        }
        Outcome::Unknown => {
            let mut work = work;
            work.completion = None;
            ctx.set("org_cancel_work", Json(work));
        }
    }
    Ok(())
}
async fn later(ctx: &ObjectContext<'_>) -> Result<(), TerminalError> {
    ctx.object_client::<OrganizationDeliveryClient>(ctx.key())
        .drive_cancellation()
        .send_after(std::time::Duration::from_secs(60))
        .await?;
    Ok(())
}
async fn read(
    owner: &OrganizationDelivery,
    ctx: &ObjectContext<'_>,
    value: &mut Retained,
    input: &OrganizationAttempt,
    created_at: DateTime<Utc>,
) -> Result<Option<(Observation, ghinvite_core::Account, DateTime<Utc>)>, TerminalError> {
    let Some(account) = ctx
        .object_client::<crate::availability::AccountInstallationClient>(key(value))
        .current_installation()
        .call()
        .await?
        .0
    else {
        return Ok(None);
    };
    let apps = owner
        .state
        .github_apps
        .as_ref()
        .ok_or_else(|| TerminalError::new_with_code(503, "organization app unavailable"))?;
    let at = crate::clock::now(ctx, "organization_cancel_read_time").await?;
    let observed = ctx
        .run(|| async {
            Ok::<_, HandlerError>(Json(
                io::observe(apps, input, &account, created_at, at).await,
            ))
        })
        .name("organization_cancel_read")
        .await?
        .0;
    if let Observation::Limited(limit) = &observed {
        owner.record_limit(ctx, value, limit.clone()).await?;
    }
    if matches!(
        observed,
        Observation::Pending { .. } | Observation::Accepted | Observation::Absent
    ) {
        value.last_observed_at = Some(value.last_observed_at.map_or(at, |old| old.max(at)));
        let OrganizationWrite::Cancel { upstream_id, .. } = input.action else {
            return Err(conflict());
        };
        telemetry::invitation(value, upstream_id, at);
        save(ctx, value).await?;
        project(ctx, value).await?;
    }
    Ok(Some((observed, account, at)))
}
pub(super) async fn drive(
    owner: &OrganizationDelivery,
    ctx: &ObjectContext<'_>,
) -> Result<(), TerminalError> {
    if !owner.state.cancellation_enabled {
        return Ok(());
    }
    let Some(Json(mut value)) = ctx.get::<Json<Retained>>("organization").await? else {
        return Ok(());
    };
    flush(owner, ctx, &mut value).await?;
    let Some((upstream_id, created_at)) = owned(&value) else {
        return Ok(());
    };
    let Some(Disposition::CancelOwned { reason, .. }) =
        value.steps["org"].control.as_ref().map(|s| s.disposition())
    else {
        return Ok(());
    };
    let reason = if *reason == CancelReason::OwnExpiry {
        SettlementReason::OwnExpiry
    } else {
        SettlementReason::LinkRevoked
    };
    let input = OrganizationAttempt {
        create: value.snapshot.create.command.clone(),
        action: OrganizationWrite::Cancel {
            upstream_id,
            reason,
        },
    };
    let mut retained = ctx.get::<Json<Work>>("org_cancel_work").await?.map(|v| v.0);
    if retained.as_ref().is_some_and(|w| w.input != input) {
        return Err(conflict());
    }
    // An interrupted reservation is recoverable only before possible DELETE,
    // with an authoritative absent-fence check and the exact retained input.
    if let Some(work) = retained.as_ref()
        && !work.may_have_written
    {
        let clear = ctx
            .run(|| async {
                owner
                    .storage
                    .organization_attempt_exists(&work.input)
                    .await
                    .map(|exists| !exists)
                    .map_err(HandlerError::from)
            })
            .name("organization_cancel_unused_fence")
            .await?;
        if clear {
            let budget = ctx.object_client::<InviteBudgetClient>(key(&value));
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
            ctx.clear("org_cancel_work");
            retained = None;
        } else {
            let mut work = work.clone();
            work.may_have_written = true;
            ctx.set("org_cancel_work", Json(work.clone()));
            retained = Some(work);
        }
    }
    let Some((observation, account, _)) = read(owner, ctx, &mut value, &input, created_at).await?
    else {
        return later(ctx).await;
    };
    let now = crate::clock::now(ctx, "organization_cancel_after_read_time").await?;
    if matches!(observation, Observation::Accepted) {
        settle(ctx, &mut value, InvitationState::Accepted, now, None).await?;
        let inbox = ctx
            .get::<Json<Inbox>>("control")
            .await?
            .ok_or_else(conflict)?
            .0;
        control::advance(owner, ctx, &mut value, &inbox, now).await?;
        return Ok(());
    }
    if !matches!(observation, Observation::Pending { .. })
        || retained.as_ref().is_some_and(|w| w.may_have_written)
    {
        return later(ctx).await;
    }
    let ordinal = increment(ctx.get::<u64>("org_cancel_ordinal").await?.unwrap_or(0))?;
    ctx.set("org_cancel_ordinal", ordinal);
    let reservation = Reserve {
        request_key: format!("{}/cancel/{ordinal}", input.create.storage_id()),
        kind: BudgetKind::OtherWrite,
        link_id: input.create.budget_link_id(),
    };
    let mut work = Work {
        input: input.clone(),
        ordinal,
        reserve: reservation.clone(),
        slot: None,
        may_have_written: false,
        completion: None,
    };
    ctx.set("org_cancel_work", Json(work.clone()));
    let budget = ctx.object_client::<InviteBudgetClient>(key(&value));
    if let Some(link_id) = input.create.budget_link_id() {
        budget
            .set_link_share(Json(ghinvite_core::budget::LinkShare {
                link_id,
                percent: input.create.budget_share_pct,
            }))
            .call()
            .await?;
    }
    let slot = match budget.reserve(Json(reservation)).call().await?.0 {
        Reservation::Granted { slot_id } => slot_id,
        Reservation::RetryAt { at, .. } => {
            ctx.clear("org_cancel_work");
            ctx.object_client::<OrganizationDeliveryClient>(ctx.key())
                .drive_cancellation()
                .send_after((at - now).to_std().unwrap_or_default())
                .await?;
            return Ok(());
        }
    };
    work.slot = Some(slot);
    work.may_have_written = true;
    ctx.set("org_cancel_work", Json(work.clone()));
    let apps = owner.state.github_apps.as_ref().expect("validated app");
    let result = ctx
        .run(|| async {
            let result = io::write(
                apps,
                owner.storage.as_ref(),
                &input,
                &account,
                created_at,
                now,
            )
            .await;
            #[cfg(feature = "integration")]
            if let Some(faults) = &owner.faults
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
                return Err(HandlerError::from(std::io::Error::other(
                    "fixture organization cancellation result acknowledgement lost",
                )));
            }
            Ok::<_, HandlerError>(Json(result))
        })
        .name("organization_cancel_write")
        .await?
        .0;
    let mut result = result;
    if let Some(limit) = &mut result.limit {
        crate::delivery_budget::bind_limit(ctx, "organization-cancel-write", limit);
    }
    work.completion = Some((
        result,
        crate::clock::now(ctx, "organization_cancel_completed_at").await?,
    ));
    ctx.set("org_cancel_work", Json(work));
    flush(owner, ctx, &mut value).await?;
    if value.snapshot.settlement.is_none() {
        later(ctx).await?;
    }
    let inbox = ctx
        .get::<Json<Inbox>>("control")
        .await?
        .ok_or_else(conflict)?
        .0;
    control::advance(
        owner,
        ctx,
        &mut value,
        &inbox,
        crate::clock::now(ctx, "organization_cancel_control_time").await?,
    )
    .await?;
    project(ctx, &mut value).await?;
    Ok(())
}
pub(super) async fn tick(
    owner: &OrganizationDelivery,
    ctx: &ObjectContext<'_>,
    tick: ExpiryTick,
) -> Result<(), TerminalError> {
    if !owner.state.cancellation_enabled {
        return Ok(());
    }
    let Some(Json(mut value)) = ctx.get::<Json<Retained>>("organization").await? else {
        return Ok(());
    };
    flush(owner, ctx, &mut value).await?;
    let Some((upstream_id, created_at)) = owned(&value) else {
        return Ok(());
    };
    let now = crate::clock::now(ctx, "organization_expiry_tick_time").await?;
    let Some(state) = value.steps["org"].control.clone() else {
        return Ok(());
    };
    if !state.own_expiry_due(tick.generation, tick.upstream_id, now) {
        return Ok(());
    }
    let facts = Facts {
        phase: Phase::OwnedInvitation {
            upstream_id,
            created_at,
        },
        delivery_revision: value.snapshot.revision,
        last_observed_at: None,
        budget: BudgetDisposition::None,
    };
    let transition = model::reduce(
        &state,
        &facts,
        Event::TickOwnExpiry {
            generation: tick.generation,
            upstream_id: tick.upstream_id,
        },
        now,
    )
    .map_err(|_| conflict())?;
    value.steps.get_mut("org").expect("org").control = Some(transition.state.clone());
    save(ctx, &mut value).await?;
    let input = OrganizationAttempt {
        create: value.snapshot.create.command.clone(),
        action: OrganizationWrite::Cancel {
            upstream_id,
            reason: SettlementReason::OwnExpiry,
        },
    };
    let observation = read(owner, ctx, &mut value, &input, created_at).await?;
    let (observed, at) = observation
        .map(|(observed, _, at)| (observed, at))
        .unwrap_or((Observation::Unavailable, now));
    let after = crate::clock::now(ctx, "organization_expiry_evidence_time").await?;
    let evidence = match observed {
        Observation::Accepted => ExpiryEvidence::Accepted,
        Observation::Pending { .. } => ExpiryEvidence::StillPending,
        Observation::Absent => ExpiryEvidence::Absent,
        _ => ExpiryEvidence::Unavailable,
    };
    let next = model::reduce(
        &transition.state,
        &facts,
        Event::OwnExpiryObserved {
            generation: tick.generation,
            upstream_id,
            expected_revision: value.snapshot.revision,
            observed_at: at,
            evidence,
        },
        after,
    )
    .map_err(|_| conflict())?;
    value.steps.get_mut("org").expect("org").control = Some(next.state);
    save(ctx, &mut value).await?;
    if evidence == ExpiryEvidence::Accepted {
        settle(ctx, &mut value, InvitationState::Accepted, after, None).await?;
    }
    for effect in next.effects {
        match effect {
            model::Effect::ScheduleOwnExpiry {
                generation,
                upstream_id,
                at,
            } => {
                ctx.object_client::<OrganizationDeliveryClient>(ctx.key())
                    .tick_own_expiry(Json(ExpiryTick {
                        generation,
                        upstream_id,
                    }))
                    .send_after((at - after).to_std().unwrap_or_default())
                    .await?;
            }
            model::Effect::RequestCancellation { .. } => {
                ctx.object_client::<OrganizationDeliveryClient>(ctx.key())
                    .drive_cancellation()
                    .send();
            }
            _ => {}
        }
    }
    Ok(())
}

/// A retained submitted cancellation cannot be resolved from list absence.
pub(super) async fn uncertain(ctx: &ObjectContext<'_>) -> Result<bool, TerminalError> {
    Ok(ctx
        .get::<Json<Work>>("org_cancel_work")
        .await?
        .is_some_and(|work| work.0.may_have_written))
}

/// Drain pre-existing cancellation accounting before opening a new reissue
/// barrier. This function never changes an already terminal settlement.
pub(super) async fn prepare_reissue(
    owner: &OrganizationDelivery,
    ctx: &ObjectContext<'_>,
    value: &mut Retained,
) -> Result<Option<ghinvite_core::reissue::InvitationBarrier>, TerminalError> {
    use ghinvite_core::reissue::InvitationBarrier;
    flush(owner, ctx, value).await?;
    let Some(Json(work)) = ctx.get::<Json<Work>>("org_cancel_work").await? else {
        return Ok(None);
    };
    let fenced = ctx
        .run(|| async {
            Ok::<_, HandlerError>(
                owner
                    .storage
                    .organization_attempt_exists(&work.input)
                    .await
                    .unwrap_or(true),
            )
        })
        .name("organization_reissue_prior_fence")
        .await?;
    if fenced {
        return Ok(Some(InvitationBarrier::MayHaveWritten));
    }
    let budget = ctx.object_client::<InviteBudgetClient>(key(value));
    let slot = match work.slot {
        Some(slot) => Some(slot),
        None => match budget.reserve(Json(work.reserve)).call().await?.0 {
            Reservation::Granted { slot_id } => Some(slot_id),
            Reservation::RetryAt { .. } => None,
        },
    };
    if let Some(slot) = slot {
        budget.release(Json(slot)).call().await?;
    }
    ctx.clear("org_cancel_work");
    Ok(None)
}
