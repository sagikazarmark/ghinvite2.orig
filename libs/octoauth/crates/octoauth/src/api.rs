//! [`ApiError`]: a GitHub error response, classified from its parts.
//!
//! Works with any HTTP client: pass the status, the headers and the body as
//! the client has it, raw bytes or already parsed. The result carries fixed
//! codes, statuses and durations only, never GitHub's message or any other
//! response text, so it is safe to log and to journal.

use crate::error::{ForbiddenReason, RateLimitScope};
use http::{HeaderMap, StatusCode};
use serde_json::Value;
use std::fmt;
use std::time::Duration;

/// Larger error bodies (and messages) are not inspected: they name no reason.
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

/// At most this many `errors` entries are inspected.
const MAX_ERRORS: usize = 32;

/// Messages longer than this never match a refusal template.
const MAX_TEMPLATE_MESSAGE_BYTES: usize = 1024;

/// The body of a GitHub error response, in the form the client has it.
///
/// `Debug` prints only sizes: the body is upstream text.
#[derive(Clone, Copy)]
#[non_exhaustive]
pub enum ErrorBody<'a> {
    /// The raw body, e.g. from `reqwest::Response::bytes`. Bodies over 64 KiB
    /// are not inspected.
    Bytes(&'a [u8]),

    /// The body's `message` and `errors` fields, already parsed, e.g. from
    /// octocrab's `GitHubError`. Pass `None` for a message the client could
    /// not read from JSON.
    Parsed {
        /// The `message` field.
        message: Option<&'a str>,
        /// The `errors` array, or an empty slice.
        errors: &'a [Value],
    },

    /// No body, or one the client could not read.
    Empty,
}

impl fmt::Debug for ErrorBody<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bytes(bytes) => write!(f, "Bytes({} bytes)", bytes.len()),
            Self::Parsed { message, errors } => f
                .debug_struct("Parsed")
                .field("message_len", &message.map(str::len))
                .field("errors", &errors.len())
                .finish(),
            Self::Empty => f.write_str("Empty"),
        }
    }
}

/// A validation failure GitHub reported on a 422. Codes GitHub documents
/// for `errors[].code`, plus refusals recognised by their full message
/// template; anything else is [`Unrecognized`](Self::Unrecognized) and its
/// text is discarded.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum ValidationCode {
    /// `missing`: a resource does not exist.
    Missing,
    /// `missing_field`: a required field was not set.
    MissingField,
    /// `invalid`: a field's value is malformed.
    Invalid,
    /// `already_exists`: another resource has the same unique value.
    AlreadyExists,
    /// `unprocessable`: the inputs were invalid.
    Unprocessable,
    /// `custom`: a rule GitHub explains only in its message (discarded).
    Custom,
    /// The collaborator role is below the organization's base role
    /// (`Cannot assign {member} permission of {role}`).
    CannotAssignBelowBase,
    /// Organization or enterprise policy forbids outside collaborators.
    OutsideCollaboratorsRestricted,
    /// Any other code.
    Unrecognized,
}

impl ValidationCode {
    /// GitHub's code (`already_exists`, …), or this crate's name for a
    /// template match (`cannot_assign_below_base`, …).
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::MissingField => "missing_field",
            Self::Invalid => "invalid",
            Self::AlreadyExists => "already_exists",
            Self::Unprocessable => "unprocessable",
            Self::Custom => "custom",
            Self::CannotAssignBelowBase => "cannot_assign_below_base",
            Self::OutsideCollaboratorsRestricted => "outside_collaborators_restricted",
            Self::Unrecognized => "unrecognized",
        }
    }

    fn from_github(code: &str) -> Self {
        match code {
            "missing" => Self::Missing,
            "missing_field" => Self::MissingField,
            "invalid" => Self::Invalid,
            "already_exists" => Self::AlreadyExists,
            "unprocessable" => Self::Unprocessable,
            "custom" => Self::Custom,
            _ => Self::Unrecognized,
        }
    }
}

