//! Known-invitation evidence and retained settlement in RepositoryDelivery.
use crate::settlement_policy::{AcceptancePolicy, ReconcileDecision};
use crate::{AppState, github_invitation::*};
use chrono::{DateTime, Utc};
use ghinvite_core::{
    Account, InvitationState,
    audit::{ActorKind, AuditEvent},
    delivery::{CreateOutcome, DeliverySnapshot, Settlement},
};
use restate_sdk::{
    context::{ContextClient, ContextReadState, ContextSideEffects, ObjectContext, RunFuture},
    errors::TerminalError,
    serde::Json,
};

pub type ReconcileEvidence = crate::settlement_policy::Observation;

pub fn eligible(snapshot: &DeliverySnapshot) -> bool {
    snapshot.settlement.is_none() && snapshot.create.outcome.needs_settlement()
}

/// Exact upstream identity, complete listing and numeric membership evidence.
/// Unlike unknown-create recovery, absence can settle this known invitation.
pub async fn observe(
    state: &AppState,
    snapshot: &DeliverySnapshot,
    account: &Account,
    at: DateTime<Utc>,
) -> crate::Result<Option<ReconcileEvidence>> {
    use ghinvite_github::{AppSlot, CallError, profiles};
    let app = state
        .github_apps
        .as_ref()
        .and_then(|apps| apps.apps.app(AppSlot::Repo).ok())
        .ok_or_else(|| crate::HandlerError::Invariant("repository app is not configured".into()))?;
    let app_rest = app.app_rest().map_err(octotoken::TokenError::from)?;
    let app_rest = &app_rest.rest();
    // Not replayed on a 401 (the token is replaced for the next observation).
    app.write_once(
        account.installation_id,
        &profiles::REPO_READ,
        |handle| async move {
            crate::settlement_observation::observe(
                &handle,
                app_rest,
                account,
                snapshot,
                at,
                AcceptancePolicy::default(),
            )
            .await
            .map_err(CallError::<std::convert::Infallible>::Api)
        },
    )
    .await
    .map_err(Into::into)
}

async fn load(
    ctx: &ObjectContext<'_>,
    id: ghinvite_core::GithubInvitationId,
) -> Result<DeliverySnapshot, TerminalError> {
    let Json(snapshot) = crate::repository_audit::snapshot(ctx)
        .await?
        .map(Json)
        .ok_or_else(|| TerminalError::new_with_code(503, "delivery not yet available"))?;
    if snapshot.create.command.delivery_key() != ctx.key()
        || snapshot.create.command.invitation_id != Some(id)
    {
        return Err(TerminalError::new_with_code(
            409,
            "delivery identity conflict",
        ));
    }
    Ok(snapshot)
}

pub(crate) async fn record(
    ctx: &ObjectContext<'_>,
    mut snapshot: DeliverySnapshot,
    state: InvitationState,
    at: DateTime<Utc>,
    actor: ActorKind,
    by_user: Option<u64>,
    metadata: serde_json::Value,
) -> Result<(), TerminalError> {
    if snapshot.tracking_stopped.is_some() || !eligible(&snapshot) {
        return Ok(());
    }
    let command = &snapshot.create.command;
    let (target_kind, target_id) =
        ghinvite_core::delivery_audit::repository_target(&snapshot.create)
            .map_err(TerminalError::new)?;
    let event_id = ghinvite_core::delivery_audit::repository_settlement_id(&snapshot.create)
        .map_err(TerminalError::new)?;
    snapshot.settlement = Some(Settlement {
        state,
        event: AuditEvent {
            id: event_id,
            account_id: command.account_id,
            occurred_at: at,
            event_type: ghinvite_core::storage::settlement::event_type(state)
                .map_err(|e| TerminalError::new(e.to_string()))?,
            actor_kind: actor,
            actor_id: by_user,
            target_kind,
            target_id,
            metadata,
            request_id: None,
        },
    });
    snapshot.revision = snapshot
        .revision
        .checked_add(1)
        .ok_or_else(|| TerminalError::new("delivery revision exhausted"))?;
    crate::repository_audit::save(ctx, &snapshot).await?;
    crate::delivery::send_projection(ctx, &snapshot).await
}

