//! Receiving-side create receipts. Private ingress; ADR 0004.
use crate::AppState;
use ghinvite_core::delivery::{
    BlockedReason, CreateCommand, CreateOutcome, CreateReceipt, DeliverySnapshot,
};
use ghinvite_core::targets::RepoRole;
use restate_sdk::{
    context::{
        Context, ContextClient, ContextReadState, ContextSideEffects, ContextWriteState,
        ObjectContext, RunFuture, SharedObjectContext,
    },
    endpoint::Builder,
    errors::{HandlerError, TerminalError},
    serde::Json,
};

pub struct RepositoryDelivery {
    state: AppState,
    #[cfg(feature = "integration")]
    faults: Option<std::sync::Arc<DeliveryFaults>>,
}

/// Separate per-delivery queue: SQL retries never hold the create owner's lock.
pub struct DeliveryProjection {
    state: AppState,
    #[cfg(feature = "integration")]
    faults: Option<std::sync::Arc<DeliveryFaults>>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct DeliveryRecheck {
    pub command: CreateCommand,
    pub generation: u64,
}

#[cfg(feature = "integration")]
#[derive(Default)]
pub struct DeliveryFaults {
    pub cancel_after_result: std::sync::atomic::AtomicBool,
    pub cancel_after_reserve: std::sync::atomic::AtomicBool,
    pub lose_cancel_results: std::sync::atomic::AtomicUsize,
    pub lost_cancel_results: std::sync::atomic::AtomicUsize,
    pub stage: std::sync::Mutex<Option<String>>,
    pub interruptions: std::sync::atomic::AtomicUsize,
    pub projection_unavailable: std::sync::atomic::AtomicBool,
    pub projection_failures: std::sync::atomic::AtomicUsize,
    pub lose_http_result: std::sync::atomic::AtomicBool,
    pub lose_http_results: std::sync::atomic::AtomicUsize,
    pub lose_projection_ack: std::sync::atomic::AtomicBool,
    pub http_result_losses: std::sync::atomic::AtomicUsize,
    pub lose_release_ack: std::sync::atomic::AtomicBool,
    pub release_ack_losses: std::sync::atomic::AtomicUsize,
}

pub fn bind(builder: Builder, state: AppState) -> Builder {
    builder
        .bind(crate::sweep_schedule::SweepSchedule {
            organization_enabled: state.organization_delivery.is_some()
                || state.email_deliveries.is_some(),
            #[cfg(feature = "integration")]
            clock: state.sweep_clock.clone(),
        })
        .bind(RepositoryDelivery {
            state: state.clone(),
            #[cfg(feature = "integration")]
            faults: None,
        })
        .bind(DeliveryProjection {
            state: state.clone(),
            #[cfg(feature = "integration")]
            faults: None,
        })
        .bind(DeliveryRecovery)
        .bind(crate::settlement::DeliveryObservation {
            state: state.clone(),
        })
}

#[cfg(feature = "integration")]
pub fn bind_with_faults(
    builder: Builder,
    state: AppState,
    faults: std::sync::Arc<DeliveryFaults>,
) -> Builder {
    builder
        .bind(crate::sweep_schedule::SweepSchedule {
            organization_enabled: state.organization_delivery.is_some()
                || state.email_deliveries.is_some(),
            #[cfg(feature = "integration")]
            clock: state.sweep_clock.clone(),
        })
        .bind(RepositoryDelivery {
            state: state.clone(),
            faults: Some(faults.clone()),
        })
        .bind(DeliveryProjection {
            state: state.clone(),
            faults: Some(faults),
        })
        .bind(DeliveryRecovery)
        .bind(crate::settlement::DeliveryObservation { state })
}

pub struct DeliveryRecovery;

#[restate_sdk::service]
impl DeliveryRecovery {
    #[handler]
    async fn recover(
        &self,
        ctx: Context<'_>,
        Json(query): Json<crate::admission::RequestStatus>,
    ) -> Result<Json<ghinvite_core::request_lifecycle::DeliveryStatus>, TerminalError> {
        ctx.object_client::<crate::request_owner::InvitationRequestClient>(
            query.request_id.to_string(),
        )
        .recover(Json(query))
        .call()
        .await
    }
}

#[restate_sdk::object]
impl RepositoryDelivery {
    #[handler]
    async fn cancel_unsent(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<ghinvite_core::bulk_cancel::RepositoryCancelUnsent>,
    ) -> Result<Json<ghinvite_core::bulk_cancel::CancelUnsentResult>, TerminalError> {
        if self.state.bulk.is_none() {
            return Err(TerminalError::new_with_code(503, "bulk unavailable"));
        }
        crate::delivery_batch_cancel::cancel(&ctx, &self.state, input)
            .await
            .map(Json)
    }

    #[handler]
    async fn prepare_reissue(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<ghinvite_core::reissue::RepositoryReissueBarrierQuery>,
    ) -> Result<Json<ghinvite_core::reissue::InvitationBarrier>, TerminalError> {
        crate::delivery_reissue::prepare(
            &ctx,
            &self.state,
            input,
            #[cfg(feature = "integration")]
            self.faults.as_deref(),
        )
        .await
        .map(Json)
    }

