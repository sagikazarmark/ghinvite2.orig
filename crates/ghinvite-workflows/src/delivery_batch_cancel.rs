//! Permanent exact-command stop barrier for reviewed batch rows. It cannot
//! delete invitations or revoke access, and never refunds an uncertain write.
use crate::AppState;
use ghinvite_core::{
    bulk_cancel::{CancelUnsentResult, RepositoryCancelUnsent},
    delivery::{AbortReason, CreateCommand, CreateOutcome, CreateReceipt, DeliverySnapshot},
};
use restate_sdk::{
    context::{ContextReadState, ContextSideEffects, ContextWriteState, ObjectContext, RunFuture},
    errors::{HandlerError, TerminalError},
    serde::Json,
};
const STOP: &str = "batch_cancel/input";
#[derive(serde::Serialize, serde::Deserialize)]
struct Receipt {
    input: RepositoryCancelUnsent,
    result: CancelUnsentResult,
}
fn conflict() -> TerminalError {
    TerminalError::new_with_code(409, "batch cancellation identity conflict")
}
fn classify(snapshot: &DeliverySnapshot) -> Option<CancelUnsentResult> {
    match snapshot.create.outcome {
        CreateOutcome::Aborted {
            reason: AbortReason::BatchCancelled,
        } => Some(CancelUnsentResult::Aborted),
        CreateOutcome::Created { .. } | CreateOutcome::Upgraded { .. } => {
            Some(CancelUnsentResult::AlreadyWritten)
        }
        ref outcome if outcome.confirmed() => Some(CancelUnsentResult::AlreadyCompleted),
        _ => None,
    }
}
pub(crate) async fn cancel(
    ctx: &ObjectContext<'_>,
    state: &AppState,
    input: RepositoryCancelUnsent,
) -> Result<CancelUnsentResult, TerminalError> {
    if ctx.key() != input.command.delivery_key()
        || input.command.link_id.is_some()
        || input.command.batch.is_none_or(|b| b.validate().is_err())
        || input.admin.account_id != input.command.account_id
        || input.admin.user_id == 0
        || input.admin.user_id > ghinvite_core::storage::integer::MAX_EXACT
    {
        return Err(TerminalError::new_with_code(
            400,
            "invalid batch cancellation",
        ));
    }
    crate::delivery::validate_notice_snapshot(
        ctx.key(),
        &DeliverySnapshot::from(CreateReceipt {
            audit_version: ghinvite_core::delivery_audit::new_owner_version(
                input.command.invitation_id,
            ),
            command: input.command.clone(),
            outcome: CreateOutcome::OutcomeUnknown,
            confirmed_at: None,
            revision: 1,
        }),
    )?;
    let key = format!(
        "batch_cancel/operation/{}",
        String::from(input.operation_id.clone())
    );
    if let Some(Json(old)) = ctx.get::<Json<Receipt>>(&key).await? {
        return if old.input == input {
            Ok(old.result)
        } else {
            Err(conflict())
        };
    }
    if let Some(Json(old)) = ctx.get::<Json<CreateCommand>>("input").await?
        && old != input.command
    {
        return Err(conflict());
    }
    if let Some(Json(old)) = ctx.get::<Json<RepositoryCancelUnsent>>(STOP).await? {
        if old.command != input.command {
            return Err(conflict());
        }
    } else {
        ctx.set(STOP, Json(input.clone()));
    }
    // Input and stop are retained before any cross-owner accounting. Delayed
    // create/recheck must enter gate() and cannot resume a write after a kill.
    ctx.set("input", Json(input.command.clone()));
    let result = finish(ctx, state, &input.command).await?;
    ctx.set(&key, Json(Receipt { input, result }));
    Ok(result)
}
pub(crate) async fn gate(
    ctx: &ObjectContext<'_>,
    state: &AppState,
    command: &CreateCommand,
) -> Result<Option<CreateReceipt>, TerminalError> {
    let Some(Json(input)) = ctx.get::<Json<RepositoryCancelUnsent>>(STOP).await? else {
        return Ok(None);
    };
    if input.command != *command {
        return Err(conflict());
    }
    finish(ctx, state, command).await?;
    Ok(Some(
        crate::repository_audit::receipt(ctx)
            .await?
            .ok_or_else(conflict)?,
    ))
}
async fn finish(
    ctx: &ObjectContext<'_>,
    state: &AppState,
    command: &CreateCommand,
) -> Result<CancelUnsentResult, TerminalError> {
    let old = crate::repository_audit::snapshot(ctx).await?;
    if let Some(old) = &old {
        if old.create.command != *command {
            return Err(conflict());
        }
        if let Some(result) = classify(old) {
            crate::delivery::send_projection(ctx, old).await?;
            return Ok(result);
        }
    }
    let absent = ctx
        .run(|| async {
            Ok::<_, HandlerError>(matches!(
                state.storage.delivery_attempt_exists(command).await,
                Ok(false)
            ))
        })
        .name("batch_cancel_write_fence")
        .await?;
    let outcome = if absent {
        ctx.set(crate::delivery_control::MAY_HAVE_WRITTEN, false);
        if state.invite_budget.is_some() {
            crate::delivery_budget::recover_unused(ctx, state.storage.as_ref(), command).await?;
        }
        CreateOutcome::Aborted {
            reason: AbortReason::BatchCancelled,
        }
    } else {
        ctx.set(crate::delivery_control::MAY_HAVE_WRITTEN, true);
        CreateOutcome::OutcomeUnknown
    };
    if old
        .as_ref()
        .is_some_and(|old| old.create.outcome == outcome)
    {
        return Ok(CancelUnsentResult::MayHaveWritten);
    }
    let now = crate::clock::now(ctx, "batch_cancel_time").await?;
    let snapshot = cancellation_snapshot(command, old.as_ref(), outcome, now)?;
    crate::repository_audit::save(ctx, &snapshot).await?;
    crate::delivery::send_projection(ctx, &snapshot).await?;
    Ok(if absent {
        CancelUnsentResult::Aborted
    } else {
        CancelUnsentResult::MayHaveWritten
    })
}

// An existing generation keeps its original audit contract. Only a previously
// uncreated owner chooses a version from its exact supplied command identity.
fn cancellation_snapshot(
    command: &CreateCommand,
    old: Option<&DeliverySnapshot>,
    outcome: CreateOutcome,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<DeliverySnapshot, TerminalError> {
    let revision = old
        .map_or(Some(1), |old| old.revision.checked_add(1))
        .filter(|v| *v <= ghinvite_core::storage::integer::MAX_EXACT)
        .ok_or_else(conflict)?;
    let snapshot = DeliverySnapshot::from(CreateReceipt {
        audit_version: old.map_or(
            ghinvite_core::delivery_audit::new_owner_version(command.invitation_id),
            |old| old.create.audit_version,
        ),
        command: command.clone(),
        confirmed_at: outcome.confirmed().then_some(now),
        outcome,
        revision,
    });
    Ok(snapshot)
}

#[cfg(test)]
#[path = "delivery_batch_cancel_tests.rs"]
mod tests;
