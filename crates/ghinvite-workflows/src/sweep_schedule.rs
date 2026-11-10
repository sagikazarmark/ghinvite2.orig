//! One retained polling chain per acting app and account. SQL is never its work set.
use chrono::{DateTime, Utc};
use ghinvite_core::account::{AccountInstallationRef, App};
use restate_sdk::{
    context::{
        ContextClient, ContextReadState, ContextWriteState, ObjectContext, SharedObjectContext,
    },
    errors::TerminalError,
    serde::Json,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const ACTIVE_SECONDS: u64 = 60 * 60;
pub const IDLE_SECONDS: u64 = 24 * ACTIVE_SECONDS;
pub type Tracked = BTreeMap<u64, BTreeSet<String>>;

#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct Registration {
    pub target_id: u64,
    pub delivery_key: String,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct Schedule {
    pub tracked: Tracked,
    pub token: u64,
    pub next_run: Option<DateTime<Utc>>,
    pub cadence_seconds: u64,
    pub completed_runs: u64,
}
#[derive(Default)]
pub struct SweepSchedule {
    pub organization_enabled: bool,
    #[cfg(feature = "integration")]
    pub clock: Option<std::sync::Arc<std::sync::atomic::AtomicI64>>,
}
fn account(key: &str, organization_enabled: bool) -> Result<AccountInstallationRef, TerminalError> {
    let account: AccountInstallationRef = key
        .parse()
        .map_err(|_| TerminalError::new_with_code(400, "invalid sweep account"))?;
    if account.app == App::Org && !organization_enabled {
        return Err(TerminalError::new_with_code(
            400,
            "organization sweeps are disabled",
        ));
    }
    Ok(account)
}
fn registration(input: &Registration) -> Result<(), TerminalError> {
    if let Some(key) = input.delivery_key.strip_prefix("email:") {
        let parts = key.split(':').collect::<Vec<_>>();
        if input.target_id > 0
            && parts.len() == 3
            && parts[0].parse::<ghinvite_core::bulk::BulkBatchId>().is_ok()
            && parts[1]
                .parse::<u16>()
                .is_ok_and(|r| (1..=200).contains(&r))
            && parts[2].parse::<u64>().ok() == Some(input.target_id)
        {
            return Ok(());
        }
        return Err(TerminalError::new_with_code(
            400,
            "invalid email sweep registration",
        ));
    }
    let parts = input.delivery_key.split(':').collect::<Vec<_>>();
    if input.target_id == 0
        || parts.len() != 3
        || parts[0].parse::<ghinvite_core::RequestId>().is_err()
        || parts[1].parse::<u64>().ok() != Some(input.target_id)
        || !parts[2].parse::<u64>().is_ok_and(|v| v > 0)
    {
        return Err(TerminalError::new_with_code(
            400,
            "invalid sweep registration",
        ));
    }
    Ok(())
}
impl SweepSchedule {
    async fn live(
        &self,
        ctx: &ObjectContext<'_>,
        input: &Registration,
    ) -> Result<bool, TerminalError> {
        let owner = account(ctx.key(), self.organization_enabled)?;
        registration(input)?;
        if let Some(key) = input.delivery_key.strip_prefix("email:") {
            if owner.app != App::Org {
                return Err(TerminalError::new_with_code(
                    400,
                    "email sweep account mismatch",
                ));
            }
            let Json(snapshot) = ctx
                .object_client::<crate::email_delivery::EmailInviteDeliveryClient>(key)
                .status()
                .call()
                .await?;
            let Some(snapshot) = snapshot else {
                return Ok(false);
            };
            if snapshot.command.account_id != owner.account_id
                || snapshot.command.organization.org_id != input.target_id
                || snapshot.command.delivery_key() != key
            {
                return Err(TerminalError::new_with_code(
                    400,
                    "email sweep owner mismatch",
                ));
            }
            return Ok(snapshot.settlement.is_none()
                && matches!(
                    snapshot.outcome,
                    ghinvite_core::email_delivery::EmailCreateOutcome::Created { .. }
                        | ghinvite_core::email_delivery::EmailCreateOutcome::OutcomeUnknown
                ));
        }
        if owner.app == App::Org {
            let Json(snapshot) = ctx
                .object_client::<crate::org_delivery::OrganizationDeliveryClient>(
                    &input.delivery_key,
                )
                .status()
                .call()
                .await?;
            let Some(snapshot) = snapshot else {
                return Ok(false);
            };
            let command = &snapshot.create.command;
            if command.account_id != owner.account_id
                || command.organization.org_id != input.target_id
                || command.delivery_key() != input.delivery_key
            {
                return Err(TerminalError::new_with_code(
                    400,
                    "organization sweep registration owner mismatch",
                ));
            }
            return Ok(snapshot.needs_tracking());
        }
        let Json(snapshot) = ctx
            .object_client::<crate::delivery::RepositoryDeliveryClient>(&input.delivery_key)
            .status()
            .call()
            .await?;
        let Some(snapshot) = snapshot else {
            return Ok(false);
        };
        let command = &snapshot.create.command;
        if command.account_id != owner.account_id
            || command.repo_id != input.target_id
            || command.delivery_key() != input.delivery_key
        {
            return Err(TerminalError::new_with_code(
                400,
                "sweep registration owner mismatch",
            ));
        }
        Ok(snapshot.settlement.is_none()
            && snapshot.tracking_stopped.is_none()
            && (snapshot.create.outcome.needs_settlement()
                || matches!(
                    snapshot.create.outcome,
                    ghinvite_core::delivery::CreateOutcome::OutcomeUnknown
                )))
    }
    async fn update(
        &self,
        ctx: &ObjectContext<'_>,
        input: Registration,
    ) -> Result<(), TerminalError> {
        let live = self.live(ctx, &input).await?;
        let mut state = ctx
            .get::<Json<Schedule>>("schedule")
            .await?
            .map(|v| v.0)
            .unwrap_or_default();
        if live {
            state.cadence_seconds = ACTIVE_SECONDS;
            state
                .tracked
                .entry(input.target_id)
                .or_default()
                .insert(input.delivery_key);
            let now = self.now(ctx, "track_time").await?;
            if state
                .next_run
                .is_none_or(|at| at > now + chrono::Duration::seconds(ACTIVE_SECONDS as i64))
            {
                return self.schedule(ctx, &mut state, now, ACTIVE_SECONDS).await;
            }
        } else if let Some(keys) = state.tracked.get_mut(&input.target_id) {
            keys.remove(&input.delivery_key);
            if keys.is_empty() {
                state.tracked.remove(&input.target_id);
            }
        }
        ctx.set("schedule", Json(state));
        Ok(())
    }
    async fn now(
        &self,
        ctx: &ObjectContext<'_>,
        name: &'static str,
    ) -> Result<DateTime<Utc>, TerminalError> {
        #[cfg(feature = "integration")]
        if let Some(clock) = &self.clock {
            return crate::clock::with_clock(ctx, name, || {
                DateTime::from_timestamp_millis(clock.load(std::sync::atomic::Ordering::SeqCst))
                    .expect("fixture clock")
            })
            .await;
        }
        crate::clock::now(ctx, name).await
    }
    async fn schedule(
        &self,
        ctx: &ObjectContext<'_>,
        state: &mut Schedule,
        now: DateTime<Utc>,
        seconds: u64,
    ) -> Result<(), TerminalError> {
        state.token = state
            .token
            .checked_add(1)
            .filter(|v| *v <= 9_007_199_254_740_991)
            .ok_or_else(|| TerminalError::new("sweep token exhausted"))?;
        state.next_run = Some(now + chrono::Duration::seconds(seconds as i64));
        state.cadence_seconds = seconds;
        ctx.set("schedule", Json(state.clone()));
        ctx.object_client::<SweepScheduleClient>(ctx.key())
            .run(state.token)
            .send_after(std::time::Duration::from_secs(seconds))
            .await?;
        Ok(())
    }
}
#[restate_sdk::object]
impl SweepSchedule {
    #[handler]
    async fn ensure_scheduled(&self, ctx: ObjectContext<'_>) -> Result<(), TerminalError> {
        account(ctx.key(), self.organization_enabled)?;
        let mut state = ctx
            .get::<Json<Schedule>>("schedule")
            .await?
            .map(|v| v.0)
            .unwrap_or_default();
        if state.next_run.is_none() {
            let now = self.now(&ctx, "schedule_time").await?;
            let seconds = if state.tracked.is_empty() {
                IDLE_SECONDS
            } else {
                ACTIVE_SECONDS
            };
            self.schedule(&ctx, &mut state, now, seconds).await?;
        }
        Ok(())
    }
    #[handler]
    async fn track(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<Registration>,
    ) -> Result<(), TerminalError> {
        self.update(&ctx, input).await
    }
    #[handler]
    async fn untrack(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<Registration>,
    ) -> Result<(), TerminalError> {
        self.update(&ctx, input).await
    }
    #[handler]
    async fn run(&self, ctx: ObjectContext<'_>, token: u64) -> Result<(), TerminalError> {
        let account = account(ctx.key(), self.organization_enabled)?;
        let Some(Json(mut state)) = ctx.get::<Json<Schedule>>("schedule").await? else {
            return Ok(());
        };
        if state.token != token || state.next_run.is_none() {
            return Ok(());
        }
        let now = self.now(&ctx, "sweep_time").await?;
        if state.next_run.is_some_and(|at| at > now) {
            return Ok(());
        }
        ctx.service_client::<crate::reconcile::ReconcileClient>()
            .sweep(Json(crate::reconcile::SweepInput {
                account,
                token,
                tracked: state.tracked.clone(),
            }))
            .call()
            .await?;
        state.completed_runs = state
            .completed_runs
            .checked_add(1)
            .ok_or_else(|| TerminalError::new("sweep count exhausted"))?;
        let now = self.now(&ctx, "schedule_time").await?;
        let seconds = if state.tracked.is_empty() {
            IDLE_SECONDS
        } else {
            ACTIVE_SECONDS
        };
        self.schedule(&ctx, &mut state, now, seconds).await
    }
    #[handler]
    async fn status(
        &self,
        ctx: SharedObjectContext<'_>,
    ) -> Result<Json<Option<Schedule>>, TerminalError> {
        account(ctx.key(), self.organization_enabled)?;
        Ok(Json(
            ctx.get::<Json<Schedule>>("schedule").await?.map(|v| v.0),
        ))
    }
}
