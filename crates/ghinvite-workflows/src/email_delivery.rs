//! Explicit M2 email-addressed owner. It never manufactures a requester identity.
mod audit;
mod protocol;
use crate::{AppState, invite_budget::InviteBudgetClient};
use chrono::{DateTime, Utc};
use ghinvite_core::{
    budget::{BudgetKind, Reservation, Reserve, SlotId},
    email_delivery::*,
    storage::{
        email_delivery::EmailDeliveryStorage,
        email_vault::{EmailVaultBinding, EmailVaultStorage},
    },
};
use ghinvite_email_vault::EmailVault;
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
#[derive(Clone)]
pub struct Dependencies {
    pub storage: Arc<dyn EmailDeliveryStorage>,
    pub vault: Arc<EmailVault>,
    pub records: Arc<dyn EmailVaultStorage>,
}
#[cfg(feature = "integration")]
#[derive(Default)]
pub struct EmailAuditFaults {
    pub pause: std::sync::atomic::AtomicBool,
    pub reached: std::sync::atomic::AtomicUsize,
    pub lose_projection_ack: std::sync::atomic::AtomicBool,
    pub lost_projection_acks: std::sync::atomic::AtomicUsize,
}
pub struct EmailInviteDelivery {
    state: AppState,
    dependencies: Dependencies,
}
pub struct EmailDeliveryProjection {
    #[cfg(feature = "integration")]
    faults: Option<Arc<EmailAuditFaults>>,
    storage: Arc<dyn EmailDeliveryStorage>,
}
pub fn bind(builder: Builder, state: AppState, dependencies: Dependencies) -> Builder {
    #[cfg(feature = "integration")]
    let faults = state.email_audit_faults.clone();
    builder
        .bind(EmailInviteDelivery {
            state,
            dependencies: dependencies.clone(),
        })
        .bind(EmailDeliveryProjection {
            #[cfg(feature = "integration")]
            faults,
            storage: dependencies.storage,
        })
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Retained {
    snapshot: EmailDeliverySnapshot,
    ordinal: u64,
    reserve: Option<Reserve>,
    slot: Option<SlotId>,
    completion: Option<(protocol::Completion, DateTime<Utc>)>,
    wake: u64,
    #[serde(default)]
    drained_revision: u64,
    #[serde(default)]
    audit: audit::Audit,
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Wake {
    pub generation: u64,
}
fn conflict() -> TerminalError {
    TerminalError::new_with_code(409, "email delivery identity conflict")
}
fn next(n: u64) -> Result<u64, TerminalError> {
    n.checked_add(1)
        .filter(|n| *n <= ghinvite_core::storage::integer::MAX_EXACT)
        .ok_or_else(conflict)
}
fn binding(c: &EmailCreateCommand) -> EmailVaultBinding {
    EmailVaultBinding {
        email_ref: c.email_ref,
        batch: c.batch,
        account_id: c.account_id,
        installation_id: c.installation_id,
    }
}
fn save(ctx: &ObjectContext<'_>, v: &Retained) -> Result<(), TerminalError> {
    v.snapshot.validate().map_err(|_| conflict())?;
    ctx.set("email", Json(v.clone()));
    Ok(())
}
async fn project(ctx: &ObjectContext<'_>, v: &Retained) -> Result<(), TerminalError> {
    ctx.object_client::<crate::bulk::BulkInviteBatchClient>(
        v.snapshot.command.batch.batch_id.to_string(),
    )
    .on_item_progress(Json(
        ghinvite_core::bulk_commands::ItemProgressNotice::Email {
            source: v.snapshot.command.batch,
        },
    ))
    .send()
    .await?;

    ctx.object_client::<EmailDeliveryProjectionClient>(ctx.key())
        .apply(Json(
            ghinvite_core::storage::email_delivery::EmailDeliveryProjection {
                snapshot: v.snapshot.clone(),
                audit_events: v.audit.pending.clone(),
            },
        ))
        .send()
        .await?;
    ctx.object_client::<crate::sweep_schedule::SweepScheduleClient>(format!(
        "org:{}",
        v.snapshot.command.account_id
    ))
    .track(Json(crate::sweep_schedule::Registration {
        target_id: v.snapshot.command.organization.org_id,
        delivery_key: v.snapshot.command.storage_id(),
    }))
    .send()
    .await?;
    Ok(())
}
impl EmailInviteDelivery {
    async fn schedule(
        &self,
        ctx: &ObjectContext<'_>,
        v: &mut Retained,
        delay: std::time::Duration,
    ) -> Result<(), TerminalError> {
        v.wake = next(v.wake)?;
        save(ctx, v)?;
        ctx.object_client::<EmailInviteDeliveryClient>(ctx.key())
            .recheck(Json(Wake { generation: v.wake }))
            .send_after(delay)
            .await?;
        Ok(())
    }
    async fn finish(&self, ctx: &ObjectContext<'_>, v: &mut Retained) -> Result<(), TerminalError> {
        audit::initialize(v);
        let Some((completion, at)) = v.completion.clone() else {
            return Ok(());
        };
        let budget = ctx
            .object_client::<InviteBudgetClient>(format!("org:{}", v.snapshot.command.account_id));
        if let Some(limit) = completion.limit.clone() {
            budget.record_limit(Json(limit)).call().await?;
        }
        if completion.unused
            && let Some(slot) = v.slot
        {
            budget.release(Json(slot)).call().await?;
        }
        if let Some(generation) = completion.rejected_generation {
            let attempt = EmailAttempt {
                create: v.snapshot.command.clone(),
            };
            ctx.run(|| async {
                self.dependencies
                    .storage
                    .reject_email_attempt(&attempt, generation)
                    .await
                    .map_err(HandlerError::from)
            })
            .name("email_release_rejected_attempt")
            .await?;
        }
        if completion.recheck {
            ctx.object_client::<crate::installation::InstallationClient>(format!(
                "org:{}",
                v.snapshot.command.installation_id
            ))
            .recheck_permissions()
            .send()
            .await?;
        }
        if matches!(completion.outcome, EmailCreateOutcome::OutcomeUnknown)
            && v.snapshot.unknown_since.is_none()
        {
            v.snapshot.unknown_since = Some(at);
        }
        v.snapshot.outcome = completion.outcome;
        v.snapshot.completed_at = matches!(
            v.snapshot.outcome,
            EmailCreateOutcome::Failed { .. } | EmailCreateOutcome::Aborted { .. }
        )
        .then_some(at);
        v.snapshot.confirmed_at =
            matches!(v.snapshot.outcome, EmailCreateOutcome::Created { .. }).then_some(at);
        v.snapshot.revision = next(v.snapshot.revision)?;
        if completion.unused {
            v.slot = None;
            v.reserve = None;
            v.ordinal = next(v.ordinal)?;
        }
        v.completion = None;
        audit::transition(v, at)?;
        save(ctx, v)?;
        self.drain(ctx, v).await
    }
    /// Snapshot and pending status share the retained record. Retirement uses
    /// the original terminal clock, never the retry's wall time.
    async fn drain(&self, ctx: &ObjectContext<'_>, v: &mut Retained) -> Result<(), TerminalError> {
        audit::initialize(v);
        if v.drained_revision == v.snapshot.revision && v.audit.pending.is_empty() {
            return Ok(());
        }
        #[cfg(feature = "integration")]
        if !v.audit.pending.is_empty()
            && let Some(faults) = &self.state.email_audit_faults
        {
            ctx.run(|| async {
                if faults.pause.load(std::sync::atomic::Ordering::SeqCst) {
                    if faults
                        .reached
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                        == 0
                    {
                        return Err(HandlerError::from(std::io::Error::other(
                            "fixture email atomic audit checkpoint",
                        )));
                    }
                    std::future::pending::<()>().await;
                }
                Ok::<_, HandlerError>(())
            })
            .name("fixture_after_email_audit")
            .await?;
        }
        project(ctx, v).await?;
        if matches!(
            v.snapshot.outcome,
            EmailCreateOutcome::Created { .. }
                | EmailCreateOutcome::Failed { .. }
                | EmailCreateOutcome::Aborted { .. }
        ) {
            let at = v
                .snapshot
                .confirmed_at
                .or(v.snapshot.completed_at)
                .ok_or_else(conflict)?;
            let binding = binding(&v.snapshot.command);
            ctx.run(|| async {
                self.dependencies
                    .records
                    .retire_email(&binding, at.timestamp())
                    .await
                    .map(|_| ())
                    .map_err(HandlerError::from)
            })
            .name("email_retire_recovery")
            .await?;
        }
        v.audit.pending.clear();
        v.drained_revision = v.snapshot.revision;
        save(ctx, v)
    }
    async fn drive(&self, ctx: &ObjectContext<'_>, v: &mut Retained) -> Result<(), TerminalError> {
        self.finish(ctx, v).await?;
        if !v.snapshot.outcome.permits_new_attempt() {
            return self.drain(ctx, v).await;
        }
        let now = crate::clock::now(ctx, "email_delivery_time").await?;
        if let EmailCreateOutcome::Throttled { until, .. } = v.snapshot.outcome
            && until > now
        {
            return self
                .schedule(ctx, v, (until - now).to_std().map_err(|_| conflict())?)
                .await;
        }
        let account = ctx
            .object_client::<crate::availability::AccountInstallationClient>(format!(
                "org:{}",
                v.snapshot.command.account_id
            ))
            .current_installation()
            .call()
            .await?
            .0;
        let Some(account) = account else {
            v.snapshot.outcome = EmailCreateOutcome::Blocked {
                reason: EmailBlockedReason::InstallationUnavailable,
            };
            v.snapshot.revision = next(v.snapshot.revision)?;
            audit::transition(v, now)?;
            save(ctx, v)?;
            self.drain(ctx, v).await?;
            return self
                .schedule(ctx, v, std::time::Duration::from_secs(60))
                .await;
        };
        if v.reserve.is_none() {
            v.reserve = Some(Reserve {
                request_key: format!("email:{}:org-email:{}", ctx.key(), v.ordinal),
                kind: BudgetKind::OrgInvite,
                link_id: None,
            });
            save(ctx, v)?;
        }
        if v.slot.is_none() {
            match ctx
                .object_client::<InviteBudgetClient>(format!(
                    "org:{}",
                    v.snapshot.command.account_id
                ))
                .reserve(Json(v.reserve.clone().expect("retained reservation")))
                .call()
                .await?
                .0
            {
                Reservation::Granted { slot_id } => {
                    v.slot = Some(slot_id);
                    save(ctx, v)?;
                }
                Reservation::RetryAt { at, why } => {
                    // A refused reservation is an immutable budget receipt, not
                    // an outstanding slot. After its deadline the next attempt
                    // needs a new identity; replay of this invocation still uses
                    // the journaled refusal and the same retained transition.
                    v.reserve = None;
                    v.ordinal = next(v.ordinal)?;
                    v.snapshot.outcome = EmailCreateOutcome::Throttled {
                        until: at,
                        reason: Some(why),
                    };
                    v.snapshot.revision = next(v.snapshot.revision)?;
                    audit::transition(v, now)?;
                    save(ctx, v)?;
                    self.drain(ctx, v).await?;
                    return self
                        .schedule(ctx, v, (at - now).to_std().unwrap_or_default())
                        .await;
                }
            }
        }
        let apps = self
            .state
            .github_apps
            .as_ref()
            .ok_or_else(|| TerminalError::new_with_code(503, "email app unavailable"))?;
        let attempt = EmailAttempt {
            create: v.snapshot.command.clone(),
        };
        let Json(mut completion) = ctx
            .run(|| async {
                protocol::write(
                    apps,
                    self.dependencies.storage.as_ref(),
                    self.dependencies.vault.as_ref(),
                    &attempt,
                    &account,
                    now,
                )
                .await
                .map(Json)
                .map_err(HandlerError::from)
            })
            .name("email_write_stage")
            .await?;
        if let Some(limit) = &mut completion.limit {
            crate::delivery_budget::bind_limit(ctx, "email-write", limit);
        }
        let at = crate::clock::now(ctx, "email_write_completed_at").await?;
        v.completion = Some((completion, at));
        save(ctx, v)?;
        self.finish(ctx, v).await?;
        if v.snapshot.outcome.permits_new_attempt() {
            let delay = match v.snapshot.outcome {
                EmailCreateOutcome::Throttled { until, .. } => {
                    (until - at).to_std().unwrap_or_default()
                }
                _ => std::time::Duration::from_secs(60),
            };
            self.schedule(ctx, v, delay).await?;
        }
        Ok(())
    }
}
#[restate_sdk::object]
impl EmailInviteDelivery {
    #[handler]
    async fn create(
        &self,
        ctx: ObjectContext<'_>,
        Json(command): Json<EmailCreateCommand>,
    ) -> Result<Json<EmailDeliverySnapshot>, TerminalError> {
        command.validate().map_err(|_| conflict())?;
        if command.delivery_key() != ctx.key() {
            return Err(conflict());
        }
        let mut value = match ctx.get::<Json<Retained>>("email").await? {
            Some(Json(v)) => {
                if v.snapshot.command != command {
                    return Err(conflict());
                }
                v
            }
            None => Retained {
                snapshot: EmailDeliverySnapshot {
                    command,
                    revision: 1,
                    outcome: EmailCreateOutcome::Planned,
                    completed_at: None,
                    confirmed_at: None,
                    settlement: None,
                    settled_at: None,
                    unknown_since: None,
                    last_observed_at: None,
                },
                ordinal: 1,
                reserve: None,
                slot: None,
                completion: None,
                wake: 0,
                drained_revision: 0,
                audit: audit::Audit::default(),
            },
        };
        audit::initialize(&mut value);
        save(&ctx, &value)?;
        self.drain(&ctx, &mut value).await?;
        self.drive(&ctx, &mut value).await?;
        Ok(Json(value.snapshot))
    }
    /// Cancel only an unsent batch remainder. Uncertain or confirmed effects
    /// never become permission to send DELETE or to refund an occupied slot.
    #[handler]
    async fn cancel(
        &self,
        ctx: ObjectContext<'_>,
        Json(command): Json<EmailCreateCommand>,
    ) -> Result<Json<EmailDeliverySnapshot>, TerminalError> {
        command.validate().map_err(|_| conflict())?;
        if command.delivery_key() != ctx.key() {
            return Err(conflict());
        }
        let mut v = match ctx.get::<Json<Retained>>("email").await? {
            Some(Json(v)) => {
                if v.snapshot.command != command {
                    return Err(conflict());
                }
                v
            }
            None => Retained {
                snapshot: EmailDeliverySnapshot {
                    command,
                    revision: 1,
                    outcome: EmailCreateOutcome::Planned,
                    completed_at: None,
                    confirmed_at: None,
                    settlement: None,
                    settled_at: None,
                    unknown_since: None,
                    last_observed_at: None,
                },
                ordinal: 1,
                reserve: None,
                slot: None,
                completion: None,
                wake: 0,
                drained_revision: 0,
                audit: audit::Audit::default(),
            },
        };
        self.finish(&ctx, &mut v).await?;
        if !v.snapshot.outcome.permits_new_attempt() {
            self.drain(&ctx, &mut v).await?;
            return Ok(Json(v.snapshot));
        }
        let attempt = EmailAttempt {
            create: v.snapshot.command.clone(),
        };
        let fenced = ctx
            .run(|| async {
                self.dependencies
                    .storage
                    .email_attempt_exists(&attempt)
                    .await
                    .map_err(HandlerError::from)
            })
            .name("email_cancel_fence")
            .await?;
        if fenced {
            // An existing fence is positive uncertainty, unlike an unavailable
            // fence read. Use a retained clock, never approval/projection time.
            if v.snapshot.unknown_since.is_none() {
                v.snapshot.unknown_since =
                    Some(crate::clock::now(&ctx, "email_unknown_time").await?);
            }
            v.snapshot.outcome = EmailCreateOutcome::OutcomeUnknown;
            v.snapshot.revision = next(v.snapshot.revision)?;
            let at = v.snapshot.unknown_since.ok_or_else(conflict)?;
            audit::transition(&mut v, at)?;
            save(&ctx, &v)?;
            self.drain(&ctx, &mut v).await?;
            return Ok(Json(v.snapshot));
        }
        let budget = ctx
            .object_client::<InviteBudgetClient>(format!("org:{}", v.snapshot.command.account_id));
        // Resolve a possibly committed reserve RPC using its original identity.
        if v.slot.is_none()
            && let Some(reserve) = &v.reserve
            && let Reservation::Granted { slot_id } =
                budget.reserve(Json(reserve.clone())).call().await?.0
        {
            v.slot = Some(slot_id);
            save(&ctx, &v)?;
        }
        if let Some(slot) = v.slot {
            budget.release(Json(slot)).call().await?;
        }
        let at = crate::clock::now(&ctx, "email_cancel_time").await?;
        v.snapshot.completed_at = Some(at);
        v.snapshot.outcome = EmailCreateOutcome::Aborted {
            reason: ghinvite_core::delivery::AbortReason::BatchCancelled,
        };
        v.snapshot.revision = next(v.snapshot.revision)?;
        v.wake = next(v.wake)?;
        v.slot = None;
        v.reserve = None;
        audit::transition(&mut v, at)?;
        save(&ctx, &v)?;
        self.drain(&ctx, &mut v).await?;
        Ok(Json(v.snapshot))
    }
    #[handler]
    async fn recheck(
        &self,
        ctx: ObjectContext<'_>,
        Json(wake): Json<Wake>,
    ) -> Result<(), TerminalError> {
        if let Some(Json(mut v)) = ctx.get::<Json<Retained>>("email").await?
            && v.wake == wake.generation
        {
            self.drive(&ctx, &mut v).await?;
        }
        Ok(())
    }
    #[handler]
    async fn reconcile(
        &self,
        ctx: ObjectContext<'_>,
        Json(observation): Json<ghinvite_core::org_observation::OrganizationObservation>,
    ) -> Result<(), TerminalError> {
        let Some(Json(mut v)) = ctx.get::<Json<Retained>>("email").await? else {
            return Ok(());
        };
        self.drain(&ctx, &mut v).await?;
        let now = crate::clock::now(&ctx, "email_observation_time").await?;
        let account = ctx
            .object_client::<crate::availability::AccountInstallationClient>(format!(
                "org:{}",
                v.snapshot.command.account_id
            ))
            .current_installation()
            .call()
            .await?
            .0;
        if let Some(account) = account
            && account.account_id == v.snapshot.command.account_id
            && v.snapshot
                .reconcile(
                    &observation,
                    account.installation_id,
                    &account.account_login,
                    now,
                )
                .map_err(|_| conflict())?
        {
            save(&ctx, &v)?;
            self.drain(&ctx, &mut v).await?;
        }
        Ok(())
    }
    #[handler]
    async fn status(
        &self,
        ctx: SharedObjectContext<'_>,
    ) -> Result<Json<Option<EmailDeliverySnapshot>>, TerminalError> {
        Ok(Json(
            ctx.get::<Json<Retained>>("email")
                .await?
                .map(|v| v.0.snapshot),
        ))
    }
}
#[restate_sdk::object]
impl EmailDeliveryProjection {
    #[handler]
    async fn apply(
        &self,
        ctx: ObjectContext<'_>,
        Json(projection): Json<ghinvite_core::storage::email_delivery::EmailDeliveryProjection>,
    ) -> Result<(), TerminalError> {
        projection.validate().map_err(|_| conflict())?;
        if projection.snapshot.command.delivery_key() != ctx.key() {
            return Err(conflict());
        }
        ctx.run(|| async {
            self.storage
                .project_email_delivery_with_events(&projection)
                .await
                .map_err(HandlerError::from)?;
            #[cfg(feature = "integration")]
            if let Some(faults) = &self.faults
                && faults
                    .lose_projection_ack
                    .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                faults
                    .lost_projection_acks
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return Err(HandlerError::from(std::io::Error::other(
                    "fixture lost email projection acknowledgement",
                )));
            }
            Ok::<_, HandlerError>(())
        })
        .name("project_email_delivery")
        .await?;
        Ok(())
    }
}
