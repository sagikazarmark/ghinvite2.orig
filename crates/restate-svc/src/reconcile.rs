//! `Reconcile` Service: daily sweep over pending GitHub invitations.

use crate::audit::{Actor, Target};
use crate::state::AppState;
use audit::EventType;
use chrono::{DateTime, Utc};
use domain::InvitationState;
use restate_sdk::context::{Context, ContextSideEffects, RunFuture};
use restate_sdk::errors::TerminalError;
use restate_sdk::serde::Json;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct DailyRunInput {
    pub at: DateTime<Utc>,
}

#[restate_sdk::service]
pub trait Reconcile {
    async fn daily_run(input: Json<DailyRunInput>) -> std::result::Result<(), TerminalError>;
}

pub struct ReconcileImpl {
    pub state: AppState,
}

impl Reconcile for ReconcileImpl {
    async fn daily_run(
        &self,
        ctx: Context<'_>,
        input: Json<DailyRunInput>,
    ) -> std::result::Result<(), TerminalError> {
        let Json(input) = input;
        let request_id = Some(ctx.invocation_id().to_string());
        ctx.run(|| async {
            daily_run_logic(&self.state, &input, request_id.clone())
                .await
                .map_err(crate::error::to_sdk_handler_error)
        })
        .name("daily_run")
        .await
    }
}

/// For each active installation, walk our pending github_invitations rows
/// and reconcile against GitHub's reality. Idempotent.
pub async fn daily_run_logic(
    state: &AppState,
    input: &DailyRunInput,
    request_id_for_audit: Option<String>,
) -> crate::error::Result<()> {
    let installations = state.storage.list_active_installations().await?;
    for acct in installations {
        let pending = state
            .storage
            .list_pending_github_invitations_for_installation(acct.installation_id)
            .await?;
        for row in pending {
            if let Err(e) =
                reconcile_single(state, &acct, &row, input.at, request_id_for_audit.clone()).await
            {
                if !e.is_terminal() {
                    // Transient — let Restate retry the whole sweep.
                    return Err(e);
                }
                // Terminal — log and continue with the next row.
                tracing::warn!(
                    invitation_id = %row.id,
                    err = %e,
                    "reconcile_single failed terminally; continuing"
                );
            }
        }
    }
    Ok(())
}