/// A GitHub error response from any client, classified without its text.
///
/// Built with [`classify`](Self::classify). Every category needs positive
/// evidence: a 403 is a rate limit only with GitHub's rate-limit signals, an
/// IP allow list or outside-collaborator refusal only with GitHub's full
/// message, spam only with GitHub's explicit `spammed`.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ApiError {
    /// 401: the credential was not accepted (wrong, revoked or expired). For
    /// an installation token, get a newer one and retry once.
    #[error("gh:api:unauthorized")]
    Unauthorized,

    /// 403 without rate-limit evidence.
    #[error("gh:api:forbidden")]
    Forbidden {
        /// Bounded refusal classification.
        reason: ForbiddenReason,
    },

    /// 404: the resource does not exist, or the credential cannot see it.
    #[error("gh:api:not_found")]
    NotFound,

    /// 409: the request conflicts with the resource's current state.
    #[error("gh:api:conflict")]
    Conflict,

    /// 422: GitHub rejected the submitted values.
    #[error("gh:api:validation")]
    Validation {
        /// Recognised codes, deduplicated: template matches first, then
        /// GitHub's codes in its order, then at most one
        /// [`Unrecognized`](ValidationCode::Unrecognized). Empty when GitHub
        /// named none.
        codes: Vec<ValidationCode>,
    },

    /// 403 or 422 with GitHub's explicit `spammed`: abuse protection refused
    /// the operation, e.g. too many invitations.
    #[error("gh:api:spammed")]
    Spammed,

    /// GitHub throttled the request instead of answering it.
    #[error("{}", self.code())]
    RateLimited {
        /// The limit GitHub cited.
        scope: RateLimitScope,

        /// GitHub's requested wait, when it supplied one, measured on
        /// GitHub's clock. Absence is not permission to retry now.
        retry_after: Option<Duration>,
    },

    /// 5xx.
    #[error("gh:api:server:{status}")]
    Server {
        /// The HTTP status.
        status: u16,
    },

    /// Any other status, including 3xx and success statuses the caller did
    /// not expect.
    #[error("gh:api:status:{status}")]
    Status {
        /// The HTTP status.
        status: u16,
    },
}

impl ApiError {
    /// Classify an error response: rate-limit evidence first, then the status.
    ///
    /// Call it for any response that is not the success the request expected.
    /// Only the status, a few headers (`retry-after`, `x-ratelimit-*`,
    /// `date`, `x-github-sso`), and the body's `message` and `errors` are
    /// read; at most 64 KiB of body and 32 `errors` entries.
    pub fn classify(status: StatusCode, headers: &HeaderMap, body: ErrorBody<'_>) -> Self {
        let parsed;
        let view = match body {
            ErrorBody::Bytes(bytes) => {
                parsed = (bytes.len() <= MAX_ERROR_BODY_BYTES)
                    .then(|| serde_json::from_slice::<Value>(bytes).ok())
                    .flatten();
                parsed.as_ref().map(ErrorFields::from_json)
            }
            ErrorBody::Parsed { message, errors } => Some(ErrorFields {
                message: message.filter(|m| m.len() <= MAX_ERROR_BODY_BYTES),
                errors: &errors[..errors.len().min(MAX_ERRORS)],
            }),
            ErrorBody::Empty => None,
        };
        let status = status.as_u16();
        if let Some((scope, retry_after)) = rate_limit(status, headers, view.as_ref()) {
            return Self::RateLimited { scope, retry_after };
        }
        let spammed = view.as_ref().is_some_and(ErrorFields::spammed);
        match status {
            401 => Self::Unauthorized,
            403 | 422 if spammed => Self::Spammed,
            403 => Self::Forbidden {
                reason: forbidden_reason(headers, view.as_ref()),
            },
            404 => Self::NotFound,
            409 => Self::Conflict,
            422 => Self::Validation {
                codes: view
                    .as_ref()
                    .map(ErrorFields::validation_codes)
                    .unwrap_or_default(),
            },
            500..=599 => Self::Server { status },
            _ => Self::Status { status },
        }
    }

