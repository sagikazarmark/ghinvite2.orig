//! Retained organization and team delivery stages. Explicit M1 composition only.
mod audit;
mod cancel_protocol;
mod cancel_unsent;
pub mod cancellation;
mod control;
mod execution;
mod observation;
pub mod protocol;
mod reissue;
mod retry;
mod telemetry;

use crate::{AppState, delivery_control::DeliveryControlInput};
use chrono::{DateTime, Utc};
use ghinvite_core::{
    budget::{Reserve, SlotId},
    delivery_control as model,
    org_delivery::*,
    storage::org_delivery::OrgDeliveryStorage,
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
use std::{collections::BTreeMap, sync::Arc};

pub struct OrganizationDelivery {
    state: AppState,
    storage: Arc<dyn OrgDeliveryStorage>,
    #[cfg(feature = "integration")]
    faults: Option<Arc<OrganizationFaults>>,
}
pub struct OrganizationProjection {
    storage: Arc<dyn OrgDeliveryStorage>,
    #[cfg(feature = "integration")]
    faults: Option<Arc<OrganizationFaults>>,
}
#[cfg(feature = "integration")]
#[derive(Default)]
pub struct OrganizationFaults {
    pub lose_results: std::sync::atomic::AtomicUsize,
    pub lost_results: std::sync::atomic::AtomicUsize,
    pub lose_cancel_results: std::sync::atomic::AtomicUsize,
    pub lost_cancel_results: std::sync::atomic::AtomicUsize,
    pub projection_unavailable: std::sync::atomic::AtomicBool,
    pub lose_projection_ack: std::sync::atomic::AtomicBool,
    pub pause_after_audit: std::sync::atomic::AtomicBool,
    pub audit_interruptions: std::sync::atomic::AtomicUsize,
}
/// The caller also binds the real InviteBudget and app-scoped installation owner.
/// This binder does not activate organization targets in an M0 composition.
pub fn bind(builder: Builder, state: AppState, storage: Arc<dyn OrgDeliveryStorage>) -> Builder {
    builder
        .bind(OrganizationDelivery {
            state,
            storage: storage.clone(),
            #[cfg(feature = "integration")]
            faults: None,
        })
        .bind(OrganizationProjection {
            storage,
            #[cfg(feature = "integration")]
            faults: None,
        })
}
#[cfg(feature = "integration")]
pub fn bind_with_faults(
    builder: Builder,
    state: AppState,
    storage: Arc<dyn OrgDeliveryStorage>,
    faults: Arc<OrganizationFaults>,
) -> Builder {
    builder
        .bind(OrganizationDelivery {
            state,
            storage: storage.clone(),
            faults: Some(faults.clone()),
        })
        .bind(OrganizationProjection {
            storage,
            faults: Some(faults),
        })
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Completion {
    result: protocol::WriteResult,
    at: DateTime<Utc>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Step {
    ordinal: u64,
    reserve: Option<Reserve>,
    slot: Option<SlotId>,
    may_have_written: bool,
    started_at: Option<DateTime<Utc>>,
    completion: Option<Completion>,
    control: Option<model::State>,
    #[serde(default)]
    unknown_since: Option<DateTime<Utc>>,
    #[serde(default)]
    last_observed_at: Option<DateTime<Utc>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Retained {
    snapshot: OrganizationDeliverySnapshot,
    steps: BTreeMap<String, Step>,
    #[serde(default)]
    last_observed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    retry_not_before: Option<DateTime<Utc>>,
    #[serde(default)]
    audit: Option<audit::AuditState>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Inbox {
    input: DeliveryControlInput,
    state: model::State,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ControlResult {
    pub revision: u64,
    pub initialized: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct ControlReceipt {
    input: DeliveryControlInput,
    result: ControlResult,
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Wake {
    pub generation: u64,
}
fn conflict() -> TerminalError {
    TerminalError::new_with_code(409, "organization delivery identity conflict")
}
fn increment(value: u64) -> Result<u64, TerminalError> {
    value
        .checked_add(1)
        .filter(|v| *v <= ghinvite_core::storage::integer::MAX_EXACT)
        .ok_or_else(|| TerminalError::new("organization delivery revision exhausted"))
}
async fn save(ctx: &ObjectContext<'_>, value: &mut Retained) -> Result<(), TerminalError> {
    if value.snapshot.invitation_timing.is_none()
        && let OrganizationCreateOutcome::Created {
            upstream_id,
            created_at,
        } = value.snapshot.create.outcome
        && let Some(observed_at) = value.snapshot.create.confirmed_at
        && let Some(timing) = ghinvite_core::delivery::InvitationTiming::observed(
            upstream_id,
            created_at,
            observed_at,
            value.snapshot.create.command.approved_at,
            true,
        )
    {
        value.snapshot.retain_invitation_timing(timing);
    }
    if let Some(timing) = &mut value.snapshot.invitation_timing
        && timing.owned_lifetime_seconds.is_none()
        && let Some(inbox) = ctx.get::<Json<Inbox>>("control").await?
    {
        timing.owned_lifetime_seconds = Some(inbox.0.input.invitation_lifetime_seconds);
    }
    value.snapshot.validate().map_err(|_| conflict())?;
    if value.snapshot.create.command.delivery_key() != ctx.key() {
        return Err(conflict());
    }
    // Drain the previous atomic receipt/fact record before replacing it. The
    // outgoing snapshot is the durable predecessor, never uncommitted changes.
    if value.audit.as_ref().is_some_and(|a| !a.pending.is_empty()) {
        let previous = ctx
            .get::<Json<Retained>>("organization")
            .await?
            .ok_or_else(conflict)?
            .0;
        ctx.object_client::<OrganizationProjectionClient>(ctx.key())
            .apply(Json(audit::envelope(&previous)?))
            .send()
            .await?;
        value.audit.as_mut().expect("pending audit").pending.clear();
    }
    audit::prepare(ctx, value).await?;
    ctx.set("organization", Json(value.clone()));
    let command = &value.snapshot.create.command;
    let schedule = ctx.object_client::<crate::sweep_schedule::SweepScheduleClient>(format!(
        "org:{}",
        command.account_id
    ));
    let registration = Json(crate::sweep_schedule::Registration {
        target_id: command.organization.org_id,
        delivery_key: command.delivery_key(),
    });
    if value.snapshot.needs_tracking() {
        schedule.track(registration).send();
    } else {
        schedule.untrack(registration).send();
    }
    Ok(())
}
async fn project(ctx: &ObjectContext<'_>, value: &mut Retained) -> Result<(), TerminalError> {
    value.snapshot.validate().map_err(|_| conflict())?;
    let command = &value.snapshot.create.command;
    if command.delivery_key() != ctx.key() {
        return Err(conflict());
    }
    let projection = audit::envelope(value)?;
    if ctx
        .get::<bool>("access_grants_enabled")
        .await?
        .unwrap_or(false)
    {
        for grant in ghinvite_core::access_grant::accepted_organization(&value.snapshot)
            .map_err(|_| conflict())?
        {
            ctx.object_client::<crate::access_grant::AccessGrantClient>(grant.key())
                .schedule(Json(grant))
                .send()
                .await?;
        }
    }
    if !ghinvite_core::request_completion::organization(&value.snapshot).is_empty() {
        ctx.object_client::<crate::request_owner::InvitationRequestClient>(
            command.request_id.to_string(),
        )
        .on_delivery_settled(Json(ghinvite_core::request_completion::DeliveryNotice {
            kind: model::DeliveryKind::Organization,
            target_id: command.organization.org_id,
            delivery_generation: command.delivery_generation,
        }))
        .send();
    }
    ctx.object_client::<OrganizationProjectionClient>(ctx.key())
        .apply(Json(projection))
        .send()
        .await?;
    if let Some(audit) = &mut value.audit {
        audit.pending.clear();
        ctx.set("organization", Json(value.clone()));
    }
    telemetry::send(ctx, value).await?;
    Ok(())
}

fn attempt(value: &Retained, step: &str) -> OrganizationAttempt {
    OrganizationAttempt {
        create: value.snapshot.create.command.clone(),
        action: if step == "org" {
            OrganizationWrite::Invite
        } else {
            OrganizationWrite::Team {
                team_id: step
                    .strip_prefix("team:")
                    .expect("known step")
                    .parse()
                    .expect("known numeric team"),
            }
        },
    }
}
fn set_outcome(
    value: &mut Retained,
    outcome: protocol::StepResult,
    now: DateTime<Utc>,
) -> Result<(), TerminalError> {
    match outcome {
        protocol::StepResult::Organization(outcome) => {
            if value.snapshot.create.outcome.confirmed() || value.snapshot.create.outcome == outcome
            {
                return Ok(());
            }
            if outcome == OrganizationCreateOutcome::OutcomeUnknown {
                value
                    .steps
                    .get_mut("org")
                    .ok_or_else(conflict)?
                    .unknown_since
                    .get_or_insert(now);
            }
            value.snapshot.create.confirmed_at = outcome.confirmed().then_some(now);
            value.snapshot.create.outcome = outcome;
            value.snapshot.create.revision = increment(value.snapshot.create.revision)?;
        }
        protocol::StepResult::Team(_) => {
            return Err(TerminalError::new("team result requires step identity"));
        }
    }
    value.snapshot.revision = increment(value.snapshot.revision)?;
    Ok(())
}
fn set_step(
    value: &mut Retained,
    step: &str,
    outcome: protocol::StepResult,
    now: DateTime<Utc>,
) -> Result<(), TerminalError> {
    if step == "org" {
        return set_outcome(value, outcome, now);
    }
    let protocol::StepResult::Team(outcome) = outcome else {
        return Err(conflict());
    };
    let id = step
        .strip_prefix("team:")
        .and_then(|s| s.parse().ok())
        .ok_or_else(conflict)?;
    let team = value.snapshot.teams.get_mut(&id).ok_or_else(conflict)?;
    if team.create.outcome.confirmed() || team.create.outcome == outcome {
        return Ok(());
    }
    if outcome == TeamCreateOutcome::OutcomeUnknown {
        value
            .steps
            .get_mut(step)
            .ok_or_else(conflict)?
            .unknown_since
            .get_or_insert(now);
    }
    team.create.confirmed_at = outcome.confirmed().then_some(now);
    team.create.outcome = outcome;
    team.create.revision = increment(team.create.revision)?;
    team.revision = increment(team.revision)?;
    value.snapshot.revision = increment(value.snapshot.revision)?;
    Ok(())
}
fn confirmed(value: &Retained, step: &str) -> bool {
    if step == "org" {
        value.snapshot.create.outcome.confirmed()
    } else {
        value.snapshot.teams[&step[5..].parse::<u64>().expect("known step")]
            .create
            .outcome
            .confirmed()
    }
}
#[restate_sdk::object]
impl OrganizationDelivery {
    #[handler]
    async fn create(
        &self,
        ctx: ObjectContext<'_>,
        Json(command): Json<OrganizationCreateCommand>,
    ) -> Result<Json<OrganizationDeliverySnapshot>, TerminalError> {
        command
            .validate()
            .map_err(|_| TerminalError::new_with_code(400, "invalid organization delivery"))?;
        if ctx.key() != command.delivery_key() {
            return Err(conflict());
        }
        if self.state.invite_budget.is_none() || self.state.github_apps.is_none() {
            return Err(TerminalError::new_with_code(
                503,
                "organization delivery unavailable",
            ));
        }
        if self.state.access_grants.is_some() {
            ctx.set("access_grants_enabled", true);
        }
        let mut retained = match ctx.get::<Json<Retained>>("organization").await? {
            Some(Json(value)) if value.snapshot.create.command == command => value,
            Some(_) => return Err(conflict()),
            None => {
                let inbox = ctx
                    .get::<Json<Inbox>>("control")
                    .await?
                    .ok_or_else(|| {
                        TerminalError::new_with_code(409, "initial organization control required")
                    })?
                    .0;
                if !control::matches(&inbox.input, &command) {
                    return Err(conflict());
                }
                let mut snapshot =
                    OrganizationDeliverySnapshot::planned(command).map_err(|_| conflict())?;
                snapshot.create.audit_version = ghinvite_core::delivery_audit::new_owner_version(
                    snapshot.create.command.invitation_id,
                );
                let mut steps = BTreeMap::from([("org".to_owned(), Step::default())]);
                for id in snapshot.teams.keys() {
                    steps.insert(format!("team:{id}"), Step::default());
                }
                let mut value = Retained {
                    snapshot,
                    steps,
                    last_observed_at: None,
                    retry_not_before: None,
                    audit: None,
                };
                save(&ctx, &mut value).await?;
                value
            }
        };
        self.drive(&ctx, &mut retained).await?;
        Ok(Json(retained.snapshot))
    }
    #[handler]
    async fn cancel_unsent(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<ghinvite_core::bulk_cancel::OrganizationCancelUnsent>,
    ) -> Result<Json<ghinvite_core::bulk_cancel::CancelUnsentResult>, TerminalError> {
        cancel_unsent::execute(self, &ctx, input).await.map(Json)
    }
    #[handler]
    async fn control(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<DeliveryControlInput>,
    ) -> Result<Json<ControlResult>, TerminalError> {
        control::apply(self, &ctx, input).await.map(Json)
    }
    #[handler]
    async fn recheck(
        &self,
        ctx: ObjectContext<'_>,
        Json(wake): Json<Wake>,
    ) -> Result<(), TerminalError> {
        if ctx.get::<u64>("scheduled_wake").await? != Some(wake.generation) {
            return Ok(());
        }
        ctx.clear("scheduled_wake");
        ctx.clear("wake_due");
        if let Some(Json(mut value)) = ctx.get::<Json<Retained>>("organization").await? {
            self.drive(&ctx, &mut value).await?;
        }
        Ok(())
    }
    /// Capacity hints never establish a free seat. They only release the daily
    /// NoSeat sleep so the ordinary S0/S1/S3/S4 protocol can check again.
    #[handler]
    async fn capacity_changed(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<ghinvite_core::org_eligibility::MemberRemoved>,
    ) -> Result<(), TerminalError> {
        input.validate().map_err(|_| conflict())?;
        let Some(Json(mut value)) = ctx.get::<Json<Retained>>("organization").await? else {
            return Ok(());
        };
        let command = &value.snapshot.create.command;
        if command.delivery_key() != ctx.key()
            || command.account_id != input.account_id
            || command.installation_id != input.installation_id
        {
            return Ok(());
        }
        if !retry::no_seat(&value) {
            return Ok(());
        }
        value.retry_not_before = None;
        ctx.clear("scheduled_wake");
        ctx.clear("wake_due");
        save(&ctx, &mut value).await?;
        self.drive(&ctx, &mut value).await
    }
    #[handler]
    async fn observe_listed(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<ghinvite_core::org_observation::OrganizationObservation>,
    ) -> Result<(), TerminalError> {
        observation::listed(self, &ctx, input).await
    }
    #[handler]
    async fn refresh(
        &self,
        ctx: ObjectContext<'_>,
    ) -> Result<Json<OrganizationRefresh>, TerminalError> {
        let Some(Json(value)) = ctx.get::<Json<Retained>>("organization").await? else {
            return Ok(Json(OrganizationRefresh::NotTracked));
        };
        if !value.snapshot.needs_tracking() {
            return Ok(Json(OrganizationRefresh::NotTracked));
        }
        let now = crate::clock::now(&ctx, "organization_refresh_time").await?;
        if let Some(Json(at)) = ctx
            .get::<Json<DateTime<Utc>>>("manual_refreshed_at")
            .await?
            && now < at + chrono::Duration::seconds(60)
        {
            return Ok(Json(OrganizationRefresh::Throttled {
                retry_at: at + chrono::Duration::seconds(60),
            }));
        }
        ctx.set("manual_refreshed_at", Json(now));
        let command = &value.snapshot.create.command;
        ctx.service_client::<crate::org_reconcile::OrganizationReconcileClient>()
            .target(Json(crate::reconcile::TargetInput {
                account: ghinvite_core::account::AccountInstallationRef {
                    app: ghinvite_core::account::App::Org,
                    account_id: command.account_id,
                },
                target_id: command.organization.org_id,
                deliveries: vec![command.delivery_key()],
            }))
            .send()
            .await?;
        Ok(Json(OrganizationRefresh::Requested))
    }
    #[handler]
    async fn on_webhook(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<ghinvite_core::org_webhook::OrganizationWebhookWake>,
    ) -> Result<(), TerminalError> {
        input.validate().map_err(|_| conflict())?;
        let Some(Json(value)) = ctx.get::<Json<Retained>>("organization").await? else {
            return Ok(());
        };
        let command = &value.snapshot.create.command;
        if command.delivery_key() != ctx.key()
            || input.identity.link_id != command.link_id
            || input.identity.request_id != command.request_id
            || input.identity.target_id != command.organization.org_id
            || input.identity.delivery_generation != command.delivery_generation
            || input.account_id != command.account_id
            || input.installation_id != command.installation_id
            || input.requester_id != command.requester_id
            || input
                .team_id
                .is_some_and(|id| !value.snapshot.teams.contains_key(&id))
        {
            return Err(conflict());
        }
        if value.snapshot.needs_tracking() {
            ctx.service_client::<crate::org_reconcile::OrganizationReconcileClient>()
                .target(Json(crate::reconcile::TargetInput {
                    account: ghinvite_core::account::AccountInstallationRef {
                        app: ghinvite_core::account::App::Org,
                        account_id: command.account_id,
                    },
                    target_id: command.organization.org_id,
                    deliveries: vec![command.delivery_key()],
                }))
                .send()
                .await?;
        }
        Ok(())
    }
    #[handler]
    async fn prepare_reissue(
        &self,
        ctx: ObjectContext<'_>,
        Json(query): Json<ghinvite_core::reissue::OrganizationReissueBarrierQuery>,
    ) -> Result<Json<ghinvite_core::reissue::InvitationBarrier>, TerminalError> {
        reissue::prepare(self, &ctx, query).await.map(Json)
    }
    #[handler]
    async fn drive_cancellation(&self, ctx: ObjectContext<'_>) -> Result<(), TerminalError> {
        cancellation::drive(self, &ctx).await
    }
    #[handler]
    async fn tick_own_expiry(
        &self,
        ctx: ObjectContext<'_>,
        Json(tick): Json<cancellation::ExpiryTick>,
    ) -> Result<(), TerminalError> {
        cancellation::tick(self, &ctx, tick).await
    }
    #[handler]
    async fn status(
        &self,
        ctx: SharedObjectContext<'_>,
    ) -> Result<Json<Option<OrganizationDeliverySnapshot>>, TerminalError> {
        Ok(Json(
            ctx.get::<Json<Retained>>("organization")
                .await?
                .map(|v| v.0.snapshot),
        ))
    }
    #[handler]
    async fn cascade_progress(
        &self,
        ctx: SharedObjectContext<'_>,
        Json(query): Json<ghinvite_core::revocation::DeliveryCascadeQuery>,
    ) -> Result<Json<ghinvite_core::revocation::DeliveryCascadeProgress>, TerminalError> {
        control::progress(&ctx, query).await.map(Json)
    }
}
#[restate_sdk::object]
impl OrganizationProjection {
    #[handler]
    async fn telemetry(
        &self,
        ctx: ObjectContext<'_>,
        Json(value): Json<
            ghinvite_core::storage::organization_telemetry::OrganizationStepTelemetry,
        >,
    ) -> Result<(), TerminalError> {
        if value.key() != format!("org:{}", ctx.key()) {
            return Err(conflict());
        }
        ctx.run(|| async {
            self.storage
                .project_organization_telemetry(&value)
                .await
                .map_err(HandlerError::from)
        })
        .name("project_organization_telemetry")
        .await?;
        Ok(())
    }
    #[handler]
    async fn apply(
        &self,
        ctx: ObjectContext<'_>,
        Json(projection): Json<ghinvite_core::delivery_audit::OrganizationDeliveryProjection>,
    ) -> Result<(), TerminalError> {
        let snapshot = &projection.snapshot;
        if snapshot.create.command.delivery_key() != ctx.key() {
            return Err(conflict());
        }
        snapshot.validate().map_err(|_| conflict())?;
        projection.events().map_err(|_| conflict())?;
        ctx.run(|| async {
            #[cfg(feature = "integration")]
            if self.faults.as_ref().is_some_and(|faults| {
                faults
                    .projection_unavailable
                    .load(std::sync::atomic::Ordering::SeqCst)
            }) {
                return Err(HandlerError::from(std::io::Error::other(
                    "fixture organization projection unavailable",
                )));
            }
            self.storage
                .project_organization_delivery_with_events(&projection)
                .await
                .map_err(HandlerError::from)?;
            #[cfg(feature = "integration")]
            if self.faults.as_ref().is_some_and(|faults| {
                faults
                    .lose_projection_ack
                    .swap(false, std::sync::atomic::Ordering::SeqCst)
            }) {
                return Err(HandlerError::from(std::io::Error::other(
                    "fixture organization projection acknowledgement lost",
                )));
            }
            Ok::<_, HandlerError>(())
        })
        .name("project_organization_delivery")
        .await?;
        Ok(())
    }
}
