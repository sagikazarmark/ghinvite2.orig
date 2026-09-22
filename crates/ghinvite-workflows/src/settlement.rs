//! Invitation-keyed settlement.
use crate::repository_access::{RepositoryAccess, verify_repository_access};
use crate::{AppState, github_invitation::*};
use chrono::{DateTime, Utc};
use ghinvite_core::{
    GithubInvitation, InvitationState,
    audit::{ActorKind, AuditEvent, TargetKind},
    storage::settlement::Settlement,
};
use ghinvite_github::InvitationDeletion;
use restate_sdk::{
    context::{ContextSideEffects, ObjectContext, RunFuture},
    errors::TerminalError,
    serde::Json,
};
use serde::{Deserialize, Serialize};

mod context;

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReconcileEvidence {
    pub expected: GithubInvitation,
    pub accepted: bool,
    pub at: DateTime<Utc>,
}

pub fn eligible(row: &GithubInvitation) -> bool {
    row.state == InvitationState::Sent && row.github_invitation_id.is_some()
}

/// Read-only observation: unknown creates belong exclusively to GithubCreate.
pub async fn observe(
    state: &AppState,
    row: &GithubInvitation,
    at: DateTime<Utc>,
) -> crate::Result<Option<ReconcileEvidence>> {
    if !eligible(row) {
        return Ok(None);
    }
    let Some(context) = context::load_verified(state, row).await? else {
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
        Ok((id, role)) if id == context.request.requester_id => role.has_access(),
        Ok(_) => return Ok(None), // rename/reassignment raced identity lookup
        // GitHub's 404 here does not say which resource is missing. The
        // repository was verified reachable above, so it is read as the login.
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

/// What a settlement records: the terminal state, when it was reached, and who
/// reached it.
struct Outcome {
    state: InvitationState,
    at: DateTime<Utc>,
    actor: ActorKind,
    by_user: Option<u64>,
    metadata: serde_json::Value,
}

/// Settle `expected` on evidence from outside, auditing it under the account
/// its link belongs to.
async fn settle(
    state: &AppState,
    expected: GithubInvitation,
    outcome: Outcome,
) -> crate::Result<()> {
    if !eligible(&expected) {
        return Ok(());
    }
    let account_id = context::account_id(state, &expected).await?;
    record(state, account_id, expected, outcome).await
}

/// Record `outcome` for `expected`, audited under `account_id`, which the
/// caller has already read.
async fn record(
    state: &AppState,
    account_id: u64,
    expected: GithubInvitation,
    outcome: Outcome,
) -> crate::Result<()> {
    let event_type = ghinvite_core::storage::settlement::event_type(outcome.state)?;
    // One terminal event per invitation, independent of invocation/journal retention.
    let event = AuditEvent {
        id: ghinvite_core::AuditEventId::from_ulid(expected.id.as_ulid()),
        account_id,
        occurred_at: outcome.at,
        event_type,
        actor_kind: outcome.actor,
        actor_id: outcome.by_user,
        target_kind: TargetKind::GithubInvitation,
        target_id: expected.id.to_string(),
        metadata: outcome.metadata,
        request_id: None,
    };
    state
        .storage
        .settle_github_invitation(&Settlement {
            expected,
            state: outcome.state,
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
            Outcome {
                state: if input.accepted {
                    InvitationState::Accepted
                } else {
                    InvitationState::Cancelled
                },
                at: input.at,
                actor: ActorKind::System,
                by_user: None,
                metadata: serde_json::json!({"reconciled": true}),
            },
        )
        .await
        .map_err(crate::error::to_sdk_handler_error)
    })
    .name("settle_reconciled")
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
            Outcome {
                state: match input.action {
                    WebhookAction::Accepted => InvitationState::Accepted,
                    WebhookAction::Declined => InvitationState::Declined,
                },
                at: input.at,
                actor: ActorKind::Github,
                by_user: None,
                metadata: serde_json::json!({"action": input.action}),
            },
        )
        .await
        .map_err(crate::error::to_sdk_handler_error)
    })
    .name("settle_webhook")
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
            Withdrawal::Cancel {
                by_user: input.by_user,
            },
        )
        .await
        .map_err(crate::error::to_sdk_handler_error)
    })
    .name("settle_cancel")
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
            Withdrawal::Expire,
        )
        .await
        .map_err(crate::error::to_sdk_handler_error)
    })
    .name("settle_expire")
    .await
}

