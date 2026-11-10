//! The provider's error type.

use std::time::Duration;

/// Why a provider could not supply a token. `Clone` so concurrent waiters can
/// share one outcome; secret-free like [`octoauth::Error`].
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum TokenError {
    /// Creating the token failed; see [`octoauth::Error`] for the cases.
    #[error(transparent)]
    GitHub(#[from] octoauth::Error),

    /// [`replace`](crate::InstallationTokenProvider::replace) was given a
    /// token created by a different GitHub App than the provider's.
    #[error("gh:provider:foreign_token")]
    ForeignToken,
}

impl TokenError {
    /// Stable code for metrics and journals.
    pub fn code(&self) -> &'static str {
        match self {
            Self::GitHub(error) => error.code(),
            Self::ForeignToken => "gh:provider:foreign_token",
        }
    }

    /// Whether asking again later may succeed without any configuration change.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::GitHub(error) => error.is_transient(),
            Self::ForeignToken => false,
        }
    }

    /// GitHub's requested wait for a rate limit. Guidance only.
    pub fn suggested_wait(&self) -> Option<Duration> {
        match self {
            Self::GitHub(error) => error.suggested_wait(),
            Self::ForeignToken => None,
        }
    }
}