pub async fn reconcile(
    ctx: &ObjectContext<'_>,
    Json(input): Json<ReconcileEvidence>,
) -> Result<(), TerminalError> {
    let Json(snapshot) = crate::repository_audit::snapshot(ctx)
        .await?
        .map(Json)
        .ok_or_else(|| TerminalError::new_with_code(503, "delivery not yet available"))?;
    if snapshot.create.command.delivery_key() != ctx.key() {
        return Err(TerminalError::new_with_code(
            409,
            "delivery identity conflict",
        ));
    }
    apply_observation(ctx, snapshot, &input).await
}

/// Shared owner-only application for scheduled evidence and synchronous explicit
/// recovery. The HTTP observation has already been journaled by its caller.
pub(crate) async fn apply_observation(
    ctx: &ObjectContext<'_>,
    snapshot: DeliverySnapshot,
    input: &ReconcileEvidence,
) -> Result<(), TerminalError> {
    use crate::operational::ObservationProgress;
    let observed = crate::operational::record_observation(ctx, &snapshot, input).await?;
    // A complete unchanged observation advances time without changing revision.
    // Older evidence must not settle that same revision after the newer read.
    if matches!(observed, ObservationProgress::Rejected) {
        return Ok(());
    }
    // An ambiguous cancellation cannot be relabelled as somebody else's
    // decline by an ordinary absence sweep. Positive acceptance still wins.
    if ctx
        .get::<bool>("cancellation_unknown")
        .await?
        .unwrap_or(false)
        && matches!(
            input.evidence,
            crate::settlement_policy::Evidence::Absent { .. }
        )
        && !matches!(
            crate::settlement_policy::evaluate(&snapshot, input, AcceptancePolicy::default()),
            ReconcileDecision::Settle {
                state: InvitationState::Accepted,
                ..
            }
        )
    {
        if matches!(observed, ObservationProgress::Advanced) {
            crate::operational::send_projection(ctx, &snapshot).await?;
        }
        return Ok(());
    }
    match crate::settlement_policy::evaluate(&snapshot, input, AcceptancePolicy::default()) {
        ReconcileDecision::NoChange => {
            let mut snapshot = snapshot;
            let timing = match input.invitation_timing.clone() {
                Some(timing) => {
                    Some(crate::delivery_control::invitation_timing(ctx, timing).await?)
                }
                None => None,
            };
            if timing.is_some_and(|timing| snapshot.retain_invitation_timing(timing)) {
                crate::repository_audit::save(ctx, &snapshot).await?;
                crate::delivery::send_projection(ctx, &snapshot).await?;
            }
            if matches!(observed, ObservationProgress::Advanced) {
                crate::operational::send_projection(ctx, &snapshot).await?;
            }
            Ok(())
        }
        ReconcileDecision::ResolveCreated { upstream_id } => {
            let mut snapshot = snapshot;
            snapshot.revision = snapshot
                .revision
                .checked_add(1)
                .ok_or_else(|| TerminalError::new("delivery revision exhausted"))?;
            snapshot.create.outcome = CreateOutcome::Created { upstream_id };
            snapshot.create.confirmed_at = Some(input.at);
            snapshot.create.revision = snapshot.revision;
            if let Some(timing) = &input.invitation_timing {
                snapshot.retain_invitation_timing(
                    crate::delivery_control::invitation_timing(ctx, timing.clone()).await?,
                );
            }
            crate::repository_audit::save(ctx, &snapshot).await?;
            crate::delivery::send_projection(ctx, &snapshot).await
        }
        ReconcileDecision::Settle { state, provenance } => {
            record(
                ctx,
                snapshot,
                state,
                input.at,
                ActorKind::System,
                None,
                serde_json::json!({"reconciled":true,"evidence":provenance}),
            )
            .await
        }
    }
}

pub async fn webhook(
    ctx: ObjectContext<'_>,
    Json(input): Json<OnWebhookInput>,
) -> Result<(), TerminalError> {
    let snapshot = load(&ctx, input.invitation_id).await?;
    if snapshot.tracking_stopped.is_some() || !eligible(&snapshot) {
        return Ok(());
    }
    if input.verify || matches!(input.action, WebhookAction::Accepted) {
        ctx.service_client::<DeliveryObservationClient>()
            .observe(Json(snapshot))
            .send()
            .await?;
        return Ok(());
    }
    record(
        &ctx,
        snapshot,
        match input.action {
            WebhookAction::Accepted => InvitationState::Accepted,
            WebhookAction::Declined => InvitationState::Declined,
        },
        input.at,
        ActorKind::Github,
        None,
        serde_json::json!({"action":input.action}),
    )
    .await
}

