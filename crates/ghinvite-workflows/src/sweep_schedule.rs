//! One retained polling chain per acting app and account. SQL is never its work set.
//!
//! The chain is the only scheduler (#86): hourly while it tracks work waiting on
//! GitHub, daily otherwise. The web's hourly watchdog only calls
//! `ensure_scheduled`, which re-arms a missing or overdue chain with one
//! catch-up run. Bounds, throttling deferral and failure accounting are retained
//! here so that every trigger stays an idempotent no-op on a healthy chain.
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
/// A due run that has not started this long after its time is overdue: its
/// delayed send was lost with a failed run or the runtime was down.
pub const OVERDUE_GRACE_SECONDS: u64 = 15 * 60;
/// Targets (repository or organization walks) one run fans out. A larger
/// sweep set rotates through the rest on later runs.
pub const MAX_TARGETS_PER_RUN: usize = 25;
/// Listing pages one target walk may read. A longer walk is incomplete and
/// supplies no evidence. With [`MAX_TARGETS_PER_RUN`] this caps a run's listing
/// pages at 1,000 (each organization walk reads two listings).
pub const MAX_WALK_PAGES: usize = 40;
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
    /// Last target the previous run fanned out, when the sweep set was larger
    /// than [`MAX_TARGETS_PER_RUN`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<u64>,
    #[serde(default)]
    pub health: Health,
}

/// Retained run accounting. Reports name the run token they belong to, so
/// duplicate or late reports cannot count twice or touch a newer run.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
pub struct Health {
    /// Token of the run that most recently fanned out.
    #[serde(default)]
    pub last_run_token: Option<u64>,
    /// Runs whose fan-out or at least one walk failed without throttling.
    #[serde(default)]
    pub failed_runs: u64,
    #[serde(default)]
    pub last_failed_token: Option<u64>,
    #[serde(default)]
    pub last_failed_at: Option<DateTime<Utc>>,
    /// Runs GitHub throttled; these defer, they do not fail.
    #[serde(default)]
    pub throttled_runs: u64,
    #[serde(default)]
    pub last_throttled_token: Option<u64>,
    /// Consecutive throttled runs: the backoff exponent.
    #[serde(default)]
    pub deferrals: u32,
    /// Registrations cannot pull the next run before this.
    #[serde(default)]
    pub deferred_until: Option<DateTime<Utc>>,
    /// Overdue chains re-armed by `ensure_scheduled`.
    #[serde(default)]
    pub rearmed_runs: u64,
}

/// A target walk that produced no evidence, reported to the schedule by the
/// one-way fan-out. Successful walks report nothing.
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct WalkReport {
    pub token: u64,
    pub target_id: u64,
    pub outcome: WalkOutcome,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WalkOutcome {
    /// A read failed or was incomplete (including the page cap).
    Failed,
    /// GitHub throttled the read; `retry_after_seconds` is its guidance,
    /// unclamped (0 when GitHub named no wait).
    Throttled { retry_after_seconds: u64 },
}

/// What a check concludes about one retained chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Assessment {
    /// Never armed.
    Missing,
    Scheduled {
        due: DateTime<Utc>,
    },
    /// Due more than [`OVERDUE_GRACE_SECONDS`] ago and not yet run.
    Overdue {
        due: DateTime<Utc>,
    },
}

pub fn assess(schedule: Option<&Schedule>, now: DateTime<Utc>) -> Assessment {
    match schedule.and_then(|s| s.next_run) {
        None => Assessment::Missing,
        Some(due) if now > due + chrono::Duration::seconds(OVERDUE_GRACE_SECONDS as i64) => {
            Assessment::Overdue { due }
        }
        Some(due) => Assessment::Scheduled { due },
    }
}

/// Whether the chain recorded a failed sweep inside the reporting window.
pub fn recent_failure(schedule: &Schedule, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    schedule.health.last_failed_at.filter(|at| {
        now - *at <= chrono::Duration::seconds(ghinvite_core::sweep_health::FAILURE_WINDOW_SECONDS)
    })
}