    #[handler]
    async fn cascade_progress(
        &self,
        ctx: SharedObjectContext<'_>,
        Json(query): Json<ghinvite_core::revocation::DeliveryCascadeQuery>,
    ) -> Result<Json<ghinvite_core::revocation::DeliveryCascadeProgress>, TerminalError> {
        crate::delivery_control::cascade_progress(&ctx, query)
            .await
            .map(Json)
    }
    #[handler]
    async fn drive_cancellation(&self, ctx: ObjectContext<'_>) -> Result<(), TerminalError> {
        crate::delivery_cancellation::drive(
            &ctx,
            &self.state,
            #[cfg(feature = "integration")]
            self.faults.as_deref(),
        )
        .await
    }
    #[handler]
    async fn tick_own_expiry(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<crate::delivery_cancellation::ExpiryTick>,
    ) -> Result<(), TerminalError> {
        crate::delivery_cancellation::tick(&ctx, &self.state, input).await
    }
    #[handler]
    async fn on_webhook(
        &self,
        ctx: ObjectContext<'_>,
        input: Json<crate::github_invitation::OnWebhookInput>,
    ) -> Result<(), TerminalError> {
        crate::settlement::webhook(ctx, input).await
    }

    #[handler]
    async fn reconcile(
        &self,
        ctx: ObjectContext<'_>,
        input: Json<crate::settlement::ReconcileEvidence>,
    ) -> Result<(), TerminalError> {
        crate::settlement::reconcile(&ctx, input).await?;
        crate::delivery_control::advance(&ctx, &self.state).await
    }

    #[handler]
    async fn cancel(
        &self,
        ctx: ObjectContext<'_>,
        input: Json<crate::github_invitation::CancelInvitationInput>,
    ) -> Result<(), TerminalError> {
        crate::settlement::cancel(&self.state, ctx, input).await
    }

    #[handler]
    async fn tick_expire(
        &self,
        ctx: ObjectContext<'_>,
        input: Json<crate::github_invitation::TickExpireInput>,
    ) -> Result<(), TerminalError> {
        crate::settlement::expire(&self.state, ctx, input).await
    }
    #[handler]
    async fn control(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<crate::delivery_control::DeliveryControlInput>,
    ) -> Result<Json<crate::delivery_control::DeliveryControlResult>, TerminalError> {
        crate::delivery_control::control(&ctx, &self.state, input)
            .await
            .map(Json)
    }
    #[handler]
    async fn control_wake(
        &self,
        ctx: ObjectContext<'_>,
        Json(wake): Json<crate::delivery_control::ControlWake>,
    ) -> Result<(), TerminalError> {
        if let Some(command) = crate::delivery_control::wake_command(&ctx, &wake).await? {
            self.create(ctx, Json(command)).await?;
        }
        Ok(())
    }
    #[handler]
    async fn recheck(
        &self,
        ctx: ObjectContext<'_>,
        Json(wake): Json<DeliveryRecheck>,
    ) -> Result<(), TerminalError> {
        let command = wake.command;
        let previous = ctx.get::<Json<CreateCommand>>("input").await?.map(|c| c.0);
        if ctx.key() != command.delivery_key() || previous.as_ref() != Some(&command) {
            return Err(TerminalError::new_with_code(
                409,
                "recheck identity conflict",
            ));
        }
        if ctx.get::<u64>("recheck_scheduled").await? != Some(wake.generation) {
            return Ok(());
        }
        ctx.clear("recheck_scheduled");
        // Advance under this invocation: there is no queued create that an old
        // duplicate wake could race with to open a second scheduling stream.
        self.create(ctx, Json(command)).await?;
        Ok(())
    }
    #[handler]
    async fn status(
        &self,
        ctx: SharedObjectContext<'_>,
    ) -> Result<Json<Option<DeliverySnapshot>>, TerminalError> {
        Ok(Json(
            crate::repository_audit::shared_snapshot(&ctx)
                .await?
                .map(Json)
                .map(|r| r.0),
        ))
    }

    /// Schedule one read-only sweep, durably throttled per physical generation.
    #[handler]
    async fn refresh(
        &self,
        ctx: ObjectContext<'_>,
    ) -> Result<Json<ghinvite_core::delivery::RepositoryRefresh>, TerminalError> {
        use ghinvite_core::delivery::RepositoryRefresh;
        let Some(snapshot) = crate::repository_audit::snapshot(&ctx).await? else {
            return Ok(Json(RepositoryRefresh::NotTracked));
        };
        validate_notice_snapshot(ctx.key(), &snapshot)?;
        if snapshot.settlement.is_some()
            || snapshot.tracking_stopped.is_some()
            || !matches!(
                snapshot.create.outcome,
                CreateOutcome::Created { .. } | CreateOutcome::AlreadyPending { .. }
            )
        {
            return Ok(Json(RepositoryRefresh::NotTracked));
        }
        let now = crate::clock::now(&ctx, "repository_refresh_time").await?;
        if let Some(Json(at)) = ctx
            .get::<Json<chrono::DateTime<chrono::Utc>>>("manual_refreshed_at")
            .await?
            && now < at + chrono::Duration::seconds(60)
        {
            return Ok(Json(RepositoryRefresh::Throttled {
                retry_at: at + chrono::Duration::seconds(60),
            }));
        }
        ctx.set("manual_refreshed_at", Json(now));
        let command = &snapshot.create.command;
        ctx.service_client::<crate::reconcile::ReconcileClient>()
            .target(Json(crate::reconcile::TargetInput {
                account: ghinvite_core::account::AccountInstallationRef {
                    app: ghinvite_core::account::App::Repo,
                    account_id: command.account_id,
                },
                target_id: command.repo_id,
                deliveries: vec![command.delivery_key()],
            }))
            .send()
            .await?;
        Ok(Json(RepositoryRefresh::Requested))
    }

