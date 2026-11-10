//! Authenticated organization-cap policy, owned by the canonical repo account.
use crate::{
    AppState,
    availability::{
        AccountInstallationClient, InstallationChange, InstallationProjectionClient, OrgAuthority,
    },
    invite_budget::InviteBudgetClient,
};
use ghinvite_core::{
    account::{AccountInstallationRef, App},
    audit::{ActorKind, AuditEvent, EventType, TargetKind},
    budget::{OrgCapOutcome, OrgCapPolicy, OrgCapRefusal, SetOrgCap},
};
use restate_sdk::{
    context::{
        ContextClient, ContextReadState, ContextSideEffects, ContextWriteState, ObjectContext,
        RunFuture,
    },
    errors::{HandlerError, TerminalError},
    serde::Json,
};
use serde::{Deserialize, Serialize};

const PENDING: &str = "org_cap_pending";
pub(crate) const POLICY: &str = "org_cap_policy";
#[derive(Clone, Serialize, Deserialize)]
struct Pending {
    policy: OrgCapPolicy,
    event: AuditEvent,
}
fn invalid() -> TerminalError {
    TerminalError::new_with_code(400, "invalid organization budget command")
}
fn conflict() -> TerminalError {
    TerminalError::new_with_code(409, "budget operation conflict")
}
fn receipt_key(input: &SetOrgCap) -> String {
    format!("org_cap/{}", String::from(input.operation_id.clone()))
}

async fn complete(ctx: &ObjectContext<'_>, pending: Pending) -> Result<(), TerminalError> {
    ctx.object_client::<InviteBudgetClient>(format!("org:{}", pending.policy.account_id))
        .set_org_cap(Json(pending.policy.clone()))
        .call()
        .await?;
    ctx.object_client::<InstallationProjectionClient>(ctx.key())
        .apply(Json(InstallationChange {
            installation: None,
            settings: None,
            event: Some(pending.event),
            audit_facts: vec![],
        }))
        .send()
        .await?;
    ctx.set(POLICY, Json(pending.policy.clone()));
    ctx.set(
        &receipt_key(&pending.policy.command),
        Json((
            pending.policy.command.clone(),
            Ok::<_, OrgCapRefusal>(pending.policy),
        )),
    );
    ctx.clear(PENDING);
    Ok(())
}

pub(crate) async fn set(
    ctx: &ObjectContext<'_>,
    state: &AppState,
    input: SetOrgCap,
) -> Result<OrgCapOutcome, TerminalError> {
    let owner: AccountInstallationRef = ctx.key().parse().map_err(|_| invalid())?;
    if owner.app != App::Repo
        || input.actor_user_id == 0
        || input.actor_user_id > ghinvite_core::storage::integer::MAX_EXACT
    {
        return Err(invalid());
    }
    let key = receipt_key(&input);
    // Completed operations are independent of any later interrupted change.
    if let Some(Json((old, result))) = ctx.get::<Json<(SetOrgCap, OrgCapOutcome)>>(&key).await? {
        return if old == input {
            Ok(result)
        } else {
            Err(conflict())
        };
    }
    // Finish an already authorized operation before another policy can overtake
    // it. A killed caller cannot leave the ledger and its receipt divergent.
    if let Some(Json(pending)) = ctx.get::<Json<Pending>>(PENDING).await? {
        complete(ctx, pending).await?;
    }
    // Completion may have retained this operation, including its original input.
    if let Some(Json((old, result))) = ctx.get::<Json<(SetOrgCap, OrgCapOutcome)>>(&key).await? {
        return if old == input {
            Ok(result)
        } else {
            Err(conflict())
        };
    }
    let refusal = if state.invite_budget.is_none() {
        Some(OrgCapRefusal::BudgetUnavailable)
    } else {
        let Json(authority) = ctx
            .object_client::<AccountInstallationClient>(format!("org:{}", owner.account_id))
            .org_authority(Json(input.actor_user_id))
            .call()
            .await?;
        match authority {
            OrgAuthority::Owner => None,
            OrgAuthority::Absent => Some(OrgCapRefusal::OrgAppRequired),
            OrgAuthority::NonOwner | OrgAuthority::Denied => Some(OrgCapRefusal::NotOwner),
            OrgAuthority::Unavailable => {
                return Err(TerminalError::new_with_code(
                    503,
                    "organization authority unavailable",
                ));
            }
        }
    };
    if let Some(refusal) = refusal {
        let result = Err(refusal);
        ctx.set(&key, Json((input, result.clone())));
        return Ok(result);
    }
    let revision = ctx
        .get::<Json<OrgCapPolicy>>(POLICY)
        .await?
        .map_or(0, |p| p.0.revision)
        .checked_add(1)
        .filter(|n| *n <= ghinvite_core::storage::integer::MAX_EXACT)
        .ok_or_else(invalid)?;
    let changed_at = crate::clock::now(ctx, "org_cap_policy_time").await?;
    let policy = OrgCapPolicy {
        account_id: owner.account_id,
        revision,
        command: input,
        changed_at,
    };
    let Json(event) = ctx.run(|| async { Ok::<_, HandlerError>(Json(AuditEvent {
        id: ghinvite_core::AuditEventId::new(), account_id: owner.account_id,
        occurred_at: changed_at, event_type: EventType::OrganizationInviteCapChanged,
        actor_kind: ActorKind::User, actor_id: Some(policy.command.actor_user_id),
        target_kind: TargetKind::Installation, target_id: format!("org:{}", owner.account_id),
        metadata: serde_json::json!({"confirmed_established":policy.command.confirmed_established,"revision":revision,"operation_id":policy.command.operation_id}),
        request_id: Some(String::from(policy.command.operation_id.clone())),
    })) }).name("retain_org_cap_audit").await?;
    let pending = Pending {
        policy: policy.clone(),
        event,
    };
    ctx.set(PENDING, Json(pending.clone()));
    complete(ctx, pending).await?;
    Ok(Ok(policy))
}
