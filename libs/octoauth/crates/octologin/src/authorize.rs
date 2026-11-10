//! Starting a sign-in: the authorization URL and the state to keep.

use crate::SignInError;
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

/// Length of the base64url state and PKCE verifier (32 random bytes).
pub(crate) const SECRET_LEN: usize = 43;

/// Optional hints for GitHub's authorization screen. They do not change
/// which user completes the sign-in.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct AuthorizeOptions {
    pub(crate) allow_signup: Option<bool>,
    pub(crate) login: Option<String>,
}

impl AuthorizeOptions {
    /// Whether GitHub may offer to create an account.
    pub fn allow_signup(mut self, allow: bool) -> Self {
        self.allow_signup = Some(allow);
        self
    }

    /// Suggest an account to sign in with. Never proof of who signed in.
    pub fn login(mut self, login: impl Into<String>) -> Self {
        self.login = Some(login.into());
        self
    }
}

/// The URL to redirect the browser to. It carries the state and the PKCE
/// challenge, so `Debug` is redacted; don't log or store it.
pub struct AuthorizeUrl(pub(crate) String);

impl AuthorizeUrl {
    /// The URL.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The URL, owned.
    pub fn into_string(self) -> String {
        self.0
    }
}

impl std::fmt::Debug for AuthorizeUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AuthorizeUrl(..)")
    }
}

/// What to keep, server side, between redirecting the browser and its
/// callback: the state to compare and the PKCE verifier for the exchange.
/// Keep it out of logs, URLs and cookies the browser can read. Both fields
/// are public so a store can persist and rebuild it.
pub struct PendingAuthorization {
    /// The `state` sent to GitHub; the callback must return it.
    pub state: String,

    /// The PKCE verifier, sent only with the code exchange.
    pub pkce_verifier: Zeroizing<String>,
}

impl PendingAuthorization {
    /// Check the callback's `state` in constant time, consuming the pending
    /// authorization either way: one callback per authorization. Returns the
    /// PKCE verifier for [`SignIn::exchange`](crate::SignIn::exchange).
    pub fn verify(self, received_state: &str) -> Result<Zeroizing<String>, SignInError> {
        let expected = self.state.as_bytes();
        let received = received_state.as_bytes();
        let matches = expected.len() == SECRET_LEN
            && received.len() == SECRET_LEN
            && bool::from(expected.ct_eq(received));
        if matches {
            Ok(self.pkce_verifier)
        } else {
            Err(SignInError::StateMismatch)
        }
    }
}

impl std::fmt::Debug for PendingAuthorization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PendingAuthorization(..)")
    }
}
