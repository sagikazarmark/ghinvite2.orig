//! Test-only: a [`RepositoryGithub`] for unit tests. Behind `#[cfg(test)]`
//! (declared in `lib.rs`).

use crate::RepositoryGithub;
use ghinvite_github::GithubApps;
use std::sync::Arc;

/// `RepositoryGithub` around `apps`, e.g. `test_support::github_apps`.
pub(crate) fn repository_github(apps: GithubApps) -> Arc<RepositoryGithub> {
    Arc::new(RepositoryGithub::with_apps(apps))
}
