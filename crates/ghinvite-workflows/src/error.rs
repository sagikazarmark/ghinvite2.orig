//! Error type bridging Storage, GitHub, and Restate's terminal/transient model.

use restate_sdk::prelude::TerminalError;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, HandlerError>;

#[derive(Debug, Error)]
pub enum HandlerError {
    #[error(transparent)]
    Storage(#[from] ghinvite_core::storage::Error),

    /// Read-side API failure. Write ambiguity must be consumed by the delivery
    /// protocol as OutcomeUnknown before reaching this retryable error bridge.
    #[error(transparent)]
    GithubApi(#[from] ghinvite_github::ApiError),

    /// No installation token could be had (`octotoken`).
    #[error(transparent)]
    Token(#[from] octotoken::TokenError),

    /// Domain invariant the caller should not have allowed.
    #[error("invariant violated: {0}")]
    Invariant(String),
}

impl HandlerError {
    /// Returns `true` for terminal errors that should bypass Restate retry;
    /// `false` for transient failures that may be retried.
    pub fn is_terminal(&self) -> bool {
        match self {
            HandlerError::Storage(e) => match e {
                ghinvite_core::storage::Error::Database(_)
                | ghinvite_core::storage::Error::ProjectionDependency
                | ghinvite_core::storage::Error::ProjectionInvariant(_) => false, // retained for repair
                ghinvite_core::storage::Error::Conflict(_)
                | ghinvite_core::storage::Error::NotFound
                | ghinvite_core::storage::Error::Corrupt(_) => true, // terminal
            },
            HandlerError::Invariant(_) => true, // bug — terminal
            HandlerError::GithubApi(e) => !e.is_transient_for_retry_safe(true),
            HandlerError::Token(e) => !e.is_transient(),
        }
    }

    /// Convert into Restate's terminal-error wrapper if this error should NOT
    /// retry; otherwise return the underlying message wrapped in
    /// `Err(restate_sdk::error::Error::other(...))` so Restate retries.
    ///
    /// In the current SDK shape, handler return types are
    /// `Result<T, TerminalError>`; transient errors are surfaced by leaving
    /// `ctx.run` to fail and propagate. Most handlers will use `to_terminal()`
    /// after a `match self.is_terminal()`.
    pub fn to_terminal(&self) -> TerminalError {
        TerminalError::new(self.to_string())
    }

    /// GitHub's throttling evidence when this failure is a rate limit. Callers
    /// use it to defer by [`crate::throttle::backoff`] instead of leaving the
    /// transient classification to retry on a cadence of its own.
    pub fn rate_limit(&self) -> Option<RateLimit> {
        match self {
            HandlerError::GithubApi(ghinvite_github::ApiError::RateLimited {
                scope,
                retry_after,
                ..
            }) => Some(RateLimit {
                scope: *scope,
                retry_after: *retry_after,
            }),
            HandlerError::Token(e) => ghinvite_github::apps::rate_limit(e)
                .map(|(scope, retry_after)| RateLimit { scope, retry_after }),
            _ => None,
        }
    }
}

/// GitHub's throttling evidence for a failed read or token creation: which
/// limit GitHub cited, and the wait it asked for, if any.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RateLimit {
    pub scope: ghinvite_github::error::RateLimitScope,
    pub retry_after: Option<std::time::Duration>,
}

impl From<ghinvite_github::TokenCallError> for HandlerError {
    fn from(error: ghinvite_github::TokenCallError) -> Self {
        use ghinvite_github::{CallError, TokenCallError};
        match error {
            TokenCallError::Token(error) => Self::Token(error),
            TokenCallError::Call(CallError::Api(error)) => Self::GithubApi(error),
            TokenCallError::Call(CallError::App(impossible)) => match impossible {},
        }
    }
}

/// Map our `HandlerError` to the SDK's `HandlerError`, honoring
/// `is_terminal()` classification:
///
/// - **Terminal** errors (4xx GitHub, conflicts, NotFound, invariant
///   violations, corruption) become `restate_sdk::errors::TerminalError`,
///   which Restate will NOT retry.
/// - **Transient** errors (5xx, rate-limit, transport, DB connectivity)
///   become a non-terminal `restate_sdk::errors::HandlerError`, which
///   Restate WILL retry with exponential backoff.
///
/// Implemented as a free function rather than `impl From<HandlerError>`
/// because the SDK provides a blanket
/// `impl<E: Into<Box<dyn StdError + Send + Sync>>> From<E> for HandlerError`
/// that conflicts with any user-provided `From` impl. Use as
/// `.map_err(crate::error::to_sdk_handler_error)`.
pub fn to_sdk_handler_error(e: HandlerError) -> ::restate_sdk::errors::HandlerError {
    if e.is_terminal() {
        // Terminal: Restate will NOT retry.
        e.to_terminal().into()
    } else {
        // Transient: the SDK's blanket `From<E: Into<Box<dyn StdError ...>>>`
        // produces a `HandlerErrorInner::Retryable`, which Restate retries.
        let boxed: Box<dyn std::error::Error + Send + Sync> = e.to_string().into();
        ::restate_sdk::errors::HandlerError::from(boxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghinvite_core::storage::ConflictKind;
    use ghinvite_github::ApiError;

    #[test]
    fn ghinvite_github_read_failures_keep_the_api_retry_rule() {
        for (error, terminal) in [
            (ApiError::Ambiguous, false),
            (ApiError::NotSent, false),
            (ApiError::Server { status: 502 }, false),
            (ApiError::NotFound, true),
            (ApiError::Unauthorized, true),
            (ApiError::Conflict, true),
            (ApiError::Decode { what: "test" }, true),
        ] {
            let handler = HandlerError::from(error.clone());
            assert!(matches!(handler, HandlerError::GithubApi(_)));
            assert_eq!(handler.is_terminal(), terminal, "{error:?}");
            assert!(handler.rate_limit().is_none());
        }
    }

    #[test]
    fn token_failures_retry_only_when_transient() {
        use octoauth::Error as GitHub;
        for (error, terminal) in [
            (GitHub::InstallationGone, true),
            (GitHub::InstallationSuspended, true),
            (GitHub::PermissionsNotGranted, true),
            (GitHub::Decode, true),
            (GitHub::Redirected, true),
            (
                GitHub::Transient {
                    kind: octoauth::TransientKind::Server { status: 502 },
                },
                false,
            ),
        ] {
            let handler = HandlerError::from(octotoken::TokenError::GitHub(error.clone()));
            assert!(matches!(handler, HandlerError::Token(_)));
            assert_eq!(handler.is_terminal(), terminal, "{error:?}");
            assert!(handler.rate_limit().is_none());
        }
        assert!(HandlerError::from(octotoken::TokenError::ForeignToken).is_terminal());
    }

    /// Read and token-creation rate limits are transient and keep GitHub's
    /// scope and wait, without naming the token.
    #[test]
    fn rate_limits_keep_scope_and_wait() {
        use ghinvite_github::error::RateLimitScope;
        let wait = Some(std::time::Duration::from_secs(30));
        for scope in [RateLimitScope::Primary, RateLimitScope::Secondary] {
            for error in [
                HandlerError::from(ApiError::RateLimited {
                    scope,
                    retry_after: wait,
                    resource: None,
                }),
                HandlerError::from(octotoken::TokenError::GitHub(
                    octoauth::Error::RateLimited {
                        scope,
                        retry_after: wait,
                    },
                )),
            ] {
                assert!(!error.is_terminal());
                assert_eq!(
                    error.rate_limit(),
                    Some(RateLimit {
                        scope,
                        retry_after: wait,
                    })
                );
                assert!(!error.to_string().contains("token"));
            }
        }
    }

    #[test]
    fn token_call_errors_map_to_token_and_api_failures() {
        use ghinvite_github::{CallError, TokenCallError};
        let token = HandlerError::from(TokenCallError::Token(octotoken::TokenError::GitHub(
            octoauth::Error::InstallationGone,
        )));
        assert!(matches!(
            token,
            HandlerError::Token(octotoken::TokenError::GitHub(
                octoauth::Error::InstallationGone
            ))
        ));
        let call = HandlerError::from(TokenCallError::Call(CallError::Api(ApiError::NotFound)));
        assert!(matches!(call, HandlerError::GithubApi(ApiError::NotFound)));
        // A second 401 from `read` is the API's 401.
        let unauthorized =
            HandlerError::from(TokenCallError::Call(CallError::Api(ApiError::Unauthorized)));
        assert!(matches!(
            unauthorized,
            HandlerError::GithubApi(ApiError::Unauthorized)
        ));
        assert!(unauthorized.is_terminal());
    }

    #[test]
    fn storage_database_is_transient() {
        let e = HandlerError::Storage(ghinvite_core::storage::Error::Database(
            "test configuration error".to_string(),
        ));
        assert!(!e.is_terminal());
    }

    #[test]
    fn storage_conflict_is_terminal() {
        let e = HandlerError::Storage(ghinvite_core::storage::Error::Conflict(
            ConflictKind::DuplicateId,
        ));
        assert!(e.is_terminal());
    }

    #[test]
    fn storage_not_found_is_terminal() {
        let e = HandlerError::Storage(ghinvite_core::storage::Error::NotFound);
        assert!(e.is_terminal());
    }

    /// The error a GitHub response of this shape actually produces, so these
    /// cases stay pinned to the boundary's classification rather than to a
    /// variant hand-built here.
    async fn github_response(status: u16, headers: &[(&str, &str)], message: &str) -> HandlerError {
        HandlerError::from(crate::test_support::refused(status, headers, message).await)
    }

    #[tokio::test]
    async fn github_5xx_is_transient() {
        let e = github_response(502, &[], "bad gateway").await;
        assert!(!e.is_terminal());
        assert!(e.rate_limit().is_none());
    }

    #[tokio::test]
    async fn github_429_is_transient() {
        let e = github_response(429, &[("retry-after", "60")], "Too Many Requests").await;
        assert!(!e.is_terminal());
        assert!(e.rate_limit().is_some());
    }

    #[tokio::test]
    async fn github_throttled_403_is_transient() {
        let e = github_response(
            403,
            &[("retry-after", "60")],
            "You have exceeded a secondary rate limit",
        )
        .await;
        assert!(!e.is_terminal());
        assert_eq!(
            e.rate_limit().and_then(|limit| limit.retry_after),
            Some(std::time::Duration::from_secs(60))
        );
    }

    #[tokio::test]
    async fn github_permission_denied_403_is_terminal() {
        let e = github_response(403, &[], "Resource not accessible by integration").await;
        assert!(e.is_terminal());
        assert!(e.rate_limit().is_none());
    }

    #[tokio::test]
    async fn github_404_is_terminal() {
        let e = github_response(404, &[], "Not Found").await;
        assert!(e.is_terminal());
    }

    #[test]
    fn github_transport_is_transient() {
        use ghinvite_github::TransportError;
        for failure in [
            TransportError::NotSent,
            TransportError::SentNoResponse { timed_out: false },
            TransportError::SentNoResponse { timed_out: true },
        ] {
            let e = HandlerError::from(ApiError::from(failure));
            assert!(!e.is_terminal(), "{e:?}");
        }
    }

    #[test]
    fn invariant_is_terminal() {
        let e = HandlerError::Invariant("test".into());
        assert!(e.is_terminal());
    }
}