/// At most `max` targets, starting after `cursor` and wrapping, with the cursor
/// for the next run. A set that fits needs no cursor. Target IDs are positive.
pub fn select_targets(
    tracked: &Tracked,
    cursor: Option<u64>,
    max: usize,
) -> (Tracked, Option<u64>) {
    if tracked.len() <= max {
        return (tracked.clone(), None);
    }
    let start = cursor.unwrap_or(0);
    let selected = tracked
        .range((std::ops::Bound::Excluded(start), std::ops::Bound::Unbounded))
        .chain(tracked.range(..=start))
        .take(max)
        .map(|(k, v)| (*k, v.clone()))
        .collect::<Vec<_>>();
    let next = selected.last().map(|(k, _)| *k);
    (selected.into_iter().collect(), next)
}

/// Exponential backoff over consecutive throttled runs: one cadence, then
/// doubling, never below GitHub's guidance and never past the idle cadence.
pub fn deferral_seconds(deferrals: u32, retry_after_seconds: u64) -> u64 {
    let exponent = deferrals.saturating_sub(1).min(16);
    ACTIVE_SECONDS
        .saturating_mul(1 << exponent)
        .max(retry_after_seconds)
        .min(IDLE_SECONDS)
}

fn cadence(state: &Schedule) -> u64 {
    if state.tracked.is_empty() {
        IDLE_SECONDS
    } else {
        ACTIVE_SECONDS
    }
}
fn increment(value: u64, what: &'static str) -> Result<u64, TerminalError> {
    value
        .checked_add(1)
        .filter(|v| *v <= 9_007_199_254_740_991)
        .ok_or_else(|| TerminalError::new(what))
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
            // Pull forward to the active cadence, but never inside a throttling
            // deferral: new work does not buy back GitHub's quota.
            let earliest = (now + chrono::Duration::seconds(ACTIVE_SECONDS as i64))
                .max(state.health.deferred_until.unwrap_or(now));
            if state.next_run.is_none_or(|at| at > earliest) {
                let seconds = u64::try_from((earliest - now).num_seconds()).unwrap_or(0);
                return self.arm(ctx, &mut state, now, seconds).await;
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
    /// Run on the cadence `seconds`.
    async fn schedule(
        &self,
        ctx: &ObjectContext<'_>,
        state: &mut Schedule,
        now: DateTime<Utc>,
        seconds: u64,
    ) -> Result<(), TerminalError> {
        state.cadence_seconds = seconds;
        self.arm(ctx, state, now, seconds).await
    }
    /// Replace the chain's one live token with a run `seconds` from now. Every
    /// older delayed run becomes inert.
    async fn arm(
        &self,
        ctx: &ObjectContext<'_>,
        state: &mut Schedule,
        now: DateTime<Utc>,
        seconds: u64,
    ) -> Result<(), TerminalError> {
        state.token = increment(state.token, "sweep token exhausted")?;
        state.next_run = Some(now + chrono::Duration::seconds(seconds as i64));
        ctx.set("schedule", Json(state.clone()));
        let run = ctx
            .object_client::<SweepScheduleClient>(ctx.key())
            .run(state.token);
        if seconds == 0 {
            run.send().await?;
        } else {
            run.send_after(std::time::Duration::from_secs(seconds))
                .await?;
        }
        Ok(())
    }
}
#[restate_sdk::object]
impl SweepSchedule {
    /// Idempotent: arms a missing chain, re-arms an overdue one with exactly one
    /// immediate catch-up run, and leaves a scheduled chain unchanged. Onboarding
    /// and the web's hourly watchdog both call it, any number of times.
    #[handler]
    async fn ensure_scheduled(&self, ctx: ObjectContext<'_>) -> Result<(), TerminalError> {
        account(ctx.key(), self.organization_enabled)?;
        let mut state = ctx
            .get::<Json<Schedule>>("schedule")
            .await?
            .map(|v| v.0)
            .unwrap_or_default();
        let now = self.now(&ctx, "schedule_time").await?;
        match assess(Some(&state), now) {
            Assessment::Missing => {
                let seconds = cadence(&state);
                self.schedule(&ctx, &mut state, now, seconds).await?;
            }
            Assessment::Overdue { .. } => {
                // However many runs were missed, the replacement catches up once;
                // its successor returns to the ordinary cadence.
                state.health.rearmed_runs =
                    increment(state.health.rearmed_runs, "sweep re-arm count exhausted")?;
                if state.cadence_seconds == 0 {
                    state.cadence_seconds = cadence(&state);
                }
                self.arm(&ctx, &mut state, now, 0).await?;
            }
            Assessment::Scheduled { .. } => {}
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
        // Consecutive throttling grows the backoff; a run after an unthrottled
        // one starts over. A due run has outlived any deferral.
        if state.health.last_throttled_token.is_none()
            || state.health.last_throttled_token != state.health.last_run_token
        {
            state.health.deferrals = 0;
        }
        state.health.deferred_until = None;
        state.health.last_run_token = Some(token);
        let (tracked, cursor) = select_targets(&state.tracked, state.cursor, MAX_TARGETS_PER_RUN);
        state.cursor = cursor;
        let sweep = ctx
            .service_client::<crate::reconcile::ReconcileClient>()
            .sweep(Json(crate::reconcile::SweepInput {
                account,
                token,
                tracked,
            }))
            .call()
            .await;
        if sweep.is_err() {
            // A refused fan-out is a failed sweep, not the end of the chain.
            tracing::warn!("scheduled sweep fan-out failed");
            state.health.failed_runs =
                increment(state.health.failed_runs, "sweep failure count exhausted")?;
            state.health.last_failed_token = Some(token);
            state.health.last_failed_at = Some(now);
        }
        state.completed_runs = state
            .completed_runs
            .checked_add(1)
            .ok_or_else(|| TerminalError::new("sweep count exhausted"))?;
        let now = self.now(&ctx, "schedule_time").await?;
        let seconds = cadence(&state);
        self.schedule(&ctx, &mut state, now, seconds).await
    }
    /// A walk of run `token` produced no evidence. Failures count once per run;
    /// throttling defers the chain once per run with exponential backoff.
    #[handler]
    async fn report(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<WalkReport>,
    ) -> Result<(), TerminalError> {
        account(ctx.key(), self.organization_enabled)?;
        let Some(Json(mut state)) = ctx.get::<Json<Schedule>>("schedule").await? else {
            return Ok(());
        };
        if input.target_id == 0 || state.health.last_run_token != Some(input.token) {
            return Ok(());
        }
        match input.outcome {
            WalkOutcome::Failed => {
                if state.health.last_failed_token == Some(input.token) {
                    return Ok(());
                }
                let now = self.now(&ctx, "report_time").await?;
                state.health.failed_runs =
                    increment(state.health.failed_runs, "sweep failure count exhausted")?;
                state.health.last_failed_token = Some(input.token);
                state.health.last_failed_at = Some(now);
                ctx.set("schedule", Json(state));
            }
            WalkOutcome::Throttled {
                retry_after_seconds,
            } => {
                if state.health.last_throttled_token == Some(input.token) {
                    return Ok(());
                }
                let now = self.now(&ctx, "report_time").await?;
                state.health.throttled_runs = increment(
                    state.health.throttled_runs,
                    "sweep throttle count exhausted",
                )?;
                state.health.last_throttled_token = Some(input.token);
                state.health.deferrals = state.health.deferrals.saturating_add(1);
                let seconds = deferral_seconds(state.health.deferrals, retry_after_seconds);
                let until = now + chrono::Duration::seconds(seconds as i64);
                state.health.deferred_until = Some(until);
                if state.next_run.is_some_and(|at| at < until) {
                    return self.arm(&ctx, &mut state, now, seconds).await;
                }
                ctx.set("schedule", Json(state));
            }
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    fn tracked(ids: impl IntoIterator<Item = u64>) -> Tracked {
        ids.into_iter()
            .map(|id| (id, BTreeSet::from([format!("delivery-{id}")])))
            .collect()
    }
    #[test]
    fn large_sweep_sets_rotate_through_every_target_within_the_per_run_cap() {
        let all = tracked(1..=7);
        assert_eq!(select_targets(&all, None, 7), (all.clone(), None));
        assert_eq!(
            select_targets(&all, Some(3), 10).0,
            all,
            "a fitting set ignores the cursor"
        );
        let mut cursor = None;
        let mut seen = Vec::new();
        for _ in 0..3 {
            let (batch, next) = select_targets(&all, cursor, 3);
            assert_eq!(batch.len(), 3);
            seen.extend(batch.keys().copied());
            cursor = next;
        }
        assert_eq!(seen.iter().copied().collect::<BTreeSet<_>>().len(), 7);
        assert_eq!(&seen[..3], &[1, 2, 3]);
        assert_eq!(&seen[3..6], &[4, 5, 6]);
        assert_eq!(cursor, Some(2), "the third run wraps to 7, 1 and 2");
        // A removed cursor target still resumes after its position.
        let shrunk = tracked([1, 2, 5, 6, 7]);
        assert_eq!(
            select_targets(&shrunk, Some(4), 2),
            (tracked([5, 6]), Some(6))
        );
    }
    #[test]
    fn missing_scheduled_and_overdue_chains_are_told_apart_with_grace() {
        let now: DateTime<Utc> = "2026-11-24T12:05:00Z".parse().unwrap();
        assert_eq!(assess(None, now), Assessment::Missing);
        assert_eq!(assess(Some(&Schedule::default()), now), Assessment::Missing);
        let at = |due: DateTime<Utc>| Schedule {
            next_run: Some(due),
            ..Default::default()
        };
        let grace = chrono::Duration::seconds(OVERDUE_GRACE_SECONDS as i64);
        for (due, expected) in [
            (now + chrono::Duration::hours(1), false),
            (now, false),
            (now - grace, false),
            (now - grace - chrono::Duration::seconds(1), true),
            (now - chrono::Duration::days(3), true),
        ] {
            assert_eq!(
                matches!(assess(Some(&at(due)), now), Assessment::Overdue { due: d } if d == due),
                expected,
                "{due}"
            );
        }
    }
    #[test]
    fn throttling_backs_off_exponentially_within_the_cadences() {
        assert_eq!(deferral_seconds(1, 1), ACTIVE_SECONDS);
        assert_eq!(deferral_seconds(2, 60), 2 * ACTIVE_SECONDS);
        assert_eq!(deferral_seconds(3, 0), 4 * ACTIVE_SECONDS);
        assert_eq!(deferral_seconds(5, 0), 16 * ACTIVE_SECONDS);
        assert_eq!(deferral_seconds(6, 0), IDLE_SECONDS);
        assert_eq!(deferral_seconds(u32::MAX, 0), IDLE_SECONDS);
        assert_eq!(deferral_seconds(1, 3 * ACTIVE_SECONDS), 3 * ACTIVE_SECONDS);
    }
    #[test]
    fn failures_are_reported_for_one_day() {
        let now: DateTime<Utc> = "2026-11-24T12:05:00Z".parse().unwrap();
        let mut schedule = Schedule::default();
        assert_eq!(recent_failure(&schedule, now), None);
        schedule.health.last_failed_at = Some(now - chrono::Duration::hours(24));
        assert!(recent_failure(&schedule, now).is_some());
        schedule.health.last_failed_at = Some(now - chrono::Duration::hours(25));
        assert_eq!(recent_failure(&schedule, now), None);
    }
    #[test]
    fn schedules_retained_before_health_accounting_still_decode() {
        let old: Schedule = serde_json::from_value(serde_json::json!({
            "tracked": {"10": ["a"]}, "token": 4, "next_run": "2026-11-24T12:00:00Z",
            "cadence_seconds": 3600, "completed_runs": 3
        }))
        .unwrap();
        assert_eq!(old.health, Health::default());
        assert_eq!(old.cursor, None);
    }
}
