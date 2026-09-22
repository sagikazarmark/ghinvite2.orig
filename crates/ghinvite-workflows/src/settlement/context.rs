//! What settlement reads about a GitHub invitation before it acts: the
//! invitation request and link it belongs to, the repository it was sent
//! for, and the account whose installation observes it.
//!
//! Every entry point takes the invitation row its caller already holds, so a
//! settlement reads the row once.
use crate::error::{HandlerError, Result};
use crate::repository_access::{RepositoryAccess, verify_repository_access};
use crate::state::AppState;
use ghinvite_core::{
    Account, GithubInvitation, InvitationLink, InvitationLinkRepo, InvitationRequest,
    RepositoryIdentity, storage::Error::NotFound,
};

#[derive(Clone, Debug)]
pub(super) struct Context {
    pub request: InvitationRequest,
    pub link: InvitationLink,
    pub repo: InvitationLinkRepo,
    pub repository: RepositoryIdentity,
    pub account: Account,
}

/// The account a settlement of `invitation` is audited under. Unlike
/// [`load`], the invitation's repository need not still be in the link's
/// scope: a settlement records what already happened.
pub(super) async fn account_id(state: &AppState, invitation: &GithubInvitation) -> Result<u64> {
    let (_, link) = request_chain(state, invitation).await?;
    Ok(installation_account(state, link.installation_id)
        .await?
        .account_id)
}

/// The full context of `invitation`, which must have been sent through
/// `installation_id`.
pub(super) async fn load(
    state: &AppState,
    invitation: &GithubInvitation,
    installation_id: u64,
) -> Result<Context> {
    let context = load_any(state, invitation).await?;
    if context.link.installation_id != installation_id {
        return Err(HandlerError::Invariant(format!(
            "github_invitation {} expected installation {} but link uses installation {}",
            invitation.id, installation_id, context.link.installation_id
        )));
    }
    Ok(context)
}

/// The context under which `invitation` can be observed now, or `None` when
/// it cannot: the account has no active installation, GitHub does not
/// confirm it, or it no longer reaches the repository.
///
/// Observation authority follows the immutable account; the link retains its
/// original installation and fixed repository scope as historical facts.
/// The returned account supplies the verified active installation's credentials.
pub(super) async fn load_verified(
    state: &AppState,
    invitation: &GithubInvitation,
) -> Result<Option<Context>> {
    let mut context = load_any(state, invitation).await?;
    if context.account.account_id != context.link.account_id {
        return Err(HandlerError::Invariant(
            "historical installation account mismatch".into(),
        ));
    }
    let Some(account) = state
        .storage
        .get_active_installation_by_account_id(context.link.account_id)
        .await?
    else {
        return Ok(None);
    };
    let verified = state
        .github
        .get_installation(account.installation_id)
        .await?;
    if verified.id != account.installation_id
        || verified.account.id != context.link.account_id
        || account.account_id != context.link.account_id
        || verified.suspended_at.is_some()
    {
        return Ok(None);
    }
    match verify_repository_access(
        &state.github,
        &account,
        context.repo.repo_id,
        &context.repository,
    )
    .await
    {
        RepositoryAccess::Verified => (),
        RepositoryAccess::Unavailable(_) => return Ok(None),
        RepositoryAccess::Unread(error) => return Err(error.into()),
    }
    context.account = account;
    Ok(Some(context))
}

async fn load_any(state: &AppState, invitation: &GithubInvitation) -> Result<Context> {
    let (request, link) = request_chain(state, invitation).await?;
    let repo = link
        .repos
        .iter()
        .find(|repo| repo.repo_id == invitation.repo_id)
        .cloned()
        .ok_or_else(|| {
            HandlerError::Invariant(format!(
                "github_invitation {} references repo_id {} not in link.repos",
                invitation.id, invitation.repo_id
            ))
        })?;
    let repository = RepositoryIdentity::parse(repo.repo_full_name.clone()).map_err(|err| {
        HandlerError::Invariant(format!(
            "invalid repo_full_name {:?}: {err}",
            repo.repo_full_name
        ))
    })?;
    let account = installation_account(state, link.installation_id).await?;
    Ok(Context {
        request,
        link,
        repo,
        repository,
        account,
    })
}

/// The invitation's request and link, with its requester present.
async fn request_chain(
    state: &AppState,
    invitation: &GithubInvitation,
) -> Result<(InvitationRequest, InvitationLink)> {
    let request = state
        .storage
        .get_invitation_request(invitation.invitation_request_id)
        .await?
        .ok_or(HandlerError::Storage(NotFound))?;
    let link = state
        .storage
        .get_invitation_link_by_id(request.invitation_link_id)
        .await?
        .ok_or(HandlerError::Storage(NotFound))?;
    state
        .storage
        .get_user(request.requester_id)
        .await?
        .ok_or(HandlerError::Storage(NotFound))?;
    Ok((request, link))
}

