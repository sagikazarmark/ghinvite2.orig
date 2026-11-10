//! Request-owned current-generation epoch. Snapshot, manifest, operation receipt
//! and projection proof become authoritative in a single retained state write.
use crate::request_owner::{InvitationRequest, InvitationRequestClient};
use ghinvite_core::{
    RequestState,
    access_grant::{GrantTarget, RemovalBarrier, RemovalBarrierQuery},
    reissue::*,
    reissue_projection::{ReissuedDelivery, RequestReissueProjection},
    request_lifecycle::{ApprovedDispatch, RequestSnapshot},
};
use restate_sdk::{
    context::{
        ContextClient, ContextReadState, ContextWriteState, ObjectContext, SharedObjectContext,
    },
    errors::TerminalError,
    serde::Json,
};
use serde::{Deserialize, Serialize};
const CURRENT: &str = "reissue/current";
#[derive(Clone, Serialize, Deserialize)]
struct Receipt {
    input: ReissueCommand,
    outcome: ReissueOutcome,
}
#[derive(Clone, Serialize, Deserialize)]
struct Epoch {
    receipt: Receipt,
    plan: ApprovedDispatch,
    proof: RequestReissueProjection,
    pending: bool,
}
fn conflict() -> TerminalError {
    TerminalError::new_with_code(409, "reissue identity conflict")
}
fn receipt_key(input: &ReissueCommand) -> String {
    format!(
        "reissue/operation/{}",
        String::from(input.operation_id.clone())
    )
}
fn overlay_request(
    base: Option<RequestSnapshot>,
    epoch: Option<Epoch>,
) -> Result<Option<RequestSnapshot>, TerminalError> {
    let Some(epoch) = epoch else { return Ok(base) };
    match base {
        Some(base) if base.revision > epoch.proof.after.revision => Ok(Some(base)),
        Some(base) if base.revision == epoch.proof.after.revision && base != epoch.proof.after => {
            Err(conflict())
        }
        _ => Ok(Some(epoch.proof.after)),
    }
}
macro_rules! readers {
    ($request:ident,$plan:ident,$context:ident) => {
        pub(crate) async fn $request(
            ctx: &$context<'_>,
        ) -> Result<Option<RequestSnapshot>, TerminalError> {
            let base = ctx
                .get::<Json<RequestSnapshot>>("request")
                .await?
                .map(|j| j.0);
            let epoch = ctx.get::<Json<Epoch>>(CURRENT).await?.map(|j| j.0);
            overlay_request(base, epoch)
        }
        pub(crate) async fn $plan(
            ctx: &$context<'_>,
        ) -> Result<Option<ApprovedDispatch>, TerminalError> {
            if let Some(Json(epoch)) = ctx.get::<Json<Epoch>>(CURRENT).await? {
                return Ok(Some(epoch.plan));
            }
            Ok(ctx
                .get::<Json<ApprovedDispatch>>("plan")
                .await?
                .map(|j| j.0))
        }
    };
}
readers!(request, plan, ObjectContext);
readers!(shared_request, shared_plan, SharedObjectContext);
pub(crate) async fn proof(
    ctx: &ObjectContext<'_>,
) -> Result<Option<RequestReissueProjection>, TerminalError> {
    Ok(ctx.get::<Json<Epoch>>(CURRENT).await?.map(|j| j.0.proof))
}
pub(crate) async fn flush(ctx: &ObjectContext<'_>) -> Result<(), TerminalError> {
    let Some(Json(mut epoch)) = ctx.get::<Json<Epoch>>(CURRENT).await? else {
        return Ok(());
    };
    if !epoch.pending {
        return Ok(());
    }
    let key = receipt_key(&epoch.receipt.input);
    if let Some(Json(old)) = ctx.get::<Json<Receipt>>(&key).await? {
        if old.input != epoch.receipt.input || old.outcome != epoch.receipt.outcome {
            return Err(conflict());
        }
    } else {
        ctx.set(&key, Json(epoch.receipt.clone()));
    }
    ctx.object_client::<crate::projection::RequestProjectionClient>(ctx.key())
        .apply_reissue(Json(epoch.proof.clone()))
        .send()
        .await?;
    ctx.object_client::<InvitationRequestClient>(ctx.key())
        .dispatch()
        .send()
        .await?;
    ctx.clear("reissue/prepared");
    epoch.pending = false;
    ctx.set(CURRENT, Json(epoch));
    Ok(())
}
async fn stop_removal(
    ctx: &ObjectContext<'_>,
    request_id: ghinvite_core::RequestId,
    target: GrantTarget,
    generation: u64,
) -> Result<RemovalBarrier, TerminalError> {
    ctx.object_client::<crate::access_grant::AccessGrantClient>(format!(
        "{request_id}:{}:{}",
        target.kind(),
        target.id()
    ))
    .stop_for_reissue(Json(RemovalBarrierQuery {
        request_id,
        target,
        delivery_generation: generation,
    }))
    .call()
    .await
    .map(|j| j.0)
}
pub(crate) async fn execute(
    owner: &InvitationRequest,
    ctx: &ObjectContext<'_>,
    input: ReissueCommand,
) -> Result<ReissueOutcome, TerminalError> {
    input
        .validate()
        .map_err(|_| TerminalError::new_with_code(400, "invalid reissue command"))?;
    if ctx.key() != input.request_id.to_string() {
        return Err(conflict());
    }
    flush(ctx).await?;
    let key = receipt_key(&input);
    if let Some(Json(old)) = ctx.get::<Json<Receipt>>(&key).await? {
        return if old.input == input {
            Ok(old.outcome)
        } else {
            Err(conflict())
        };
    }
    if let Some(Json(prepared)) = ctx.get::<Json<ReissueCommand>>("reissue/prepared").await?
        && prepared != input
    {
        return Err(conflict());
    }
    let current = request(ctx).await?.ok_or_else(conflict)?;
    if current.account_id != input.admin.account_id {
        return Err(conflict());
    }
    let authorized = ctx
        .object_client::<crate::availability::AccountInstallationClient>(format!(
            "repo:{}",
            current.account_id
        ))
        .decision_authority(Json(input.admin.user_id))
        .call()
        .await?
        .0;
    if !authorized {
        return Err(TerminalError::new_with_code(404, "request unavailable"));
    }
    let mut plan = plan(ctx).await?.ok_or_else(conflict)?;
    let now = owner.time(ctx, "reissue_time").await?;
    let budget_app = match input.target {
        ReissueTarget::Repository { .. } => "repo",
        ReissueTarget::Organization { .. } => "org",
    };
    let budget = ctx
        .object_client::<crate::invite_budget::InviteBudgetClient>(format!(
            "{budget_app}:{}",
            current.account_id
        ))
        .snapshot()
        .call()
        .await?
        .0;
    let closed = budget
        .buckets
        .iter()
        .filter_map(|b| b.closed_until)
        .chain(budget.content_closed_until)
        .filter(|at| *at > now)
        .max();

    let (settlement, final_result, generation, barrier, removal) = match input.target {
        ReissueTarget::Repository { repo_id } => {
            let command = plan
                .commands
                .iter()
                .find(|c| c.repo_id == repo_id)
                .ok_or_else(conflict)?;
            let client = ctx
                .object_client::<crate::delivery::RepositoryDeliveryClient>(command.delivery_key());
            let snapshot = client.status().call().await?.0.ok_or_else(conflict)?;
            if snapshot.create.command != *command {
                return Err(conflict());
            }
            crate::delivery::validate_notice_snapshot(&command.delivery_key(), &snapshot)?;
            let state = snapshot.settlement.as_ref().map(|s| s.state);
            let final_result =
                ghinvite_core::request_completion::repository(&snapshot).map(|r| r.result);
            let initial = ReissueEvidence {
                request_state: current.state,
                current_generation: command.delivery_generation,
                settlement: state,
                final_result,
                invitation: InvitationBarrier::Clear,
                removal: RemovalBarrier::NoRemoval,
                budget_closed_until: None,
            }
            .decide(now);
            if let Err(refusal) = initial {
                return retain_refusal(ctx, input, refusal);
            }
            if let Some(until) = closed {
                return retain_refusal(ctx, input, ReissueRefusal::BudgetClosed { until });
            }
            ctx.set("reissue/prepared", Json(input.clone()));
            let barrier = client
                .prepare_reissue(Json(RepositoryReissueBarrierQuery {
                    operation_id: input.operation_id.clone(),
                    command: command.clone(),
                }))
                .call()
                .await?
                .0;
            let removal = if barrier == InvitationBarrier::Clear {
                stop_removal(
                    ctx,
                    current.request_id,
                    GrantTarget::Repository {
                        repo_id,
                        full_name: command.repo_full_name.clone(),
                        grant: command.permission.into(),
                    },
                    command.delivery_generation,
                )
                .await?
            } else {
                RemovalBarrier::Pending
            };
            (
                state,
                final_result,
                command.delivery_generation,
                barrier,
                removal,
            )
        }
        ReissueTarget::Organization { org_id } => {
            let command = plan
                .organization_command
                .as_ref()
                .filter(|c| c.organization.org_id == org_id)
                .ok_or_else(conflict)?;
            let client = ctx.object_client::<crate::org_delivery::OrganizationDeliveryClient>(
                command.delivery_key(),
            );
            let snapshot = client.status().call().await?.0.ok_or_else(conflict)?;
            if snapshot.create.command != *command {
                return Err(conflict());
            }
            snapshot.validate().map_err(|_| conflict())?;
            let state = snapshot.settlement.as_ref().map(|s| s.state);
            let final_result = ghinvite_core::request_completion::organization(&snapshot)
                .into_iter()
                .find(|r| {
                    r.target.kind == ghinvite_core::request_completion::TargetKind::Organization
                })
                .map(|r| r.result);
            let initial = ReissueEvidence {
                request_state: current.state,
                current_generation: command.delivery_generation,
                settlement: state,
                final_result,
                invitation: InvitationBarrier::Clear,
                removal: RemovalBarrier::NoRemoval,
                budget_closed_until: None,
            }
            .decide(now);
            if let Err(refusal) = initial {
                return retain_refusal(ctx, input, refusal);
            }
            if let Some(until) = closed {
                return retain_refusal(ctx, input, ReissueRefusal::BudgetClosed { until });
            }
            ctx.set("reissue/prepared", Json(input.clone()));
            let barrier = client
                .prepare_reissue(Json(OrganizationReissueBarrierQuery {
                    operation_id: input.operation_id.clone(),
                    command: command.clone(),
                }))
                .call()
                .await?
                .0;
            let mut removal = RemovalBarrier::Pending;
            if barrier == InvitationBarrier::Clear {
                removal = stop_removal(
                    ctx,
                    current.request_id,
                    GrantTarget::Organization {
                        org_id,
                        login: command.organization.org_login.clone(),
                    },
                    command.delivery_generation,
                )
                .await?;
                if matches!(
                    removal,
                    RemovalBarrier::NoRemoval | RemovalBarrier::DefinitivelyFinished
                ) {
                    for team in &command.teams {
                        removal = stop_removal(
                            ctx,
                            current.request_id,
                            GrantTarget::Team {
                                org_id,
                                org_login: command.organization.org_login.clone(),
                                team_id: team.team_id,
                                slug: team.team_slug.clone(),
                            },
                            command.delivery_generation,
                        )
                        .await?;
                        if matches!(
                            removal,
                            RemovalBarrier::Pending | RemovalBarrier::MayHaveWritten
                        ) {
                            break;
                        }
                    }
                }
            }
            (
                state,
                final_result,
                command.delivery_generation,
                barrier,
                removal,
            )
        }
    };
    let outcome = ReissueEvidence {
        request_state: current.state,
        current_generation: generation,
        settlement,
        final_result,
        invitation: barrier,
        removal,
        budget_closed_until: closed,
    }
    .decide(now);
    let issued = match outcome {
        Ok(value) => value,
        Err(refusal) => return retain_refusal(ctx, input, refusal),
    };
    let delivery = match input.target {
        ReissueTarget::Repository { repo_id } => {
            let after = plan
                .commands
                .iter_mut()
                .find(|c| c.repo_id == repo_id)
                .ok_or_else(conflict)?;
            let before = after.clone();
            after.invitation_id = None;
            after.delivery_generation = issued.delivery_generation.into();
            after.budget_attribution = BudgetAttribution::AdministratorReissue;
            ReissuedDelivery::Repository {
                before,
                after: after.clone(),
            }
        }
        ReissueTarget::Organization { .. } => {
            let after = plan.organization_command.as_mut().ok_or_else(conflict)?;
            let before = after.clone();
            after.invitation_id = None;
            after.delivery_generation = issued.delivery_generation.into();
            after.budget_attribution = BudgetAttribution::AdministratorReissue;
            ReissuedDelivery::Organization {
                before,
                after: after.clone(),
            }
        }
    };
    let mut after = current.clone();
    after.state = RequestState::Approved;
    after.revision = after
        .revision
        .checked_add(1)
        .filter(|r| *r <= ghinvite_core::storage::integer::MAX_EXACT)
        .ok_or_else(conflict)?;
    let original = match proof(ctx).await? {
        Some(old) => old.original_approval,
        None => plan.input.request.clone(),
    };
    let proof = RequestReissueProjection {
        command: input.clone(),
        before: current,
        after: after.clone(),
        original_approval: original.clone(),
        approval_events: crate::request_owner::events(&original),
        delivery,
        at: now,
    };
    proof.validate().map_err(|_| conflict())?;
    plan.input.request = after;
    let epoch = Epoch {
        receipt: Receipt {
            input,
            outcome: Ok(issued),
        },
        plan,
        proof,
        pending: true,
    };
    ctx.set(CURRENT, Json(epoch));
    owner.checkpoint(ctx, "after-reissue-epoch").await?;
    flush(ctx).await?;
    Ok(Ok(issued))
}
fn retain_refusal(
    ctx: &ObjectContext<'_>,
    input: ReissueCommand,
    refusal: ReissueRefusal,
) -> Result<ReissueOutcome, TerminalError> {
    ctx.clear("reissue/prepared");
    let outcome = Err(refusal);
    ctx.set(&receipt_key(&input), Json(Receipt { input, outcome }));
    Ok(outcome)
}
