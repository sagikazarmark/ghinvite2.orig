//! Secret-free classification of GitHub failures.
//!
//! [`ApiError`] keeps fixed codes, statuses and durations, never response
//! text, so it can be logged and journaled. Its rate-limit, 403 and 422
//! evidence rules are octoauth's ([`octoauth::ApiError::classify`]); this
//! crate adds what only a REST client sees: transport certainty
//! ([`ApiError::NotSent`] vs [`ApiError::Ambiguous`]), redirects, 405/410,
//! SSO-partial listings and response-shape failures.

use crate::ids::OrgId;
use crate::transport::{Response, TransportError};
use std::time::Duration;

pub use octoauth::RateLimitScope;

/// Larger error bodies are not inspected (octoauth's bound).
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

/// Bounded reason for a forbidden request, including local refusals.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ForbiddenReason {
    /// The credential lacks the required permission or access to the resource.
    Permission,
    /// Organization policy forbids outside collaborators.
    OutsideCollaboratorsRestricted,
    /// The credential requires organization SSO authorization.
    SsoRequired,
    /// An IP allow list rejected the request.
    IpAllowList,
    /// The endpoint rejected this credential class before dispatch.
    WrongCredential,
    /// The requested capability is disabled in this crate.
    Disabled,
    /// A refusal without a more specific classification.
    Other,
}

/// Why an otherwise successful observation is incomplete.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum PartialReason {
    /// Some organizations are hidden until the credential satisfies SSO.
    SsoRequired,
}

/// Known GitHub rate-limit resource; unknown text is discarded.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[non_exhaustive]
pub enum Resource {
    /// The ordinary REST quota.
    Core,
    /// The search quota.
    Search,
    /// The GraphQL quota.
    Graphql,
    /// The app-manifest quota.
    IntegrationManifest,
    /// Any other resource; the name is not retained.
    Other,
}

/// Documented validation codes; no upstream string is retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
    /// Documented collaborator rejection below an organization's base role.
    CannotAssignBelowBase,
    /// Organization policy refuses the outside-collaborator operation.
    OutsideCollaboratorsRestricted,
    /// Any other code; its text is discarded.
    Other,
}

/// Secret-free API failure.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ApiError {
    /// 401: the credential was not accepted.
    Unauthorized,
    /// The request was refused by a permission, policy or credential boundary.
    Forbidden {
        /// Bounded refusal classification.
        reason: ForbiddenReason,
    },
    /// 404: not found, or not visible to this credential.
    NotFound,
    /// 409: conflicts with the resource's current state.
    Conflict,
    /// 405: the method is not allowed.
    NotAllowed,
    /// 410: the resource is gone.
    Gone,
    /// A redirect; credentials are never forwarded automatically.
    Moved {
        /// The redirect status; the location is not retained.
        status: u16,
    },
    /// 422: GitHub rejected the submitted values.
    Validation {
        /// Recognised codes; unrecognised text is discarded.
        codes: Vec<ValidationCode>,
        /// Length of GitHub's message, without its contents.
        message_len: usize,
    },
    /// GitHub's explicit `spammed` (abuse protection).
    Spammed,
    /// GitHub throttled the request instead of answering it.
    RateLimited {
        /// The limit GitHub cited.
        scope: RateLimitScope,
        /// GitHub's requested wait, when it named one.
        retry_after: Option<Duration>,
        /// The quota resource GitHub named, if any.
        resource: Option<Resource>,
    },
    /// A successful response that is explicitly incomplete.
    Partial {
        /// Why the listing cannot be treated as exhaustive.
        reason: PartialReason,
        /// Bounded numeric organizations requiring SSO; no URLs retained.
        org_ids: Vec<OrgId>,
    },
    /// 5xx.
    Server {
        /// The HTTP status.
        status: u16,
    },
    /// The request may have reached GitHub; unsafe effects need reconciliation.
    Ambiguous,
    /// The transport proves no request was dispatched.
    NotSent,
    /// The response (or the caller's input) failed the expected shape.
    Decode {
        /// Fixed label naming the decode boundary, never upstream text.
        what: &'static str,
    },
    /// Any other status.
    Status {
        /// The HTTP status.
        status: u16,
    },
}