/// Why ghinvite, rather than GitHub, ends an invitation.
#[derive(Clone, Copy, Debug)]
enum Withdrawal {
    /// `by_user` is `None` when the system cancels.
    Cancel {
        by_user: Option<u64>,
    },
    Expire,
}

impl Withdrawal {
    fn outcome(self, at: DateTime<Utc>) -> Outcome {
        let (state, by_user, reason) = match self {
            Withdrawal::Cancel { by_user } => (InvitationState::Cancelled, by_user, "cancel"),
            Withdrawal::Expire => (InvitationState::Expired, None, "tick_expire"),
        };
        Outcome {
            state,
            at,
            actor: if by_user.is_some() {
                ActorKind::User
            } else {
                ActorKind::System
            },
            by_user,
            metadata: serde_json::json!({"reason": reason, "by_user": by_user}),
        }
    }
}

async fn cancel_or_expire(
    state: &AppState,
    id: ghinvite_core::GithubInvitationId,
    installation: u64,
    at: DateTime<Utc>,
    withdrawal: Withdrawal,
) -> crate::Result<()> {
    let row = state
        .storage
        .get_github_invitation(id)
        .await?
        .ok_or(ghinvite_core::storage::Error::ProjectionDependency)?;
    if !eligible(&row) {
        return Ok(());
    }
    let context = context::load(state, &row, installation).await?;
    if let Withdrawal::Expire = withdrawal {
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
        // A failed token mint is an error here, never `NotFound`: the
        // installation being gone says nothing about the invitation.
        match state
            .github
            .delete_invitation(
                installation,
                context.repository.owner(),
                context.repository.name(),
                row.github_invitation_id.unwrap(),
            )
            .await?
        {
            InvitationDeletion::Deleted => (),
            // GitHub answers the same 404 when this installation no longer
            // sees the repository, so the invitation may still be pending.
            // Only a repository the installation verifiably reaches makes the
            // 404 mean the invitation. Verifying after the DELETE rather than
            // before keeps the common, deleted path to one GitHub call.
            InvitationDeletion::NotFound => match verify_repository_access(
                &state.github,
                &context.account,
                context.repo.repo_id,
                &context.repository,
            )
            .await
            {
                RepositoryAccess::Verified => (),
                // Cannot observe now (ADR 0006): nothing is settled.
                RepositoryAccess::Unavailable(_) => return Ok(()),
                RepositoryAccess::Unread(error) => return Err(error.into()),
            },
        }
    }
    record(
        state,
        context.account.account_id,
        row,
        withdrawal.outcome(at),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        dt, fixture_github_client, invitation_item, invitation_page, seed_pending_invitation,
        token_mint,
    };
    use ghinvite_github::mocks::{Expectation, MockTransport};
    use ghinvite_github::transport::Method;
    use std::sync::Arc;

    async fn seeded_state(mock: MockTransport) -> (AppState, GithubInvitation) {
        let (state, row, _) = seeded(mock).await;
        (state, row)
    }

    /// [`seeded_state`] plus the concrete storage, for debug-only audit reads.
    async fn seeded(
        mock: MockTransport,
    ) -> (
        AppState,
        GithubInvitation,
        Arc<ghinvite_storage_sqlx::SqlxStorage>,
    ) {
        let storage = Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::in_memory()
                .await
                .unwrap(),
        );
        let state = AppState::new(storage.clone(), fixture_github_client(Arc::new(mock)));
        let seeded = seed_pending_invitation(&storage, "acme/api").await;
        let row = state
            .storage
            .get_github_invitation(seeded.invitation_id)
            .await
            .unwrap()
            .unwrap();
        (state, row, storage)
    }

    #[tokio::test]
    async fn observe_finds_no_settlement_when_still_pending_on_a_later_page() {
        let mock = MockTransport::scripted(vec![
            Expectation::ok_json(
                Method::Get,
                "https://api.github.test/app/installations/9",
                serde_json::json!({"id":9,"account":{"id":100,"login":"acme"},"suspended_at":null}),
            ),
            token_mint(9),
            Expectation::ok_json(
                Method::Get,
                "https://api.github.test/repos/acme/api",
                serde_json::json!({"id":10,"full_name":"acme/api","private":true}),
            ),
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
    async fn an_uninstalled_installation_cannot_observe_rather_than_failing() {
        let mock = MockTransport::scripted(vec![Expectation::status(
            Method::Get,
            "https://api.github.test/app/installations/9",
            404,
        )]);
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
            Expectation::ok_json(
                Method::Get,
                "https://api.github.test/app/installations/9",
                serde_json::json!({"id":9,"account":{"id":100,"login":"acme"},"suspended_at":null}),
            ),
            token_mint(9),
            Expectation::ok_json(
                Method::Get,
                "https://api.github.test/repos/acme/api",
                serde_json::json!({"id":10,"full_name":"acme/api","private":true}),
            ),
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

    #[tokio::test]
    async fn observe_retries_a_throttled_listing_rather_than_skipping_it() {
        let throttled = crate::test_support::refusal(
            403,
            &[("retry-after", "60")],
            "You have exceeded a secondary rate limit",
        );
        let mock = MockTransport::scripted(vec![
            Expectation::ok_json(
                Method::Get,
                "https://api.github.test/app/installations/9",
                serde_json::json!({"id":9,"account":{"id":100,"login":"acme"},"suspended_at":null}),
            ),
            token_mint(9),
            Expectation::ok_json(
                Method::Get,
                "https://api.github.test/repos/acme/api",
                serde_json::json!({"id":10,"full_name":"acme/api","private":true}),
            ),
            Expectation {
                method: Method::Get,
                url: "https://api.github.test/repos/acme/api/invitations?per_page=100".into(),
                required_headers: Default::default(),
                expected_body: None,
                response: throttled,
            },
        ]);
        let (state, row) = seeded_state(mock.clone()).await;

        // A throttled 403 used to read as terminal, which dropped this row from
        // the sweep silently. It is an unread observation, so the sweep retries.
        let err = observe(&state, &row, dt("2026-05-05T13:00:00Z"))
            .await
            .unwrap_err();

        assert!(!err.is_terminal(), "got {err:?}");
        mock.assert_exhausted();
    }

    const DELETE_URL: &str = "https://api.github.test/repos/acme/api/invitations/9988";

    async fn stored_state(state: &AppState, row: &GithubInvitation) -> InvitationState {
        state
            .storage
            .get_github_invitation(row.id)
            .await
            .unwrap()
            .unwrap()
            .state
    }

    async fn cancel_row(state: &AppState, row: &GithubInvitation) -> crate::Result<()> {
        cancel_or_expire(
            state,
            row.id,
            9,
            dt("2026-05-05T13:00:00Z"),
            Withdrawal::Cancel { by_user: Some(7) },
        )
        .await
    }

    const REPO_URL: &str = "https://api.github.test/repos/acme/api";

    async fn cancelled_events(storage: &ghinvite_storage_sqlx::SqlxStorage) -> usize {
        storage
            .debug_list_audit(100)
            .await
            .unwrap()
            .into_iter()
            .filter(|event| {
                event.event_type == ghinvite_core::audit::EventType::InvitationCancelled
            })
            .count()
    }

    #[tokio::test]
    async fn cancelling_settles_on_githubs_deletion_without_another_read() {
        let mock = MockTransport::scripted(vec![
            token_mint(9),
            Expectation::status(Method::Delete, DELETE_URL, 204),
        ]);
        let (state, row, storage) = seeded(mock.clone()).await;

        cancel_row(&state, &row).await.unwrap();

        assert_eq!(stored_state(&state, &row).await, InvitationState::Cancelled);
        assert_eq!(cancelled_events(&storage).await, 1);
        mock.assert_exhausted();
    }

    #[tokio::test]
    async fn cancelling_an_invitation_github_already_removed_settles_it() {
        // The 404 is read as the invitation only once the installation is
        // shown to still reach the repository.
        let mock = MockTransport::scripted(vec![
            token_mint(9),
            Expectation::status(Method::Delete, DELETE_URL, 404),
            Expectation::ok_json(
                Method::Get,
                REPO_URL,
                serde_json::json!({"id":10,"full_name":"acme/api","private":true}),
            ),
        ]);
        let (state, row, storage) = seeded(mock.clone()).await;

        cancel_row(&state, &row).await.unwrap();

        assert_eq!(stored_state(&state, &row).await, InvitationState::Cancelled);
        assert_eq!(cancelled_events(&storage).await, 1);
        mock.assert_exhausted();
    }

    #[tokio::test]
    async fn a_404_from_a_repository_the_installation_no_longer_reaches_settles_nothing() {
        // GitHub answers the DELETE with the same 404 when the installation
        // cannot see the repository; the invitation may still be pending.
        let mock = MockTransport::scripted(vec![
            token_mint(9),
            Expectation::status(Method::Delete, DELETE_URL, 404),
            Expectation::status(Method::Get, REPO_URL, 404),
        ]);
        let (state, row, storage) = seeded(mock.clone()).await;

        cancel_row(&state, &row).await.unwrap_err();

        assert_eq!(stored_state(&state, &row).await, InvitationState::Sent);
        assert_eq!(cancelled_events(&storage).await, 0);
        mock.assert_exhausted();
    }

    #[tokio::test]
    async fn a_404_under_a_name_now_held_by_another_repository_settles_nothing() {
        let mock = MockTransport::scripted(vec![
            token_mint(9),
            Expectation::status(Method::Delete, DELETE_URL, 404),
            Expectation::ok_json(
                Method::Get,
                REPO_URL,
                serde_json::json!({"id":12,"full_name":"acme/api","private":true}),
            ),
        ]);
        let (state, row, storage) = seeded(mock.clone()).await;

        // Cannot observe now: nothing is settled and the row stays Sent.
        cancel_row(&state, &row).await.unwrap();

        assert_eq!(stored_state(&state, &row).await, InvitationState::Sent);
        assert_eq!(cancelled_events(&storage).await, 0);
        mock.assert_exhausted();
    }

    #[tokio::test]
    async fn a_404_whose_repository_read_fails_retries_rather_than_settling() {
        let mock = MockTransport::scripted(vec![
            token_mint(9),
            Expectation::status(Method::Delete, DELETE_URL, 404),
            Expectation::status(Method::Get, REPO_URL, 502),
        ]);
        let (state, row, storage) = seeded(mock.clone()).await;

        let err = cancel_row(&state, &row).await.unwrap_err();

        assert!(!err.is_terminal(), "got {err:?}");
        assert_eq!(stored_state(&state, &row).await, InvitationState::Sent);
        assert_eq!(cancelled_events(&storage).await, 0);
        mock.assert_exhausted();
    }

    #[tokio::test]
    async fn a_token_mint_404_is_not_evidence_the_invitation_is_gone() {
        // The installation is gone, not the invitation: GitHub still holds it.
        let mock = MockTransport::scripted(vec![Expectation::status(
            Method::Post,
            "https://api.github.test/app/installations/9/access_tokens",
            404,
        )]);
        let (state, row) = seeded_state(mock.clone()).await;

        cancel_row(&state, &row).await.unwrap_err();

        assert_eq!(stored_state(&state, &row).await, InvitationState::Sent);
        mock.assert_exhausted();
    }
}
