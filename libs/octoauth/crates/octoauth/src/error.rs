//! Fixed-code, secret-free error types.
//!
//! No error here carries upstream response text, a URL, a token, key material
//! or a nested library error: values end up in logs and durable journals.

use std::time::Duration;

/// Invalid configuration or input. Names the field and a fixed reason, never the value.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("invalid {field} ({what})")]
pub struct ConfigError {
    field: &'static str,
    what: &'static str,
}

impl ConfigError {
    /// A configuration error for `field` with a fixed reason code `what`, for
    /// crates building on this one. Never pass a value.
    pub const fn new(field: &'static str, what: &'static str) -> Self {
        Self { field, what }
    }

    /// The rejected field, e.g. `signing_key`.
    pub const fn field(&self) -> &'static str {
        self.field
    }

    /// Fixed reason code, e.g. `modulus`.
    pub const fn what(&self) -> &'static str {
        self.what
    }

    /// Stable code for metrics and journals.
    pub const fn code(&self) -> &'static str {
        "gh:config:invalid"
    }
}

/// Failure to sign the app's credentials locally. Returned by
/// [`AppKeySigner`](crate::AppKeySigner) implementations, and inside
/// [`Error::Signing`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum SigningError {
    /// The signer failed, or produced a signature of an impossible length.
    #[error("gh:signing:failed")]
    Failed,

    /// The clock reading cannot produce a valid validity window (before
    /// 1970-01-01T00:01:00Z or overflowing).
    #[error("gh:signing:invalid_time")]
    InvalidTime,
}

impl SigningError {
    /// Stable code for metrics and journals.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Failed => "gh:signing:failed",
            Self::InvalidTime => "gh:signing:invalid_time",
        }
    }
}

/// Which documented GitHub limit a throttled response reports.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RateLimitScope {
    /// The hourly quota: `x-ratelimit-remaining: 0`, cleared at `x-ratelimit-reset`.
    Primary,

    /// A secondary limit: `retry-after`, a 429, or GitHub's rate-limit wording.
    Secondary,
}

/// Bounded classification of a 403 without rate-limit evidence. A reason
/// other than [`Other`](Self::Other) needs GitHub's header or full message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ForbiddenReason {
    /// GitHub sent `X-GitHub-SSO`: organization SAML SSO blocks the request.
    SsoRequired,

    /// An organization IP allow list rejected the caller's address.
    IpAllowList,

    /// Organization or enterprise policy forbids outside collaborators.
    OutsideCollaboratorsRestricted,

    /// Any other refusal.
    Other,
}

/// Why creating a token (or another request) may succeed if retried.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TransientKind {
    /// GitHub answered with a 5xx status.
    Server {
        /// The HTTP status.
        status: u16,
    },

    /// The transport proves the request was never sent.
    NotSent,

    /// The request may have reached GitHub but no complete response arrived.
    NoResponse,
}

