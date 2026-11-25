//! What signing in yields: the user's token, identity and email addresses.

use std::time::SystemTime;
use zeroize::Zeroizing;

/// A GitHub user's numeric ID: the stable identity (logins can change).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct UserId(u64);

impl UserId {
    /// Wrap a numeric user ID.
    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    /// The numeric ID.
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for UserId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// A user access token from the sign-in exchange (`ghu_…`), bound to the app
/// (client ID) that obtained it.
///
/// Not `Clone`, `Serialize` or `Display`; `Debug` is redacted; the secret is
/// zeroized when dropped. Use it inside
/// [`SignIn::with_user_token`](crate::SignIn::with_user_token), which revokes
/// it afterwards: signing in needs nothing longer-lived.
pub struct UserToken {
    secret: Zeroizing<String>,
    client_id: String,
}

impl UserToken {
    pub(crate) fn new(secret: Zeroizing<String>, client_id: String) -> Self {
        Self { secret, client_id }
    }

    /// The token, for an `Authorization: Bearer` header with another client.
    /// Never log, store or journal it.
    pub fn expose_secret(&self) -> &str {
        &self.secret
    }

    /// The client ID of the app that obtained the token.
    pub fn client_id(&self) -> &str {
        &self.client_id
    }
}

impl std::fmt::Debug for UserToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserToken")
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

/// The signed-in user (`GET /user`).
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct User {
    /// The stable identity; never zero.
    pub id: UserId,

    /// The current login (can change; key on [`id`](Self::id)).
    pub login: String,

    /// GitHub's avatar URL, if any.
    pub avatar_url: Option<String>,

    /// When the account was created, if GitHub said and the value parsed.
    pub created_at: Option<SystemTime>,
}

/// One of the signed-in user's email addresses that GitHub verified
/// (`GET /user/emails`), so it can prove e.g. membership of a domain.
/// Personal data: `Debug` omits the address.
#[derive(Clone, Eq, PartialEq)]
#[non_exhaustive]
pub struct VerifiedEmail {
    /// The address.
    pub email: String,

    /// Whether it is the user's primary address.
    pub primary: bool,
}

impl VerifiedEmail {
    /// A verified address, e.g. for tests of code that consumes them.
    pub fn new(email: impl Into<String>, primary: bool) -> Self {
        Self {
            email: email.into(),
            primary,
        }
    }
}

impl std::fmt::Debug for VerifiedEmail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifiedEmail")
            .field("primary", &self.primary)
            .finish_non_exhaustive()
    }
}
