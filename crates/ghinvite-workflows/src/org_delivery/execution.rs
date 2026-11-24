//! Durable per-step accounting surrounds journaled GitHub stages.
use super::*;
use crate::invite_budget::InviteBudgetClient;
use ghinvite_core::{
    budget::{BudgetKind, LinkShare, RecordLimit, Reservation},
    org_delivery::OrganizationBlockedReason,
};

fn budget_key(value: &Retained) -> String {
    format!("org:{}", value.snapshot.create.command.account_id)
}
fn map_org(step: &str, outcome: OrganizationCreateOutcome) -> protocol::StepResult {
    if step == "org" {
        return protocol::StepResult::Organization(outcome);
    }
    protocol::StepResult::Team(match outcome {
        OrganizationCreateOutcome::Blocked { reason } => TeamCreateOutcome::Blocked { reason },
        OrganizationCreateOutcome::Throttled { until, reason } => {
            TeamCreateOutcome::Throttled { until, reason }
        }
        _ => TeamCreateOutcome::OutcomeUnknown,
    })
}
impl OrganizationDelivery {
    pub(super) async fn schedule(
        &self,
        ctx: &ObjectContext<'_>,
        delay: std::time::Duration,
    ) -> Result<(), TerminalError> {
        let now = crate::clock::now(ctx, "organization_wake_time").await?;
        let due = now
            .checked_add_signed(chrono::Duration::from_std(delay).map_err(|_| conflict())?)
            .ok_or_else(conflict)?;
        if ctx.get::<u64>("scheduled_wake").await?.is_some()
            && ctx
                .get::<Json<DateTime<Utc>>>("wake_due")
                .await?
                .is_some_and(|old| old.0 <= due)
        {
            return Ok(());
        }
        let generation = increment(ctx.get::<u64>("wake_generation").await?.unwrap_or(0))?;
        ctx.set("wake_generation", generation);
        ctx.set("scheduled_wake", generation);
        ctx.set("wake_due", Json(due));
        ctx.object_client::<OrganizationDeliveryClient>(ctx.key())
            .recheck(Json(Wake { generation }))
            .send_after(delay)
            .await?;
        Ok(())
    }
    pub(super) async fn record_limit(
        &self,
        ctx: &ObjectContext<'_>,
        value: &Retained,
        mut limit: RecordLimit,
    ) -> Result<(), TerminalError> {
        crate::delivery_budget::bind_limit(ctx, "organization-read", &mut limit);
        ctx.set("read_limit", Json((budget_key(value), limit.clone())));
        ctx.object_client::<InviteBudgetClient>(budget_key(value))
            .record_limit(Json(limit))
            .call()
            .await?;
        ctx.clear("read_limit");
        Ok(())
    }
    pub(super) async fn flush_all(
        &self,
        ctx: &ObjectContext<'_>,
        value: &mut Retained,
    ) -> Result<(), TerminalError> {
        if let Some(Json((key, limit))) =
            ctx.get::<Json<(String, RecordLimit)>>("read_limit").await?
        {
            ctx.object_client::<InviteBudgetClient>(key)
                .record_limit(Json(limit))
                .call()
                .await?;
            ctx.clear("read_limit");
        }
        for step in value.steps.keys().cloned().collect::<Vec<_>>() {
            self.finish(ctx, value, &step).await?;
        }
        cancellation::flush(self, ctx, value).await?;
        Ok(())
    }
    async fn finish(
        &self,
        ctx: &ObjectContext<'_>,
        value: &mut Retained,
        step: &str,
    ) -> Result<(), TerminalError> {
        let Some(completion) = value.steps[step].completion.clone() else {
            return Ok(());
        };
        let input = attempt(value, step);
        if let Some(limit) = completion.result.limit.clone() {
            ctx.object_client::<InviteBudgetClient>(budget_key(value))
                .record_limit(Json(limit))
                .call()
                .await?;
        }
        if completion.result.unused
            && let Some(slot) = value.steps[step].slot
        {
            ctx.object_client::<InviteBudgetClient>(budget_key(value))
                .release(Json(slot))
                .call()
                .await?;
        }
        if let Some(generation) = completion.result.rejected_generation {
            ctx.run(|| async {
                self.storage
                    .reject_organization_attempt(&input, generation)
                    .await
                    .map_err(HandlerError::from)
            })
            .name("organization_release_rejected_attempt")
            .await?;
        }
        if completion.result.recheck_permissions {
            ctx.object_client::<crate::installation::InstallationClient>(format!(
                "org:{}",
                input.create.installation_id
            ))
            .recheck_permissions()
            .send();
        }
        let record = value.steps.get_mut(step).expect("step");
        record.may_have_written = !completion.result.unused;
        record.reserve = None;
        record.slot = None;
        record.completion = None;
        if step == "org" && completion.result.unused {
            for (name, record) in &mut value.steps {
                if name != "org" {
                    record.may_have_written = false;
                }
            }
        }
        let org_unknown = step == "org"
            && matches!(
                completion.result.outcome,
                protocol::StepResult::Organization(OrganizationCreateOutcome::OutcomeUnknown)
            );
        set_step(value, step, completion.result.outcome, completion.at)?;
        if step == "org" && (completion.result.unused || org_unknown) {
            for id in value.snapshot.teams.keys().copied().collect::<Vec<_>>() {
                set_step(
                    value,
                    &format!("team:{id}"),
                    protocol::StepResult::Team(if org_unknown {
                        TeamCreateOutcome::OutcomeUnknown
                    } else {
                        TeamCreateOutcome::Planned
                    }),
                    completion.at,
                )?;
            }
        }
        // One successful org POST carries all immutable teams. No Team PUT is
        // issued for these attached teams, and their individual receipts freeze.
        if step == "org"
            && let OrganizationCreateOutcome::Created { upstream_id, .. } =
                value.snapshot.create.outcome
        {
            for id in value.snapshot.teams.keys().copied().collect::<Vec<_>>() {
                set_step(
                    value,
                    &format!("team:{id}"),
                    protocol::StepResult::Team(TeamCreateOutcome::Created {
                        via: TeamCreation::OrganizationInvitation { upstream_id },
                    }),
                    completion.at,
                )?;
            }
        }
        save(ctx, value).await?;
        #[cfg(feature = "integration")]
        if let Some(faults) = &self.faults {
            ctx.run(|| async {
                if faults
                    .pause_after_audit
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    if faults
                        .audit_interruptions
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                        == 0
                    {
                        return Err(HandlerError::from(std::io::Error::other(
                            "fixture organization atomic receipt checkpoint",
                        )));
                    }
                    std::future::pending::<()>().await;
                }
                Ok::<_, HandlerError>(())
            })
            .name("fixture_after_organization_audit")
            .await?;
        }
        project(ctx, value).await?;
        Ok(())
    }
    /// A retained reservation whose write stage never began is recovered only
    /// with positive absent-fence evidence; outage/uncertainty retains its charge.
    pub(super) async fn recover_unused(
        &self,
        ctx: &ObjectContext<'_>,
        value: &mut Retained,
        step: &str,
    ) -> Result<(), TerminalError> {
        let record = &value.steps[step];
        let Some(reservation) = record.reserve.clone() else {
            return Ok(());
        };
        if record.may_have_written || record.completion.is_some() {
            return Ok(());
        }
        let input = attempt(value, step);
        let clear = ctx
            .run(|| async {
                self.storage
                    .organization_attempt_exists(&input)
                    .await
                    .map(|exists| !exists)
                    .map_err(HandlerError::from)
            })
            .name("organization_unused_fence")
            .await?;
        if !clear {
            return Ok(());
        }
        let budget = ctx.object_client::<InviteBudgetClient>(budget_key(value));
        let slot = match record.slot {
            Some(slot) => Some(slot),
            None => match budget.reserve(Json(reservation)).call().await?.0 {
                Reservation::Granted { slot_id } => Some(slot_id),
                Reservation::RetryAt { .. } => None,
            },
        };
        if let Some(slot) = slot {
            budget.release(Json(slot)).call().await?;
        }
        let record = value.steps.get_mut(step).expect("step");
        record.reserve = None;
        record.slot = None;
        save(ctx, value).await?;
        Ok(())
    }
    async fn reserve(
        &self,
        ctx: &ObjectContext<'_>,
        value: &mut Retained,
        step: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, TerminalError> {
        let budget = ctx.object_client::<InviteBudgetClient>(budget_key(value));
        if let Some(link_id) = value.snapshot.create.command.budget_link_id() {
            budget
                .set_link_share(Json(LinkShare {
                    link_id,
                    percent: value.snapshot.create.command.budget_share_pct,
                }))
                .call()
                .await?;
        }
        let record = value.steps.get_mut(step).expect("step");
        if record.reserve.is_none() {
            record.ordinal = increment(record.ordinal)?;
            record.reserve = Some(Reserve {
                request_key: format!(
                    "{}/{step}/{}",
                    value.snapshot.create.command.storage_id(),
                    record.ordinal
                ),
                kind: if step == "org" {
                    BudgetKind::OrgInvite
                } else {
                    BudgetKind::OtherWrite
                },
                link_id: value.snapshot.create.command.budget_link_id(),
            });
        }
        let request = record.reserve.clone().expect("retained reserve");
        save(ctx, value).await?;
        match budget.reserve(Json(request)).call().await?.0 {
            Reservation::Granted { slot_id } => {
                value.steps.get_mut(step).expect("step").slot = Some(slot_id);
                save(ctx, value).await?;
                Ok(true)
            }
            Reservation::RetryAt { at, why } => {
                value.steps.get_mut(step).expect("step").reserve = None;
                set_step(
                    value,
                    step,
                    map_org(
                        step,
                        OrganizationCreateOutcome::Throttled {
                            until: at,
                            reason: Some(why),
                        },
                    ),
                    now,
                )?;
                save(ctx, value).await?;
                project(ctx, value).await?;
                self.schedule(ctx, (at - now).to_std().unwrap_or_default())
                    .await?;
                Ok(false)
            }
        }
    }
    async fn perform(
        &self,
        ctx: &ObjectContext<'_>,
        value: &mut Retained,
        step: &str,
        account: &ghinvite_core::Account,
        now: DateTime<Utc>,
    ) -> Result<(), TerminalError> {
        if !self.reserve(ctx, value, step, now).await? {
            return Ok(());
        }
        value.steps.get_mut(step).expect("step").may_have_written = true;
        value.steps.get_mut(step).expect("step").started_at = Some(now);
        if step == "org" {
            // The same POST carries every team. They share its possible effect
            // even though no independent team:<id> write fence exists yet.
            for (name, record) in &mut value.steps {
                if name != "org" {
                    record.may_have_written = true;
                }
            }
        }
        set_step(
            value,
            step,
            if step == "org" {
                protocol::StepResult::Organization(OrganizationCreateOutcome::Submitted)
            } else {
                protocol::StepResult::Team(TeamCreateOutcome::Submitted)
            },
            now,
        )?;
        if step == "org" {
            for id in value.snapshot.teams.keys().copied().collect::<Vec<_>>() {
                set_step(
                    value,
                    &format!("team:{id}"),
                    protocol::StepResult::Team(TeamCreateOutcome::Submitted),
                    now,
                )?;
            }
        }
        save(ctx, value).await?;
        project(ctx, value).await?;
        let input = attempt(value, step);
        let apps = self
            .state
            .github_apps
            .as_ref()
            .expect("checked composition");
        let result = ctx
            .run(|| async {
                let result =
                    protocol::write(apps, self.storage.as_ref(), &input, account, now).await;
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
                    return Err(HandlerError::from(std::io::Error::other(
                        "fixture organization result acknowledgement lost",
                    )));
                }
                Ok::<_, HandlerError>(Json(result))
            })
            .name("organization_write_stage")
            .await?
            .0;
        // Retain positive outcome/limit/no-effect evidence before any RPC.
        let mut result = result;
        if let Some(limit) = &mut result.limit {
            crate::delivery_budget::bind_limit(ctx, &format!("organization-write:{step}"), limit);
        }
        value.steps.get_mut(step).expect("step").completion = Some(Completion {
            result,
            at: crate::clock::now(ctx, "organization_write_completed_at").await?,
        });
        save(ctx, value).await?;
        self.finish(ctx, value, step).await
    }
    pub(super) async fn drive(
        &self,
        ctx: &ObjectContext<'_>,
        value: &mut Retained,
    ) -> Result<(), TerminalError> {
        self.flush_all(ctx, value).await?;
        if cancel_unsent::stopped(ctx, &value.snapshot.create.command).await? {
            return project(ctx, value).await;
        }
        for step in value.steps.keys().cloned().collect::<Vec<_>>() {
            self.recover_unused(ctx, value, &step).await?;
        }
        let inbox = ctx
            .get::<Json<Inbox>>("control")
            .await?
            .ok_or_else(conflict)?
            .0;
        let now = crate::clock::now(ctx, "organization_delivery_time").await?;
        control::advance(self, ctx, value, &inbox, now).await?;
        project(ctx, value).await?;
        let has_work = value.steps.keys().any(|step| {
            !confirmed(value, step)
                && !retry::unsupported(value, step)
                && (control::ready(value, step) || value.steps[step].may_have_written)
        });
        if !has_work {
            return Ok(());
        }
        if let Some(at) = value.retry_not_before
            && now < at
        {
            return self
                .schedule(ctx, (at - now).to_std().map_err(|_| conflict())?)
                .await;
        }
        value.retry_not_before = None;
        let account = ctx
            .object_client::<crate::availability::AccountInstallationClient>(format!(
                "org:{}",
                value.snapshot.create.command.account_id
            ))
            .current_installation()
            .call()
            .await?
            .0;
        let Some(account) = account else {
            for step in value.steps.keys().cloned().collect::<Vec<_>>() {
                if !confirmed(value, &step) && control::ready(value, &step) {
                    set_step(
                        value,
                        &step,
                        map_org(
                            &step,
                            OrganizationCreateOutcome::Blocked {
                                reason: OrganizationBlockedReason::InstallationUnavailable,
                            },
                        ),
                        now,
                    )?;
                }
            }
            save(ctx, value).await?;
            project(ctx, value).await?;
            return self.schedule(ctx, std::time::Duration::from_secs(60)).await;
        };
        let apps = self
            .state
            .github_apps
            .as_ref()
            .ok_or_else(|| TerminalError::new_with_code(503, "organization app unavailable"))?;
        let command = value.snapshot.create.command.clone();
        let read = ctx
            .run(|| async {
                Ok::<_, HandlerError>(Json(
                    protocol::pre_read(apps, &command, &account, now).await,
                ))
            })
            .name("organization_read_stage")
            .await?
            .0;
        let read = match read {
            protocol::PreRead::Ready(read) => read,
            protocol::PreRead::Stopped { outcome, limit } => {
                if let Some(limit) = limit {
                    self.record_limit(ctx, value, limit).await?;
                }
                for step in value.steps.keys().cloned().collect::<Vec<_>>() {
                    if !confirmed(value, &step)
                        && !value.steps[&step].may_have_written
                        && control::ready(value, &step)
                    {
                        set_step(value, &step, map_org(&step, outcome.clone()), now)?;
                    }
                }
                save(ctx, value).await?;
                project(ctx, value).await?;
                return self.schedule(ctx, std::time::Duration::from_secs(60)).await;
            }
        };
        value.last_observed_at = Some(value.last_observed_at.map_or(now, |old| old.max(now)));
        telemetry::memberships(value, &read, now);
        // A positive target refusal is visible even when the organization POST
        // cannot proceed with its immutable all-team set. Never overwrite an
        // uncertain attached team or confirmed receipt with a read refusal.
        for (id, observed) in &read.teams {
            let step = format!("team:{id}");
            if matches!(observed, protocol::TeamRead::Blocked { .. })
                && !confirmed(value, &step)
                && !value.steps[&step].may_have_written
                && control::ready(value, &step)
                && let Some(outcome) = protocol::team_plan(observed)
            {
                set_step(value, &step, protocol::StepResult::Team(outcome), now)?;
            }
        }
        if !confirmed(value, "org") {
            if value.steps["org"].may_have_written {
                let recovered = match &read.organization {
                    protocol::OrganizationRead::Pending {
                        upstream_id,
                        role: OrganizationInvitationRole::DirectMember,
                        created_at,
                        teams,
                    } if value.steps["org"]
                        .started_at
                        .is_some_and(|start| created_at.timestamp() >= start.timestamp())
                        && *created_at <= now
                        && teams
                            == &command.teams.iter().map(|t| t.team_id).collect::<Vec<_>>() =>
                    {
                        Some(OrganizationCreateOutcome::Created {
                            upstream_id: *upstream_id,
                            created_at: *created_at,
                        })
                    }
                    _ => None,
                };
                if let Some(outcome) = recovered {
                    set_step(
                        value,
                        "org",
                        protocol::StepResult::Organization(outcome),
                        now,
                    )?;
                    if let OrganizationCreateOutcome::Created { upstream_id, .. } =
                        value.snapshot.create.outcome
                    {
                        for id in command.teams.iter().map(|t| t.team_id) {
                            set_step(
                                value,
                                &format!("team:{id}"),
                                protocol::StepResult::Team(TeamCreateOutcome::Created {
                                    via: TeamCreation::OrganizationInvitation { upstream_id },
                                }),
                                now,
                            )?;
                        }
                    }
                } else {
                    set_step(
                        value,
                        "org",
                        protocol::StepResult::Organization(
                            OrganizationCreateOutcome::OutcomeUnknown,
                        ),
                        now,
                    )?;
                }
            } else if control::ready(value, "org") {
                if let Some(outcome) = protocol::organization_plan(&read) {
                    set_step(
                        value,
                        "org",
                        protocol::StepResult::Organization(outcome),
                        now,
                    )?;
                } else {
                    self.perform(ctx, value, "org", &account, now).await?;
                }
            }
        }
        let organization_ready = matches!(
            value.snapshot.create.outcome,
            OrganizationCreateOutcome::Created { .. }
                | OrganizationCreateOutcome::AlreadySatisfied { .. }
                | OrganizationCreateOutcome::AlreadyPending { .. }
        );
        if organization_ready {
            for id in command.teams.iter().map(|t| t.team_id) {
                let step = format!("team:{id}");
                if confirmed(value, &step) || retry::unsupported(value, &step) {
                    continue;
                }
                if value.steps[&step].may_have_written {
                    if let Some(outcome) = protocol::team_plan(&read.teams[&id])
                        && matches!(outcome, TeamCreateOutcome::AlreadySatisfied { .. })
                    {
                        set_step(value, &step, protocol::StepResult::Team(outcome), now)?;
                    }
                } else if control::ready(value, &step) {
                    if let Some(outcome) = protocol::team_plan(&read.teams[&id]) {
                        set_step(value, &step, protocol::StepResult::Team(outcome), now)?;
                    } else {
                        self.perform(ctx, value, &step, &account, now).await?;
                    }
                }
            }
        }
        if let protocol::OrganizationRead::Pending {
            upstream_id,
            created_at,
            ..
        } = &read.organization
            && let Some(timing) = ghinvite_core::delivery::InvitationTiming::observed(
                *upstream_id,
                *created_at,
                now,
                command.approved_at,
                matches!(
                    value.snapshot.create.outcome,
                    OrganizationCreateOutcome::Created { .. }
                ),
            )
        {
            value.snapshot.retain_invitation_timing(timing);
        }
        if retry::no_seat(value) {
            value.retry_not_before =
                Some(now + chrono::Duration::seconds(retry::NO_SEAT_SECONDS as i64));
        }
        save(ctx, value).await?;
        let control_now = crate::clock::now(ctx, "organization_post_write_control_time").await?;
        control::advance(self, ctx, value, &inbox, control_now).await?;
        project(ctx, value).await?;
        if value.steps.keys().any(|step| {
            !confirmed(value, step)
                && !retry::unsupported(value, step)
                && !matches!(
                    value.steps[step].control.as_ref().map(|s| s.disposition()),
                    Some(model::Disposition::Held { .. } | model::Disposition::Aborted)
                )
        }) {
            let seconds = if retry::no_seat(value) {
                retry::NO_SEAT_SECONDS
            } else {
                60
            };
            self.schedule(ctx, std::time::Duration::from_secs(seconds))
                .await?;
        }
        Ok(())
    }
}