/// Failure of a GitHub operation ([`Installation::access_token`],
/// [`Installation::get`], [`Installation::is_suspended`]) or of signing the
/// app's credential ([`App::credential`]). Setup problems are [`ConfigError`]
/// instead.
///
/// Statuses are classified for the `/app/installations/{id}` endpoints, e.g. a
/// 404 is [`InstallationGone`](Self::InstallationGone). `Clone` so later layers
/// can share one result among coalesced waiters.
///
/// [`Installation::access_token`]: crate::Installation::access_token
/// [`Installation::get`]: crate::Installation::get
/// [`Installation::is_suspended`]: crate::Installation::is_suspended
/// [`App::credential`]: crate::App::credential
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The app's credentials could not be signed locally; nothing was sent.
    #[error("gh:signing")]
    Signing(SigningError),

    /// 401: GitHub rejected the app's credentials: a wrong or deleted key, a
    /// wrong App ID or client ID, or a host clock too far off to correct.
    /// Clock problems GitHub's `Date` reveals are corrected by one retry before
    /// this is returned. Retrying with another key is the caller's decision.
    #[error("gh:app_credentials_rejected")]
    AppCredentialsRejected {
        /// GitHub's clock minus ours, in seconds, from the `Date` of the last
        /// 401, when they differ by more than 30 s. Beyond ±3600 s the clock
        /// cannot be corrected and must be fixed; within it, the credential
        /// was already corrected for it, so the key or app identity is the
        /// likelier cause (the clock is still worth fixing).
        clock_skew: Option<i64>,
    },

    /// 404: the installation does not exist or the app cannot see it.
    #[error("gh:installation_gone")]
    InstallationGone,

    /// 403, and `GET /app/installations/{id}` confirmed the installation is suspended.
    #[error("gh:installation_suspended")]
    InstallationSuspended,

    /// Any other 403.
    #[error("gh:forbidden")]
    Forbidden {
        /// Bounded refusal classification.
        reason: ForbiddenReason,
    },

    /// 422: the requested permissions exceed what the installation granted.
    #[error("gh:permissions_not_granted")]
    PermissionsNotGranted,

    /// GitHub throttled the request instead of answering it.
    #[error("{}", self.code())]
    RateLimited {
        /// The limit GitHub cited.
        scope: RateLimitScope,

        /// GitHub's requested wait, when it supplied one. Absence is not permission to retry now.
        retry_after: Option<Duration>,
    },

    /// A server or transport failure that may succeed on retry.
    #[error("gh:transient")]
    Transient {
        /// What failed.
        kind: TransientKind,
    },

    /// An HTTP status the endpoint does not document. With the default client
    /// this includes 3xx: redirects are never followed.
    #[error("gh:unexpected:{status}")]
    Unexpected {
        /// The HTTP status.
        status: u16,
    },

    /// The response was oversize or did not have the documented shape.
    #[error("gh:decode")]
    Decode,

    /// A caller-supplied client followed a redirect, so the request was re-sent
    /// to another URL. The response is discarded. Configure custom clients with
    /// `reqwest::redirect::Policy::none()`.
    #[error("gh:redirected")]
    Redirected,
}

impl Error {
    /// Stable code for metrics and journals.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Signing(_) => "gh:signing",
            Self::AppCredentialsRejected { .. } => "gh:app_credentials_rejected",
            Self::InstallationGone => "gh:installation_gone",
            Self::InstallationSuspended => "gh:installation_suspended",
            Self::Forbidden { .. } => "gh:forbidden",
            Self::PermissionsNotGranted => "gh:permissions_not_granted",
            Self::RateLimited {
                scope: RateLimitScope::Primary,
                ..
            } => "gh:rate_limited:primary",
            Self::RateLimited {
                scope: RateLimitScope::Secondary,
                ..
            } => "gh:rate_limited:secondary",
            Self::Transient { .. } => "gh:transient",
            Self::Unexpected { .. } => "gh:unexpected",
            Self::Decode => "gh:decode",
            Self::Redirected => "gh:redirected",
        }
    }

    /// Whether the same request may succeed later without any configuration change.
    pub const fn is_transient(&self) -> bool {
        matches!(self, Self::Transient { .. } | Self::RateLimited { .. })
    }

    /// GitHub's requested wait for a rate limit, or 60 s when it named none.
    /// Guidance only: this crate never sleeps.
    pub fn suggested_wait(&self) -> Option<Duration> {
        match self {
            Self::RateLimited { retry_after, .. } => {
                Some(retry_after.unwrap_or(Duration::from_secs(60)))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_the_code() {
        let all = [
            Error::Signing(SigningError::Failed),
            Error::AppCredentialsRejected { clock_skew: None },
            Error::InstallationGone,
            Error::InstallationSuspended,
            Error::Forbidden {
                reason: ForbiddenReason::Other,
            },
            Error::PermissionsNotGranted,
            Error::RateLimited {
                scope: RateLimitScope::Primary,
                retry_after: None,
            },
            Error::RateLimited {
                scope: RateLimitScope::Secondary,
                retry_after: Some(Duration::from_secs(3)),
            },
            Error::Transient {
                kind: TransientKind::NoResponse,
            },
            Error::Decode,
            Error::Redirected,
        ];
        for error in all {
            assert_eq!(error.to_string(), error.code());
        }
        assert_eq!(
            Error::Unexpected { status: 307 }.to_string(),
            "gh:unexpected:307"
        );
    }

    #[test]
    fn retry_guidance() {
        let limited = Error::RateLimited {
            scope: RateLimitScope::Secondary,
            retry_after: None,
        };
        assert!(limited.is_transient());
        assert_eq!(limited.suggested_wait(), Some(Duration::from_secs(60)));
        assert!(!Error::InstallationGone.is_transient());
        assert_eq!(Error::InstallationGone.suggested_wait(), None);
    }
}
