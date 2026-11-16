//! Token-free M0 sign-in orchestration: the only sign-in path.
//!
//! `/login` and `/oauth/callback` use durable single-use pending state and
//! rotate a token-free session after completion.
mod domains;
pub mod storage;
pub use domains::{CompletedSignIn, DomainContext, DomainRestriction};

use async_trait::async_trait;
use ghinvite_config::apps::SignInApp;
use ghinvite_core::clock::{Clock, SystemClock};
use octologin::{AuthorizeOptions, PendingAuthorization, SignIn};
pub use octologin::{AuthorizeUrl, OAuthErrorCode};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use zeroize::Zeroizing;

/// Pending OAuth material belongs in protected server-side storage, never in
/// the authorize state itself. Serialization is solely an adapter boundary.
/// `state` and `verifier` are octologin's `PendingAuthorization` fields,
/// stored one to one.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingSignIn {
    state: String,
    #[serde(with = "secret_string")]
    verifier: Zeroizing<String>,
    issued_at: i64,
}
impl std::fmt::Debug for PendingSignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PendingSignIn([REDACTED])")
    }
}
mod secret_string {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        value: &Zeroizing<String>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        s.serialize_str(value)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Zeroizing<String>, D::Error> {
        String::deserialize(d).map(Zeroizing::new)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StateUnavailable {
    #[error("sign-in state unavailable")]
    Unavailable,
    #[error("sign-in key generation not yet deployed")]
    KeyringRetry,
}

/// The key is an authenticated server-side session identifier, never callback
/// input. Implementations MUST encrypt pending material and atomically remove
/// it before returning from `take`, including across isolates. An uncertain
/// write/take is an error. A KV read followed by delete does not meet this contract.
#[async_trait]
pub trait PendingSignInStore: Send + Sync {
    async fn replace(
        &self,
        session_id: &str,
        pending: PendingSignIn,
    ) -> Result<(), StateUnavailable>;
    async fn take(&self, session_id: &str) -> Result<Option<PendingSignIn>, StateUnavailable>;
}

/// Browser-controlled callback data. No Debug/Display output includes parameters.
pub struct Callback {
    state: Zeroizing<String>,
    outcome: Result<Zeroizing<String>, OAuthErrorCode>,
}
impl Callback {
    pub fn code(state: String, code: String) -> Self {
        Self {
            state: Zeroizing::new(state),
            outcome: Ok(Zeroizing::new(code)),
        }
    }
    /// GitHub's `error` parameter, classified by
    /// [`OAuthErrorCode::from_github`]; its text, including unrecognized
    /// prose, is not kept.
    pub fn denied(state: String, error: &str) -> Self {
        Self {
            state: Zeroizing::new(state),
            outcome: Err(OAuthErrorCode::from_github(error)),
        }
    }
}
impl std::fmt::Debug for Callback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Callback([REDACTED])")
    }
}

/// The only successful callback result. No credential or authorization state
/// can be returned to the session adapter through this type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub github_user_id: u64,
    pub login: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SignInError {
    #[error("sign-in configuration unavailable")]
    Configuration,
    #[error("sign-in state unavailable")]
    StateUnavailable,
    #[error("sign-in key generation not yet deployed")]
    KeyringRetry,
    #[error("no pending sign-in")]
    NoPendingSignIn,
    #[error("invalid or expired sign-in state")]
    InvalidState,
    #[error("sign-in authorization declined")]
    Declined,
    /// The callback carried another documented (or an unrecognized) error.
    #[error("sign-in authorization failed")]
    Provider(OAuthErrorCode),
    #[error("sign-in exchange failed")]
    Exchange,
    #[error("sign-in identity unavailable")]
    IdentityUnavailable,
    #[error("sign-in domain verification unavailable")]
    DomainUnavailable,
}
impl From<StateUnavailable> for SignInError {
    fn from(error: StateUnavailable) -> Self {
        match error {
            StateUnavailable::Unavailable => Self::StateUnavailable,
            StateUnavailable::KeyringRetry => Self::KeyringRetry,
        }
    }
}

/// The `User-Agent` of sign-in requests.
const USER_AGENT: &str = "p1/signin";

pub struct SignInService {
    sign_in: SignIn,
    /// GitHub's avatar CDN, or the app's `avatar_base_url`.
    avatar_base: String,
    clock: Arc<dyn Clock>,
}
impl SignInService {
    /// Sign-in with the app's credentials, against GitHub unless the app
    /// names other base URLs (tests).
    pub fn new(app: &SignInApp) -> Result<Self, SignInError> {
        let mut builder = SignIn::builder(
            app.client_id.as_str(),
            app.client_secret.as_str(),
            app.redirect_uri.as_str(),
        )
        .user_agent(USER_AGENT);
        if let Some(url) = &app.web_base_url {
            builder = builder.web_base_url(url.as_str());
        }
        if let Some(url) = &app.api_base_url {
            builder = builder.api_base_url(url.as_str());
        }
        let avatar_base = match &app.avatar_base_url {
            Some(url) => ghinvite_github::rest::parse_base_url(url)
                .ok_or(SignInError::Configuration)?
                .to_string(),
            None => ghinvite_github::avatar::DEFAULT_AVATAR_BASE.to_owned(),
        };
        Ok(Self {
            sign_in: builder.build().map_err(|_| SignInError::Configuration)?,
            avatar_base,
            clock: Arc::new(SystemClock),
        })
    }