    #[handler]
    async fn create(
        &self,
        ctx: ObjectContext<'_>,
        Json(command): Json<CreateCommand>,
    ) -> Result<Json<CreateReceipt>, TerminalError> {
        if ctx.key() != command.delivery_key()
            || ghinvite_core::bulk::validate_origin(command.link_id, command.batch).is_err()
            || command.requester_id == 0
            || command.delivery_generation == 0
        {
            return Err(TerminalError::new_with_code(400, "invalid create command"));
        }
        if self.state.access_grants.is_some() {
            ctx.set("access_grants_enabled", true);
        }
        match ctx.get::<Json<CreateCommand>>("input").await? {
            Some(Json(old)) if old != command => {
                return Err(TerminalError::new_with_code(409, "create input conflict"));
            }
            Some(_) => (),
            None => {
                // Only the private request-owned issuance path produces approved
                // commands. Bind the complete command before any external work.
                ctx.set("input", Json(command.clone()));
            }
        }
        if let Some(receipt) =
            crate::delivery_batch_cancel::gate(&ctx, &self.state, &command).await?
        {
            return Ok(Json(receipt));
        }
        if self.state.invite_budget.is_some() {
            crate::delivery_budget::recover_unused(&ctx, self.state.storage.as_ref(), &command)
                .await?;
        }
        if let Some(receipt) = crate::delivery_control::gate(&ctx, &self.state, &command).await? {
            return Ok(Json(receipt));
        }
        let previous = crate::repository_audit::receipt(&ctx)
            .await?
            .map(Json)
            .map(|r| r.0);
        let audit_version = previous.as_ref().map_or(
            ghinvite_core::delivery_audit::new_owner_version(command.invitation_id),
            |receipt| receipt.audit_version,
        );
        if let Some(receipt) = &previous
            && receipt.outcome.confirmed()
        {
            let snapshot = crate::repository_audit::snapshot(&ctx)
                .await?
                .map(Json)
                .ok_or_else(|| TerminalError::new_with_code(500, "missing delivery snapshot"))?;
            send_projection(&ctx, &snapshot.0).await?;
            return Ok(Json(receipt.clone()));
        }
        let Json(account) = ctx
            .object_client::<crate::availability::AccountInstallationClient>(format!(
                "repo:{}",
                command.account_id
            ))
            .current_installation()
            .call()
            .await?;
        if matches!(
            previous.as_ref().map(|r| &r.outcome),
            Some(CreateOutcome::OutcomeUnknown)
        ) {
            let Json(snapshot) = crate::repository_audit::snapshot(&ctx)
                .await?
                .map(Json)
                .ok_or_else(|| TerminalError::new("missing unknown delivery snapshot"))?;
            let at = crate::clock::now(&ctx, "recovery_time").await?;
            let Json((observation, retry)) = ctx
                .run(|| async {
                    let result = match account.as_ref() {
                        Some(account) => {
                            match crate::settlement::observe(&self.state, &snapshot, account, at)
                                .await
                            {
                                Ok(observation) => (observation, None),
                                Err(error) if !error.is_terminal() => (
                                    None,
                                    Some(
                                        error
                                            .rate_limit()
                                            .map(|limit| {
                                                crate::throttle::backoff(limit.retry_after)
                                            })
                                            .unwrap_or(crate::throttle::MAXIMUM)
                                            .as_secs(),
                                    ),
                                ),
                                Err(_) => (None, None),
                            }
                        }
                        None => (None, None),
                    };
                    Ok::<_, HandlerError>(Json(result))
                })
                .name("recover_unknown_invitation")
                .await?;
            if let Some(observation) = observation {
                crate::settlement::apply_observation(&ctx, snapshot, &observation).await?;
                crate::delivery_control::advance(&ctx, &self.state).await?;
            }
            if let Some(seconds) = retry {
                self.schedule_recheck(&ctx, command, std::time::Duration::from_secs(seconds))
                    .await?;
            }
            let receipt = crate::repository_audit::receipt(&ctx)
                .await?
                .map(Json)
                .ok_or_else(|| TerminalError::new("missing recovered receipt"))?;
            return Ok(receipt);
        }
        let repo_app = self
            .state
            .github_apps
            .as_ref()
            .and_then(|apps| apps.apps.app(ghinvite_github::AppSlot::Repo).ok())
            .ok_or_else(|| {
                TerminalError::new_with_code(503, "repository app runtime unavailable")
            })?;
        let repo_app = &repo_app;
        if let Some(generation) = ctx.get::<u64>("rejected_generation").await? {
            let released = ctx
                .run(|| async {
                    Ok::<_, HandlerError>(
                        self.state
                            .storage
                            .reject_delivery_attempt(&command, generation)
                            .await
                            .is_ok(),
                    )
                })
                .name("release_rejected_generation")
                .await?;
            if !released {
                self.schedule_recheck(&ctx, command, BLOCKED_RECHECK_INTERVAL)
                    .await?;
                return previous
                    .map(Json)
                    .ok_or_else(|| TerminalError::new("missing rejected receipt"));
            }
            ctx.clear("rejected_generation");
        }
        let now = crate::clock::now(&ctx, "delivery_time").await?;
        let Json((fence_available, read)) = ctx.run(|| async {
            let available = self.state.storage.delivery_attempt_exists(&command).await.is_ok();
            let read = match account.as_ref() {
                Some(account) if account.account_id == command.account_id && account.uninstalled_at.is_none()
                    && !matches!(&account.selected_repos, ghinvite_core::SelectedRepos::Subset(ids) if !ids.contains(&command.repo_id)) => {
                    if available {
                        crate::repository_protocol::pre_read(repo_app,
                            account.installation_id, &command, now).await
                    } else {
                        crate::repository_protocol::PreRead::Stopped(CreateOutcome::Blocked { reason: BlockedReason::InstallationUnavailable })
                    }
                },
                _ => crate::repository_protocol::PreRead::Stopped(CreateOutcome::Blocked { reason: BlockedReason::InstallationUnavailable }),
            };
            Ok::<_, HandlerError>(Json((available, read)))
        }).name("pre_read_repository").await?;
        if !fence_available {
            self.schedule_recheck(&ctx, command, BLOCKED_RECHECK_INTERVAL)
                .await?;
            return Err(TerminalError::new_with_code(
                503,
                "delivery fence unavailable",
            ));
        }
        // S2 is pure policy; S3 is a durable owner-to-budget call, outside
        // both I/O runs. M0 composition leaves this explicitly disabled.
        if self.state.invite_budget.is_some()
            && let crate::repository_protocol::PreRead::Limited { limit, .. } = &read
        {
            crate::delivery_budget::finish(
                &ctx,
                &command,
                &crate::repository_protocol::BudgetEvidence {
                    effect: crate::repository_protocol::BudgetEffect::Retain,
                    limit: Some(limit.clone()),
                },
            )
            .await?;
        }
        let mut plan = crate::repository_protocol::plan(&read, RepoRole::from(command.permission));
        if self.state.invite_budget.is_some()
            && !matches!(plan, crate::repository_protocol::Plan::Complete(_))
        {
            match crate::delivery_budget::reserve(&ctx, &command, &plan).await? {
                Some(ghinvite_core::budget::Reservation::Granted { .. }) => {
                    self.checkpoint(&ctx, "after-budget-reserve").await?;
                }
                Some(ghinvite_core::budget::Reservation::RetryAt { at, why }) => {
                    plan = crate::repository_protocol::Plan::Complete(CreateOutcome::Throttled {
                        until: at,
                        reason: Some(why),
                    });
                }
                None => {
                    plan = crate::repository_protocol::Plan::Complete(CreateOutcome::OutcomeUnknown)
                }
            }
        }
        let mut attempted = match plan {
            crate::repository_protocol::Plan::Complete(outcome) => Attempt::from(CreateReceipt {
                audit_version,
                command: command.clone(),
                confirmed_at: outcome.confirmed().then_some(now),
                outcome,
                revision: 1,
            }),
            crate::repository_protocol::Plan::Put
            | crate::repository_protocol::Plan::Patch { .. } => {
                let account = account
                    .as_ref()
                    .expect("write plan requires a verified installation");
                // Retained before the run: a lost acknowledgement or cancelled
                // invocation cannot let a later control invent no-write proof.
                ctx.set(crate::delivery_control::MAY_HAVE_WRITTEN, true);
                let Json(attempted) = ctx
                    .run(|| async {
                        let result = crate::repository_protocol::write_stage(
                            repo_app,
                            account.installation_id,
                            self.state.storage.as_ref(),
                            &command,
                            &read,
                            now,
                        )
                        .await;
                        let mut attempted = Attempt::from(CreateReceipt {
                            audit_version,
                            command: command.clone(),
                            confirmed_at: result.outcome.confirmed().then_some(now),
                            outcome: result.outcome,
                            revision: 1,
                        });
                        attempted.timing = result.timing;
                        attempted.rejected_generation = result.rejected_generation;
                        attempted.budget = result.budget;
                        #[cfg(feature = "runtime-tests")]
                        if attempted.receipt.outcome.confirmed()
                            && crate::delivery_run_fixture::lose_confirmed_result()
                        {
                            return Err(std::io::Error::other(
                                "fixture: confirmed GitHub result lost before run acknowledgement",
                            )
                            .into());
                        }
                        #[cfg(feature = "integration")]
                        if let Some(faults) = &self.faults {
                            use std::sync::atomic::Ordering;
                            if attempted.receipt.outcome.confirmed()
                                && (faults.lose_http_result.swap(false, Ordering::SeqCst)
                                    || faults
                                        .lose_http_results
                                        .fetch_update(
                                            Ordering::SeqCst,
                                            Ordering::SeqCst,
                                            |remaining| remaining.checked_sub(1),
                                        )
                                        .is_ok())
                            {
                                faults.http_result_losses.fetch_add(1, Ordering::SeqCst);
                                return Err(std::io::Error::other(
                                    "fixture: GitHub result lost before journal acknowledgement",
                                )
                                .into());
                            }
                        }
                        Ok::<_, HandlerError>(Json(attempted))
                    })
                    .name("guarded_github_create")
                    .await?;
                if !matches!(
                    attempted.receipt.outcome,
                    CreateOutcome::OutcomeUnknown | CreateOutcome::Submitted
                ) {
                    ctx.set(crate::delivery_control::MAY_HAVE_WRITTEN, false);
                }
                attempted
            }
        };
        // Persist positive rejection before any cross-object budget call can
        // suspend this invocation. Recovery must not lose no-effect evidence.
        if let Some(generation) = attempted.rejected_generation {
            ctx.set("rejected_generation", generation);
        }
        if self.state.invite_budget.is_some() {
            crate::delivery_budget::finish(&ctx, &command, &attempted.budget).await?;
        }
        if let CreateOutcome::Throttled { until, .. } = &attempted.receipt.outcome {
            attempted.throttled_for_secs = Some((*until - now).num_seconds().max(1) as u64);
        }
        // Positive non-application evidence is journaled before release. Lost
        // release acknowledgements never turn a refused PUT into uncertainty.
        if let Some(generation) = attempted.rejected_generation {
            let released = ctx
                .run(|| async {
                    let released = self
                        .state
                        .storage
                        .reject_delivery_attempt(&command, generation)
                        .await
                        .is_ok();
                    #[cfg(feature = "integration")]
                    if let Some(faults) = &self.faults
                        && released
                        && faults
                            .lose_release_ack
                            .swap(false, std::sync::atomic::Ordering::SeqCst)
                    {
                        faults
                            .release_ack_losses
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        return Ok::<_, HandlerError>(false);
                    }
                    Ok::<_, HandlerError>(released)
                })
                .name("release_refused_put")
                .await?;
            if released {
                ctx.clear("rejected_generation");
            }
        }
        let timing = attempted.timing.clone().or_else(|| match &read {
            crate::repository_protocol::PreRead::Ready {
                pending: Some(pending),
                ..
            } => pending.timing.clone(),
            _ => None,
        });
        let NextReceipt {
            receipt,
            recheck_after,
        } = next_receipt(previous.as_ref(), attempted);
        crate::operational::record_unknown(&ctx, previous.as_ref(), &receipt).await?;
        if matches!(
            receipt.outcome,
            CreateOutcome::Blocked {
                reason: BlockedReason::InstallationUnavailable
                    | BlockedReason::InstallationPermissions
            }
        ) && let Some(account) = &account
        {
            ctx.object_client::<crate::installation::InstallationClient>(format!(
                "repo:{}",
                account.installation_id
            ))
            .recheck_permissions()
            .send()
            .await?;
        }
        let mut snapshot = DeliverySnapshot::from(receipt.clone());
        if let Some(timing) = timing {
            let timing = crate::delivery_control::invitation_timing(&ctx, timing).await?;
            // New owner state can include metadata in this same atomic revision.
            let revision = snapshot.revision;
            if snapshot.retain_invitation_timing(timing) {
                snapshot.revision = revision;
            }
        }
        crate::repository_audit::save(&ctx, &snapshot).await?;
        let projection = retain_projection(&ctx, &snapshot).await?;
        self.checkpoint(&ctx, "after-receipt").await?;
        send_retained_projection(&ctx, projection).await?;
        self.checkpoint(&ctx, "after-projection-send").await?;
        crate::delivery_control::advance(&ctx, &self.state).await?;
        // The object lock is released before the recheck — the wait is a
        // continuation, not a held retry. Exactly one recheck stands per
        // invitation. Explicit recovery keeps an existing timer, even if its
        // new guidance is sooner. Its generation rejects duplicate/stale wakes.
        if let Some(wait) = recheck_after {
            self.schedule_recheck(&ctx, command, wait).await?;
        }
        Ok(Json(receipt))
    }
}

impl RepositoryDelivery {
    async fn schedule_recheck(
        &self,
        ctx: &ObjectContext<'_>,
        command: CreateCommand,
        wait: std::time::Duration,
    ) -> Result<(), TerminalError> {
        if ctx.get::<u64>("recheck_scheduled").await?.is_none() {
            // A wake can reschedule without producing a receipt (fence outage).
            // Never reuse its generation, even after the scheduled slot clears.
            let generation = ctx
                .get::<u64>("recheck_generation")
                .await?
                .unwrap_or(0)
                .checked_add(1)
                .ok_or_else(|| TerminalError::new("delivery recheck generation exhausted"))?;
            ctx.set("recheck_generation", generation);
            ctx.set("recheck_scheduled", generation);
            ctx.object_client::<RepositoryDeliveryClient>(command.delivery_key())
                .recheck(Json(DeliveryRecheck {
                    command,
                    generation,
                }))
                .send_after(wait)
                .await?;
            self.checkpoint(ctx, "after-recheck-send").await?;
        }
        Ok(())
    }

