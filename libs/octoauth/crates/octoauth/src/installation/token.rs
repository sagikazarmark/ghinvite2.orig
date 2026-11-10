//! [`InstallationToken`], its [`TokenScope`] and [`TokenId`].

use super::unix_time;
use crate::error::{ConfigError, Error};
use crate::id::{AppId, InstallationId};
use crate::permissions::Permissions;
use crate::transport::Response;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::SystemTime;
use zeroize::Zeroizing;

/// Upper bound on an accepted token string. Tokens are about 520 characters
/// since 2026-04; the bound only rejects garbage.
const MAX_TOKEN_BYTES: usize = 8 * 1024;

/// What an installation token is requested for: its permissions (and later,
/// optionally, a subset of repositories). Two tokens with equal scopes are
/// interchangeable, so this is the cache key alongside the installation.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct TokenScope {
    permissions: Permissions,
}

impl TokenScope {
    /// A scope covering every repository the installation can access.
    pub fn new(permissions: Permissions) -> Self {
        Self { permissions }
    }

    /// The requested permissions.
    pub fn permissions(&self) -> &Permissions {
        &self.permissions
    }
}

impl From<Permissions> for TokenScope {
    fn from(permissions: Permissions) -> Self {
        Self::new(permissions)
    }
}

/// A token's identity: the first 16 bytes of the SHA-256 of its secret.
/// Safe to log and store (it does not reveal the secret); equal tokens have
/// equal ids. `Display` is lowercase hex.
#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct TokenId([u8; 16]);

impl TokenId {
    fn of(secret: &str) -> Self {
        let digest = Sha256::digest(secret.as_bytes());
        let mut id = [0; 16];
        id.copy_from_slice(&digest[..16]);
        Self(id)
    }

    /// The raw 16 bytes.
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl std::fmt::Display for TokenId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

impl std::fmt::Debug for TokenId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TokenId({self})")
    }
}

/// An installation access token (`ghs_…`). `Debug` is redacted, and there is
/// no `Display` or `Serialize`. Cloning shares the one secret (zeroized when
/// the last clone is dropped) instead of copying it.
///
/// Send it as a bearer token with any HTTP client, e.g. with reqwest (which
/// marks the header sensitive, so its `Debug` output redacts it):
///
/// ```no_run
/// # fn example(token: &octoauth::InstallationToken) {
/// let request = reqwest::Client::new()
///     .get("https://api.github.com/orgs/acme/invitations")
///     .bearer_auth(token.expose_secret());
/// # }
/// ```
#[derive(Clone)]
pub struct InstallationToken(Arc<Inner>);

struct Inner {
    id: TokenId,
    app_id: AppId,
    installation_id: InstallationId,
    scope: TokenScope,
    expires_at: SystemTime,
    issued_at: Option<SystemTime>,
    secret: Zeroizing<String>,
}

/// Everything an [`InstallationToken`] is made of, for rebuilding one that
/// was created elsewhere (a token store, another provider, tests). See
/// [`InstallationToken::from_parts`].
pub struct TokenParts {
    /// The token (`ghs_…`). Moved into zeroizing storage without copying.
    pub secret: String,

    /// The App that created it.
    pub app_id: AppId,

    /// The installation it acts for.
    pub installation_id: InstallationId,

    /// What it was requested for.
    pub scope: TokenScope,

    /// GitHub's `expires_at`.
    pub expires_at: SystemTime,

    /// GitHub's time when it was created, if known.
    pub issued_at: Option<SystemTime>,
}

impl std::fmt::Debug for TokenParts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenParts")
            .field("app_id", &self.app_id)
            .field("installation_id", &self.installation_id)
            .finish_non_exhaustive()
    }
}

impl InstallationToken {
    /// Rebuild a token from its parts. Fails, naming the field but never the
    /// value, if the secret is empty, over 8 KiB or not printable ASCII, or
    /// if `issued_at` is after `expires_at`.
    pub fn from_parts(parts: TokenParts) -> Result<Self, ConfigError> {
        let TokenParts {
            secret,
            app_id,
            installation_id,
            scope,
            expires_at,
            issued_at,
        } = parts;
        let secret = Zeroizing::new(secret);
        if secret.is_empty()
            || secret.len() > MAX_TOKEN_BYTES
            || !secret.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(ConfigError::new("secret", "format"));
        }
        if issued_at.is_some_and(|issued_at| issued_at > expires_at) {
            return Err(ConfigError::new("issued_at", "after_expires_at"));
        }
        Ok(Self(Arc::new(Inner {
            id: TokenId::of(&secret),
            app_id,
            installation_id,
            scope,
            expires_at,
            issued_at,
            secret,
        })))
    }

    /// This token's identity.
    pub fn id(&self) -> TokenId {
        self.0.id
    }

    /// The App that created this token.
    pub fn app_id(&self) -> AppId {
        self.0.app_id
    }

    /// The installation this token acts for.
    pub fn installation_id(&self) -> InstallationId {
        self.0.installation_id
    }

    /// What the token was requested for.
    pub fn scope(&self) -> &TokenScope {
        &self.0.scope
    }

    /// The requested permissions (shorthand for `scope().permissions()`).
    pub fn permissions(&self) -> &Permissions {
        self.0.scope.permissions()
    }

    /// GitHub's `expires_at` (whole seconds), on GitHub's clock. Never
    /// "now + 1 h".
    ///
    /// On `wasm32-unknown-unknown`, `SystemTime::now()` panics; compare against
    /// JavaScript's `Date.now()` there.
    pub fn expires_at(&self) -> SystemTime {
        self.0.expires_at
    }

    /// GitHub's time when the token was created, from the create response's
    /// `Date` header (the token response has no such field); `None` if that
    /// header was missing or unreadable. `expires_at - issued_at` is the
    /// token's lifetime, independent of the local clock.
    pub fn issued_at(&self) -> Option<SystemTime> {
        self.0.issued_at
    }

    /// The token itself. Use it only to authenticate a request; do not log,
    /// store or serialize it. The borrow keeps the zeroized owner in charge.
    pub fn expose_secret(&self) -> &str {
        &self.0.secret
    }
}