impl ApiError {
    /// Stable code without response diagnostics.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized => "gh:api:unauthorized",
            Self::Forbidden { .. } => "gh:api:forbidden",
            Self::NotFound => "gh:api:not_found",
            Self::Conflict => "gh:api:conflict",
            Self::NotAllowed => "gh:api:not_allowed",
            Self::Gone => "gh:api:gone",
            Self::Moved { .. } => "gh:api:moved",
            Self::Validation { .. } => "gh:api:validation",
            Self::Spammed => "gh:api:spammed",
            Self::RateLimited {
                scope: RateLimitScope::Primary,
                ..
            } => "gh:api:rate_limited:primary",
            Self::RateLimited { .. } => "gh:api:rate_limited:secondary",
            Self::Partial { .. } => "gh:api:partial",
            Self::Server { .. } => "gh:api:server",
            Self::Ambiguous => "gh:api:ambiguous",
            Self::NotSent => "gh:api:not_sent",
            Self::Decode { .. } => "gh:api:decode",
            Self::Status { .. } => "gh:api:status",
        }
    }

    /// Whether a caller may retry, given its endpoint's duplicate-effect safety.
    pub fn is_transient_for_retry_safe(&self, retry_safe: bool) -> bool {
        matches!(
            self,
            Self::Server { .. } | Self::NotSent | Self::RateLimited { .. }
        ) || (retry_safe && matches!(self, Self::Ambiguous))
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

    /// Classify a response that is not the success the caller expected.
    ///
    /// Rate limits, 403 reasons, spam and 422 codes come from
    /// [`octoauth::ApiError::classify`]; redirects, 405 and 410 are this
    /// crate's own, as are the 422 message length and the rate-limit resource.
    pub(crate) fn from_response(response: &Response) -> Self {
        let Ok(status) = http::StatusCode::from_u16(response.status) else {
            return Self::Status {
                status: response.status,
            };
        };
        let mut headers = http::HeaderMap::new();
        for (name, value) in &response.headers {
            if let (Ok(name), Ok(value)) = (
                http::HeaderName::from_bytes(name.as_bytes()),
                http::HeaderValue::from_str(value),
            ) {
                headers.insert(name, value);
            }
        }
        let classified = octoauth::ApiError::classify(
            status,
            &headers,
            octoauth::ErrorBody::Bytes(&response.body),
        );
        match classified {
            octoauth::ApiError::RateLimited { scope, retry_after } => Self::RateLimited {
                scope,
                retry_after,
                resource: response
                    .headers
                    .get("x-ratelimit-resource")
                    .map(|r| match r.as_str() {
                        "core" => Resource::Core,
                        "search" => Resource::Search,
                        "graphql" => Resource::Graphql,
                        "integration_manifest" => Resource::IntegrationManifest,
                        _ => Resource::Other,
                    }),
            },
            octoauth::ApiError::Unauthorized => Self::Unauthorized,
            octoauth::ApiError::Forbidden { reason } => Self::Forbidden {
                reason: match reason {
                    octoauth::ForbiddenReason::SsoRequired => ForbiddenReason::SsoRequired,
                    octoauth::ForbiddenReason::IpAllowList => ForbiddenReason::IpAllowList,
                    octoauth::ForbiddenReason::OutsideCollaboratorsRestricted => {
                        ForbiddenReason::OutsideCollaboratorsRestricted
                    }
                    _ => ForbiddenReason::Other,
                },
            },
            octoauth::ApiError::NotFound => Self::NotFound,
            octoauth::ApiError::Conflict => Self::Conflict,
            octoauth::ApiError::Validation { codes } => {
                let mut mapped = Vec::with_capacity(codes.len());
                for code in codes {
                    let code = map_validation_code(code);
                    if !mapped.contains(&code) {
                        mapped.push(code);
                    }
                }
                Self::Validation {
                    codes: mapped,
                    message_len: message_len(&response.body),
                }
            }
            octoauth::ApiError::Spammed => Self::Spammed,
            octoauth::ApiError::Server { status } => Self::Server { status },
            // octoauth's `Status`: the statuses only a REST client tells apart.
            _ => match response.status {
                405 => Self::NotAllowed,
                410 => Self::Gone,
                300..=399 => Self::Moved {
                    status: response.status,
                },
                status => Self::Status { status },
            },
        }
    }
}

fn map_validation_code(code: octoauth::ValidationCode) -> ValidationCode {
    match code {
        octoauth::ValidationCode::Missing => ValidationCode::Missing,
        octoauth::ValidationCode::MissingField => ValidationCode::MissingField,
        octoauth::ValidationCode::Invalid => ValidationCode::Invalid,
        octoauth::ValidationCode::AlreadyExists => ValidationCode::AlreadyExists,
        octoauth::ValidationCode::Unprocessable => ValidationCode::Unprocessable,
        octoauth::ValidationCode::Custom => ValidationCode::Custom,
        octoauth::ValidationCode::CannotAssignBelowBase => ValidationCode::CannotAssignBelowBase,
        octoauth::ValidationCode::OutsideCollaboratorsRestricted => {
            ValidationCode::OutsideCollaboratorsRestricted
        }
        _ => ValidationCode::Other,
    }
}

/// Length of a bounded JSON body's `message`, or 0.
fn message_len(body: &[u8]) -> usize {
    if body.len() > MAX_ERROR_BODY_BYTES {
        return 0;
    }
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|body| body.get("message")?.as_str().map(str::len))
        .unwrap_or(0)
}

impl From<TransportError> for ApiError {
    fn from(e: TransportError) -> Self {
        match e {
            TransportError::NotSent => Self::NotSent,
            TransportError::SentNoResponse { .. } => Self::Ambiguous,
            TransportError::Oversize => Self::Decode { what: "oversize" },
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.code())?;
        if let Self::Moved { status } | Self::Server { status } | Self::Status { status } = self {
            write!(f, ":{status}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ApiError {}

/// A closure's failure: a GitHub API error or the application's own.
#[derive(Clone, Debug)]
pub enum CallError<E = std::convert::Infallible> {
    /// A typed GitHub API failure.
    Api(ApiError),
    /// An application-defined failure; the application owns its hygiene.
    App(E),
}

impl<E> From<ApiError> for CallError<E> {
    fn from(e: ApiError) -> Self {
        Self::Api(e)
    }
}

impl<E> CallError<E> {
    /// Fixed code; never formats the application's payload.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Api(e) => e.code(),
            Self::App(_) => "gh:call:app",
        }
    }
}

impl<E> std::fmt::Display for CallError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Api(e) => e.fmt(f),
            Self::App(_) => f.write_str("gh:call:app"),
        }
    }
}

impl<E: std::fmt::Debug> std::error::Error for CallError<E> {}

#[cfg(test)]
mod tests;