    /// Stable code for metrics and journals.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized => "gh:api:unauthorized",
            Self::Forbidden { .. } => "gh:api:forbidden",
            Self::NotFound => "gh:api:not_found",
            Self::Conflict => "gh:api:conflict",
            Self::Validation { .. } => "gh:api:validation",
            Self::Spammed => "gh:api:spammed",
            Self::RateLimited {
                scope: RateLimitScope::Primary,
                ..
            } => "gh:api:rate_limited:primary",
            Self::RateLimited {
                scope: RateLimitScope::Secondary,
                ..
            } => "gh:api:rate_limited:secondary",
            Self::Server { .. } => "gh:api:server",
            Self::Status { .. } => "gh:api:status",
        }
    }

    /// Whether this is a 401, the one error a newer installation token can fix.
    pub const fn is_unauthorized(&self) -> bool {
        matches!(self, Self::Unauthorized)
    }

    /// Whether the same request may succeed later without any change: a rate
    /// limit or a server error. Whether retrying is *safe* depends on the
    /// request, not on this classification.
    pub const fn is_transient(&self) -> bool {
        matches!(self, Self::RateLimited { .. } | Self::Server { .. })
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

/// The fields of an error body that classification reads.
struct ErrorFields<'a> {
    message: Option<&'a str>,
    errors: &'a [Value],
}

impl<'a> ErrorFields<'a> {
    fn from_json(body: &'a Value) -> Self {
        Self {
            message: body.get("message").and_then(Value::as_str),
            errors: body
                .get("errors")
                .and_then(Value::as_array)
                .map_or(&[][..], |errors| &errors[..errors.len().min(MAX_ERRORS)]),
        }
    }

    /// `message` and every `errors[].message` short enough to match a template.
    fn messages(&self) -> impl Iterator<Item = &'a str> + '_ {
        self.message
            .into_iter()
            .chain(
                self.errors
                    .iter()
                    .filter_map(|error| error.get("message")?.as_str()),
            )
            .filter(|message| message.len() <= MAX_TEMPLATE_MESSAGE_BYTES)
    }

    fn codes(&self) -> impl Iterator<Item = &'a str> + '_ {
        self.errors
            .iter()
            .filter_map(|error| error.get("code")?.as_str())
    }

    /// GitHub's explicit `spammed`, as the message or an error code. Prose
    /// that merely mentions spam is not evidence.
    fn spammed(&self) -> bool {
        let spammed = |value: &str| value.eq_ignore_ascii_case("spammed");
        self.message.is_some_and(spammed) || self.codes().any(spammed)
    }

    fn validation_codes(&self) -> Vec<ValidationCode> {
        let mut codes = Vec::new();
        if self.messages().any(outside_collaborators_restricted) {
            codes.push(ValidationCode::OutsideCollaboratorsRestricted);
        }
        if self.messages().any(cannot_assign_below_base) {
            codes.push(ValidationCode::CannotAssignBelowBase);
        }
        for code in self.codes().map(ValidationCode::from_github) {
            if !codes.contains(&code) {
                codes.push(code);
            }
        }
        // Recognised codes first; the order is otherwise GitHub's.
        codes.sort_by_key(|code| *code == ValidationCode::Unrecognized);
        codes
    }
}

/// Throttling evidence on a 403/429, or `None` if GitHub answered the request.
///
/// GitHub names a limit three ways: `retry-after` (secondary), rate-limit
/// wording in the message (either), and `x-ratelimit-remaining: 0` (primary).
/// The quota headers ride along on every response, so an exhausted quota only
/// counts when the body names no other reason; a 403 without evidence is a
/// refusal. A 429 is always a secondary limit, and a `retry-after` cites a
/// limit even when its value is unreadable.
fn rate_limit(
    status: u16,
    headers: &HeaderMap,
    view: Option<&ErrorFields<'_>>,
) -> Option<(RateLimitScope, Option<Duration>)> {
    if status != 403 && status != 429 {
        return None;
    }
    let cited = headers.contains_key("retry-after") || status == 429;
    let quota_exhausted =
        header(headers, "x-ratelimit-remaining").and_then(|v| v.parse::<u64>().ok()) == Some(0);
    let named_limit = view.and_then(|view| view.message).map(names_rate_limit);
    if !(cited || named_limit == Some(true) || (quota_exhausted && named_limit.is_none())) {
        return None;
    }
    // The scope names the limit GitHub cited. The wait follows the evidence:
    // `retry-after` names this request's wait; otherwise only an exhausted
    // quota makes its reset this request's wait.
    let scope = if cited || !quota_exhausted {
        RateLimitScope::Secondary
    } else {
        RateLimitScope::Primary
    };
    let wait = retry_after(headers).or_else(|| {
        quota_exhausted
            .then(|| {
                after_response_date(headers, header(headers, "x-ratelimit-reset")?.parse().ok()?)
            })
            .flatten()
    });
    Some((scope, wait))
}