impl std::fmt::Debug for InstallationToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InstallationToken(..)")
    }
}

impl InstallationToken {
    /// Parse a 201 from `POST /app/installations/{id}/access_tokens`.
    pub(super) fn from_response(
        response: Response,
        app_id: AppId,
        installation_id: InstallationId,
        scope: TokenScope,
    ) -> Result<Self, Error> {
        #[derive(serde::Deserialize)]
        struct Wire {
            token: String,
            expires_at: String,
        }
        let issued_at = response.date().and_then(unix_time);
        let body = response.body;
        let wire: Wire = serde_json::from_slice(&body).map_err(|_| Error::Decode)?;
        let seconds = chrono::DateTime::parse_from_rfc3339(&wire.expires_at)
            .map_err(|_| Error::Decode)?
            .timestamp();
        let expires_at = unix_time(seconds).ok_or(Error::Decode)?;
        Self::from_parts(TokenParts {
            secret: wire.token,
            app_id,
            installation_id,
            scope,
            expires_at,
            // A `Date` later than `expires_at` is bogus; drop it, keep the token.
            issued_at: issued_at.filter(|issued_at| *issued_at <= expires_at),
        })
        .map_err(|_| Error::Decode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Access, OrganizationPermission};
    use std::time::{Duration, UNIX_EPOCH};

    fn scope() -> TokenScope {
        Permissions::new([(OrganizationPermission::Members, Access::Read)])
            .unwrap()
            .into()
    }

    fn parts(secret: &str) -> TokenParts {
        TokenParts {
            secret: secret.into(),
            app_id: AppId::new(1),
            installation_id: InstallationId::new(2),
            scope: scope(),
            expires_at: UNIX_EPOCH + Duration::from_secs(3600),
            issued_at: Some(UNIX_EPOCH),
        }
    }

    #[test]
    fn exposes_the_secret_only_on_request() {
        let token = InstallationToken::from_parts(parts("ghs_abc")).unwrap();
        assert_eq!(token.expose_secret(), "ghs_abc");
        assert_eq!(format!("{token:?}"), "InstallationToken(..)");
        assert!(!format!("{:?}", parts("ghs_abc")).contains("ghs_abc"));
    }

    #[test]
    fn clones_share_the_secret() {
        let token = InstallationToken::from_parts(parts("ghs_abc")).unwrap();
        let clone = token.clone();
        assert!(std::ptr::eq(token.expose_secret(), clone.expose_secret()));
        assert_eq!(token.id(), clone.id());
    }

    #[test]
    fn ids_identify_secrets() {
        let a = InstallationToken::from_parts(parts("ghs_a")).unwrap();
        let a2 = InstallationToken::from_parts(parts("ghs_a")).unwrap();
        let b = InstallationToken::from_parts(parts("ghs_b")).unwrap();
        assert_eq!(a.id(), a2.id());
        assert_ne!(a.id(), b.id());
        assert_eq!(a.id().to_string().len(), 32);
        assert!(!format!("{:?}", a.id()).contains("ghs_a"));
    }

    #[test]
    fn accessors() {
        let token = InstallationToken::from_parts(parts("ghs_abc")).unwrap();
        assert_eq!(token.app_id(), AppId::new(1));
        assert_eq!(token.installation_id(), InstallationId::new(2));
        assert_eq!(token.scope(), &scope());
        assert_eq!(token.permissions(), scope().permissions());
        assert_eq!(token.issued_at(), Some(UNIX_EPOCH));
    }

    #[test]
    fn from_parts_validates() {
        let error = |parts| {
            let e = InstallationToken::from_parts(parts).unwrap_err();
            (e.field(), e.what())
        };
        for secret in ["", "ghs_a b", "ghs_a\nb", &"a".repeat(MAX_TOKEN_BYTES + 1)] {
            assert_eq!(error(parts(secret)), ("secret", "format"));
        }
        let mut late = parts("ghs_abc");
        late.issued_at = Some(late.expires_at + Duration::from_secs(1));
        assert_eq!(error(late), ("issued_at", "after_expires_at"));
        let mut unknown = parts("ghs_abc");
        unknown.issued_at = None;
        assert!(InstallationToken::from_parts(unknown).is_ok());
    }
}