    async fn checkpoint(
        &self,
        ctx: &ObjectContext<'_>,
        stage: &'static str,
    ) -> Result<(), TerminalError> {
        #[cfg(feature = "integration")]
        if let Some(faults) = &self.faults {
            ctx.run(|| async {
                if faults.stage.lock().unwrap().as_deref() == Some(stage) {
                    if faults
                        .interruptions
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                        == 0
                    {
                        return Err(std::io::Error::other(
                            "fixture: interrupt delivery journal boundary",
                        )
                        .into());
                    }
                    std::future::pending::<()>().await;
                }
                Ok::<_, HandlerError>(())
            })
            .name(stage)
            .await?;
        }
        let _ = (ctx, stage);
        Ok(())
    }
}

pub(crate) fn validate_notice_snapshot(
    key: &str,
    snapshot: &DeliverySnapshot,
) -> Result<(), TerminalError> {
    let command = &snapshot.create.command;
    ghinvite_core::delivery_audit::validate_identity(
        snapshot.create.audit_version,
        command.invitation_id,
    )
    .map_err(|message| TerminalError::new_with_code(409, message))?;
    let numeric = |id| id > 0 && id <= ghinvite_core::storage::integer::MAX_EXACT;
    if command.delivery_key() != key
        || ![
            command.account_id,
            command.installation_id,
            command.requester_id,
            command.repo_id,
            command.delivery_generation,
        ]
        .into_iter()
        .all(numeric)
        || snapshot
            .create
            .confirmed_at
            .is_some_and(|at| at < command.approved_at)
        || snapshot.settlement.as_ref().is_some_and(|settled| {
            snapshot
                .create
                .confirmed_at
                .is_none_or(|at| settled.event.occurred_at < at)
        })
    {
        return Err(TerminalError::new_with_code(
            409,
            "invalid retained repository delivery",
        ));
    }
    ghinvite_core::storage::delivery_projection::encode(snapshot)
        .map_err(|_| TerminalError::new_with_code(409, "invalid retained repository delivery"))?;
    Ok(())
}

pub(crate) async fn send_projection(
    ctx: &ObjectContext<'_>,
    receipt: &DeliverySnapshot,
) -> Result<(), TerminalError> {
    let projection = retain_projection(ctx, receipt).await?;
    send_retained_projection(ctx, projection).await
}

async fn retain_projection(
    ctx: &ObjectContext<'_>,
    receipt: &DeliverySnapshot,
) -> Result<ghinvite_core::delivery_audit::RepositoryDeliveryProjection, TerminalError> {
    crate::repository_audit::projection(ctx, receipt).await
}

async fn send_retained_projection(
    ctx: &ObjectContext<'_>,
    projection: ghinvite_core::delivery_audit::RepositoryDeliveryProjection,
) -> Result<(), TerminalError> {
    let retained_snapshot = projection.snapshot.clone();
    let receipt = &retained_snapshot;
    if ctx
        .get::<bool>("access_grants_enabled")
        .await?
        .unwrap_or(false)
        && let Some(grant) =
            ghinvite_core::access_grant::accepted_repository(receipt).map_err(TerminalError::new)?
    {
        ctx.object_client::<crate::access_grant::AccessGrantClient>(grant.key())
            .schedule(Json(grant))
            .send()
            .await?;
    }
    if ghinvite_core::request_completion::repository(receipt).is_some() {
        ctx.object_client::<crate::request_owner::InvitationRequestClient>(
            receipt.create.command.request_id.to_string(),
        )
        .on_delivery_settled(Json(ghinvite_core::request_completion::DeliveryNotice {
            kind: ghinvite_core::delivery_control::DeliveryKind::Repository,
            target_id: receipt.create.command.repo_id,
            delivery_generation: receipt.create.command.delivery_generation,
        }))
        .send()
        .await?;
    }
    let command = &receipt.create.command;
    let schedule = ctx.object_client::<crate::sweep_schedule::SweepScheduleClient>(format!(
        "repo:{}",
        command.account_id
    ));
    let registration = Json(crate::sweep_schedule::Registration {
        target_id: command.repo_id,
        delivery_key: command.delivery_key(),
    });
    if receipt.settlement.is_none()
        && receipt.tracking_stopped.is_none()
        && (receipt.create.outcome.needs_settlement()
            || matches!(receipt.create.outcome, CreateOutcome::OutcomeUnknown))
    {
        schedule.track(registration).send().await?;
    } else {
        schedule.untrack(registration).send().await?;
    }
    ctx.object_client::<DeliveryProjectionClient>(receipt.create.command.delivery_key())
        .apply(Json(projection))
        .send()
        .await?;
    crate::repository_audit::acknowledge(ctx).await?;
    crate::operational::send_projection(ctx, receipt).await
}

#[restate_sdk::object]
impl DeliveryProjection {
    #[handler]
    async fn telemetry(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<ghinvite_core::storage::operational::RepositoryTelemetry>,
    ) -> Result<(), TerminalError> {
        if ctx.key() != input.key() {
            return Err(TerminalError::new_with_code(400, "projection key mismatch"));
        }
        ctx.run(|| async {
            self.state
                .storage
                .project_repository_telemetry(&input)
                .await?;
            Ok::<_, HandlerError>(())
        })
        .name("project_delivery_telemetry")
        .await
    }

