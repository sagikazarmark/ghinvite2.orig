//! Full-snapshot S0 inbox, independently evaluated against every delivery step.
use super::*;
use ghinvite_core::{
    delivery::{AbortReason, HoldReason, SettlementReason},
    delivery_control::{BudgetDisposition, DeliveryKind, Disposition, Event, Facts, Phase},
    revocation::{CascadeEntryState, DeliveryCascadeProgress, DeliveryCascadeQuery},
};

pub(super) fn matches(input: &DeliveryControlInput, command: &OrganizationCreateCommand) -> bool {
    let id = &input.envelope.identity;
    id.kind == DeliveryKind::Organization
        && id.link_id == command.link_id
        && id.batch == command.batch
        && id.request_id == command.request_id
        && id.target_id == command.organization.org_id
        && id.delivery_generation == command.delivery_generation
        && input.admitted_at <= command.approved_at
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
fn no_write() -> Facts {
    Facts {
        phase: Phase::NoWriteProven,
        delivery_revision: 1,
        last_observed_at: None,
        budget: BudgetDisposition::None,
    }
}
fn identity_key(id: &model::Identity) -> String {
    ghinvite_core::delivery::delivery_key(id.request_id, id.target_id, id.delivery_generation)
}

pub(super) async fn apply(
    owner: &OrganizationDelivery,
    ctx: &ObjectContext<'_>,
    input: DeliveryControlInput,
) -> Result<ControlResult, TerminalError> {
    if input.envelope.identity.kind != DeliveryKind::Organization
        || identity_key(&input.envelope.identity) != ctx.key()
    {
        return Err(conflict());
    }
    let receipt_key = format!(
        "control_receipt/{}",
        String::from(input.envelope.operation_id.clone())
    );
    if let Some(Json(old)) = ctx.get::<Json<ControlReceipt>>(&receipt_key).await? {
        return if old.input == input {
            Ok(old.result)
        } else {
            Err(conflict())
        };
    }
    let existing = ctx.get::<Json<Inbox>>("control").await?.map(|v| v.0);
    let state = if let Some(old) = &existing {
        if old.input.admitted_at != input.admitted_at
            || old.input.invitation_lifetime_seconds != input.invitation_lifetime_seconds
            || old.input.envelope.identity != input.envelope.identity
        {
            return Err(conflict());
        }
        old.state.clone()
    } else {
        model::State::new(
            input.envelope.identity.clone(),
            input.admitted_at,
            input.invitation_lifetime_seconds,
        )
        .map_err(model_error)?
    };
    let mut retained = ctx
        .get::<Json<Retained>>("organization")
        .await?
        .map(|v| v.0);
    if retained
        .as_ref()
        .is_some_and(|v| !matches(&input, &v.snapshot.create.command))
    {
        return Err(conflict());
    }
    let now = crate::clock::now(ctx, "organization_control_time").await?;
    let transition = model::reduce(
        &state,
        &no_write(),
        Event::Apply(input.envelope.clone()),
        now,
    )
    .map_err(model_error)?;
    let latest = existing
        .filter(|old| old.input.envelope.revision > input.envelope.revision)
        .unwrap_or_else(|| Inbox {
            input: input.clone(),
            state: transition.state,
        });
    ctx.set("control", Json(latest.clone()));
    if let Some(value) = retained.as_mut() {
        owner.flush_all(ctx, value).await?;
        advance(owner, ctx, value, &latest, now).await?;
        save(ctx, value).await?;
        project(ctx, value).await?;
        // A durable one-way wake cannot block the control acknowledgement.
        owner.schedule(ctx, std::time::Duration::ZERO).await?;
    }
    let result = ControlResult {
        revision: latest.input.envelope.revision,
        initialized: retained.is_some(),
    };
    ctx.set(
        &receipt_key,
        Json(ControlReceipt {
            input,
            result: result.clone(),
        }),
    );
    Ok(result)
}
async fn phase(
    owner: &OrganizationDelivery,
    ctx: &ObjectContext<'_>,
    value: &mut Retained,
    step: &str,
) -> Result<Phase, TerminalError> {
    if step == "org" {
        if value.snapshot.settlement.is_some() || value.snapshot.tracking_stopped.is_some() {
            return Ok(Phase::Final);
        }
        match value.snapshot.create.outcome {
            OrganizationCreateOutcome::Created {
                upstream_id,
                created_at,
            } => {
                return Ok(Phase::OwnedInvitation {
                    upstream_id,
                    created_at,
                });
            }
            OrganizationCreateOutcome::AlreadyPending { upstream_id, .. } => {
                return Ok(Phase::ForeignInvitation { upstream_id });
            }
            ref other if other.confirmed() => return Ok(Phase::Final),
            _ => {}
        }
    } else {
        let team = &value.snapshot.teams[&step[5..].parse::<u64>().expect("known team")];
        if team.create.outcome.confirmed() {
            return Ok(Phase::Final);
        }
    }
    if value.steps[step].may_have_written
        || (step != "org"
            && value.steps["org"].may_have_written
            && !value.snapshot.create.outcome.confirmed())
    {
        return Ok(Phase::MayHaveWritten);
    }
    let input = attempt(value, step);
    let fenced = ctx
        .run(|| async {
            Ok::<_, HandlerError>(owner.storage.organization_attempt_exists(&input).await.ok())
        })
        .name("organization_control_fence")
        .await?;
    if fenced == Some(true) {
        value.steps.get_mut(step).expect("step").may_have_written = true;
        save(ctx, value).await?;
    }
    Ok(if fenced == Some(false) {
        Phase::NoWriteProven
    } else {
        Phase::MayHaveWritten
    })
}
pub(super) async fn advance(
    owner: &OrganizationDelivery,
    ctx: &ObjectContext<'_>,
    value: &mut Retained,
    inbox: &Inbox,
    now: DateTime<Utc>,
) -> Result<(), TerminalError> {
    for step in value.steps.keys().cloned().collect::<Vec<_>>() {
        if step != "org"
            && matches!(
                inbox.input.envelope.control,
                model::LinkControl::Revoked {
                    cancel_pending_invitations: true,
                    ..
                }
            )
        {
            let id = step[5..].parse::<u64>().expect("known team");
            let team = value.snapshot.teams.get_mut(&id).expect("known team");
            if matches!(
                team.create.outcome,
                TeamCreateOutcome::Created {
                    via: TeamCreation::PendingMembership
                }
            ) && team.settlement.is_none()
                && team.tracking_stopped.is_none()
            {
                // Team PUT has no owned invitation ID. Revocation stops local
                // tracking and never borrows a foreign organization DELETE.
                team.tracking_stopped = Some(SettlementReason::LinkRevoked);
                team.revision = increment(team.revision)?;
                value.snapshot.revision = increment(value.snapshot.revision)?;
            }
        }
        let phase = phase(owner, ctx, value, &step).await?;
        let facts = Facts {
            phase,
            delivery_revision: value.snapshot.revision,
            last_observed_at: None,
            budget: BudgetDisposition::None,
        };
        let previous = value.steps[&step].control.clone().unwrap_or(
            model::State::new(
                inbox.input.envelope.identity.clone(),
                inbox.input.admitted_at,
                inbox.input.invitation_lifetime_seconds,
            )
            .map_err(model_error)?,
        );
        let next = model::reduce(
            &previous,
            &facts,
            Event::Apply(inbox.input.envelope.clone()),
            now,
        )
        .map_err(model_error)?;
        let mut effects = next.effects;
        let next = model::reduce(&next.state, &facts, Event::Advance, now).map_err(model_error)?;
        effects.extend(next.effects);
        let disposition = next.state.disposition().clone();
        value.steps.get_mut(&step).expect("step").control = Some(next.state);
        let result = match disposition {
            Disposition::Held { .. } => Some(if step == "org" {
                protocol::StepResult::Organization(OrganizationCreateOutcome::Held {
                    reason: HoldReason::AbuseSuspected,
                })
            } else {
                protocol::StepResult::Team(TeamCreateOutcome::Held {
                    reason: HoldReason::AbuseSuspected,
                })
            }),
            Disposition::Aborted => Some(if step == "org" {
                protocol::StepResult::Organization(OrganizationCreateOutcome::Aborted {
                    reason: AbortReason::LinkRevoked,
                })
            } else {
                protocol::StepResult::Team(TeamCreateOutcome::Aborted {
                    reason: AbortReason::LinkRevoked,
                })
            }),
            Disposition::Ready => {
                let held = if step == "org" {
                    matches!(
                        value.snapshot.create.outcome,
                        OrganizationCreateOutcome::Held { .. }
                    )
                } else {
                    matches!(
                        value.snapshot.teams[&step[5..].parse::<u64>().expect("team")]
                            .create
                            .outcome,
                        TeamCreateOutcome::Held { .. }
                    )
                };
                held.then(|| {
                    if step == "org" {
                        protocol::StepResult::Organization(OrganizationCreateOutcome::Planned)
                    } else {
                        protocol::StepResult::Team(TeamCreateOutcome::Planned)
                    }
                })
            }
            Disposition::StopTrackingForeign if step == "org" => {
                if value.snapshot.tracking_stopped.is_none() {
                    value.snapshot.tracking_stopped = Some(SettlementReason::LinkRevoked);
                    value.snapshot.revision = increment(value.snapshot.revision)?;
                }
                None
            }
            // Unknown and cancellation are retained intentions. Neither an
            // absent list nor this local reducer authorizes a new write/DELETE.
            _ => None,
        };
        if let Disposition::Held { epoch } = disposition {
            let already_held = if step == "org" {
                matches!(
                    value.snapshot.create.outcome,
                    OrganizationCreateOutcome::Held { .. }
                )
            } else {
                matches!(
                    value.snapshot.teams[&step[5..].parse::<u64>().expect("team")]
                        .create
                        .outcome,
                    TeamCreateOutcome::Held { .. }
                )
            };
            if already_held && audit::held_epoch_changed(value, &step, epoch) {
                // The receipt enum is unchanged, but a fresh hold episode must
                // have a distinct globally revision-bound audit identity.
                value.snapshot.revision = increment(value.snapshot.revision)?;
            }
        }
        if let Some(result) = result {
            set_step(value, &step, result, now)?;
        }
        save(ctx, value).await?;
        if step == "org" && owner.state.cancellation_enabled {
            for effect in effects {
                if let model::Effect::ScheduleOwnExpiry {
                    generation,
                    upstream_id,
                    at,
                } = effect
                {
                    ctx.object_client::<OrganizationDeliveryClient>(ctx.key())
                        .tick_own_expiry(Json(super::cancellation::ExpiryTick {
                            generation,
                            upstream_id,
                        }))
                        .send_after((at - now).to_std().unwrap_or_default())
                        .await?;
                }
            }
        }
        if step == "org" && matches!(disposition, Disposition::CancelOwned { .. }) {
            if owner.state.cancellation_enabled
                && !ctx
                    .get::<bool>("org_cancel_pending")
                    .await?
                    .unwrap_or(false)
            {
                ctx.set("org_cancel_pending", true);
                ctx.object_client::<OrganizationDeliveryClient>(ctx.key())
                    .drive_cancellation()
                    .send();
            }
        } else if step == "org"
            && matches!(
                disposition,
                Disposition::Final | Disposition::StopTrackingForeign
            )
        {
            ctx.clear("org_cancel_pending");
        }

        if matches!(disposition, Disposition::Held { .. } | Disposition::Aborted) {
            owner.recover_unused(ctx, value, &step).await?;
        }
    }
    save(ctx, value).await?;
    Ok(())
}
pub(super) fn ready(value: &Retained, step: &str) -> bool {
    value.steps[step]
        .control
        .as_ref()
        .is_some_and(|s| matches!(s.disposition(), Disposition::Ready))
}

pub(super) async fn progress(
    ctx: &SharedObjectContext<'_>,
    query: DeliveryCascadeQuery,
) -> Result<DeliveryCascadeProgress, TerminalError> {
    if query.identity.kind != DeliveryKind::Organization
        || identity_key(&query.identity) != ctx.key()
        || query.required_control_revision == 0
    {
        return Err(conflict());
    }
    let inbox = ctx.get::<Json<Inbox>>("control").await?.map(|v| v.0);
    let observed = inbox.as_ref().map_or(0, |v| v.input.envelope.revision);
    if inbox
        .as_ref()
        .is_some_and(|v| v.input.envelope.identity != query.identity)
    {
        return Err(conflict());
    }
    let mut result = DeliveryCascadeProgress {
        observed_control_revision: observed,
        state: CascadeEntryState::Pending,
    };
    if observed < query.required_control_revision {
        return Ok(result);
    }
    let retained = ctx
        .get::<Json<Retained>>("organization")
        .await?
        .map(|v| v.0);
    result.state = match retained {
        None if inbox.is_some_and(|v| matches!(v.state.disposition(), Disposition::Aborted)) => {
            CascadeEntryState::Done
        }
        Some(value) => {
            let waiting = value.steps.values().any(|s| {
                s.control.as_ref().is_some_and(|c| {
                    matches!(
                        c.disposition(),
                        Disposition::RecoverUnknown | Disposition::CancelOwned { .. }
                    )
                })
            });
            if waiting {
                CascadeEntryState::WaitingOnGithub
            } else if value.steps.values().all(|s| {
                s.control.as_ref().is_some_and(|c| {
                    matches!(
                        c.disposition(),
                        Disposition::Final
                            | Disposition::Aborted
                            | Disposition::StopTrackingForeign
                    )
                })
            }) {
                CascadeEntryState::Done
            } else {
                CascadeEntryState::Pending
            }
        }
        _ => CascadeEntryState::Pending,
    };
    Ok(result)
}
