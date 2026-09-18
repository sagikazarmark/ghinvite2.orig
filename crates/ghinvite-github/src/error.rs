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
///
/// Every string in here is a diagnostic, not a payload. Error values are
/// formatted into logs and into Restate terminal errors long after the request
/// that produced them, so no variant may carry a raw upstream body or anything
/// derived from one that has not been through [`crate::redact`].
#[derive(Debug, Error)]
pub enum Error {
    /// The HTTP transport itself failed (DNS, TLS, broken pipe, etc).
    #[error("transport error: {0}")]
    Transport(String),

    /// The server returned a non-2xx status. Holds the numeric status and a
    /// bounded, sanitized summary of the body: the documented validation
    /// sub-codes when it was a GitHub error envelope, and its size and shape
    /// otherwise. No upstream text survives — see [`crate::redact`]. Build it
    /// with [`crate::Response::status_error`].
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
    ///
    /// `body` is the same sanitized summary [`Error::Status`] carries: a
    /// throttled response is still an upstream body. The rate-limit wording
    /// that produced this classification is read from the raw response before
    /// the summary is built, so nothing is lost by not keeping it.
    #[error("github throttled the request with status {status}: {body}")]
    RateLimited {
        status: u16,
        body: String,
        rate_limit: RateLimit,
    },

    /// The response body was not valid JSON or did not match the expected
    /// shape. Build it with [`Error::decode`] / [`Error::decode_shape`]: the
    /// body is described, never quoted.
    #[error("response decode error: {0}")]
    Decode(String),

    /// The OAuth authorization-code exchange returned an error from GitHub
    /// (`error=...`). Carries the bounded error code only; GitHub's
    /// `error_description` prose is upstream-controlled and is not kept.
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

    /// A stable, payload-free label for logs and metrics.
    ///
    /// Log this plus [`Error::status`] instead of the error itself: a
    /// `tracing` field holding the whole error drags its diagnostic string
    /// into every sink that ever sees the event.
    pub fn kind(&self) -> &'static str {
        match self {
            Error::Transport(_) => "transport",
            Error::Status { .. } => "status",
            Error::RateLimited { .. } => "rate_limited",
            Error::Decode(_) => "decode",
            Error::OAuth(_) => "oauth",
            Error::InvalidInput(_) => "invalid_input",
            Error::Jwt(_) => "jwt",
        }
    }

    /// A decode failure described by category and position.
    ///
    /// `serde_json`'s own message quotes the offending value — on a token
    /// endpoint that value is the credential — so only its classification
    /// survives.
    pub(crate) fn decode(error: &serde_json::Error, body_len: usize) -> Self {
        Error::Decode(crate::redact::decode_diagnostic(error, body_len))
    }

    /// A decode failure for a *response* that was already parsed once and then
    /// failed to match the expected shape. `context` names the call site and
    /// must be a literal — never anything derived from the response.
    ///
    /// Failures encoding a request belong in [`Error::InvalidInput`]: nothing
    /// has been received yet, so calling them a decode error misreads which
    /// side was at fault.
    pub(crate) fn decode_shape(context: &'static str) -> Self {
        Error::Decode(format!("{context}: unexpected response shape"))
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

    #[test]
    fn kind_labels_every_variant_without_its_payload() {
        let cases = [
            (Error::Transport("dns: host.example".into()), "transport"),
            (
                Error::Status {
                    status: 500,
                    body: "secret".into(),
                },
                "status",
            ),
            (throttled(429, None), "rate_limited"),
            (Error::Decode("secret".into()), "decode"),
            (Error::OAuth("secret".into()), "oauth"),
            (Error::InvalidInput("secret".into()), "invalid_input"),
            (Error::Jwt("secret".into()), "jwt"),
        ];
        for (error, expected) in cases {
            assert_eq!(error.kind(), expected);
            assert!(!error.kind().contains("secret"));
        }
    }

    #[test]
    fn decode_never_quotes_the_body() {
        let body = r#"{"access_token":"ghs_16C7e42F292c6912E7710c838347Ae178B4a"}"#;
        let failure = serde_json::from_str::<u64>(body).unwrap_err();
        let error = Error::decode(&failure, body.len());
        let rendered = format!("{error} {error:?}");
        assert!(!rendered.contains("ghs_"), "{rendered}");
        assert!(rendered.contains("shape error"), "{rendered}");
    }

    #[test]
    fn decode_shape_names_the_call_site_only() {
        let error = Error::decode_shape("oauth token response");
        assert_eq!(
            error.to_string(),
            "response decode error: oauth token response: unexpected response shape"
        );
    }
}