fn names_rate_limit(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("rate limit") || message.contains("abuse detection")
}

/// `retry-after` in seconds, or as an HTTP date measured against the
/// response's own `date`, so a skewed local clock cannot shorten it.
fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let value = header(headers, "retry-after")?;
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    after_response_date(
        headers,
        chrono::DateTime::parse_from_rfc2822(value)
            .ok()?
            .timestamp(),
    )
}

/// Seconds from the response's `date` to `instant`, if positive.
fn after_response_date(headers: &HeaderMap, instant: i64) -> Option<Duration> {
    u64::try_from(instant.checked_sub(response_date(headers)?)?)
        .ok()
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
}

/// The response's `date` header, as Unix seconds.
pub(crate) fn response_date(headers: &HeaderMap) -> Option<i64> {
    chrono::DateTime::parse_from_rfc2822(header(headers, "date")?)
        .ok()
        .map(|date| date.timestamp())
}

fn header<'h>(headers: &'h HeaderMap, name: &str) -> Option<&'h str> {
    Some(headers.get(name)?.to_str().ok()?.trim())
}

fn forbidden_reason(headers: &HeaderMap, view: Option<&ErrorFields<'_>>) -> ForbiddenReason {
    if headers.contains_key("x-github-sso") {
        return ForbiddenReason::SsoRequired;
    }
    let Some(view) = view else {
        return ForbiddenReason::Other;
    };
    if view.messages().any(outside_collaborators_restricted) {
        ForbiddenReason::OutsideCollaboratorsRestricted
    } else if view.messages().any(ip_allow_list_denial) {
        ForbiddenReason::IpAllowList
    } else {
        ForbiddenReason::Other
    }
}

/// One of the complete outside-collaborator denials, never a keyword match.
/// GitHub documents the policy; the exact wording is not verified live.
fn outside_collaborators_restricted(message: &str) -> bool {
    let message = message.strip_suffix('.').unwrap_or(message);
    [
        "Outside collaborators are not allowed in this organization",
        "You are not allowed to invite outside collaborators to this organization",
        "Inviting outside collaborators is restricted by organization policy",
        "Inviting outside collaborators is restricted by enterprise policy",
    ]
    .iter()
    .any(|denial| message.eq_ignore_ascii_case(denial))
}

/// GitHub's full IP allow-list denial text (github/github-mcp-server#748). The
/// organization name is validated and discarded.
fn ip_allow_list_denial(message: &str) -> bool {
    message
        .strip_prefix("Although you appear to have the correct authorization credentials, the `")
        .and_then(|tail| {
            tail.strip_suffix(
                "` organization has an IP allow list enabled, and your IP address is not permitted to access this resource.",
            )
        })
        .is_some_and(|org| {
            !org.is_empty()
                && org.len() <= 39
                && org.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

/// GitHub's documented collaborator rejection, `Cannot assign {member}
/// permission of {role}`. The member and role are discarded.
fn cannot_assign_below_base(message: &str) -> bool {
    message
        .strip_prefix("Cannot assign ")
        .and_then(|tail| tail.split_once(" permission of "))
        .is_some_and(|(member, role)| !member.is_empty() && !role.is_empty())
}

#[cfg(test)]
mod tests;