async fn installation_account(state: &AppState, installation_id: u64) -> Result<Account> {
    state
        .storage
        .get_installation(installation_id)
        .await?
        .ok_or(HandlerError::Storage(NotFound))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{dt, fixture_state, fixture_state_with_storage};
    use ghinvite_core::storage::{InstallationStorage, RecordStorage};
    use ghinvite_core::{
        AccountType, GithubInvitationId, InvitationLinkId, InvitationState, Permission, RequestId,
        RequestState, SelectedRepos, Slug,
    };
    use rand::SeedableRng;

    async fn seed_request_chain_with_repos(
        storage: &ghinvite_storage_sqlx::SqlxStorage,
        repos: Vec<ghinvite_core::InvitationLinkRepo>,
    ) -> (RequestId, InvitationLinkId) {
        storage
            .insert_installation(&ghinvite_core::Account {
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
        storage
            .upsert_user(&ghinvite_core::User {
                user_id: 7,
                login: "creator".into(),
                avatar_url: None,
                last_seen_at: dt("2026-05-04T12:00:00Z"),
            })
            .await
            .unwrap();
        storage
            .upsert_user(&ghinvite_core::User {
                user_id: 8,
                login: "alice".into(),
                avatar_url: None,
                last_seen_at: dt("2026-05-04T12:00:00Z"),
            })
            .await
            .unwrap();

        let link = ghinvite_core::InvitationLink {
            id: InvitationLinkId::new(),
            slug: Slug::generate(&mut rand_chacha::ChaCha8Rng::seed_from_u64(21)),
            installation_id: 9,
            account_id: 100,
            created_by: 7,
            created_at: dt("2026-05-04T12:00:00Z"),
            expires_at: None,
            max_uses: None,
            uses_count: 0,
            permission: Permission::Push,
            approval_required: false,
            description: "AI coding workshop".into(),
            internal_note: None,
            revoked_at: None,
            revoked_by: None,
            repos,
        };
        let request_id = RequestId::new();
        let request = ghinvite_core::InvitationRequest {
            id: request_id,
            invitation_link_id: link.id,
            requester_id: 8,
            justification: None,
            state: RequestState::Approved,
            decided_by: Some(7),
            decided_at: Some(dt("2026-05-04T13:00:00Z")),
            decline_reason: None,
            decision_deadline: None,
            created_at: dt("2026-05-04T12:30:00Z"),
        };
        {
            use ghinvite_core::storage::projection::{ProjectionStorage, fixture};
            storage
                .apply_transition(&fixture::envelope(&link, &[request], 1))
                .await
                .unwrap();
        }

        (request_id, link.id)
    }

    async fn seed_request_chain(
        storage: &ghinvite_storage_sqlx::SqlxStorage,
    ) -> (RequestId, InvitationLinkId) {
        seed_request_chain_with_repos(
            storage,
            vec![ghinvite_core::InvitationLinkRepo {
                repo_id: 10,
                repo_full_name: "acme/api".into(),
            }],
        )
        .await
    }

    /// A sent invitation for `repo_id` under `request_id`. Only the row is
    /// needed: every entry point takes it as its caller already holds it.
    fn sent_invitation(request_id: RequestId, repo_id: u64) -> GithubInvitation {
        GithubInvitation {
            id: GithubInvitationId::new(),
            invitation_request_id: request_id,
            repo_id,
            github_invitation_id: Some(9988),
            state: InvitationState::Sent,
            error_message: None,
            created_at: dt("2026-05-04T13:00:00Z"),
            updated_at: dt("2026-05-04T13:00:00Z"),
        }
    }

    fn scope_of_only_web() -> Vec<ghinvite_core::InvitationLinkRepo> {
        vec![ghinvite_core::InvitationLinkRepo {
            repo_id: 11,
            repo_full_name: "acme/web".into(),
        }]
    }

    #[tokio::test]
    async fn context_loads_request_link_repository_and_account() {
        let (state, storage) = fixture_state_with_storage().await;
        let (request_id, link_id) = seed_request_chain(&storage).await;

        let context = load(&state, &sent_invitation(request_id, 10), 9)
            .await
            .unwrap();

        assert_eq!(context.request.id, request_id);
        assert_eq!(context.link.id, link_id);
        assert_eq!(context.repo.repo_id, 10);
        assert_eq!(context.repository.owner(), "acme");
        assert_eq!(context.repository.name(), "api");
        assert_eq!(context.account.account_id, 100);
    }

    #[tokio::test]
    async fn an_invitation_whose_request_is_missing_is_not_found() {
        let state = fixture_state().await;
        let invitation = sent_invitation(RequestId::new(), 10);

        for err in [
            load(&state, &invitation, 9).await.unwrap_err(),
            load_verified(&state, &invitation).await.unwrap_err(),
            account_id(&state, &invitation).await.unwrap_err(),
        ] {
            assert!(
                matches!(err, HandlerError::Storage(NotFound)),
                "got {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_repository_outside_the_link_scope_is_an_invariant() {
        let (state, storage) = fixture_state_with_storage().await;
        let (request_id, _) = seed_request_chain_with_repos(&storage, scope_of_only_web()).await;

        let err = load(&state, &sent_invitation(request_id, 10), 9)
            .await
            .unwrap_err();

        match err {
            HandlerError::Invariant(message) => {
                assert!(message.contains("repo_id 10 not in link.repos"));
            }
            other => panic!("expected invariant, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn another_installation_is_an_invariant() {
        let (state, storage) = fixture_state_with_storage().await;
        let (request_id, _) = seed_request_chain(&storage).await;

        let err = load(&state, &sent_invitation(request_id, 10), 10)
            .await
            .unwrap_err();

        match err {
            HandlerError::Invariant(message) => {
                assert!(message.contains("expected installation 10"));
            }
            other => panic!("expected invariant, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_audited_account_does_not_require_the_repository_in_scope() {
        let (state, storage) = fixture_state_with_storage().await;
        let (request_id, _) = seed_request_chain_with_repos(&storage, scope_of_only_web()).await;

        let account = account_id(&state, &sent_invitation(request_id, 10))
            .await
            .unwrap();

        assert_eq!(account, 100);
    }
}
