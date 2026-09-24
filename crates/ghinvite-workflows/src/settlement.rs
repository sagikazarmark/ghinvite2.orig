//! Known-invitation evidence and retained settlement in RepositoryDelivery.
use crate::{AppState, github_invitation::*};
use chrono::{DateTime, Utc};
use ghinvite_core::{
    Account, GithubInvitation, InvitationState,
    audit::{ActorKind, AuditEvent, TargetKind},
    delivery::{CreateCommand, DeliverySnapshot, Settlement},
};
use restate_sdk::{
    context::{
        ContextClient, ContextReadState, ContextSideEffects, ContextWriteState, ObjectContext,
        RunFuture,
    },
    errors::TerminalError,
    serde::Json,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReconcileEvidence {
    pub expected: GithubInvitation,
    pub accepted: bool,
    pub at: DateTime<Utc>,
}

pub fn eligible(row: &GithubInvitation) -> bool {
    row.state == InvitationState::Sent && row.github_invitation_id.is_some()
}

async fn verified_repository(
    state: &AppState,
    command: &CreateCommand,
    account: &Account,
) -> crate::Result<Option<ghinvite_core::RepositoryIdentity>> {
    if account.account_id != command.account_id || account.uninstalled_at.is_some() {
        return Ok(None);
    }
    let Some(installation) = state
        .github
        .get_installation(account.installation_id)
        .await?
    else {
        return Ok(None);
    };
    if installation.id != account.installation_id
        || installation.account.id != command.account_id
        || installation.suspended_at.is_some()
    {
        return Ok(None);
    }
    let repository = ghinvite_core::RepositoryIdentity::parse(command.repo_full_name.clone())
        .map_err(|e| crate::HandlerError::Invariant(e.to_string()))?;
    match crate::repository_access::verify_repository_access(
        &state.github,
        account,
        command.repo_id,
        &repository,
    )
    .await
    {
        crate::repository_access::RepositoryAccess::Verified => Ok(Some(repository)),
        crate::repository_access::RepositoryAccess::Unavailable(_) => Ok(None),
        crate::repository_access::RepositoryAccess::Unread(error) => Err(error.into()),
    }
}

/// Exact upstream identity, complete listing and numeric membership evidence.
/// Unlike unknown-create recovery, absence can settle this known invitation.
pub async fn observe(
    state: &AppState,
    snapshot: &DeliverySnapshot,
    account: &Account,
    at: DateTime<Utc>,
) -> crate::Result<Option<ReconcileEvidence>> {
    let row = snapshot.invitation();
    if !eligible(&row) {
        return Ok(None);
    }
    let command = &snapshot.create.command;
    let Some(repository) = verified_repository(state, command, account).await? else {
        return Ok(None);
    };
    let pending = state
        .github
        .list_invitations(
            account.installation_id,
            repository.owner(),
            repository.name(),
        )
        .await?;
    if pending
        .iter()
        .any(|p| Some(p.id) == row.github_invitation_id)
    {
        return Ok(None);
    }
    let user = state
        .github
        .verified_user(account.installation_id, command.requester_id)
        .await?;
    let accepted = match state
        .github
        .collaborator_permission(
            account.installation_id,
            repository.owner(),
            repository.name(),
            &user.login,
        )
        .await
    {
        Ok((id, role)) if id == command.requester_id => role.has_access(),
        Ok(_) => return Ok(None),
        Err(ghinvite_github::Error::Status { status: 404, .. }) => {
            if state
                .github
                .verified_user(account.installation_id, command.requester_id)
                .await?
                .login
                != user.login
            {
                return Ok(None);
            }
            false
        }
        Err(error) => return Err(error.into()),
    };
    Ok(Some(ReconcileEvidence {
        expected: row,
        accepted,
        at,
    }))
}

async fn load(
    ctx: &ObjectContext<'_>,
    id: ghinvite_core::GithubInvitationId,
) -> Result<DeliverySnapshot, TerminalError> {
    let Json(snapshot) = ctx
        .get::<Json<DeliverySnapshot>>("snapshot")
        .await?
        .ok_or_else(|| TerminalError::new_with_code(503, "delivery not yet available"))?;
    if snapshot.create.command.delivery_key() != ctx.key()
        || snapshot.create.command.invitation_id != id
    {
        return Err(TerminalError::new_with_code(
            409,
            "delivery identity conflict",
        ));
    }
    Ok(snapshot)
}

async fn record(
    ctx: &ObjectContext<'_>,
    mut snapshot: DeliverySnapshot,
    state: InvitationState,
    at: DateTime<Utc>,
    actor: ActorKind,
    by_user: Option<u64>,
    metadata: serde_json::Value,
) -> Result<(), TerminalError> {
    if !eligible(&snapshot.invitation()) {
        return Ok(());
    }
    let command = &snapshot.create.command;
    snapshot.settlement = Some(Settlement {
        state,
        event: AuditEvent {
            id: ghinvite_core::AuditEventId::from_ulid(command.invitation_id.as_ulid()),
            account_id: command.account_id,
            occurred_at: at,
            event_type: ghinvite_core::storage::settlement::event_type(state)
                .map_err(|e| TerminalError::new(e.to_string()))?,
            actor_kind: actor,
            actor_id: by_user,
            target_kind: TargetKind::GithubInvitation,
            target_id: command.invitation_id.to_string(),
            metadata,
            request_id: None,
        },
    });
    snapshot.revision += 1;
    ctx.set("snapshot", Json(snapshot.clone()));
    crate::delivery::send_projection(ctx, &snapshot).await
}

pub async fn reconcile(
    ctx: ObjectContext<'_>,
    Json(input): Json<ReconcileEvidence>,
) -> Result<(), TerminalError> {
    let snapshot = load(&ctx, input.expected.id).await?;
    if snapshot.invitation() != input.expected {
        return Ok(());
    }
    record(
        &ctx,
        snapshot,
        if input.accepted {
            InvitationState::Accepted
        } else {
            InvitationState::Cancelled
        },
        input.at,
        ActorKind::System,
        None,
        serde_json::json!({"reconciled":true}),
    )
    .await
}

pub async fn webhook(
    ctx: ObjectContext<'_>,
    Json(input): Json<OnWebhookInput>,
) -> Result<(), TerminalError> {
    let snapshot = load(&ctx, input.invitation_id).await?;
    if input.verify {
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
        let command = &snapshot.create.command;
        let Json(account) = ctx
            .object_client::<crate::availability::AccountInstallationClient>(
                command.account_id.to_string(),
            )
            .current_installation()
            .call()
            .await?;
        let Some(account) = account else {
            return Ok(());
        };
        let Json((evidence, retry)) = ctx
            .run(|| async {
                let result = match observe(&self.state, &snapshot, &account, Utc::now()).await {
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
        Withdrawal::Cancel { by_user } => (false, by_user, InvitationState::Cancelled, "cancel"),
        Withdrawal::Expire => (true, None, InvitationState::Expired, "tick_expire"),
    };
    let snapshot = load(&ctx, id).await?;
    let row = snapshot.invitation();
    if !eligible(&row) {
        return Ok(());
    }
    let command = &snapshot.create.command;
    let Json(account) = ctx
        .object_client::<crate::availability::AccountInstallationClient>(
            command.account_id.to_string(),
        )
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
                let Some(repo) = verified_repository(state, command, &account).await? else {
                    return Ok(false);
                };
                if expire {
                    return Ok(state
                        .github
                        .list_invitations(installation_id, repo.owner(), repo.name())
                        .await?
                        .iter()
                        .any(|p| Some(p.id) == row.github_invitation_id));
                }
                match state
                    .github
                    .delete_invitation(
                        installation_id,
                        repo.owner(),
                        repo.name(),
                        row.github_invitation_id.unwrap(),
                    )
                    .await?
                {
                    ghinvite_github::InvitationDeletion::Deleted => Ok(true),
                    ghinvite_github::InvitationDeletion::NotFound => {
                        Ok(verified_repository(state, command, &account)
                            .await?
                            .is_some())
                    }
                }
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