/// Member payloads lack upstream invitation identity. Their retained association
/// selects what to inspect, never supplies evidence that this invitation ended.
pub struct DeliveryObservation {
    pub state: AppState,
}

#[restate_sdk::service]
impl DeliveryObservation {
    #[handler]
    async fn observe(
        &self,
        ctx: restate_sdk::context::Context<'_>,
        Json(snapshot): Json<DeliverySnapshot>,
    ) -> Result<(), TerminalError> {
        // Every delayed invocation refreshes authority. Its original snapshot
        // identifies the delivery, but cannot keep a settled invitation polling.
        let Json(current) = ctx
            .object_client::<crate::delivery::RepositoryDeliveryClient>(
                snapshot.create.command.delivery_key(),
            )
            .status()
            .call()
            .await?;
        let Some(snapshot) = current.filter(|current| {
            current.create.command == snapshot.create.command
                && current.settlement.is_none()
                && current.tracking_stopped.is_none()
                && (current.create.outcome.needs_settlement()
                    || matches!(current.create.outcome, CreateOutcome::OutcomeUnknown))
        }) else {
            return Ok(());
        };
        let command = &snapshot.create.command;
        let Json(account) = ctx
            .object_client::<crate::availability::AccountInstallationClient>(format!(
                "repo:{}",
                command.account_id
            ))
            .current_installation()
            .call()
            .await?;
        let Some(account) = account else {
            return Ok(());
        };
        // Read before the external observation: a delayed retry must not extend
        // the age of the GitHub evidence it replays.
        let now = crate::clock::now(&ctx, "observation_time").await?;
        let Json((evidence, retry)) = ctx
            .run(|| async {
                let result = match observe(&self.state, &snapshot, &account, now).await {
                    Ok(evidence) => (evidence, None),
                    Err(error) if !error.is_terminal() => (
                        None,
                        Some(
                            error
                                .rate_limit()
                                .map(|limit| crate::throttle::backoff(limit.retry_after))
                                .unwrap_or(crate::throttle::MAXIMUM)
                                .as_secs(),
                        ),
                    ),
                    Err(_) => (None, None),
                };
                Ok::<_, restate_sdk::errors::HandlerError>(Json(result))
            })
            .name("verify_member_invitation")
            .await?;
        if let Some(evidence) = evidence {
            ctx.object_client::<crate::delivery::RepositoryDeliveryClient>(command.delivery_key())
                .reconcile(Json(evidence))
                .call()
                .await?;
        }
        if let Some(seconds) = retry {
            ctx.service_client::<DeliveryObservationClient>()
                .observe(Json(snapshot))
                .send_after(std::time::Duration::from_secs(seconds))
                .await?;
        }
        Ok(())
    }
}

pub async fn cancel(
    state: &AppState,
    ctx: ObjectContext<'_>,
    Json(input): Json<CancelInvitationInput>,
) -> Result<(), TerminalError> {
    let snapshot = load(&ctx, input.invitation_id).await?;
    if snapshot.create.command.installation_id != input.installation_id {
        return Err(TerminalError::new_with_code(
            409,
            "cancellation identity conflict",
        ));
    }
    if !snapshot.create.outcome.tracked_foreign() {
        if state.cancellation_enabled
            && ctx
                .get::<bool>("control_cancellation_pending")
                .await?
                .unwrap_or(false)
        {
            ctx.object_client::<crate::delivery::RepositoryDeliveryClient>(ctx.key())
                .drive_cancellation()
                .send()
                .await?;
            return Ok(());
        }
        return Err(TerminalError::new_with_code(
            409,
            "cancellation requires retained control",
        ));
    }
    withdraw(
        state,
        ctx,
        input.invitation_id,
        input.installation_id,
        input.at,
        Withdrawal::Cancel {
            by_user: input.by_user,
        },
    )
    .await
}

