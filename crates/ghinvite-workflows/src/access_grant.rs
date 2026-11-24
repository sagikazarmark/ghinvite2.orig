//! Separately owned, accepted-only access removal. Explicit M2 composition.
mod protocol;
use crate::AppState;
use chrono::{DateTime, Utc};
use ghinvite_core::{
    access_grant::*,
    budget::{BudgetKind, Reservation, Reserve, SlotId},
    storage::access_grants::AccessGrantStorage,
};
use restate_sdk::{
    context::{
        ContextClient, ContextReadState, ContextSideEffects, ContextWriteState, ObjectContext,
        RunFuture, SharedObjectContext,
    },
    endpoint::Builder,
    errors::{HandlerError, TerminalError},
    serde::Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
pub struct AccessGrant {
    state: AppState,
    storage: Arc<dyn AccessGrantStorage>,
    #[cfg(feature = "integration")]
    faults: Option<Arc<AccessGrantFaults>>,
}
pub struct AccessGrantProjection {
    storage: Arc<dyn AccessGrantStorage>,
}
pub fn bind(builder: Builder, state: AppState, storage: Arc<dyn AccessGrantStorage>) -> Builder {
    builder
        .bind(AccessGrant {
            state,
            storage: storage.clone(),
            #[cfg(feature = "integration")]
            faults: None,
        })
        .bind(AccessGrantProjection { storage })
}
/// Explicit integration-only faults. Production composition has no hook or
/// environment/request switch capable of selecting these boundaries.
#[cfg(feature = "integration")]
#[derive(Default)]
pub struct AccessGrantFaults {
    pub stage: std::sync::Mutex<Option<String>>,
    pub interruptions: std::sync::atomic::AtomicUsize,
    pub interrupted_invocation: std::sync::Mutex<Option<String>>,
    pub lose_results: std::sync::atomic::AtomicUsize,
    pub lost_results: std::sync::atomic::AtomicUsize,
}
#[cfg(feature = "integration")]
pub fn bind_with_faults(
    builder: Builder,
    state: AppState,
    storage: Arc<dyn AccessGrantStorage>,
    faults: Arc<AccessGrantFaults>,
) -> Builder {
    builder
        .bind(AccessGrant {
            state,
            storage: storage.clone(),
            faults: Some(faults),
        })
        .bind(AccessGrantProjection { storage })
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RemovalTick {
    pub delivery_generation: u64,
}
#[derive(Clone, Serialize, Deserialize)]
struct Completion {
    result: protocol::WriteResult,
    at: DateTime<Utc>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Retained {
    snapshot: AccessGrantSnapshot,
    input: Option<RemovalCommand>,
    ordinal: u64,
    reservation: Option<Reserve>,
    slot: Option<SlotId>,
    may_have_written: bool,
    completion: Option<Completion>,
    retry_at: Option<DateTime<Utc>>,
}
fn conflict() -> TerminalError {
    TerminalError::new_with_code(409, "access removal identity conflict")
}
fn state_key(generation: u64) -> String {
    format!("generation/{generation}")
}
fn validate_key(ctx_key: &str, command: &AccessGrantCommand) -> Result<(), TerminalError> {
    command.validate().map_err(|_| conflict())?;
    if ctx_key != command.key() {
        return Err(conflict());
    }
    Ok(())
}
async fn project(ctx: &ObjectContext<'_>, value: &Retained) -> Result<(), TerminalError> {
    ctx.object_client::<AccessGrantProjectionClient>(value.snapshot.command.storage_id())
        .apply(Json(value.snapshot.clone()))
        .send()
        .await?;
    Ok(())
}
fn save(ctx: &ObjectContext<'_>, value: &Retained) {
    ctx.set(
        &state_key(value.snapshot.command.delivery_generation),
        Json(value.clone()),
    );
}
fn outcome(value: &mut Retained, next: RemovalOutcome) -> Result<(), TerminalError> {
    if value.snapshot.outcome == next {
        return Ok(());
    }
    let mut snapshot = value.snapshot.clone();
    snapshot.revision = snapshot.revision.checked_add(1).ok_or_else(conflict)?;
    snapshot.outcome = next;
    value
        .snapshot
        .validate_successor(&snapshot)
        .map_err(|_| conflict())?;
    value.snapshot = snapshot;
    Ok(())
}
async fn validate_source(
    ctx: &ObjectContext<'_>,
    command: &AccessGrantCommand,
) -> Result<(), TerminalError> {
    let key = ghinvite_core::delivery::delivery_key(
        command.request_id,
        command.target.delivery_target(),
        command.delivery_generation,
    );
    let grants = match command.target {
        GrantTarget::Repository { .. } => {
            let Some(snapshot) = ctx
                .object_client::<crate::delivery::RepositoryDeliveryClient>(key)
                .status()
                .call()
                .await?
                .0
            else {
                return Err(conflict());
            };
            crate::delivery::validate_notice_snapshot(
                &snapshot.create.command.delivery_key(),
                &snapshot,
            )?;
            accepted_repository(&snapshot)
                .map_err(|_| conflict())?
                .into_iter()
                .collect::<Vec<_>>()
        }
        _ => {
            let Some(snapshot) = ctx
                .object_client::<crate::org_delivery::OrganizationDeliveryClient>(key)
                .status()
                .call()
                .await?
                .0
            else {
                return Err(conflict());
            };
            accepted_organization(&snapshot).map_err(|_| conflict())?
        }
    };
    if !grants.contains(command) {
        return Err(conflict());
    }
    Ok(())
}
async fn wake(
    ctx: &ObjectContext<'_>,
    generation: u64,
    at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), TerminalError> {
    ctx.object_client::<AccessGrantClient>(ctx.key())
        .tick_remove(Json(RemovalTick {
            delivery_generation: generation,
        }))
        .send_after((at - now).to_std().unwrap_or_default())
        .await?;
    Ok(())
}
#[restate_sdk::object]
impl AccessGrant {
    #[handler]
    async fn schedule(
        &self,
        ctx: ObjectContext<'_>,
        Json(command): Json<AccessGrantCommand>,
    ) -> Result<(), TerminalError> {
        validate_key(ctx.key(), &command)?;
        let generation = command.delivery_generation;
        if let Some(Json(stopped)) = ctx
            .get::<Json<RemovalBarrierQuery>>(&format!("stopped/{generation}"))
            .await?
        {
            if stopped.request_id != command.request_id || stopped.target != command.target {
                return Err(conflict());
            }
            // A delayed Accepted notification must never revive removal of an
            // old generation after the reissue owner passed its barrier.
            return Ok(());
        }
        let retained = ctx.get::<Json<Retained>>(&state_key(generation)).await?;
        let value = if let Some(Json(value)) = retained {
            if value.snapshot.command != command {
                return Err(conflict());
            }
            value
        } else {
            validate_source(&ctx, &command).await?;
            let current = ctx.get::<u64>("current_generation").await?.unwrap_or(0);
            if generation < current {
                return Err(conflict());
            }
            if current > 0
                && current < generation
                && let Some(Json(old)) = ctx.get::<Json<Retained>>(&state_key(current)).await?
                && (!old.snapshot.outcome.terminal()
                    || old.may_have_written
                    || old.completion.is_some())
            {
                return Err(TerminalError::new_with_code(
                    409,
                    "previous access removal is pending",
                ));
            }
            let value = Retained {
                snapshot: AccessGrantSnapshot {
                    command: command.clone(),
                    revision: 1,
                    outcome: RemovalOutcome::Scheduled,
                },
                input: None,
                ordinal: 1,
                reservation: None,
                slot: None,
                may_have_written: false,
                completion: None,
                retry_at: None,
            };
            save(&ctx, &value);
            self.checkpoint(&ctx, "access-after-registration").await?;
            value
        };
        // Exact replay repairs a registration interrupted between these two
        // durable writes. Never move the pointer back over a newer generation.
        let current = ctx.get::<u64>("current_generation").await?.unwrap_or(0);
        if generation > current {
            if current > 0
                && let Some(Json(old)) = ctx.get::<Json<Retained>>(&state_key(current)).await?
                && (!old.snapshot.outcome.terminal()
                    || old.may_have_written
                    || old.completion.is_some())
            {
                return Err(TerminalError::new_with_code(
                    409,
                    "previous access removal is pending",
                ));
            }
            ctx.set("current_generation", generation);
        } else if generation < current && !value.snapshot.outcome.terminal() {
            return Err(conflict());
        }
        project(&ctx, &value).await?;
        if !value.snapshot.outcome.terminal()
            && let Some(at) = command.remove_at().map_err(|_| conflict())?
        {
            let now = crate::clock::now(&ctx, "access_schedule_time").await?;
            wake(&ctx, generation, at, now).await?;
        }
        Ok(())
    }
    #[handler]
    async fn tick_remove(
        &self,
        ctx: ObjectContext<'_>,
        Json(tick): Json<RemovalTick>,
    ) -> Result<(), TerminalError> {
        let Some(Json(mut value)) = ctx
            .get::<Json<Retained>>(&state_key(tick.delivery_generation))
            .await?
        else {
            return Ok(());
        };
        validate_key(ctx.key(), &value.snapshot.command)?;
        if value.snapshot.outcome.terminal() {
            return project(&ctx, &value).await;
        }
        if ctx.get::<u64>("current_generation").await? != Some(tick.delivery_generation) {
            return Ok(());
        }
        let now = crate::clock::now(&ctx, "access_tick_time").await?;
        let due = if value.input.is_some() {
            value.retry_at.unwrap_or(now)
        } else {
            let Some(at) = value.snapshot.command.remove_at().map_err(|_| conflict())? else {
                return Ok(());
            };
            at
        };
        if now < due {
            return Ok(());
        }
        if value.input.is_none() {
            value.input = Some(RemovalCommand {
                grant: value.snapshot.command.clone(),
                reason: RemovalReason::AccessExpiry,
            });
            save(&ctx, &value);
        }
        self.drive(&ctx, value, now).await
    }
    #[handler]
    async fn remove_now(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<RemovalCommand>,
    ) -> Result<(), TerminalError> {
        validate_key(ctx.key(), &input.grant)?;
        if input.reason != RemovalReason::LinkRevoked {
            return Err(conflict());
        }
        let generation = input.grant.delivery_generation;
        if let Some(Json(stopped)) = ctx
            .get::<Json<RemovalBarrierQuery>>(&format!("stopped/{generation}"))
            .await?
        {
            if stopped.request_id != input.grant.request_id || stopped.target != input.grant.target
            {
                return Err(conflict());
            }
            return Ok(());
        }
        if ctx
            .get::<u64>("current_generation")
            .await?
            .is_some_and(|current| current > generation)
        {
            return Ok(());
        }
        let Some(Json(mut value)) = ctx
            .get::<Json<Retained>>(&state_key(input.grant.delivery_generation))
            .await?
        else {
            return Err(TerminalError::new_with_code(
                503,
                "accepted access registration pending",
            ));
        };
        if value.snapshot.command != input.grant {
            return Err(conflict());
        }
        if value.snapshot.outcome.terminal() {
            return project(&ctx, &value).await;
        }
        // The first expiry/revocation cause is retained across all later wakeups.
        if value.input.is_none() {
            value.input = Some(input);
            save(&ctx, &value);
        }
        let now = crate::clock::now(&ctx, "access_remove_time").await?;
        self.drive(&ctx, value, now).await
    }
    #[handler]
    async fn stop_for_reissue(
        &self,
        ctx: ObjectContext<'_>,
        Json(query): Json<RemovalBarrierQuery>,
    ) -> Result<Json<RemovalBarrier>, TerminalError> {
        if format!(
            "{}:{}:{}",
            query.request_id,
            query.target.kind(),
            query.target.id()
        ) != ctx.key()
            || query.delivery_generation == 0
        {
            return Err(conflict());
        }
        let Some(Json(mut value)) = ctx
            .get::<Json<Retained>>(&state_key(query.delivery_generation))
            .await?
        else {
            let tombstone = format!("stopped/{}", query.delivery_generation);
            if let Some(Json(previous)) = ctx.get::<Json<RemovalBarrierQuery>>(&tombstone).await?
                && previous != query
            {
                return Err(conflict());
            }
            ctx.set(&tombstone, Json(query));
            return Ok(Json(RemovalBarrier::NoRemoval));
        };
        if value.snapshot.command.target != query.target {
            return Err(conflict());
        }
        if value.completion.is_some() {
            self.finish(&ctx, value).await?;
            value = ctx
                .get::<Json<Retained>>(&state_key(query.delivery_generation))
                .await?
                .ok_or_else(conflict)?
                .0;
        }
        if value.snapshot.outcome.terminal() {
            project(&ctx, &value).await?;
            return Ok(Json(RemovalBarrier::DefinitivelyFinished));
        }
        if matches!(value.snapshot.outcome, RemovalOutcome::OutcomeUnknown) {
            return Ok(Json(RemovalBarrier::MayHaveWritten));
        }
        let input = value.input.clone().unwrap_or_else(|| RemovalCommand {
            grant: value.snapshot.command.clone(),
            reason: RemovalReason::AccessExpiry,
        });
        let fenced = ctx
            .run(|| async {
                self.storage
                    .removal_attempt_exists(&input)
                    .await
                    .map_err(HandlerError::from)
            })
            .name("access_reissue_removal_fence")
            .await?;
        if fenced {
            return Ok(Json(RemovalBarrier::MayHaveWritten));
        }
        if let Some(reservation) = value.reservation.clone() {
            let budget = ctx.object_client::<crate::invite_budget::InviteBudgetClient>(format!(
                "{}:{}",
                input.grant.target.app().as_str(),
                input.grant.account_id
            ));
            let slot = match value.slot {
                Some(slot) => Some(slot),
                None => match budget.reserve(Json(reservation)).call().await?.0 {
                    Reservation::Granted { slot_id } => Some(slot_id),
                    Reservation::RetryAt { .. } => None,
                },
            };
            if let Some(slot) = slot {
                budget.release(Json(slot)).call().await?;
            }
        }
        let at = crate::clock::now(&ctx, "access_reissue_stop_time").await?;
        outcome(
            &mut value,
            RemovalOutcome::RemovalSkipped {
                reason: RemovalSkip::SupersededGeneration,
                at,
            },
        )?;
        value.slot = None;
        value.reservation = None;
        value.may_have_written = false;
        value.retry_at = None;
        save(&ctx, &value);
        self.checkpoint(&ctx, "access-after-terminal").await?;
        project(&ctx, &value).await?;
        Ok(Json(RemovalBarrier::DefinitivelyFinished))
    }
    #[handler]
    async fn status(
        &self,
        ctx: SharedObjectContext<'_>,
        Json(query): Json<RemovalBarrierQuery>,
    ) -> Result<Json<Option<AccessGrantSnapshot>>, TerminalError> {
        if format!(
            "{}:{}:{}",
            query.request_id,
            query.target.kind(),
            query.target.id()
        ) != ctx.key()
        {
            return Err(conflict());
        }
        let value = ctx
            .get::<Json<Retained>>(&state_key(query.delivery_generation))
            .await?;
        if let Some(Json(value)) = &value
            && (value.snapshot.command.target != query.target
                || value.snapshot.command.request_id != query.request_id)
        {
            return Err(conflict());
        }
        Ok(Json(value.map(|v| v.0.snapshot)))
    }
    #[handler]
    async fn removal_progress(
        &self,
        ctx: SharedObjectContext<'_>,
        Json(command): Json<AccessGrantCommand>,
    ) -> Result<Json<ghinvite_core::revocation::CascadeEntryState>, TerminalError> {
        use ghinvite_core::revocation::CascadeEntryState;
        validate_key(ctx.key(), &command)?;
        let generation = command.delivery_generation;
        if let Some(Json(stopped)) = ctx
            .get::<Json<RemovalBarrierQuery>>(&format!("stopped/{generation}"))
            .await?
        {
            if stopped.request_id != command.request_id || stopped.target != command.target {
                return Err(conflict());
            }
            return Ok(Json(CascadeEntryState::Done));
        }
        let Some(Json(value)) = ctx.get::<Json<Retained>>(&state_key(generation)).await? else {
            return Ok(Json(CascadeEntryState::Pending));
        };
        if value.snapshot.command != command {
            return Err(conflict());
        }
        Ok(Json(
            if value.may_have_written
                || value.completion.is_some()
                || matches!(value.snapshot.outcome, RemovalOutcome::OutcomeUnknown)
            {
                CascadeEntryState::WaitingOnGithub
            } else if value.snapshot.outcome.terminal() {
                CascadeEntryState::Done
            } else {
                CascadeEntryState::Pending
            },
        ))
    }
    #[handler]
    async fn removal_barrier(
        &self,
        ctx: SharedObjectContext<'_>,
        Json(query): Json<RemovalBarrierQuery>,
    ) -> Result<Json<RemovalBarrier>, TerminalError> {
        if format!(
            "{}:{}:{}",
            query.request_id,
            query.target.kind(),
            query.target.id()
        ) != ctx.key()
        {
            return Err(conflict());
        }
        let Some(Json(value)) = ctx
            .get::<Json<Retained>>(&state_key(query.delivery_generation))
            .await?
        else {
            return Ok(Json(RemovalBarrier::NoRemoval));
        };
        if value.snapshot.command.target != query.target {
            return Err(conflict());
        }
        Ok(Json(
            if value.may_have_written || value.completion.is_some() {
                RemovalBarrier::MayHaveWritten
            } else {
                value.snapshot.barrier()
            },
        ))
    }
}
impl AccessGrant {
    async fn retry(
        &self,
        ctx: &ObjectContext<'_>,
        mut value: Retained,
        now: DateTime<Utc>,
        at: Option<DateTime<Utc>>,
    ) -> Result<(), TerminalError> {
        let at = at.unwrap_or(now + chrono::Duration::seconds(60));
        value.retry_at = Some(at);
        save(ctx, &value);
        project(ctx, &value).await?;
        wake(ctx, value.snapshot.command.delivery_generation, at, now).await
    }
    async fn drive(
        &self,
        ctx: &ObjectContext<'_>,
        mut value: Retained,
        now: DateTime<Utc>,
    ) -> Result<(), TerminalError> {
        if value.completion.is_some() {
            return self.finish(ctx, value).await;
        }
        if value.snapshot.outcome.terminal() {
            return project(ctx, &value).await;
        }
        if value.retry_at.is_some_and(|at| at > now) {
            return Ok(());
        }
        let input = value.input.clone().ok_or_else(conflict)?;
        self.flush_limit(ctx, &input).await?;
        if matches!(value.snapshot.outcome, RemovalOutcome::OutcomeUnknown) {
            return project(ctx, &value).await;
        }
        if value.may_have_written || value.reservation.is_some() {
            let fenced = ctx
                .run(|| async {
                    self.storage
                        .removal_attempt_exists(&input)
                        .await
                        .map_err(HandlerError::from)
                })
                .name("access_removal_fence")
                .await?;
            if fenced {
                outcome(&mut value, RemovalOutcome::OutcomeUnknown)?;
                save(ctx, &value);
                return project(ctx, &value).await;
            }
            value.may_have_written = false;
            // Resolve an interrupted reserve RPC and release its exact slot only
            // after the real SQL fence proves that no DELETE was submitted.
            if let Some(reservation) = value.reservation.clone() {
                let budget =
                    ctx.object_client::<crate::invite_budget::InviteBudgetClient>(format!(
                        "{}:{}",
                        input.grant.target.app().as_str(),
                        input.grant.account_id
                    ));
                let slot = match value.slot {
                    Some(slot) => Some(slot),
                    None => match budget.reserve(Json(reservation)).call().await?.0 {
                        Reservation::Granted { slot_id } => Some(slot_id),
                        Reservation::RetryAt { .. } => None,
                    },
                };
                if let Some(slot) = slot {
                    budget.release(Json(slot)).call().await?;
                }
                value.slot = None;
                value.reservation = None;
                value.ordinal = value.ordinal.checked_add(1).ok_or_else(conflict)?;
            }
            save(ctx, &value);
        }
        let account = ctx
            .object_client::<crate::availability::AccountInstallationClient>(format!(
                "{}:{}",
                input.grant.target.app().as_str(),
                input.grant.account_id
            ))
            .current_installation()
            .call()
            .await?
            .0;
        let Some(account) = account else {
            outcome(
                &mut value,
                RemovalOutcome::Blocked {
                    reason: RemovalBlocked::InstallationUnavailable,
                },
            )?;
            return self.retry(ctx, value, now, None).await;
        };
        let apps =
            self.state.github_apps.as_ref().ok_or_else(|| {
                TerminalError::new_with_code(503, "access removal app unavailable")
            })?;
        let observation = ctx
            .run(|| async {
                Ok::<_, HandlerError>(Json(
                    protocol::observe(apps, &input.grant, &account, now).await,
                ))
            })
            .name("access_removal_read")
            .await?
            .0;
        match observation {
            protocol::Observation::Ready { .. } => {}
            protocol::Observation::Absent => {
                outcome(
                    &mut value,
                    RemovalOutcome::RemovalSkipped {
                        reason: RemovalSkip::AlreadyAbsent,
                        at: now,
                    },
                )?;
                save(ctx, &value);
                return project(ctx, &value).await;
            }
            protocol::Observation::Skip { role } => {
                outcome(
                    &mut value,
                    RemovalOutcome::RemovalSkipped {
                        reason: RemovalSkip::RoleChanged { now: role },
                        at: now,
                    },
                )?;
                save(ctx, &value);
                return project(ctx, &value).await;
            }
            protocol::Observation::Blocked { reason } => {
                outcome(&mut value, RemovalOutcome::Blocked { reason })?;
                return self.retry(ctx, value, now, None).await;
            }
            protocol::Observation::Limited { mut limit } => {
                crate::delivery_budget::bind_limit(ctx, "access-removal-read", &mut limit);
                ctx.set("read_limit", Json(limit.clone()));
                self.flush_limit(ctx, &input).await?;
                outcome(
                    &mut value,
                    RemovalOutcome::Blocked {
                        reason: RemovalBlocked::RateLimited,
                    },
                )?;
                return self.retry(ctx, value, now, limit.until).await;
            }
        }
        self.flush_limit(ctx, &input).await?;
        let budget = ctx.object_client::<crate::invite_budget::InviteBudgetClient>(format!(
            "{}:{}",
            input.grant.target.app().as_str(),
            input.grant.account_id
        ));
        let reserve = value
            .reservation
            .get_or_insert_with(|| Reserve {
                request_key: format!("access:{}:{}", input.grant.storage_id(), value.ordinal),
                kind: BudgetKind::OtherWrite,
                link_id: input.grant.link_id,
            })
            .clone();
        save(ctx, &value);
        let slot = if let Some(slot) = value.slot {
            slot
        } else {
            match budget.reserve(Json(reserve)).call().await?.0 {
                Reservation::Granted { slot_id } => {
                    value.slot = Some(slot_id);
                    save(ctx, &value);
                    slot_id
                }
                Reservation::RetryAt { at, .. } => {
                    return self.retry(ctx, value, now, Some(at)).await;
                }
            }
        };
        let _ = slot;
        self.checkpoint(ctx, "access-after-reserve").await?;
        value.may_have_written = true;
        save(ctx, &value);
        let mut result = ctx
            .run(|| async {
                let result =
                    protocol::write(apps, self.storage.as_ref(), &input, &account, now).await;
                #[cfg(feature = "integration")]
                if let Some(faults) = &self.faults
                    && faults
                        .lose_results
                        .try_update(
                            std::sync::atomic::Ordering::SeqCst,
                            std::sync::atomic::Ordering::SeqCst,
                            |n| n.checked_sub(1),
                        )
                        .is_ok()
                {
                    faults
                        .lost_results
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    return Err(
                        std::io::Error::other("fixture: access removal run result lost").into(),
                    );
                }
                Ok::<_, HandlerError>(Json(result))
            })
            .name("guarded_access_removal")
            .await?
            .0;
        if let Some(limit) = &mut result.limit {
            crate::delivery_budget::bind_limit(ctx, "access-removal-write", limit);
        }
        let at = crate::clock::now(ctx, "access_removal_completed").await?;
        value.completion = Some(Completion { result, at });
        save(ctx, &value);
        self.checkpoint(ctx, "access-after-completion").await?;
        self.finish(ctx, value).await
    }
    async fn checkpoint(
        &self,
        ctx: &ObjectContext<'_>,
        stage: &'static str,
    ) -> Result<(), TerminalError> {
        #[cfg(feature = "integration")]
        if let Some(faults) = &self.faults
            && faults.stage.lock().unwrap().as_deref() == Some(stage)
        {
            ctx.run(|| async {
                *faults.interrupted_invocation.lock().unwrap() =
                    Some(ctx.invocation_id().to_string());
                faults
                    .interruptions
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                std::future::pending::<Result<(), HandlerError>>().await
            })
            .name(stage)
            .await?;
        }
        let _ = (ctx, stage);
        Ok(())
    }
    async fn flush_limit(
        &self,
        ctx: &ObjectContext<'_>,
        input: &RemovalCommand,
    ) -> Result<(), TerminalError> {
        if let Some(Json(limit)) = ctx
            .get::<Json<ghinvite_core::budget::RecordLimit>>("read_limit")
            .await?
        {
            ctx.object_client::<crate::invite_budget::InviteBudgetClient>(format!(
                "{}:{}",
                input.grant.target.app().as_str(),
                input.grant.account_id
            ))
            .record_limit(Json(limit))
            .call()
            .await?;
            ctx.clear("read_limit");
        }
        Ok(())
    }
    async fn finish(
        &self,
        ctx: &ObjectContext<'_>,
        mut value: Retained,
    ) -> Result<(), TerminalError> {
        let completed = value.completion.clone().ok_or_else(conflict)?;
        let input = value.input.clone().ok_or_else(conflict)?;
        let budget = ctx.object_client::<crate::invite_budget::InviteBudgetClient>(format!(
            "{}:{}",
            input.grant.target.app().as_str(),
            input.grant.account_id
        ));
        if let Some(limit) = completed.result.limit.clone() {
            budget.record_limit(Json(limit)).call().await?;
        }
        if let Some(generation) = completed.result.rejected_generation {
            ctx.run(|| async {
                self.storage
                    .reject_removal_attempt(&input, generation)
                    .await
                    .map_err(HandlerError::from)
            })
            .name("release_access_removal_attempt")
            .await?;
        }
        let unused = matches!(
            completed.result.outcome,
            protocol::WriteOutcome::NoEffect { .. } | protocol::WriteOutcome::Skipped { .. }
        );
        if unused {
            budget
                .release(Json(value.slot.ok_or_else(conflict)?))
                .call()
                .await?;
        }
        let next = match completed.result.outcome {
            protocol::WriteOutcome::Removed => RemovalOutcome::Removed {
                reason: input.reason,
                at: completed.at,
            },
            protocol::WriteOutcome::Skipped { reason } => RemovalOutcome::RemovalSkipped {
                reason,
                at: completed.at,
            },
            protocol::WriteOutcome::NoEffect { reason } => RemovalOutcome::Blocked { reason },
            protocol::WriteOutcome::Unknown => RemovalOutcome::OutcomeUnknown,
        };
        outcome(&mut value, next)?;
        value.completion = None;
        if unused {
            value.slot = None;
            value.reservation = None;
            value.may_have_written = false;
            value.ordinal = value.ordinal.checked_add(1).ok_or_else(conflict)?;
        } else if value.snapshot.outcome.terminal() {
            value.may_have_written = false;
        }
        save(ctx, &value);
        if value.snapshot.outcome.terminal() {
            self.checkpoint(ctx, "access-after-terminal").await?;
        }
        project(ctx, &value).await?;
        if matches!(value.snapshot.outcome, RemovalOutcome::Blocked { .. }) {
            return self
                .retry(
                    ctx,
                    value,
                    completed.at,
                    completed.result.limit.and_then(|l| l.until),
                )
                .await;
        }
        Ok(())
    }
}
#[restate_sdk::object]
impl AccessGrantProjection {
    #[handler]
    async fn apply(
        &self,
        ctx: ObjectContext<'_>,
        Json(snapshot): Json<AccessGrantSnapshot>,
    ) -> Result<(), TerminalError> {
        snapshot.validate().map_err(|_| conflict())?;
        if ctx.key() != snapshot.command.storage_id() {
            return Err(conflict());
        }
        ctx.run(|| async {
            self.storage
                .project_access_grant(&snapshot)
                .await
                .map_err(HandlerError::from)
        })
        .name("project_access_grant")
        .await?;
        Ok(())
    }
}
