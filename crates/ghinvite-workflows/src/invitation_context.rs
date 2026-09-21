use crate::error::{HandlerError, Result};
use crate::state::AppState;
use ghinvite_core::{
    Account, GithubInvitation, GithubInvitationId, InvitationLink, InvitationLinkRepo,
    InvitationRequest, RepositoryIdentity, RequestId, User,
};

#[derive(Clone, Debug)]
pub(crate) struct InvitationRequestContext {
    pub request: InvitationRequest,
    pub link: InvitationLink,
}

#[derive(Clone, Debug)]
pub(crate) struct GithubInvitationContext {
    pub invitation: GithubInvitation,
    pub request: InvitationRequest,
    pub link: InvitationLink,
    pub repo: InvitationLinkRepo,
    pub repository: RepositoryIdentity,
    pub account: Account,
}

#[derive(Clone, Debug)]
pub(crate) struct GithubInvitationAccountContext {
    pub account: Account,
}

pub(crate) async fn load_installation_account(
    state: &AppState,
    installation_id: u64,
) -> Result<Account> {
    state
        .storage
        .get_installation(installation_id)
        .await?
        .ok_or(HandlerError::Storage(
            ghinvite_core::storage::Error::NotFound,
        ))
}

pub(crate) async fn load_github_invitation_context(
    state: &AppState,
    invitation_id: GithubInvitationId,
    expected_installation_id: u64,
) -> Result<GithubInvitationContext> {
    let invitation = load_github_invitation(state, invitation_id).await?;
    let context = load_github_invitation_context_for_loaded(state, &invitation).await?;
    expect_installation(&context, expected_installation_id)?;
    Ok(context)
}

pub(crate) async fn load_github_invitation_account_context(
    state: &AppState,
    invitation_id: GithubInvitationId,
) -> Result<GithubInvitationAccountContext> {
    let invitation = load_github_invitation(state, invitation_id).await?;
    let link = load_invitation_request_context(state, invitation.invitation_request_id)
        .await?
        .link;
    let account = load_installation_account(state, link.installation_id).await?;

    Ok(GithubInvitationAccountContext { account })
}

/// Observation authority follows the immutable account; the link retains its
/// original installation and fixed repository scope as historical facts.
/// The returned account supplies the verified active installation's credentials.
pub(crate) async fn load_verified_settlement_context(
    state: &AppState,
    invitation: &GithubInvitation,
) -> Result<Option<GithubInvitationContext>> {
    let mut context = load_github_invitation_context_for_loaded(state, invitation).await?;
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
    if let ghinvite_core::SelectedRepos::Subset(ids) = &account.selected_repos
        && !ids.contains(&context.repo.repo_id)
    {
        return Ok(None);
    }
    let repo = state
        .github
        .get_repo(
            account.installation_id,
            context.repository.owner(),
            context.repository.name(),
        )
        .await?;
    if repo.id != context.repo.repo_id {
        return Ok(None);
    }
    context.account = account;
    Ok(Some(context))
}

async fn load_invitation_link_for_request(
    state: &AppState,
    request: &InvitationRequest,
) -> Result<InvitationLink> {
    state
        .storage
        .get_invitation_link_by_id(request.invitation_link_id)
        .await?
        .ok_or(HandlerError::Storage(
            ghinvite_core::storage::Error::NotFound,
        ))
}

async fn load_requester_for_request(state: &AppState, request: &InvitationRequest) -> Result<User> {
    state
        .storage
        .get_user(request.requester_id)
        .await?
        .ok_or(HandlerError::Storage(
            ghinvite_core::storage::Error::NotFound,
        ))
}

pub(crate) async fn load_invitation_request_context(
    state: &AppState,
    request_id: RequestId,
) -> Result<InvitationRequestContext> {
    let request = state
        .storage
        .get_invitation_request(request_id)
        .await?
        .ok_or(HandlerError::Storage(
            ghinvite_core::storage::Error::NotFound,
        ))?;
    let link = load_invitation_link_for_request(state, &request).await?;
    load_requester_for_request(state, &request).await?;

    Ok(InvitationRequestContext { request, link })
}

async fn load_github_invitation(
    state: &AppState,
    invitation_id: GithubInvitationId,
) -> Result<GithubInvitation> {
    state
        .storage
        .get_github_invitation(invitation_id)
        .await?
        .ok_or(HandlerError::Storage(
            ghinvite_core::storage::Error::NotFound,
        ))
}

async fn load_github_invitation_context_for_loaded(
    state: &AppState,
    invitation: &GithubInvitation,
) -> Result<GithubInvitationContext> {
    let InvitationRequestContext { request, link } =
        load_invitation_request_context(state, invitation.invitation_request_id).await?;
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
    let account = load_installation_account(state, link.installation_id).await?;

    Ok(GithubInvitationContext {
        invitation: invitation.clone(),
        request,
        link,
        repo,
        repository,
        account,
    })
}