pub async fn expire(
    state: &AppState,
    ctx: ObjectContext<'_>,
    Json(input): Json<TickExpireInput>,
) -> Result<(), TerminalError> {
    withdraw(
        state,
        ctx,
        input.invitation_id,
        input.installation_id,
        input.at,
        Withdrawal::Expire,
    )
    .await
}

#[derive(Clone, Copy)]
enum Withdrawal {
    Cancel { by_user: Option<u64> },
    Expire,
}

async fn withdraw(
    state: &AppState,
    ctx: ObjectContext<'_>,
    id: ghinvite_core::GithubInvitationId,
    installation_id: u64,
    at: DateTime<Utc>,
    withdrawal: Withdrawal,
) -> Result<(), TerminalError> {
    let (expire, by_user, terminal, reason) = match withdrawal {
        Withdrawal::Cancel { by_user } => {
            (false, by_user, InvitationState::Cancelled, "link_revoked")
        }
        Withdrawal::Expire => (true, None, InvitationState::Expired, "github_expiry"),
    };
    let snapshot = load(&ctx, id).await?;
    if snapshot.tracking_stopped.is_some() || !eligible(&snapshot) {
        return Ok(());
    }
    if snapshot.create.outcome.tracked_foreign() {
        let mut snapshot = snapshot;
        snapshot.tracking_stopped = Some(if expire {
            ghinvite_core::delivery::SettlementReason::OwnExpiry
        } else {
            ghinvite_core::delivery::SettlementReason::LinkRevoked
        });
        snapshot.revision = snapshot
            .revision
            .checked_add(1)
            .ok_or_else(|| TerminalError::new("delivery revision exhausted"))?;
        crate::repository_audit::save(&ctx, &snapshot).await?;
        return crate::delivery::send_projection(&ctx, &snapshot).await;
    }
    if !crate::settlement_policy::can_withdraw(&snapshot) {
        return Ok(());
    }
    let command = &snapshot.create.command;
    let Json(account) = ctx
        .object_client::<crate::availability::AccountInstallationClient>(format!(
            "repo:{}",
            command.account_id
        ))
        .current_installation()
        .call()
        .await?;
    let Some(account) = account else {
        return Ok(());
    };
    // Authorization naming a retired installation cannot withdraw through its replacement.
    if account.installation_id != installation_id {
        return Ok(());
    }
    let confirmed = ctx
        .run(|| async {
            let result: crate::Result<bool> = async {
                if expire {
                    let observation = observe(state, &snapshot, &account, at).await?;
                    return Ok(observation.as_ref().is_some_and(|observation| {
                        matches!(
                            crate::settlement_policy::evaluate(
                                &snapshot,
                                observation,
                                AcceptancePolicy::default()
                            ),
                            ReconcileDecision::Settle {
                                state: InvitationState::Expired,
                                ..
                            }
                        )
                    }));
                }
                // Owned cancellation is executed only by the separately fenced
                // and budgeted delivery-control executor. The legacy input can
                // stop tracking foreign invitations, but cannot authorize DELETE.
                Ok(false)
            }
            .await;
            // One bounded observation; unavailability never becomes terminal evidence.
            Ok::<_, restate_sdk::errors::HandlerError>(match result {
                Ok(confirmed) => Some(confirmed),
                Err(error) if error.is_terminal() => Some(false),
                Err(_) => None,
            })
        })
        .name("withdraw_known_invitation")
        .await?;
    if confirmed.is_none() {
        let owner =
            ctx.object_client::<crate::delivery::RepositoryDeliveryClient>(command.delivery_key());
        if expire {
            owner
                .tick_expire(Json(TickExpireInput {
                    invitation_id: id,
                    installation_id,
                    at,
                }))
                .send_after(crate::throttle::MAXIMUM)
                .await?;
        } else {
            owner
                .cancel(Json(CancelInvitationInput {
                    invitation_id: id,
                    installation_id,
                    at,
                    by_user,
                }))
                .send_after(crate::throttle::MAXIMUM)
                .await?;
        }
    }
    if confirmed == Some(true) {
        record(
            &ctx,
            snapshot,
            terminal,
            at,
            if by_user.is_some() {
                ActorKind::User
            } else {
                ActorKind::System
            },
            by_user,
            serde_json::json!({"reason":reason,"by_user":by_user}),
        )
        .await?;
    }
    Ok(())
}
