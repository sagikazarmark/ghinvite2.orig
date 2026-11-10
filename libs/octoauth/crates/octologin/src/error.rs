//! Fixed-code, secret-free errors.

use octoauth::{ApiError, TransientKind};

/// An `error` code GitHub's OAuth web flow documents, on the callback
/// (`?error=access_denied`) or in a token response. Anything else is
/// [`Unrecognized`](Self::Unrecognized) and its text is discarded.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum OAuthErrorCode {
    /// The user declined the authorization.
    AccessDenied,
    /// The app is suspended.
    ApplicationSuspended,
    /// The code is wrong, expired or already used.
    BadVerificationCode,
    /// The client ID or secret is wrong.
    IncorrectClientCredentials,
    /// The redirect URI does not match the app's callback URL.
    RedirectUriMismatch,
    /// The authorization request was malformed.
    UnsupportedResponseType,
    /// The user's primary email is not verified.
    UnverifiedUserEmail,
    /// Any other code.
    Unrecognized,
}

impl OAuthErrorCode {
    /// Classify a code from GitHub, e.g. the callback's `error` parameter.
    pub fn from_github(code: &str) -> Self {
        match code {
            "access_denied" => Self::AccessDenied,
            "application_suspended" => Self::ApplicationSuspended,
            "bad_verification_code" => Self::BadVerificationCode,
            "incorrect_client_credentials" => Self::IncorrectClientCredentials,
            "redirect_uri_mismatch" => Self::RedirectUriMismatch,
            "unsupported_response_type" => Self::UnsupportedResponseType,
            "unverified_user_email" => Self::UnverifiedUserEmail,
            _ => Self::Unrecognized,
        }
    }

    /// GitHub's code, or `unrecognized`.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::AccessDenied => "access_denied",
            Self::ApplicationSuspended => "application_suspended",
            Self::BadVerificationCode => "bad_verification_code",
            Self::IncorrectClientCredentials => "incorrect_client_credentials",
            Self::RedirectUriMismatch => "redirect_uri_mismatch",
            Self::UnsupportedResponseType => "unsupported_response_type",
            Self::UnverifiedUserEmail => "unverified_user_email",
            Self::Unrecognized => "unrecognized",
        }
    }
}

/// Why a sign-in step failed. Carries fixed codes and statuses only, never
/// a token, a code, a secret or upstream response text.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum SignInError {
    /// The callback's `state` does not match the pending authorization.
    #[error("gh:signin:state_mismatch")]
    StateMismatch,

    /// GitHub's OAuth endpoint answered with an `error`.
    #[error("gh:signin:provider")]
    Provider {
        /// The documented code, or `Unrecognized`.
        code: OAuthErrorCode,
    },

    /// A caller-supplied value is malformed (an empty or oversized code, a
    /// PKCE verifier outside RFC 7636's grammar). Nothing was sent.
    #[error("gh:signin:invalid_input")]
    InvalidInput,

    /// The token belongs to another app (another client ID) than this
    /// [`SignIn`](crate::SignIn). Nothing was sent.
    #[error("gh:signin:foreign_token")]
    ForeignToken,

    /// GitHub answered with an error status.
    #[error(transparent)]
    Api(ApiError),

    /// No complete answer: the request may or may not have reached GitHub.
    #[error("gh:signin:transient")]
    Transient {
        /// What failed.
        kind: TransientKind,
    },

    /// The response was oversize or did not have the documented shape.
    #[error("gh:signin:decode")]
    Decode,

    /// The client followed a redirect, so the request was re-sent to another
    /// URL (on Workers, `fetch` always follows). The response is discarded.
    #[error("gh:signin:redirected")]
    Redirected,

    /// GitHub returned more than one page (100) of email addresses; a partial
    /// list is never returned.
    #[error("gh:signin:incomplete")]
    Incomplete,

    /// The platform's random number generator failed.
    #[error("gh:signin:randomness")]
    Randomness,
}

impl SignInError {
    /// Stable code for metrics and logs.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::StateMismatch => "gh:signin:state_mismatch",
            Self::Provider { .. } => "gh:signin:provider",
            Self::InvalidInput => "gh:signin:invalid_input",
            Self::ForeignToken => "gh:signin:foreign_token",
            Self::Api(error) => error.code(),
            Self::Transient { .. } => "gh:signin:transient",
            Self::Decode => "gh:signin:decode",
            Self::Redirected => "gh:signin:redirected",
            Self::Incomplete => "gh:signin:incomplete",
            Self::Randomness => "gh:signin:randomness",
        }
    }
}
