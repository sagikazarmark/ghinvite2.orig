//! Repository S0 adapter. Cancellation writes belong to the separate fenced executor.
use crate::{AppState, delivery::RepositoryDeliveryClient};
use chrono::{DateTime, Utc};
use ghinvite_core::{
    delivery::{
        AbortReason, CreateCommand, CreateOutcome, CreateReceipt, DeliverySnapshot, HoldReason,
        SettlementReason,
    },
    delivery_control::{
        self as model, BudgetDisposition, ControlEnvelope, DeliveryKind, Disposition, Effect,
        Event, Facts, Identity, Phase,
    },
};
use restate_sdk::{
    context::{
        ContextClient, ContextReadState, ContextSideEffects, ContextWriteState, ObjectContext,
        RunFuture,
    },
    errors::{HandlerError, TerminalError},
    serde::Json,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeliveryControlInput {
    pub envelope: ControlEnvelope,
    pub admitted_at: DateTime<Utc>,
    pub invitation_lifetime_seconds: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeliveryControlResult {
    pub revision: u64,
    pub disposition: Disposition,
    pub initialized: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ControlWake {
    pub identity: Identity,
    pub generation: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Retained {
    admitted_at: DateTime<Utc>,
    invitation_lifetime_seconds: u32,
    revision: u64,
    pub(crate) state: model::State,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Receipt {
    input: DeliveryControlInput,
    result: DeliveryControlResult,
}
pub(crate) const STATE: &str = "delivery_control";
pub(crate) const MAY_HAVE_WRITTEN: &str = "control_may_have_written";
pub(crate) async fn invitation_timing(
    ctx: &ObjectContext<'_>,
    mut timing: ghinvite_core::delivery::InvitationTiming,
) -> Result<ghinvite_core::delivery::InvitationTiming, TerminalError> {
    timing.owned_lifetime_seconds = ctx
        .get::<Json<Retained>>(STATE)
        .await?
        .map(|r| r.0.invitation_lifetime_seconds);
    Ok(timing)
}
fn conflict() -> TerminalError {
    TerminalError::new_with_code(409, "delivery control conflict")
}
fn model_error(error: model::Error) -> TerminalError {
    TerminalError::new_with_code(
        if error == model::Error::Invalid {
            400
        } else {
            409
        },
        error.to_string(),
    )
}
fn key(identity: &Identity) -> String {
    ghinvite_core::delivery::delivery_key(
        identity.request_id,
        identity.target_id,
        identity.delivery_generation,
    )
}
fn command_matches(identity: &Identity, command: &CreateCommand) -> bool {
    identity.kind == DeliveryKind::Repository
        && identity.link_id == command.link_id
        && identity.batch == command.batch
        && identity.request_id == command.request_id
        && identity.target_id == command.repo_id
        && identity.delivery_generation == command.delivery_generation
}
fn increment(n: u64) -> Result<u64, TerminalError> {
    n.checked_add(1)
        .filter(|n| *n <= ghinvite_core::storage::integer::MAX_EXACT)
        .ok_or_else(|| TerminalError::new("delivery revision exhausted"))
}
async fn facts(
    ctx: &ObjectContext<'_>,
    app: &AppState,
    command: Option<&CreateCommand>,
    snapshot: Option<&DeliverySnapshot>,
) -> Result<Facts, TerminalError> {
    let phase = match snapshot {
        Some(s) if s.settlement.is_some() || s.tracking_stopped.is_some() => Phase::Final,
        Some(s) => match s.create.outcome {
            CreateOutcome::Created { upstream_id } => {
                let created_at = ctx
                    .get::<Json<DateTime<Utc>>>("owned_created_at")
                    .await?
                    .map(|v| v.0);
                // A confirmed create identifies ownership, but its local receipt
                // time is not GitHub's creation time. Until that immutable fact
                // is read, preserve uncertainty and schedule no own-expiry timer.
                created_at.map_or(Phase::MayHaveWritten, |created_at| Phase::OwnedInvitation {
                    upstream_id,
                    created_at,
                })
            }
            CreateOutcome::AlreadyPending { upstream_id, .. } => {
                Phase::ForeignInvitation { upstream_id }
            }
            CreateOutcome::Submitted | CreateOutcome::OutcomeUnknown => Phase::MayHaveWritten,
            ref outcome if outcome.confirmed() => Phase::Final,
            _ => possible_write(ctx, app, command).await?,
        },
        None => possible_write(ctx, app, command).await?,
    };
    Ok(Facts {
        phase,
        delivery_revision: snapshot.map_or(1, |s| s.revision),
        last_observed_at: ctx
            .get::<Json<DateTime<Utc>>>("last_observed_at")
            .await?
            .map(|v| v.0),
        // The adapter recovers real unused reservations through the budget
        // owner only after durable no-write and absent-fence proof. A reducer
        // fact never invents a slot or authorizes an unchecked release.
        budget: BudgetDisposition::None,
    })
}
async fn possible_write(
    ctx: &ObjectContext<'_>,
    app: &AppState,
    command: Option<&CreateCommand>,
) -> Result<Phase, TerminalError> {
    if ctx.get::<bool>(MAY_HAVE_WRITTEN).await?.unwrap_or(false) {
        return Ok(Phase::MayHaveWritten);
    }
    let Some(command) = command else {
        return Ok(Phase::NoWriteProven);
    };
    // This generation is retained only after a positively rejected run result.
    // A failed release acknowledgement cannot erase that no-effect evidence.
    if ctx.get::<u64>("rejected_generation").await?.is_some() {
        return Ok(Phase::NoWriteProven);
    }
    let fenced = ctx
        .run(|| async {
            // An unavailable safety database cannot prove that no write occurred.
            Ok::<_, HandlerError>(app.storage.delivery_attempt_exists(command).await.ok())
        })
        .name("control_attempt_fence")
        .await?;
    Ok(match fenced {
        Some(true) => {
            // A real retained fence is positive uncertainty evidence, including
            // an older owner without the explicit pre-run marker.
            ctx.set(MAY_HAVE_WRITTEN, true);
            Phase::MayHaveWritten
        }
        Some(false) => Phase::NoWriteProven,
        None => Phase::MayHaveWritten,
    })
}

pub(crate) async fn control(
    ctx: &ObjectContext<'_>,
    app: &AppState,
    input: DeliveryControlInput,
) -> Result<DeliveryControlResult, TerminalError> {
    if input.envelope.identity.kind != DeliveryKind::Repository
        || key(&input.envelope.identity) != ctx.key()
    {
        return Err(conflict());
    }
    let receipt_key = format!(
        "control_receipt/{}",
        String::from(input.envelope.operation_id.clone())
    );
    if let Some(Json(old)) = ctx.get::<Json<Receipt>>(&receipt_key).await? {
        return if old.input == input {
            Ok(old.result)
        } else {
            Err(conflict())
        };
    }
    let command = ctx.get::<Json<CreateCommand>>("input").await?.map(|v| v.0);
    if command.as_ref().is_some_and(|c| {
        !command_matches(&input.envelope.identity, c) || c.approved_at < input.admitted_at
    }) {
        return Err(conflict());
    }
    let mut retained = match ctx.get::<Json<Retained>>(STATE).await? {
        Some(Json(old)) => {
            if old.admitted_at != input.admitted_at
                || old.invitation_lifetime_seconds != input.invitation_lifetime_seconds
                || old.state.identity() != &input.envelope.identity
            {
                return Err(conflict());
            }
            old
        }
        None => Retained {
            admitted_at: input.admitted_at,
            invitation_lifetime_seconds: input.invitation_lifetime_seconds,
            revision: 0,
            state: model::State::new(
                input.envelope.identity.clone(),
                input.admitted_at,
                input.invitation_lifetime_seconds,
            )
            .map_err(model_error)?,
        },
    };
    let snapshot = crate::repository_audit::snapshot(ctx)
        .await?
        .map(Json)
        .map(|v| v.0);
    let facts = facts(ctx, app, command.as_ref(), snapshot.as_ref()).await?;
    let now = crate::clock::now(ctx, "delivery_control_time").await?;
    let transition = model::reduce(
        &retained.state,
        &facts,
        Event::Apply(input.envelope.clone()),
        now,
    )
    .map_err(model_error)?;
    retained.state = transition.state;
    retained.revision = retained.revision.max(input.envelope.revision);
    ctx.set(STATE, Json(retained.clone()));
    apply(
        ctx,
        app,
        &retained,
        command.as_ref(),
        snapshot,
        transition.effects,
        now,
    )
    .await?;
    let result = DeliveryControlResult {
        revision: retained.revision,
        disposition: retained.state.disposition().clone(),
        initialized: command.is_some(),
    };
    ctx.set(
        &receipt_key,
        Json(Receipt {
            input,
            result: result.clone(),
        }),
    );
    Ok(result)
}

/// Apply S0 after command binding and before any prerequisite/read/write stage.
pub(crate) async fn gate(
    ctx: &ObjectContext<'_>,
    app: &AppState,
    command: &CreateCommand,
) -> Result<Option<CreateReceipt>, TerminalError> {
    let Some(Json(mut retained)) = ctx.get::<Json<Retained>>(STATE).await? else {
        return Ok(None);
    };
    if !command_matches(retained.state.identity(), command)
        || command.approved_at < retained.admitted_at
    {
        return Err(conflict());
    }
    let snapshot = crate::repository_audit::snapshot(ctx)
        .await?
        .map(Json)
        .map(|v| v.0);
    let facts = facts(ctx, app, Some(command), snapshot.as_ref()).await?;
    let now = crate::clock::now(ctx, "delivery_control_gate_time").await?;
    let transition =
        model::reduce(&retained.state, &facts, Event::Advance, now).map_err(model_error)?;
    retained.state = transition.state;
    ctx.set(STATE, Json(retained.clone()));
    apply(
        ctx,
        app,
        &retained,
        Some(command),
        snapshot,
        transition.effects,
        now,
    )
    .await?;
    if matches!(
        retained.state.disposition(),
        Disposition::Final | Disposition::StopTrackingForeign
    ) {
        ctx.clear("control_cancellation_pending");
    }
    if matches!(
        retained.state.disposition(),
        Disposition::Held { .. } | Disposition::Aborted
    ) {
        return Ok(crate::repository_audit::receipt(ctx)
            .await?
            .map(Json)
            .map(|v| v.0));
    }
    Ok(None)
}

pub(crate) async fn advance(ctx: &ObjectContext<'_>, app: &AppState) -> Result<(), TerminalError> {
    if let Some(Json(command)) = ctx.get::<Json<CreateCommand>>("input").await? {
        gate(ctx, app, &command).await?;
    }
    Ok(())
}
pub(crate) async fn wake_command(
    ctx: &ObjectContext<'_>,
    wake: &ControlWake,
) -> Result<Option<CreateCommand>, TerminalError> {
    if key(&wake.identity) != ctx.key() {
        return Err(conflict());
    }
    let Some(Json(retained)) = ctx.get::<Json<Retained>>(STATE).await? else {
        return Ok(None);
    };
    if retained.state.identity() != &wake.identity {
        return Err(conflict());
    }
    if !retained.state.accepts_wake(wake.generation)
        || ctx
            .get::<u64>("control_wake_consumed")
            .await?
            .is_some_and(|n| n >= wake.generation)
    {
        return Ok(None);
    }
    ctx.set("control_wake_consumed", wake.generation);
    Ok(ctx.get::<Json<CreateCommand>>("input").await?.map(|v| v.0))
}
async fn apply(
    ctx: &ObjectContext<'_>,
    app: &AppState,
    retained: &Retained,
    command: Option<&CreateCommand>,
    snapshot: Option<DeliverySnapshot>,
    effects: Vec<Effect>,
    now: DateTime<Utc>,
) -> Result<(), TerminalError> {
    if matches!(
        retained.state.disposition(),
        Disposition::Final | Disposition::StopTrackingForeign
    ) {
        ctx.clear("control_cancellation_pending");
    } else if matches!(retained.state.disposition(), Disposition::RecoverUnknown)
        && retained.state.cancel_on_resolve()
    {
        ctx.set("control_cancellation_pending", true);
    }
    if matches!(
        retained.state.disposition(),
        Disposition::Held { .. } | Disposition::Aborted
    ) {
        ctx.clear("recheck_scheduled");
    }
    let Some(command) = command else {
        return Ok(());
    };
    if app.invite_budget.is_some()
        && matches!(
            retained.state.disposition(),
            Disposition::Held { .. } | Disposition::Aborted
        )
    {
        crate::delivery_budget::recover_unused(ctx, app.storage.as_ref(), command).await?;
    }
    let mut snapshot = snapshot;
    let outcome = match retained.state.disposition() {
        Disposition::Held { .. } => Some(CreateOutcome::Held {
            reason: HoldReason::AbuseSuspected,
        }),
        Disposition::Aborted => Some(CreateOutcome::Aborted {
            reason: AbortReason::LinkRevoked,
        }),
        Disposition::RecoverUnknown
            if ctx.get::<bool>(MAY_HAVE_WRITTEN).await?.unwrap_or(false)
                || snapshot.as_ref().is_some_and(|s| {
                    matches!(
                        s.create.outcome,
                        CreateOutcome::Submitted | CreateOutcome::OutcomeUnknown
                    )
                }) =>
        {
            Some(CreateOutcome::OutcomeUnknown)
        }
        Disposition::Ready
            if snapshot
                .as_ref()
                .is_some_and(|s| matches!(s.create.outcome, CreateOutcome::Held { .. })) =>
        {
            Some(CreateOutcome::Planned)
        }
        _ => None,
    };
    let hold_epoch_changed = if let Disposition::Held { epoch } = retained.state.disposition() {
        crate::repository_audit::held_epoch_changed(ctx, *epoch).await?
    } else {
        false
    };
    if let Some(outcome) = outcome
        && !snapshot
            .as_ref()
            .is_some_and(|s| s.create.outcome.confirmed())
        && (hold_epoch_changed
            || !snapshot
                .as_ref()
                .is_some_and(|s| s.create.outcome == outcome))
    {
        let previous = snapshot.as_ref().map(|s| &s.create);
        let receipt = CreateReceipt {
            audit_version: previous.map_or(
                ghinvite_core::delivery_audit::new_owner_version(command.invitation_id),
                |receipt| receipt.audit_version,
            ),
            command: command.clone(),
            revision: previous.map_or(Ok(1), |s| increment(s.revision))?,
            confirmed_at: outcome.confirmed().then_some(now),
            outcome,
        };
        crate::operational::record_unknown(ctx, previous, &receipt).await?;
        let updated = DeliverySnapshot::from(receipt);
        crate::repository_audit::save(ctx, &updated).await?;
        crate::delivery::send_projection(ctx, &updated).await?;
        snapshot = Some(updated);
    }
    if app.cancellation_enabled
        && (matches!(
            retained.state.disposition(),
            Disposition::CancelOwned { .. }
        ) || ctx
            .get::<Json<DateTime<Utc>>>("owned_created_at")
            .await?
            .is_none())
        && snapshot.as_ref().is_some_and(|s| {
            matches!(s.create.outcome, CreateOutcome::Created { .. })
                && s.settlement.is_none()
                && s.tracking_stopped.is_none()
        })
    {
        ctx.object_client::<RepositoryDeliveryClient>(ctx.key())
            .drive_cancellation()
            .send()
            .await?;
    }
    for effect in effects {
        match effect {
            Effect::Wake { generation } => {
                ctx.object_client::<RepositoryDeliveryClient>(ctx.key())
                    .control_wake(Json(ControlWake {
                        identity: retained.state.identity().clone(),
                        generation,
                    }))
                    .send()
                    .await?;
            }
            Effect::RecoverReadOnly => {
                if let Some(snapshot) = &snapshot {
                    ctx.service_client::<crate::settlement::DeliveryObservationClient>()
                        .observe(Json(snapshot.clone()))
                        .send()
                        .await?;
                }
            }
            Effect::StopTrackingForeign => {
                if let Some(mut snapshot) = snapshot.clone()
                    && snapshot.tracking_stopped.is_none()
                    && snapshot.settlement.is_none()
                    && snapshot.create.outcome.tracked_foreign()
                {
                    snapshot.tracking_stopped = Some(SettlementReason::LinkRevoked);
                    snapshot.revision = increment(snapshot.revision)?;
                    crate::repository_audit::save(ctx, &snapshot).await?;
                    crate::delivery::send_projection(ctx, &snapshot).await?;
                }
            }
            // Retain intent independently of effect completion. The explicitly
            // enabled executor is woken above; ordinary composition stays inert.
            Effect::RequestCancellation { .. } => ctx.set("control_cancellation_pending", true),
            Effect::ScheduleOwnExpiry {
                generation,
                upstream_id,
                at,
            } => {
                ctx.set("control_own_expiry_pending", true);
                if app.cancellation_enabled {
                    ctx.object_client::<RepositoryDeliveryClient>(ctx.key())
                        .tick_own_expiry(Json(crate::delivery_cancellation::ExpiryTick {
                            generation,
                            upstream_id,
                        }))
                        .send_after((at - now).to_std().unwrap_or_default())
                        .await?;
                }
            }
            Effect::RecordHeld { .. } | Effect::RecordAborted => {}
            Effect::ReleaseUnusedSlot { .. }
            | Effect::ObserveOwnExpiry { .. }
            | Effect::RecordAccepted => {
                return Err(TerminalError::new("unsupported delivery control effect"));
            }
        }
    }
    Ok(())
}

/// Apply a live own-expiry event under this delivery's exclusive owner lock.
pub(crate) async fn expiry_event(
    ctx: &ObjectContext<'_>,
    app: &AppState,
    event: Event,
    now: DateTime<Utc>,
) -> Result<(), TerminalError> {
    let Some(Json(mut retained)) = ctx.get::<Json<Retained>>(STATE).await? else {
        return Ok(());
    };
    let command = ctx.get::<Json<CreateCommand>>("input").await?.map(|v| v.0);
    let snapshot = crate::repository_audit::snapshot(ctx)
        .await?
        .map(Json)
        .map(|v| v.0);
    let facts = facts(ctx, app, command.as_ref(), snapshot.as_ref()).await?;
    let transition = model::reduce(&retained.state, &facts, event, now).map_err(model_error)?;
    retained.state = transition.state;
    ctx.set(STATE, Json(retained.clone()));
    // The owning tick performs the read and records acceptance; the remaining
    // effects schedule another bounded tick or the separately fenced executor.
    let effects = transition
        .effects
        .into_iter()
        .filter(|e| !matches!(e, Effect::ObserveOwnExpiry { .. } | Effect::RecordAccepted))
        .collect();
    apply(
        ctx,
        app,
        &retained,
        command.as_ref(),
        snapshot,
        effects,
        now,
    )
    .await
}

pub(crate) async fn cascade_progress(
    ctx: &restate_sdk::context::SharedObjectContext<'_>,
    query: ghinvite_core::revocation::DeliveryCascadeQuery,
) -> Result<ghinvite_core::revocation::DeliveryCascadeProgress, TerminalError> {
    use ghinvite_core::revocation::{CascadeEntryState as Progress, DeliveryCascadeProgress};
    if query.identity.kind != DeliveryKind::Repository
        || key(&query.identity) != ctx.key()
        || query.required_control_revision == 0
        || query.required_control_revision > ghinvite_core::storage::integer::MAX_EXACT
    {
        return Err(conflict());
    }
    if let Some(Json(command)) = ctx.get::<Json<CreateCommand>>("input").await?
        && !command_matches(&query.identity, &command)
    {
        return Err(conflict());
    }
    let Some(Json(retained)) = ctx.get::<Json<Retained>>(STATE).await? else {
        return Ok(DeliveryCascadeProgress {
            observed_control_revision: 0,
            state: Progress::Pending,
        });
    };
    if retained.state.identity() != &query.identity {
        return Err(conflict());
    }
    let snapshot = crate::repository_audit::shared_snapshot(ctx)
        .await?
        .map(Json)
        .map(|v| v.0);
    let state = if retained.revision < query.required_control_revision {
        Progress::Pending
    } else if snapshot
        .as_ref()
        .is_some_and(|s| s.settlement.is_some() || s.tracking_stopped.is_some())
    {
        Progress::Done
    } else {
        match retained.state.disposition() {
            Disposition::Aborted
            | Disposition::Final
            | Disposition::StopTrackingForeign
            | Disposition::Ready => Progress::Done,
            Disposition::CancelOwned { .. } | Disposition::RecoverUnknown => {
                Progress::WaitingOnGithub
            }
            Disposition::Held { .. } => Progress::Pending,
        }
    };
    Ok(DeliveryCascadeProgress {
        observed_control_revision: retained.revision,
        state,
    })
}