fn expect_installation(
    context: &GithubInvitationContext,
    expected_installation_id: u64,
) -> Result<()> {
    if context.link.installation_id != expected_installation_id {
        return Err(HandlerError::Invariant(format!(
            "github_invitation {} expected installation {} but link uses installation {}",
            context.invitation.id, expected_installation_id, context.link.installation_id
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HandlerError;
    use crate::test_support::{dt, fixture_state, fixture_state_with_storage};
    use ghinvite_core::{
        AccountType, InvitationLinkId, InvitationState, Permission, RequestState, SelectedRepos,
        Slug,
    };
    use rand::SeedableRng;

    async fn seed_request_chain_with_repos(
        storage: &ghinvite_storage_sqlx::SqlxStorage,
        repos: Vec<ghinvite_core::InvitationLinkRepo>,
    ) -> (RequestId, InvitationLinkId) {
        use ghinvite_core::storage::Storage;
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

    async fn seed_github_invitation(
        state: &AppState,
        request_id: RequestId,
        repo_id: u64,
    ) -> GithubInvitationId {
        let invitation_id = GithubInvitationId::new();
        state
            .storage
            .insert_github_invitation(&ghinvite_core::GithubInvitation {
                id: invitation_id,
                invitation_request_id: request_id,
                repo_id,
                github_invitation_id: Some(9988),
                state: InvitationState::Sent,
                error_message: None,
                created_at: dt("2026-05-04T13:00:00Z"),
                updated_at: dt("2026-05-04T13:00:00Z"),
            })
            .await
            .unwrap();
        invitation_id
    }

    #[tokio::test]
    async fn request_context_loads_request_and_link() {
        let (state, storage) = fixture_state_with_storage().await;
        let (request_id, link_id) = seed_request_chain(&storage).await;

        let context = load_invitation_request_context(&state, request_id)
            .await
            .unwrap();

        assert_eq!(context.request.id, request_id);
        assert_eq!(context.link.id, link_id);
    }

    #[tokio::test]
    async fn missing_request_returns_not_found() {
        let state = fixture_state().await;

        let err = load_invitation_request_context(&state, RequestId::new())
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            HandlerError::Storage(ghinvite_core::storage::Error::NotFound)
        ));
    }

    #[tokio::test]
    async fn github_context_loads_invitation_request_link_repo_requester_and_account() {
        let (state, storage) = fixture_state_with_storage().await;
        let (request_id, link_id) = seed_request_chain(&storage).await;
        let invitation_id = seed_github_invitation(&state, request_id, 10).await;

        let context = load_github_invitation_context(&state, invitation_id, 9)
            .await
            .unwrap();

        assert_eq!(context.invitation.id, invitation_id);
        assert_eq!(context.request.id, request_id);
        assert_eq!(context.link.id, link_id);
        assert_eq!(context.repo.repo_id, 10);
        assert_eq!(context.repository.owner(), "acme");
        assert_eq!(context.repository.name(), "api");
        assert_eq!(context.account.account_id, 100);
    }

    #[tokio::test]
    async fn missing_github_invitation_returns_not_found() {
        let state = fixture_state().await;

        let err = load_github_invitation_context(&state, GithubInvitationId::new(), 9)
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            HandlerError::Storage(ghinvite_core::storage::Error::NotFound)
        ));
    }

    #[tokio::test]
    async fn missing_invitation_link_returns_not_found() {
        let state = fixture_state().await;
        let request = ghinvite_core::InvitationRequest {
            id: RequestId::new(),
            invitation_link_id: InvitationLinkId::new(),
            requester_id: 8,
            justification: None,
            state: ghinvite_core::RequestState::Approved,
            decided_by: Some(7),
            decided_at: Some(dt("2026-05-04T13:00:00Z")),
            decline_reason: None,
            decision_deadline: None,
            created_at: dt("2026-05-04T12:30:00Z"),
        };

        let err = load_invitation_link_for_request(&state, &request)
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            HandlerError::Storage(ghinvite_core::storage::Error::NotFound)
        ));
    }

    #[tokio::test]
    async fn missing_requester_returns_not_found() {
        let state = fixture_state().await;
        let request = ghinvite_core::InvitationRequest {
            id: RequestId::new(),
            invitation_link_id: InvitationLinkId::new(),
            requester_id: 8,
            justification: None,
            state: ghinvite_core::RequestState::Approved,
            decided_by: Some(7),
            decided_at: Some(dt("2026-05-04T13:00:00Z")),
            decline_reason: None,
            decision_deadline: None,
            created_at: dt("2026-05-04T12:30:00Z"),
        };

        let err = load_requester_for_request(&state, &request)
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            HandlerError::Storage(ghinvite_core::storage::Error::NotFound)
        ));
    }

    #[tokio::test]
    async fn missing_installation_returns_not_found() {
        let state = fixture_state().await;

        let err = load_installation_account(&state, 9).await.unwrap_err();

        assert!(matches!(
            err,
            HandlerError::Storage(ghinvite_core::storage::Error::NotFound)
        ));
    }

    #[tokio::test]
    async fn github_context_missing_repo_returns_invariant() {
        let (state, storage) = fixture_state_with_storage().await;
        let (request_id, _) = seed_request_chain_with_repos(
            &storage,
            vec![ghinvite_core::InvitationLinkRepo {
                repo_id: 11,
                repo_full_name: "acme/web".into(),
            }],
        )
        .await;
        let invitation_id = seed_github_invitation(&state, request_id, 10).await;

        let err = load_github_invitation_context(&state, invitation_id, 9)
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
    async fn github_context_wrong_installation_returns_invariant() {
        let (state, storage) = fixture_state_with_storage().await;
        let (request_id, _) = seed_request_chain(&storage).await;
        let invitation_id = seed_github_invitation(&state, request_id, 10).await;

        let err = load_github_invitation_context(&state, invitation_id, 10)
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
    async fn github_account_context_does_not_require_matching_repo() {
        let (state, storage) = fixture_state_with_storage().await;
        let (request_id, _) = seed_request_chain_with_repos(
            &storage,
            vec![ghinvite_core::InvitationLinkRepo {
                repo_id: 11,
                repo_full_name: "acme/web".into(),
            }],
        )
        .await;
        let invitation_id = seed_github_invitation(&state, request_id, 10).await;

        let context = load_github_invitation_account_context(&state, invitation_id)
            .await
            .unwrap();
        assert_eq!(context.account.account_id, 100);
    }
}
