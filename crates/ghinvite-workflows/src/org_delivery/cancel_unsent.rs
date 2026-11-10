//! Exact batch stop barrier. No cancellation DELETE or settlement rewrite.
use super::*;
use ghinvite_core::{
    bulk_cancel::{CancelUnsentResult, OrganizationCancelUnsent},
    delivery::AbortReason,
};
const STOP: &str = "batch_cancel/input";
#[derive(Clone, Serialize, Deserialize)]
struct Receipt {
    input: OrganizationCancelUnsent,
    result: CancelUnsentResult,
}
pub(super) async fn stopped(
    ctx: &ObjectContext<'_>,
    command: &OrganizationCreateCommand,
) -> Result<bool, TerminalError> {
    match ctx.get::<Json<OrganizationCancelUnsent>>(STOP).await? {
        Some(Json(input)) if input.command == *command => Ok(true),
        Some(_) => Err(conflict()),
        None => Ok(false),
    }
}
pub(super) async fn execute(
    owner: &OrganizationDelivery,
    ctx: &ObjectContext<'_>,
    input: OrganizationCancelUnsent,
) -> Result<CancelUnsentResult, TerminalError> {
    input.command.validate().map_err(|_| conflict())?;
    if input.command.delivery_key() != ctx.key()
        || input.command.batch.is_none()
        || input.command.link_id.is_some()
        || input.admin.account_id != input.command.account_id
        || input.admin.user_id == 0
        || input.admin.user_id > ghinvite_core::storage::integer::MAX_EXACT
    {
        return Err(conflict());
    }
    let key = format!(
        "batch_cancel/operation/{}",
        String::from(input.operation_id.clone())
    );
    if let Some(Json(receipt)) = ctx.get::<Json<Receipt>>(&key).await? {
        return if receipt.input == input {
            Ok(receipt.result)
        } else {
            Err(conflict())
        };
    }
    let retained = ctx.get::<Json<Retained>>("organization").await?;
    if retained
        .as_ref()
        .is_some_and(|old| old.0.snapshot.create.command != input.command)
    {
        return Err(conflict());
    }
    if let Some(Json(old)) = ctx.get::<Json<OrganizationCancelUnsent>>(STOP).await? {
        if old.command != input.command {
            return Err(conflict());
        }
    } else {
        // Retain the exact stop before any yielding operation. A delayed create
        // cannot pass its driver while capture/accounting is interrupted.
        ctx.set(STOP, Json(input.clone()));
    }
    let mut value = if let Some(Json(value)) = retained {
        value
    } else {
        let mut snapshot =
            OrganizationDeliverySnapshot::planned(input.command.clone()).map_err(|_| conflict())?;
        snapshot.create.audit_version =
            ghinvite_core::delivery_audit::new_owner_version(snapshot.create.command.invitation_id);
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
        save(ctx, &mut value).await?;
        value
    };
    owner.flush_all(ctx, &mut value).await?;
    let now = crate::clock::now(ctx, "organization_batch_cancel_time").await?;
    let mut uncertain = false;
    let mut written = false;
    let mut completed = false;
    for step in value.steps.keys().cloned().collect::<Vec<_>>() {
        if confirmed(&value, &step) {
            let (owned, aborted) = if step == "org" {
                (
                    matches!(
                        value.snapshot.create.outcome,
                        OrganizationCreateOutcome::Created { .. }
                    ),
                    matches!(
                        value.snapshot.create.outcome,
                        OrganizationCreateOutcome::Aborted {
                            reason: AbortReason::BatchCancelled
                        }
                    ),
                )
            } else {
                let id = step[5..].parse::<u64>().map_err(|_| conflict())?;
                (
                    matches!(
                        value.snapshot.teams[&id].create.outcome,
                        TeamCreateOutcome::Created { .. } | TeamCreateOutcome::Upgraded
                    ),
                    matches!(
                        value.snapshot.teams[&id].create.outcome,
                        TeamCreateOutcome::Aborted {
                            reason: AbortReason::BatchCancelled
                        }
                    ),
                )
            };
            written |= owned;
            completed |= !owned && !aborted;
            continue;
        }
        // An org POST includes every attached team. Its unknown effect also
        // protects their receipts from being mislabeled no-write Aborted.
        let inherited = step != "org"
            && matches!(
                value.snapshot.create.outcome,
                OrganizationCreateOutcome::OutcomeUnknown
            );
        let attempt = attempt(&value, &step);
        let fenced = ctx
            .run(|| async {
                owner
                    .storage
                    .organization_attempt_exists(&attempt)
                    .await
                    .map_err(HandlerError::from)
            })
            .name("organization_batch_cancel_fence")
            .await?;
        if fenced || inherited {
            uncertain = true;
            let outcome = if step == "org" {
                protocol::StepResult::Organization(OrganizationCreateOutcome::OutcomeUnknown)
            } else {
                protocol::StepResult::Team(TeamCreateOutcome::OutcomeUnknown)
            };
            set_step(&mut value, &step, outcome, now)?;
            save(ctx, &mut value).await?;
            continue;
        }
        // An acknowledged absent fence proves this reservation did not become
        // an external write. Resolve a possibly lost reserve ACK before refund.
        value
            .steps
            .get_mut(&step)
            .ok_or_else(conflict)?
            .may_have_written = false;
        save(ctx, &mut value).await?;
        owner.recover_unused(ctx, &mut value, &step).await?;
        let outcome = if step == "org" {
            protocol::StepResult::Organization(OrganizationCreateOutcome::Aborted {
                reason: AbortReason::BatchCancelled,
            })
        } else {
            protocol::StepResult::Team(TeamCreateOutcome::Aborted {
                reason: AbortReason::BatchCancelled,
            })
        };
        set_step(&mut value, &step, outcome, now)?;
        save(ctx, &mut value).await?;
    }
    ctx.clear("scheduled_wake");
    ctx.clear("wake_due");
    project(ctx, &mut value).await?;
    let result = if uncertain {
        CancelUnsentResult::MayHaveWritten
    } else if written {
        CancelUnsentResult::AlreadyWritten
    } else if completed {
        CancelUnsentResult::AlreadyCompleted
    } else {
        CancelUnsentResult::Aborted
    };
    ctx.set(&key, Json(Receipt { input, result }));
    Ok(result)
}