    /// Call only after the browser's five-starts/ten-minutes rate budget admits
    /// this request. Failure to persist pending state never returns a redirect.
    pub async fn begin(
        &self,
        store: &dyn PendingSignInStore,
        session_id: &str,
    ) -> Result<AuthorizeUrl, SignInError> {
        let (url, pending) = self
            .sign_in
            .authorize(AuthorizeOptions::default())
            .map_err(|_| SignInError::StateUnavailable)?;
        store
            .replace(
                session_id,
                PendingSignIn {
                    state: pending.state,
                    verifier: pending.pkce_verifier,
                    issued_at: self.clock.now(),
                },
            )
            .await?;
        Ok(url)
    }

    /// Every callback attempt consumes pending state before parsing provider
    /// outcome or performing I/O. Replays, including concurrent ones, cannot
    /// exchange a second token when the store meets its atomic take contract.
    pub async fn complete(
        &self,
        store: &dyn PendingSignInStore,
        session_id: &str,
        callback: Callback,
    ) -> Result<Identity, SignInError> {
        self.complete_scoped(store, session_id, callback, None)
            .await
            .map(|result| result.identity)
    }

    pub async fn complete_scoped(
        &self,
        store: &dyn PendingSignInStore,
        session_id: &str,
        callback: Callback,
        domains: Option<&dyn DomainContext>,
    ) -> Result<CompletedSignIn, SignInError> {
        let pending = store
            .take(session_id)
            .await?
            .ok_or(SignInError::NoPendingSignIn)?;
        // Match the anonymous session's fixed thirty-minute maximum. Future
        // timestamps fail closed; neither callback nor retries extend lifetime.
        if !self
            .clock
            .now()
            .checked_sub(pending.issued_at)
            .is_some_and(|age| (0..1800).contains(&age))
        {
            return Err(SignInError::InvalidState);
        }
        let verifier = PendingAuthorization {
            state: pending.state,
            pkce_verifier: pending.verifier,
        }
        .verify(&callback.state)
        .map_err(|_| SignInError::InvalidState)?;
        let code = match callback.outcome {
            Ok(code) => code,
            Err(OAuthErrorCode::AccessDenied) => return Err(SignInError::Declined),
            Err(code) => return Err(SignInError::Provider(code)),
        };
        let token = self
            .sign_in
            .exchange(&code, &verifier)
            .await
            .map_err(|error| {
                tracing::warn!(error_kind = error.code(), "sign-in exchange failed");
                SignInError::Exchange
            })?;
        let result = async {
            let user = self.sign_in.user(&token).await.map_err(|error| {
                tracing::warn!(error_kind = error.code(), "sign-in identity read failed");
                SignInError::IdentityUnavailable
            })?;
            if user.id.get() == 0
                || user.login.is_empty()
                || user.login.len() > 39
                || !user
                    .login
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            {
                return Err(SignInError::IdentityUnavailable);
            }
            let restriction = match domains {
                Some(context) => context.restriction(user.id.get()).await?,
                None => None,
            };
            let domain_evidence =
                if let Some(restriction) = restriction.filter(|r| !r.rules.is_empty()) {
                    // Verified addresses only; more than one page is `Incomplete`.
                    let emails = self.sign_in.emails(&token).await.map_err(|error| {
                        tracing::warn!(
                            error_kind = error.code(),
                            "sign-in email verification unavailable"
                        );
                        SignInError::DomainUnavailable
                    })?;
                    Some(domains::evidence(restriction, emails, self.clock.now())?)
                } else {
                    None
                };
            Ok(CompletedSignIn {
                avatar_url: user.avatar_url.filter(|url| !url.is_empty()).and_then(|_| {
                    ghinvite_github::avatar::canonical_url_at(
                        &self.avatar_base,
                        ghinvite_github::ids::UserId::new(user.id.get()),
                    )
                }),
                github_created_at: user
                    .created_at
                    .map(chrono::DateTime::<chrono::Utc>::from)
                    .filter(|at| at.timestamp() <= self.clock.now()),
                identity: Identity {
                    github_user_id: user.id.get(),
                    login: user.login,
                },
                domain_evidence,
            })
        }
        .await;
        // Do not use `?` on the identity read: every obtained token reaches the
        // revoke attempt, even if identity decoding/validation fails. A revoke
        // failure never replaces the original identity result or grants access.
        if let Err(error) = self.sign_in.revoke(token).await {
            tracing::warn!(error_kind = error.code(), "sign-in token revocation failed");
        }
        result
    }
}

#[cfg(test)]
mod tests;
