//! Request-owned batch stop inbox. The exact initialization is retained before
//! target calls so pre-initialize cancellation produces truthful aborted owners.
use crate::request_owner::InvitationRequest;
use ghinvite_core::{bulk_cancel::*, request_lifecycle::ApprovedDispatch};
use restate_sdk::{
    context::{ContextClient, ContextReadState, ContextWriteState, ObjectContext},
    errors::TerminalError,
    serde::Json,
};
use serde::{Deserialize, Serialize};
const CURRENT: &str = "batch_cancel/current";
#[derive(Clone, Serialize, Deserialize)]
struct Record {
    input: CancelRequest,
    plan: Option<ApprovedDispatch>,
    receipt: Option<CancelRequestReceipt>,
}
fn conflict() -> TerminalError {
    TerminalError::new_with_code(409, "batch cancellation identity conflict")
}
fn key(input: &CancelRequest) -> String {
    format!(
        "batch_cancel/operation/{}",
        String::from(input.operation_id.clone())
    )
}
pub(crate) async fn execute(
    owner: &InvitationRequest,
    ctx: &ObjectContext<'_>,
    input: CancelRequest,
) -> Result<CancelRequestReceipt, TerminalError> {
    owner.validate_initial(&input.input, true)?;
    if ctx.key() != input.input.request.request_id.to_string()
        || input.admin.account_id != input.input.request.account_id
        || input.admin.user_id == 0
    {
        return Err(conflict());
    }
    let operation = key(&input);
    if let Some(Json(old)) = ctx.get::<Json<Record>>(&operation).await? {
        return if old.input == input {
            old.receipt.ok_or_else(conflict)
        } else {
            Err(conflict())
        };
    }
    let authorized = ctx
        .object_client::<crate::availability::AccountInstallationClient>(format!(
            "repo:{}",
            input.admin.account_id
        ))
        .decision_authority(Json(input.admin.user_id))
        .call()
        .await?
        .0;
    if !authorized {
        return Err(TerminalError::new_with_code(404, "request unavailable"));
    }
    if let Some(Json(old)) = ctx
        .get::<Json<ghinvite_core::request_lifecycle::InitializeRequest>>("input")
        .await?
        && old != input.input
    {
        return Err(conflict());
    }
    if let Some(Json(mut old)) = ctx.get::<Json<Record>>(CURRENT).await? {
        if old.input.input != input.input {
            return Err(conflict());
        }
        if old.input != input && old.receipt.is_none() {
            return Err(conflict());
        }
        if let Some(receipt) = old.receipt.clone() {
            // Archive the completed generation cut before a new operation can
            // capture the current manifest. Old-operation replay never retargets.
            ctx.set(&key(&old.input), Json(old.clone()));
            if old.input == input {
                return Ok(receipt);
            }
            old = Record {
                input: input.clone(),
                plan: None,
                receipt: None,
            };
            ctx.set(CURRENT, Json(old));
        }
    } else {
        ctx.set(
            CURRENT,
            Json(Record {
                input: input.clone(),
                plan: None,
                receipt: None,
            }),
        );
    }
    // This sends only a self-dispatch. The exclusive request still holds the
    // inbox until every target is stopped; dispatch also drains this record if
    // this invocation is killed between initialization and acknowledgements.
    owner
        .initialize_inner(ctx, input.input.clone(), true)
        .await?;
    flush(ctx).await?.ok_or_else(conflict)
}
pub(crate) async fn flush(
    ctx: &ObjectContext<'_>,
) -> Result<Option<CancelRequestReceipt>, TerminalError> {
    let Some(Json(mut record)) = ctx.get::<Json<Record>>(CURRENT).await? else {
        return Ok(None);
    };
    if let Some(receipt) = record.receipt {
        return Ok(Some(receipt));
    }
    let plan = match record.plan.as_ref() {
        Some(plan) => plan.clone(),
        None => {
            let Some(plan) = crate::request_reissue::plan(ctx).await? else {
                return Ok(None);
            };
            record.plan = Some(plan.clone());
            ctx.set(CURRENT, Json(record.clone()));
            plan
        }
    };
    if plan.input.request.request_id != record.input.input.request.request_id
        || plan.input.request.batch != record.input.input.request.batch
        || plan.input.targets != record.input.input.targets
    {
        return Err(conflict());
    }
    let mut receipt = CancelRequestReceipt {
        request_id: record.input.input.request.request_id,
        stopped: true,
        aborted: 0,
        already_written: 0,
        already_completed: 0,
        uncertain: 0,
    };
    for command in plan.commands {
        let result = ctx
            .object_client::<crate::delivery::RepositoryDeliveryClient>(command.delivery_key())
            .cancel_unsent(Json(RepositoryCancelUnsent {
                operation_id: record.input.operation_id.clone(),
                admin: record.input.admin.clone(),
                command,
            }))
            .call()
            .await?
            .0;
        count(&mut receipt, result)?;
    }
    if let Some(command) = plan.organization_command {
        let result = ctx
            .object_client::<crate::org_delivery::OrganizationDeliveryClient>(
                command.delivery_key(),
            )
            .cancel_unsent(Json(OrganizationCancelUnsent {
                operation_id: record.input.operation_id.clone(),
                admin: record.input.admin.clone(),
                command,
            }))
            .call()
            .await?
            .0;
        count(&mut receipt, result)?;
    }
    record.receipt = Some(receipt.clone());
    // The result and captured target set are one durable value. Operation map
    // publication can be recovered without recapturing a newer generation.
    ctx.set(CURRENT, Json(record.clone()));
    ctx.set(&key(&record.input), Json(record));
    Ok(Some(receipt))
}
fn count(
    receipt: &mut CancelRequestReceipt,
    result: CancelUnsentResult,
) -> Result<(), TerminalError> {
    let value = match result {
        CancelUnsentResult::Aborted => &mut receipt.aborted,
        CancelUnsentResult::AlreadyWritten => &mut receipt.already_written,
        CancelUnsentResult::AlreadyCompleted => &mut receipt.already_completed,
        CancelUnsentResult::MayHaveWritten => &mut receipt.uncertain,
    };
    *value = value.checked_add(1).ok_or_else(conflict)?;
    Ok(())
}
