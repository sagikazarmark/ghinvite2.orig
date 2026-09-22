//! Whether an installation can still reach one repository of a fixed
//! repository scope.
//!
//! Delivery and settlement both ask this before touching a repository, and
//! both ask it of the active installation they have already chosen. What each
//! does with the answer differs (ADR 0004): delivery blocks, throttles or
//! leaves the outcome unknown; settlement declines to observe or propagates
//! the GitHub error.
use ghinvite_core::{Account, RepositoryIdentity, SelectedRepos};
use ghinvite_github::InstallationClient;

#[derive(Debug)]
pub(crate) enum RepositoryAccess {
    /// GitHub answers for the repository's name with the numeric repository
    /// the scope fixed.
    Verified,
    /// GitHub, or the installation's own record, says the repository cannot
    /// be reached as the scope fixed it.
    Unavailable(Unavailable),
    /// GitHub did not answer, so nothing is known about the repository.
    Unread(ghinvite_github::Error),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unavailable {
    /// The installation's available repositories exclude it.
    NotSelected,
    /// The name now belongs to a different repository.
    IdentityMismatch,
}

/// Verify that `account`'s installation can reach the repository `repo_id`
/// at `repository`. The numeric ID is the identity: a repository GitHub
/// reports under a new name is still the one the scope fixed.
pub(crate) async fn verify_repository_access(
    github: &InstallationClient,
    account: &Account,
    repo_id: u64,
    repository: &RepositoryIdentity,
) -> RepositoryAccess {
    if let SelectedRepos::Subset(ids) = &account.selected_repos
        && !ids.contains(&repo_id)
    {
        return RepositoryAccess::Unavailable(Unavailable::NotSelected);
    }
    match github
        .get_repo(
            account.installation_id,
            repository.owner(),
            repository.name(),
        )
        .await
    {
        Ok(repo) if repo.id == repo_id => RepositoryAccess::Verified,
        Ok(_) => RepositoryAccess::Unavailable(Unavailable::IdentityMismatch),
        Err(error) => RepositoryAccess::Unread(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{dt, fixture_github_client, refusal, token_mint};
    use ghinvite_core::AccountType;
    use ghinvite_github::mocks::{Expectation, MockTransport};
    use ghinvite_github::transport::Method;
    use std::sync::Arc;

    const REPO_URL: &str = "https://api.github.test/repos/acme/api";

    fn account(selected_repos: SelectedRepos) -> Account {
        Account {
            installation_id: 9,
            account_id: 100,
            account_login: "acme".into(),
            account_type: AccountType::Organization,
            installed_at: dt("2026-05-04T12:00:00Z"),
            uninstalled_at: None,
            selected_repos,
        }
    }

    fn repository() -> RepositoryIdentity {
        RepositoryIdentity::parse("acme/api".to_owned()).unwrap()
    }

    async fn verify(script: Vec<Expectation>, selected_repos: SelectedRepos) -> RepositoryAccess {
        let mock = MockTransport::scripted(script);
        let github = fixture_github_client(Arc::new(mock.clone()));
        let access =
            verify_repository_access(&github, &account(selected_repos), 10, &repository()).await;
        mock.assert_exhausted();
        access
    }

    fn repo_answer(id: u64, full_name: &str) -> Vec<Expectation> {
        vec![
            token_mint(9),
            Expectation::ok_json(
                Method::Get,
                REPO_URL,
                serde_json::json!({"id": id, "full_name": full_name, "private": true}),
            ),
        ]
    }

    fn repo_refusal(status: u16, headers: &[(&str, &str)], message: &str) -> Vec<Expectation> {
        vec![
            token_mint(9),
            Expectation {
                method: Method::Get,
                url: REPO_URL.into(),
                required_headers: Default::default(),
                expected_body: None,
                response: refusal(status, headers, message),
            },
        ]
    }

    #[tokio::test]
    async fn matching_repository_is_verified() {
        let access = verify(repo_answer(10, "acme/api"), SelectedRepos::Subset(vec![10])).await;

        assert!(matches!(access, RepositoryAccess::Verified));
    }

    #[tokio::test]
    async fn repository_outside_the_selection_is_unavailable_without_asking_github() {
        let access = verify(vec![], SelectedRepos::Subset(vec![11])).await;

        assert!(matches!(
            access,
            RepositoryAccess::Unavailable(Unavailable::NotSelected)
        ));
    }

    #[tokio::test]
    async fn another_repository_at_the_name_is_unavailable() {
        let access = verify(repo_answer(12, "acme/api"), SelectedRepos::All).await;

        assert!(matches!(
            access,
            RepositoryAccess::Unavailable(Unavailable::IdentityMismatch)
        ));
    }

    #[tokio::test]
    async fn renamed_repository_is_still_verified_by_numeric_identity() {
        let access = verify(repo_answer(10, "acme/api-v2"), SelectedRepos::All).await;

        assert!(matches!(access, RepositoryAccess::Verified));
    }

    #[tokio::test]
    async fn missing_repository_is_unread_rather_than_unavailable() {
        let access = verify(repo_refusal(404, &[], "Not Found"), SelectedRepos::All).await;

        assert!(matches!(
            access,
            RepositoryAccess::Unread(ghinvite_github::Error::Status { status: 404, .. })
        ));
    }

    #[tokio::test]
    async fn rate_limited_read_is_unread_with_githubs_guidance() {
        let access = verify(
            repo_refusal(403, &[("retry-after", "30")], "API rate limit exceeded"),
            SelectedRepos::All,
        )
        .await;

        let RepositoryAccess::Unread(error) = access else {
            panic!("expected an unread repository, got {access:?}");
        };
        assert!(error.rate_limit().is_some());
    }

    #[tokio::test]
    async fn transport_failure_is_unread() {
        struct Unreachable;

        #[async_trait::async_trait]
        impl ghinvite_github::HttpTransport for Unreachable {
            async fn send(
                &self,
                _: ghinvite_github::transport::Request,
            ) -> ghinvite_github::Result<ghinvite_github::transport::Response> {
                Err(ghinvite_github::Error::Transport("connection reset".into()))
            }
        }

        let github = fixture_github_client(Arc::new(Unreachable));
        let access =
            verify_repository_access(&github, &account(SelectedRepos::All), 10, &repository())
                .await;

        assert!(matches!(
            access,
            RepositoryAccess::Unread(ghinvite_github::Error::Transport(_))
        ));
    }
}
