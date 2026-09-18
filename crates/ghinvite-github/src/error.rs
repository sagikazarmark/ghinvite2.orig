use std::time::Duration;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

/// Which documented GitHub limit a throttled response reports.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RateLimitScope {
    /// The hourly request quota, reported as `x-ratelimit-remaining: 0` and
    /// cleared at `x-ratelimit-reset`.
    Primary,
    /// A secondary (burst) limit, reported as `retry-after`, a 429, or the
    /// documented rate-limit wording in the response body.
    Secondary,
}

/// Throttling evidence GitHub returned instead of deciding a request, and the
/// wait it asked for when it supplied one.
///
/// `retry_after` is guidance about when the limit clears, not a deadline: it is
/// absent whenever GitHub named a limit without saying how long it lasts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RateLimit {
    pub scope: RateLimitScope,
    pub retry_after: Option<Duration>,
}

/// All failure modes a GitHub-side call can produce. Callers in Plan 3 / Plan 4
/// will branch on these variants; in particular `Error::Status(404)` on a
/// `get_repo` is *not* fatal — it just means the App lost access.
#[derive(Debug, Error)]
pub enum Error {
    /// The HTTP transport itself failed (DNS, TLS, broken pipe, etc).
    #[error("transport error: {0}")]
    Transport(String),

    /// The server returned a non-2xx status. Holds the numeric status and the
    /// raw body (truncated to 4 KiB) for diagnostics.
    ///
    /// A 403 arriving as this variant carries no rate-limit evidence, so it is
    /// a genuine permission refusal rather than throttling; throttled responses
    /// are [`Error::RateLimited`] instead.
    #[error("github returned status {status}: {body}")]
    Status { status: u16, body: String },

    /// GitHub throttled the request rather than deciding it: a 403 or 429
    /// carrying documented rate-limit evidence. The request was refused
    /// outright, so nothing was applied and it may be retried once the limit
    /// named in `rate_limit` clears.
    #[error("github throttled the request with status {status}: {body}")]
    RateLimited {
        status: u16,
        body: String,
        rate_limit: RateLimit,
    },

    /// The response body was not valid JSON or did not match the expected shape.
    #[error("response decode error: {0}")]
    Decode(String),

    /// The OAuth authorization-code exchange returned an error from GitHub
    /// (`error=...`/`error_description=...`).
    #[error("oauth error: {0}")]
    OAuth(String),

    /// Something is wrong with the calling code's inputs (e.g. bad URL,
    /// unparseable PEM, expired JWT key), or a well-formed response this crate
    /// refuses to act on: an incomplete repository refresh, a pending-invitation
    /// listing whose pages could not be walked, an identity mismatch. Never
    /// thrown by the transport itself. Callers treat it as unknown, not as a
    /// terminal answer.
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// JWT minting / signing failed.
    #[error("jwt error: {0}")]
    Jwt(String),
}

impl Error {
    /// Convenience: extract the status code of any response-carrying variant,
    /// throttled responses included. Plan 3 handlers branch on specific codes
    /// (404 = lost access, 422 = already a member, etc.), so giving them this
    /// rather than `match`ing keeps call sites tight.
    pub fn status(&self) -> Option<u16> {
        match self {
            Error::Status { status, .. } | Error::RateLimited { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// The throttling evidence and retry guidance GitHub returned, if this is a
    /// throttled response. `None` for every other failure, a permission-denied
    /// 403 included, so callers can classify a refusal without re-reading
    /// headers.
    pub fn rate_limit(&self) -> Option<RateLimit> {
        match self {
            Error::RateLimited { rate_limit, .. } => Some(*rate_limit),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn throttled(status: u16, retry_after: Option<Duration>) -> Error {
        Error::RateLimited {
            status,
            body: "rate limit".into(),
            rate_limit: RateLimit {
                scope: RateLimitScope::Secondary,
                retry_after,
            },
        }
    }

    #[test]
    fn status_extracts_only_for_status_variant() {
        assert_eq!(
            Error::Status {
                status: 404,
                body: "x".into(),
            }
            .status(),
            Some(404)
        );
        assert_eq!(Error::Decode("nope".into()).status(), None);
    }

    #[test]
    fn status_covers_a_throttled_response() {
        assert_eq!(throttled(429, None).status(), Some(429));
    }

    #[test]
    fn rate_limit_separates_throttling_from_a_permission_refusal() {
        assert_eq!(
            throttled(403, Some(Duration::from_secs(45)))
                .rate_limit()
                .and_then(|limit| limit.retry_after),
            Some(Duration::from_secs(45))
        );
        assert_eq!(
            Error::Status {
                status: 403,
                body: "Resource not accessible by integration".into(),
            }
            .rate_limit(),
            None
        );
        assert_eq!(Error::Transport("dns".into()).rate_limit(), None);
    }

    #[test]
    fn errors_render_useful_messages() {
        let s = Error::Status {
            status: 401,
            body: "bad creds".into(),
        }
        .to_string();
        assert!(s.contains("401"));
        assert!(s.contains("bad creds"));
    }
}