    #[handler]
    async fn apply(
        &self,
        ctx: ObjectContext<'_>,
        Json(projection): Json<ghinvite_core::delivery_audit::RepositoryDeliveryProjection>,
    ) -> Result<(), TerminalError> {
        if ctx.key() != projection.snapshot.create.command.delivery_key() {
            return Err(TerminalError::new_with_code(400, "projection key mismatch"));
        }
        ctx.run(|| async {
            #[cfg(feature = "integration")]
            if let Some(faults) = &self.faults
                && faults
                    .projection_unavailable
                    .load(std::sync::atomic::Ordering::SeqCst)
            {
                faults
                    .projection_failures
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return Err(
                    std::io::Error::other("fixture: receipt/audit projection unavailable").into(),
                );
            }
            self.state
                .storage
                .project_delivery_with_events(&projection)
                .await?;
            #[cfg(feature = "integration")]
            if let Some(faults) = &self.faults
                && faults
                    .lose_projection_ack
                    .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return Err(std::io::Error::other(
                    "fixture: SQL committed but acknowledgement lost",
                )
                .into());
            }
            Ok::<_, HandlerError>(())
        })
        .name("project_create_receipt")
        .await
    }
}

/// One create attempt's receipt, plus the bounded wait to recheck after when
/// GitHub throttled it rather than deciding it.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Attempt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timing: Option<ghinvite_core::delivery::InvitationTiming>,
    receipt: CreateReceipt,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    throttled_for_secs: Option<u64>,
    #[serde(default)]
    rejected_generation: Option<u64>,
    #[serde(default)]
    budget: crate::repository_protocol::BudgetEvidence,
}