async fn reconcile_single(
    state: &AppState,
    acct: &domain::Account,
    row: &domain::GithubInvitation,
    at: DateTime<Utc>,
    request_id_for_audit: Option<String>,
) -> crate::error::Result<()> {
    let context =
        crate::invitation_context::load_github_invitation_context_for_account(state, acct, row)
            .await?;
    debug_assert_eq!(context.request.id, row.invitation_request_id);
    debug_assert_eq!(context.repo.repo_id, row.repo_id);

    let pending = state
        .github
        .list_invitations(
            acct.installation_id,
            context.repository.owner(),
            context.repository.name(),
        )
        .await?;
    let still_pending = row
        .github_invitation_id
        .map(|id| pending.iter().any(|p| p.id == id))
        .unwrap_or(false);

    if still_pending {
        return Ok(());
    }

    // GitHub no longer lists it. Disambiguate accepted vs. cancelled by
    // checking is_collaborator.
    let is_member = state
        .github
        .is_collaborator(
            acct.installation_id,
            context.repository.owner(),
            context.repository.name(),
            &context.requester.login,
        )
        .await?;

    let (new_state, event) = if is_member {
        (InvitationState::Accepted, EventType::InvitationAccepted)
    } else {
        (InvitationState::Cancelled, EventType::InvitationCancelled)
    };

    state
        .storage
        .update_github_invitation(&storage::GithubInvitationUpdate {
            id: row.id,
            state: new_state,
            github_invitation_id: None,
            error_message: None,
            updated_at: at,
        })
        .await?;

    crate::audit::emit(
        state,
        context.account.account_id,
        event,
        Actor::System,
        Target::github_invitation(row.id),
        serde_json::json!({"reconciled": true}),
        request_id_for_audit,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{dt, fixture_github_client, fixture_storage};
    use domain::{
        AccountType, GithubInvitationId, Permission, RequestId, RequestState, SelectedRepos,
        ShareLink, ShareLinkId, ShareLinkRepo, Slug,
    };
    use github::mocks::{Expectation, MockTransport};
    use github::transport::{Method, Response};
    use rand::SeedableRng;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    fn token_mint(installation_id: u64) -> Expectation {
        Expectation {
            method: Method::Post,
            url: format!(
                "https://api.github.test/app/installations/{}/access_tokens",
                installation_id
            ),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 201,
                headers: BTreeMap::new(),
                body: br#"{"token":"ghs_xxx","expires_at":"2099-01-01T00:00:00Z"}"#.to_vec(),
            },
        }
    }

    /// Seed: one installation, two users, one share_link with one repo, one
    /// invitation_request, one github_invitation row in `Sent` state.
    /// Returns the inserted invitation id.
    async fn seed_one_pending_with_repo_full_name(
        state: &AppState,
        repo_full_name: &str,
    ) -> GithubInvitationId {
        state
            .storage
            .insert_installation(&domain::Account {
                installation_id: 9,
                account_id: 100,
                account_login: "acme".into(),
                account_type: AccountType::Organization,
                installed_at: dt("2026-05-04T12:00:00Z"),
                uninstalled_at: None,
                selected_repos: SelectedRepos::All,
            })
            .await
            .unwrap();
        state
            .storage
            .upsert_user(&domain::User {
                user_id: 7,
                login: "creator".into(),
                avatar_url: None,
                last_seen_at: dt("2026-05-04T12:00:00Z"),
            })
            .await
            .unwrap();
        state
            .storage
            .upsert_user(&domain::User {
                user_id: 8,
                login: "alice".into(),
                avatar_url: None,
                last_seen_at: dt("2026-05-04T12:00:00Z"),
            })
            .await
            .unwrap();
        let link = ShareLink {
            id: ShareLinkId::new(),
            slug: Slug::generate(&mut rand_chacha::ChaCha8Rng::seed_from_u64(7)),
            installation_id: 9,
            account_id: 100,
            created_by: 7,
            created_at: dt("2026-05-04T12:00:00Z"),
            expires_at: None,
            max_uses: None,
            uses_count: 0,
            permission: Permission::Push,
            approval_required: false,
            internal_note: None,
            revoked_at: None,
            revoked_by: None,
            repos: vec![ShareLinkRepo {
                repo_id: 10,
                repo_full_name: repo_full_name.into(),
            }],
        };
        state.storage.insert_share_link(&link).await.unwrap();
        let req_id = RequestId::new();
        let req = domain::InvitationRequest {
            id: req_id,
            share_link_id: link.id,
            requester_id: 8,
            justification: None,
            state: RequestState::Approved,
            decided_by: Some(7),
            decided_at: Some(dt("2026-05-04T13:00:00Z")),
            decline_reason: None,
            created_at: dt("2026-05-04T12:30:00Z"),
        };
        state
            .storage
            .insert_invitation_request_and_increment_uses(&req)
            .await
            .unwrap();
        let inv_id = GithubInvitationId::new();
        state
            .storage
            .insert_github_invitation(&domain::GithubInvitation {
                id: inv_id,
                invitation_request_id: req_id,
                repo_id: 10,
                github_invitation_id: Some(9988),
                state: InvitationState::Sent,
                error_message: None,
                created_at: dt("2026-05-04T13:00:00Z"),
                updated_at: dt("2026-05-04T13:00:00Z"),
            })
            .await
            .unwrap();
        inv_id
    }

    async fn seed_one_pending(state: &AppState) -> GithubInvitationId {
        seed_one_pending_with_repo_full_name(state, "acme/api").await
    }

    #[tokio::test]
    async fn daily_run_no_change_when_still_pending() {
        let storage = fixture_storage().await;
        let mock = MockTransport::scripted(vec![
            token_mint(9),
            Expectation::ok_json(
                Method::Get,
                "https://api.github.test/repos/acme/api/invitations?per_page=100",
                serde_json::json!([
                    {
                        "id": 9988,
                        "invitee": {"id": 42, "login": "alice"},
                        "permissions": "write",
                        "created_at": "2026-05-04T13:00:00Z"
                    }
                ]),
            ),
        ]);
        let github = fixture_github_client(Arc::new(mock));
        let state = AppState::new(storage, github);
        let inv_id = seed_one_pending(&state).await;

        daily_run_logic(
            &state,
            &DailyRunInput {
                at: dt("2026-05-05T13:00:00Z"),
            },
            None,
        )
        .await
        .unwrap();

        let row = state
            .storage
            .get_github_invitation(inv_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, InvitationState::Sent);
    }

    #[tokio::test]
    async fn daily_run_continues_when_repo_full_name_is_invalid() {
        let storage = fixture_storage().await;
        let mock = MockTransport::scripted(vec![]);
        let github = fixture_github_client(Arc::new(mock));
        let state = AppState::new(storage, github);
        let inv_id = seed_one_pending_with_repo_full_name(&state, "acme/team/api").await;

        daily_run_logic(
            &state,
            &DailyRunInput {
                at: dt("2026-05-05T13:00:00Z"),
            },
            None,
        )
        .await
        .unwrap();

        let row = state
            .storage
            .get_github_invitation(inv_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, InvitationState::Sent);
    }

    #[tokio::test]
    async fn daily_run_marks_accepted_when_user_is_collaborator() {
        let storage = fixture_storage().await;
        let mock = MockTransport::scripted(vec![
            token_mint(9),
            Expectation::ok_json(
                Method::Get,
                "https://api.github.test/repos/acme/api/invitations?per_page=100",
                serde_json::json!([]),
            ),
            Expectation {
                method: Method::Get,
                url: "https://api.github.test/repos/acme/api/collaborators/alice".into(),
                required_headers: BTreeMap::new(),
                expected_body: None,
                response: Response {
                    status: 204,
                    headers: BTreeMap::new(),
                    body: vec![],
                },
            },
        ]);
        let github = fixture_github_client(Arc::new(mock));
        let state = AppState::new(storage, github);
        let inv_id = seed_one_pending(&state).await;

        daily_run_logic(
            &state,
            &DailyRunInput {
                at: dt("2026-05-05T13:00:00Z"),
            },
            None,
        )
        .await
        .unwrap();

        let row = state
            .storage
            .get_github_invitation(inv_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, InvitationState::Accepted);
    }

    #[tokio::test]
    async fn daily_run_marks_cancelled_when_user_is_not_collaborator() {
        let storage = fixture_storage().await;
        let mock = MockTransport::scripted(vec![
            token_mint(9),
            Expectation::ok_json(
                Method::Get,
                "https://api.github.test/repos/acme/api/invitations?per_page=100",
                serde_json::json!([]),
            ),
            Expectation::status(
                Method::Get,
                "https://api.github.test/repos/acme/api/collaborators/alice",
                404,
            ),
        ]);
        let github = fixture_github_client(Arc::new(mock));
        let state = AppState::new(storage, github);
        let inv_id = seed_one_pending(&state).await;

        daily_run_logic(
            &state,
            &DailyRunInput {
                at: dt("2026-05-05T13:00:00Z"),
            },
            None,
        )
        .await
        .unwrap();

        let row = state
            .storage
            .get_github_invitation(inv_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, InvitationState::Cancelled);
    }
}
