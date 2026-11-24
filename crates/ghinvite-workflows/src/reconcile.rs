//! Authoritative scheduled sweep. Each invocation inspects at most one delivery;
//! repository pagination is completed once and reused by its read-only fan-out.
use crate::{
    AppState,
    settlement_observation::ListedInvitation,
    sweep_schedule::{Tracked, WalkOutcome},
};
use chrono::{DateTime, Utc};
use ghinvite_core::{
    account::{AccountInstallationRef, App},
    delivery::CreateOutcome,
};
use ghinvite_github::{CallError, profiles};
use restate_sdk::{
    context::{Context, ContextClient, ContextSideEffects, RunFuture},
    errors::TerminalError,
    serde::Json,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct SweepInput {
    pub account: AccountInstallationRef,
    pub token: u64,
    pub tracked: Tracked,
}
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct TargetInput {
    pub account: AccountInstallationRef,
    pub target_id: u64,
    pub deliveries: Vec<String>,
    /// The scheduled run this walk belongs to; a requester refresh has none
    /// and reports nothing to the schedule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<u64>,
}
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct Observations {
    pub target: TargetInput,
    pub installation_id: u64,
    pub repository: String,
    pub at: DateTime<Utc>,
    pub invitations: Vec<ListedInvitation>,
}
pub struct Reconcile {
    pub state: AppState,
}

/// How a scheduled read that supplied no evidence counts for its run.
pub(crate) fn walk_outcome(error: &crate::HandlerError) -> WalkOutcome {
    match error.rate_limit() {
        Some(limit) => WalkOutcome::Throttled {
            retry_after_seconds: crate::throttle::backoff(limit.retry_after).as_secs(),
        },
        None => WalkOutcome::Failed,
    }
}

/// Tell the run's schedule that this walk produced no evidence. One-way: the
/// fan-out never waits on the schedule object.
pub(crate) async fn report(
    ctx: &Context<'_>,
    target: &TargetInput,
    outcome: WalkOutcome,
) -> Result<(), TerminalError> {
    let Some(token) = target.token else {
        return Ok(());
    };
    ctx.object_client::<crate::sweep_schedule::SweepScheduleClient>(
        target.account.key().map_err(TerminalError::new)?,
    )
    .report(Json(crate::sweep_schedule::WalkReport {
        token,
        target_id: target.target_id,
        outcome,
    }))
    .send()
    .await?;
    Ok(())
}

impl Reconcile {
    /// The repository app, configured with the GitHub runtime.
    fn repo_app(&self) -> Result<ghinvite_github::GithubApp, TerminalError> {
        self.state
            .github_apps
            .as_ref()
            .and_then(|apps| apps.apps.app(ghinvite_github::AppSlot::Repo).ok())
            .ok_or_else(|| TerminalError::new_with_code(503, "repository app runtime unavailable"))
    }
}

#[restate_sdk::service]
impl Reconcile {
    #[handler]
    async fn sweep(
        &self,
        ctx: Context<'_>,
        Json(input): Json<SweepInput>,
    ) -> Result<(), TerminalError> {
        if input.account.app == App::Org && self.state.organization_delivery.is_none() {
            return Err(TerminalError::new_with_code(
                400,
                "organization sweeps are disabled",
            ));
        }
        let account_key = input.account.key().map_err(TerminalError::new)?;
        // Empty schedules still check installation drift. This is a one-way
        // request: AccountInstallation never waits on the schedule owner.
        ctx.object_client::<crate::availability::AccountInstallationClient>(account_key)
            .recheck()
            .send()
            .await?;
        for (target_id, keys) in input.tracked {
            if input.account.app == App::Org {
                ctx.service_client::<crate::org_reconcile::OrganizationReconcileClient>()
                    .target(Json(TargetInput {
                        account: input.account,
                        target_id,
                        deliveries: keys.into_iter().collect(),
                        token: Some(input.token),
                    }))
                    .send()
                    .await?;
                continue;
            }
            ctx.service_client::<ReconcileClient>()
                .target(Json(TargetInput {
                    account: input.account,
                    target_id,
                    deliveries: keys.into_iter().collect(),
                    token: Some(input.token),
                }))
                .send()
                .await?;
        }
        Ok(())
    }
    #[handler]
    async fn target(
        &self,
        ctx: Context<'_>,
        Json(mut input): Json<TargetInput>,
    ) -> Result<(), TerminalError> {
        let Some(key) = input.deliveries.first() else {
            return Ok(());
        };
        let Json(Some(snapshot)) = ctx
            .object_client::<crate::delivery::RepositoryDeliveryClient>(key)
            .status()
            .call()
            .await?
        else {
            input.deliveries.remove(0);
            if !input.deliveries.is_empty() {
                ctx.service_client::<ReconcileClient>()
                    .target(Json(input))
                    .send()
                    .await?;
            }
            return Ok(());
        };
        let command = &snapshot.create.command;
        if input.account.app != App::Repo
            || command.account_id != input.account.account_id
            || command.repo_id != input.target_id
            || command.delivery_key() != *key
        {
            return Err(TerminalError::new_with_code(
                400,
                "sweep target identity mismatch",
            ));
        }
        let Json(Some(account)) = ctx
            .object_client::<crate::availability::AccountInstallationClient>(
                input.account.key().map_err(TerminalError::new)?,
            )
            .current_installation()
            .call()
            .await?
        else {
            return Ok(());
        };
        let at = crate::clock::now(&ctx, "sweep_observation_time").await?;
        let Json(invitations) = ctx
            .run(|| async {
                let app = self.repo_app()?;
                // Not replayed on a 401: the next account sweep reads again.
                match app
                    .write_once(
                        account.installation_id,
                        &profiles::REPO_READ,
                        |handle| async move {
                            crate::settlement_observation::list_within(
                                &handle.rest(),
                                &command.repo_full_name,
                                crate::sweep_schedule::MAX_WALK_PAGES,
                            )
                            .await
                            .map_err(CallError::<std::convert::Infallible>::Api)
                        },
                    )
                    .await
                {
                    Ok(list) => Ok::<_, restate_sdk::errors::HandlerError>(Json(Ok(list))),
                    Err(error) => {
                        let error: crate::HandlerError = error.into();
                        // A failed/partial read supplies no evidence. The next
                        // account sweep retries it; never accumulate an unbounded
                        // Restate retry chain alongside the hourly schedule.
                        tracing::warn!(error = %error, "scheduled observation unavailable");
                        Ok(Json(Err(walk_outcome(&error))))
                    }
                }
            })
            .name("list_repository_invitations")
            .await?;
        let invitations = match invitations {
            Ok(invitations) => Some(invitations),
            Err(outcome) => {
                report(&ctx, &input, outcome).await?;
                None
            }
        };
        if let Some(invitations) = invitations {
            ctx.service_client::<ReconcileClient>()
                .observe_next(Json(Observations {
                    installation_id: account.installation_id,
                    repository: command.repo_full_name.clone(),
                    target: input,
                    at,
                    invitations,
                }))
                .send()
                .await?;
        }
        Ok(())
    }
    #[handler]
    async fn observe_next(
        &self,
        ctx: Context<'_>,
        Json(mut input): Json<Observations>,
    ) -> Result<(), TerminalError> {
        let Some(key) = input.target.deliveries.pop() else {
            return Ok(());
        };
        // Queue the continuation before the external read. One throttled or
        // unavailable delivery cannot hold the remaining fan-out behind it.
        if !input.target.deliveries.is_empty() {
            ctx.service_client::<ReconcileClient>()
                .observe_next(Json(input.clone()))
                .send()
                .await?;
        }
        let Json(Some(snapshot)) = ctx
            .object_client::<crate::delivery::RepositoryDeliveryClient>(&key)
            .status()
            .call()
            .await?
        else {
            return Ok(());
        };
        let command = &snapshot.create.command;
        if command.delivery_key() != key
            || command.account_id != input.target.account.account_id
            || command.repo_id != input.target.target_id
            || command.repo_full_name != input.repository
            || input.target.account.app != App::Repo
        {
            return Err(TerminalError::new_with_code(
                400,
                "sweep delivery identity mismatch",
            ));
        }
        if snapshot.settlement.is_some()
            || snapshot.tracking_stopped.is_some()
            || !(snapshot.create.outcome.needs_settlement()
                || matches!(snapshot.create.outcome, CreateOutcome::OutcomeUnknown))
        {
            return Ok(());
        }
        let Json(Some(account)) = ctx
            .object_client::<crate::availability::AccountInstallationClient>(
                input.target.account.key().map_err(TerminalError::new)?,
            )
            .current_installation()
            .call()
            .await?
        else {
            return Ok(());
        };
        if account.installation_id != input.installation_id {
            return Ok(());
        }
        let Json(evidence) = ctx
            .run(|| async {
                let app = self.repo_app()?;
                let app_rest = app
                    .app_rest()
                    .map_err(octotoken::TokenError::from)
                    .map_err(crate::HandlerError::from)
                    .map_err(crate::error::to_sdk_handler_error)?;
                let app_rest = &app_rest.rest();
                let account = &account;
                let snapshot = &snapshot;
                let input = &input;
                // Not replayed on a 401: the next account sweep reads again.
                match app
                    .write_once(
                        account.installation_id,
                        &profiles::REPO_READ,
                        |handle| async move {
                            crate::settlement_observation::observe_listed(
                                &handle,
                                app_rest,
                                account,
                                snapshot,
                                input.at,
                                crate::settlement_policy::AcceptancePolicy::default(),
                                &input.invitations,
                            )
                            .await
                            .map_err(CallError::<std::convert::Infallible>::Api)
                        },
                    )
                    .await
                {
                    Ok(evidence) => Ok::<_, restate_sdk::errors::HandlerError>(Json(Ok(evidence))),
                    Err(error) => {
                        let error: crate::HandlerError = error.into();
                        // A failed/partial read supplies no evidence. The next
                        // account sweep retries it; never accumulate an unbounded
                        // Restate retry chain alongside the hourly schedule.
                        tracing::warn!(error = %error, "scheduled observation unavailable");
                        Ok(Json(Err(walk_outcome(&error))))
                    }
                }
            })
            .name("verify_sweep_delivery")
            .await?;
        let evidence = match evidence {
            Ok(evidence) => evidence,
            Err(outcome) => {
                report(&ctx, &input.target, outcome).await?;
                None
            }
        };
        if let Some(evidence) = evidence {
            ctx.object_client::<crate::delivery::RepositoryDeliveryClient>(key)
                .reconcile(Json(evidence))
                .send()
                .await?;
        }
        Ok(())
    }
}
