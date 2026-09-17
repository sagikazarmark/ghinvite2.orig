//! Invitation-keyed settlement. New handlers leave deployed legacy journals intact.
use crate::{AppState, github_invitation::*};
use chrono::{DateTime, Utc};
use ghinvite_core::{
    GithubInvitation, InvitationState,
    audit::{ActorKind, AuditEvent, TargetKind},
    storage::settlement::Settlement,
};
use restate_sdk::{
    context::{ContextSideEffects, ObjectContext, RunFuture},
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

/// Read-only observation: unknown creates belong exclusively to GithubCreateV1.
pub async fn observe(
    state: &AppState,
    row: &GithubInvitation,
    at: DateTime<Utc>,
) -> crate::Result<Option<ReconcileEvidence>> {
    if !eligible(row) {
        return Ok(None);
    }
    let Some(context) = crate::invitation_context::load_verified_settlement_context(state, row)
        .await?
    else {
        return Ok(None);
    };
    let pending = state
        .github
        .list_invitations(
            context.account.installation_id,
            context.repository.owner(),
            context.repository.name(),
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
        .verified_user(
            context.account.installation_id,
            context.request.requester_id,
        )
        .await?;
    let accepted = match state
        .github
        .collaborator_permission(
            context.account.installation_id,
            context.repository.owner(),
            context.repository.name(),
            &user.login,
        )
        .await
    {
        Ok((id, permission)) if id == context.request.requester_id => permission != "none",
        Ok(_) => return Ok(None), // rename/reassignment raced identity lookup
        Err(ghinvite_github::Error::Status { status: 404, .. }) => {
            // Recheck identity after negative login-addressed evidence.
            if state
                .github
                .verified_user(
                    context.account.installation_id,
                    context.request.requester_id,
                )
                .await?
                .login
                != user.login
            {
                return Ok(None);
            }
            false
        }
        Err(e) => return Err(e.into()),
    };
    Ok(Some(ReconcileEvidence {
        expected: row.clone(),
        accepted,
        at,
    }))
}

async fn settle(
    state: &AppState,
    expected: GithubInvitation,
    next: InvitationState,
    at: DateTime<Utc>,
    actor: ActorKind,
    by_user: Option<u64>,
    metadata: serde_json::Value,
) -> crate::Result<()> {
    if !eligible(&expected) {
        return Ok(());
    }
    let context =
        crate::invitation_context::load_github_invitation_account_context(state, expected.id)
            .await?;
    let event_type = ghinvite_core::storage::settlement::event_type(next)?;
    // One terminal event per invitation, independent of invocation/journal retention.
    let event = AuditEvent {
        id: ghinvite_core::AuditEventId::from_ulid(expected.id.as_ulid()),
        account_id: context.account.account_id,
        occurred_at: at,
        event_type,
        actor_kind: actor,
        actor_id: by_user,
        target_kind: TargetKind::GithubInvitation,
        target_id: expected.id.to_string(),
        metadata,
        request_id: None,
    };
    state
        .storage
        .settle_github_invitation(&Settlement {
            expected,
            state: next,
            event,
        })
        .await?;
    Ok(())
}

fn validate_key(
    ctx: &ObjectContext<'_>,
    id: ghinvite_core::GithubInvitationId,
) -> Result<(), TerminalError> {
    if ctx.key() != id.to_string() {
        return Err(TerminalError::new_with_code(400, "invitation key mismatch"));
    }
    Ok(())
}

pub async fn reconcile(
    state: &AppState,
    ctx: ObjectContext<'_>,
    Json(input): Json<ReconcileEvidence>,
) -> Result<(), TerminalError> {
    validate_key(&ctx, input.expected.id)?;
    ctx.run(|| async {
        settle(
            state,
            input.expected.clone(),
            if input.accepted {
                InvitationState::Accepted
            } else {
                InvitationState::Cancelled
            },
            input.at,
            ActorKind::System,
            None,
            serde_json::json!({"reconciled": true}),
        )
        .await
        .map_err(crate::error::to_sdk_handler_error)
    })
    .name("settle_reconciled_v1")
    .await
}

pub async fn webhook(
    state: &AppState,
    ctx: ObjectContext<'_>,
    Json(input): Json<OnWebhookInput>,
) -> Result<(), TerminalError> {
    validate_key(&ctx, input.invitation_id)?;
    ctx.run(|| async {
        let row = state
            .storage
            .get_github_invitation(input.invitation_id)
            .await?
            .ok_or(ghinvite_core::storage::Error::ProjectionDependency)?;
        settle(
            state,
            row,
            match input.action {
                WebhookAction::Accepted => InvitationState::Accepted,
                WebhookAction::Declined => InvitationState::Declined,
            },
            input.at,
            ActorKind::Github,
            None,
            serde_json::json!({"action": input.action}),
        )
        .await
        .map_err(crate::error::to_sdk_handler_error)
    })
    .name("settle_webhook_v1")
    .await
}

pub async fn cancel(
    state: &AppState,
    ctx: ObjectContext<'_>,
    Json(input): Json<CancelInvitationInput>,
) -> Result<(), TerminalError> {
    validate_key(&ctx, input.invitation_id)?;
    ctx.run(|| async {
        cancel_or_expire(
            state,
            input.invitation_id,
            input.installation_id,
            input.at,
            input.by_user,
            false,
        )
        .await
        .map_err(crate::error::to_sdk_handler_error)
    })
    .name("settle_cancel_v1")
    .await
}

pub async fn expire(
    state: &AppState,
    ctx: ObjectContext<'_>,
    Json(input): Json<TickExpireInput>,
) -> Result<(), TerminalError> {
    validate_key(&ctx, input.invitation_id)?;
    ctx.run(|| async {
        cancel_or_expire(
            state,
            input.invitation_id,
            input.installation_id,
            input.at,
            None,
            true,
        )
        .await
        .map_err(crate::error::to_sdk_handler_error)
    })
    .name("settle_expire_v1")
    .await
}

async fn cancel_or_expire(
    state: &AppState,
    id: ghinvite_core::GithubInvitationId,
    installation: u64,
    at: DateTime<Utc>,
    by_user: Option<u64>,
    expire: bool,
) -> crate::Result<()> {
    let row = state
        .storage
        .get_github_invitation(id)
        .await?
        .ok_or(ghinvite_core::storage::Error::ProjectionDependency)?;
    if !eligible(&row) {
        return Ok(());
    }
    let context =
        crate::invitation_context::load_github_invitation_context(state, id, installation).await?;
    if expire {
        let pending = state
            .github
            .list_invitations(
                installation,
                context.repository.owner(),
                context.repository.name(),
            )
            .await?;
        if !pending
            .iter()
            .any(|p| Some(p.id) == row.github_invitation_id)
        {
            return Ok(());
        }
    } else {
        match state
            .github
            .delete_invitation(
                installation,
                context.repository.owner(),
                context.repository.name(),
                row.github_invitation_id.unwrap(),
            )
            .await
        {
            Ok(()) | Err(ghinvite_github::Error::Status { status: 404, .. }) => (),
            Err(e) => return Err(e.into()),
        }
    }
    settle(state, row, if expire { InvitationState::Expired } else { InvitationState::Cancelled }, at, if by_user.is_some() { ActorKind::User } else { ActorKind::System }, by_user, serde_json::json!({"reason": if expire { "tick_expire" } else { "cancel" }, "by_user": by_user})).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        dt, fixture_github_client, fixture_storage, invitation_item, invitation_page,
        seed_pending_invitation, token_mint,
    };
    use ghinvite_github::mocks::{Expectation, MockTransport};
    use ghinvite_github::transport::Method;
    use std::sync::Arc;

    async fn seeded_state(mock: MockTransport) -> (AppState, GithubInvitation) {
        let state = AppState::new(
            fixture_storage().await,
            fixture_github_client(Arc::new(mock)),
        );
        let seeded = seed_pending_invitation(&state, "acme/api").await;
        let row = state
            .storage
            .get_github_invitation(seeded.invitation_id)
            .await
            .unwrap()
            .unwrap();
        (state, row)
    }

    #[tokio::test]
    async fn observe_finds_no_settlement_when_still_pending_on_a_later_page() {
        let mock = MockTransport::scripted(vec![
            Expectation::ok_json(Method::Get, "https://api.github.test/app/installations/9",
                serde_json::json!({"id":9,"account":{"id":100,"login":"acme"},"suspended_at":null})),
            token_mint(9),
            Expectation::ok_json(Method::Get, "https://api.github.test/repos/acme/api",
                serde_json::json!({"id":10,"full_name":"acme/api","private":true})),
            invitation_page(
                "https://api.github.test/repos/acme/api/invitations?per_page=100",
                serde_json::json!([invitation_item(7001)]),
                Some("https://api.github.test/repos/acme/api/invitations?per_page=100&page=2"),
            ),
            invitation_page(
                "https://api.github.test/repos/acme/api/invitations?per_page=100&page=2",
                serde_json::json!([invitation_item(9988)]),
                None,
            ),
        ]);
        let (state, row) = seeded_state(mock.clone()).await;

        let evidence = observe(&state, &row, dt("2026-05-05T13:00:00Z"))
            .await
            .unwrap();

        assert!(evidence.is_none(), "got {evidence:?}");
        mock.assert_exhausted();
    }

    #[tokio::test]
    async fn observe_fails_rather_than_settling_when_a_later_page_fails() {
        let mock = MockTransport::scripted(vec![
            Expectation::ok_json(Method::Get, "https://api.github.test/app/installations/9",
                serde_json::json!({"id":9,"account":{"id":100,"login":"acme"},"suspended_at":null})),
            token_mint(9),
            Expectation::ok_json(Method::Get, "https://api.github.test/repos/acme/api",
                serde_json::json!({"id":10,"full_name":"acme/api","private":true})),
            invitation_page(
                "https://api.github.test/repos/acme/api/invitations?per_page=100",
                serde_json::json!([invitation_item(7001)]),
                Some("https://api.github.test/repos/acme/api/invitations?per_page=100&page=2"),
            ),
            Expectation::status(
                Method::Get,
                "https://api.github.test/repos/acme/api/invitations?per_page=100&page=2",
                502,
            ),
        ]);
        let (state, row) = seeded_state(mock.clone()).await;

        // Transient, so the sweep retries later. The truncated list never
        // reaches the collaborator probe that would settle the invitation.
        let err = observe(&state, &row, dt("2026-05-05T13:00:00Z"))
            .await
            .unwrap_err();

        assert!(!err.is_terminal(), "got {err:?}");
        mock.assert_exhausted();
    }
}