impl From<CreateReceipt> for Attempt {
    fn from(receipt: CreateReceipt) -> Self {
        Self {
            receipt,
            timing: None,
            throttled_for_secs: None,
            rejected_generation: None,
            budget: Default::default(),
        }
    }
}

/// How long a create waits before rechecking the dependency that blocked it.
/// Throttling replaces this with GitHub's own guidance, which the same bound
/// caps, so an unexplained block never rechecks more slowly than a named one.
const BLOCKED_RECHECK_INTERVAL: std::time::Duration = crate::throttle::MAXIMUM;

/// The receipt a create retains after an attempt, and how long to wait before
/// rechecking it.
#[derive(Debug)]
struct NextReceipt {
    receipt: CreateReceipt,
    recheck_after: Option<std::time::Duration>,
}

/// Fold an attempt over the receipt retained before it.
///
/// A retained outcome unknown is never downgraded to blocked (ADR 0004).
/// `attempt` normally answers unknown itself from the delivery fence; the
/// retained receipt is the authority that keeps it so if the fence cannot.
///
/// A throttle clears on GitHub's own schedule, usually long before a missing
/// dependency does, so it rechecks on the bounded wait GitHub asked for. It
/// earns that recheck even where the outcome stayed unknown: rereading is
/// safe, and it is the only way a delivery GitHub would not let us read ever
/// resolves on its own.
fn next_receipt(previous: Option<&CreateReceipt>, attempt: Attempt) -> NextReceipt {
    let Attempt {
        mut receipt,
        throttled_for_secs,
        ..
    } = attempt;
    receipt.revision = previous.map_or(1, |r| r.revision + 1);
    if let Some(previous) = previous {
        receipt.audit_version = previous.audit_version;
    }
    if matches!(
        previous.map(|r| &r.outcome),
        Some(CreateOutcome::OutcomeUnknown)
    ) && matches!(
        receipt.outcome,
        CreateOutcome::Blocked { .. } | CreateOutcome::Throttled { .. }
    ) {
        receipt.outcome = CreateOutcome::OutcomeUnknown;
    }
    let blocked = matches!(
        receipt.outcome,
        CreateOutcome::Blocked { .. } | CreateOutcome::Throttled { .. }
    );
    let recheck_after = match throttled_for_secs {
        Some(secs) => Some(std::time::Duration::from_secs(secs)),
        None => blocked.then_some(BLOCKED_RECHECK_INTERVAL),
    };
    NextReceipt {
        receipt,
        recheck_after,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::dt;
    use ghinvite_core::GithubInvitationId;

    fn receipt(outcome: CreateOutcome, revision: u64) -> CreateReceipt {
        CreateReceipt {
            audit_version: Default::default(),
            command: CreateCommand {
                batch: None,
                budget_attribution: Default::default(),
                access_lifetime_seconds: None,
                budget_share_pct: 50,
                delivery_generation: 1,
                invitation_id: Some(GithubInvitationId::new()),
                link_id: Some(ghinvite_core::InvitationLinkId::new()),
                request_id: ghinvite_core::RequestId::new(),
                approval_id: "approval-1".into(),
                account_id: 100,
                installation_id: 9,
                requester_id: 8,
                repo_id: 10,
                repo_full_name: "acme/api".into(),
                permission: ghinvite_core::Permission::Push,
                approved_at: dt("2026-05-04T13:00:00Z"),
            },
            outcome,
            revision,
            confirmed_at: None,
        }
    }

    #[test]
    fn final_notice_rejects_invalid_retained_identity_receipt_and_revision() {
        let mut create = receipt(
            CreateOutcome::AlreadySatisfied {
                effective_role: ghinvite_core::targets::EffectiveRole::Write,
            },
            1,
        );
        create.confirmed_at = Some(create.command.approved_at);
        let snapshot = DeliverySnapshot::from(create);
        let key = snapshot.create.command.delivery_key();
        assert!(validate_notice_snapshot(&key, &snapshot).is_ok());
        assert!(validate_notice_snapshot("other:key:1", &snapshot).is_err());
        let mut invalid = snapshot.clone();
        invalid.create.command.account_id = 0;
        assert!(validate_notice_snapshot(&key, &invalid).is_err());
        let mut invalid = snapshot.clone();
        invalid.create.confirmed_at = None;
        assert!(validate_notice_snapshot(&key, &invalid).is_err());
        let mut invalid = snapshot.clone();
        invalid.create.confirmed_at =
            Some(invalid.create.command.approved_at - chrono::Duration::seconds(1));
        assert!(validate_notice_snapshot(&key, &invalid).is_err());
        let mut invalid = snapshot.clone();
        invalid.revision = 0;
        assert!(validate_notice_snapshot(&key, &invalid).is_err());
        let mut invalid = snapshot;
        invalid.tracking_stopped = Some(ghinvite_core::delivery::SettlementReason::LinkRevoked);
        assert!(validate_notice_snapshot(&key, &invalid).is_err());
    }

    fn blocked() -> CreateOutcome {
        CreateOutcome::Blocked {
            reason: BlockedReason::TargetUnavailable,
        }
    }

    fn attempted(outcome: CreateOutcome, throttled_for_secs: Option<u64>) -> Attempt {
        Attempt {
            timing: None,
            receipt: receipt(outcome, 1),
            throttled_for_secs,
            rejected_generation: None,
            budget: Default::default(),
        }
    }

    #[test]
    fn a_first_attempt_is_revision_one() {
        let next = next_receipt(None, attempted(CreateOutcome::OutcomeUnknown, None));

        assert_eq!(next.receipt.revision, 1);
    }

    #[test]
    fn a_later_attempt_follows_the_retained_revision() {
        let previous = receipt(blocked(), 4);

        let next = next_receipt(Some(&previous), attempted(blocked(), None));

        assert_eq!(next.receipt.revision, 5);
    }

    #[test]
    fn confirming_uncertainty_preserves_the_retained_audit_contract() {
        use ghinvite_core::delivery_audit::DeliveryAuditVersion::{CanonicalV1, Legacy};
        for (retained, incoming) in [(Legacy, CanonicalV1), (CanonicalV1, Legacy)] {
            let mut previous = receipt(CreateOutcome::OutcomeUnknown, 1);
            previous.audit_version = retained;
            let mut attempt = attempted(CreateOutcome::Created { upstream_id: 77 }, None);
            attempt.receipt.audit_version = incoming;
            let next = next_receipt(Some(&previous), attempt);
            assert_eq!(next.receipt.audit_version, retained);
            assert_eq!(
                next.receipt.outcome,
                CreateOutcome::Created { upstream_id: 77 }
            );
        }
    }

    #[test]
    fn a_retained_unknown_outcome_is_never_downgraded_to_blocked() {
        // The fence normally makes `attempt` answer unknown itself; the
        // retained receipt keeps that true even if the fence row is gone.
        let previous = receipt(CreateOutcome::OutcomeUnknown, 1);

        let next = next_receipt(Some(&previous), attempted(blocked(), None));

        assert_eq!(next.receipt.outcome, CreateOutcome::OutcomeUnknown);
        assert_eq!(next.recheck_after, None);
    }

    #[test]
    fn a_retained_unknown_outcome_still_rechecks_on_githubs_wait() {
        let previous = receipt(CreateOutcome::OutcomeUnknown, 1);

        let next = next_receipt(Some(&previous), attempted(blocked(), Some(45)));

        assert_eq!(next.receipt.outcome, CreateOutcome::OutcomeUnknown);
        assert_eq!(next.recheck_after, Some(std::time::Duration::from_secs(45)));
    }

    #[test]
    fn a_retained_unknown_outcome_may_be_confirmed() {
        let previous = receipt(CreateOutcome::OutcomeUnknown, 1);

        let next = next_receipt(
            Some(&previous),
            attempted(CreateOutcome::Created { upstream_id: 77 }, None),
        );

        assert_eq!(
            next.receipt.outcome,
            CreateOutcome::Created { upstream_id: 77 }
        );
        assert_eq!(next.recheck_after, None);
    }

    #[test]
    fn a_block_rechecks_on_the_dependency_cadence() {
        let next = next_receipt(None, attempted(blocked(), None));

        assert_eq!(next.receipt.outcome, blocked());
        assert_eq!(next.recheck_after, Some(BLOCKED_RECHECK_INTERVAL));
    }

    #[test]
    fn a_throttled_block_rechecks_on_githubs_wait() {
        let next = next_receipt(None, attempted(blocked(), Some(45)));

        assert_eq!(next.recheck_after, Some(std::time::Duration::from_secs(45)));
    }

    #[test]
    fn an_unthrottled_unknown_outcome_waits_for_explicit_recovery() {
        let next = next_receipt(None, attempted(CreateOutcome::OutcomeUnknown, None));

        assert_eq!(next.recheck_after, None);
    }
}
