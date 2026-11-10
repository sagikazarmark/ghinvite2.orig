//! Credentials a [`Rest`](crate::rest::Rest) client sends. The family is
//! sealed and has no raw accessor: the secret leaves only through the
//! `Authorization` header this crate builds.
//!
//! Both credentials wrap octoauth values; this crate never mints, caches or
//! refreshes one. A 401 is returned as
//! [`ApiError::Unauthorized`](crate::error::ApiError::Unauthorized) and the
//! caller replaces the token.

use crate::ids::{AppId, InstallationId};
use std::time::SystemTime;

/// Authentication scheme, checked against endpoint metadata before dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum CredentialKind {
    /// A signed App JWT, for app-level endpoints.
    AppJwt,
    /// An installation access token.
    Installation,
}

/// Rate-limit identity: whose quota a request spends. Never contains
/// credential material.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[non_exhaustive]
pub enum QuotaBucket {
    /// An installation's quota, isolated across Apps even when installation
    /// IDs coincide.
    Installation {
        /// The App that created the token.
        app: AppId,
        /// The installation the token acts for.
        installation: InstallationId,
    },
    /// An App's own quota.
    App(AppId),
}

/// Opaque authorization material, readable only inside this crate.
#[doc(hidden)]
pub struct Authorization<'a> {
    pub(crate) value: &'a str,
}

pub(crate) mod sealed {
    pub trait Sealed {
        fn authorization(&self) -> super::Authorization<'_>;
    }
}

/// Closed credential family. There is no public raw-string accessor.
pub trait Credential: sealed::Sealed + Send + Sync {
    /// The credential class endpoint metadata checks before dispatch.
    fn kind(&self) -> CredentialKind;
    /// The principal whose quota the request spends.
    fn bucket(&self) -> QuotaBucket;
}

/// An installation access token from octoauth (or octotoken's providers).
///
/// `Debug` shows the IDs only.
#[derive(Clone)]
pub struct InstallationCredential {
    token: octoauth::InstallationToken,
}

impl InstallationCredential {
    /// Wrap a token. Its app and installation IDs become the quota identity.
    pub fn new(token: octoauth::InstallationToken) -> Self {
        Self { token }
    }

    /// The App that created the token.
    pub fn app_id(&self) -> AppId {
        self.token.app_id()
    }

    /// The installation the token acts for.
    pub fn installation_id(&self) -> InstallationId {
        self.token.installation_id()
    }

    /// The token's identity, e.g. to replace it in a provider after a 401.
    pub fn token_id(&self) -> octoauth::TokenId {
        self.token.id()
    }

    /// GitHub's `expires_at` for the token.
    pub fn expires_at(&self) -> SystemTime {
        self.token.expires_at()
    }
}

impl From<octoauth::InstallationToken> for InstallationCredential {
    fn from(token: octoauth::InstallationToken) -> Self {
        Self::new(token)
    }
}

impl std::fmt::Debug for InstallationCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstallationCredential")
            .field("app_id", &self.app_id())
            .field("installation_id", &self.installation_id())
            .finish_non_exhaustive()
    }
}

impl sealed::Sealed for InstallationCredential {
    fn authorization(&self) -> Authorization<'_> {
        Authorization {
            value: self.token.expose_secret(),
        }
    }
}

impl Credential for InstallationCredential {
    fn kind(&self) -> CredentialKind {
        CredentialKind::Installation
    }
    fn bucket(&self) -> QuotaBucket {
        QuotaBucket::Installation {
            app: self.app_id(),
            installation: self.installation_id(),
        }
    }
}

/// An App's signed credential (`octoauth::App::credential`), for app-level
/// endpoints such as `GET /app/installation-requests`.
///
/// Not `Clone`, like the value it wraps. `Debug` shows the app ID only.
pub struct AppCredential {
    credential: octoauth::AppCredential,
}

impl AppCredential {
    /// Wrap an App credential.
    pub fn new(credential: octoauth::AppCredential) -> Self {
        Self { credential }
    }

    /// The App the credential authenticates.
    pub fn app_id(&self) -> AppId {
        self.credential.app_id()
    }

    /// The credential's expiry, on octoauth's signing clock.
    pub fn expires_at(&self) -> SystemTime {
        self.credential.expires_at()
    }
}

impl From<octoauth::AppCredential> for AppCredential {
    fn from(credential: octoauth::AppCredential) -> Self {
        Self::new(credential)
    }
}

impl std::fmt::Debug for AppCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppCredential")
            .field("app_id", &self.app_id())
            .finish_non_exhaustive()
    }
}

impl sealed::Sealed for AppCredential {
    fn authorization(&self) -> Authorization<'_> {
        Authorization {
            value: self.credential.expose_secret(),
        }
    }
}

impl Credential for AppCredential {
    fn kind(&self) -> CredentialKind {
        CredentialKind::AppJwt
    }
    fn bucket(&self) -> QuotaBucket {
        QuotaBucket::App(self.app_id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mocks;

    #[test]
    fn installation_credential_binds_quota_to_the_tokens_ids() {
        let credential = mocks::installation_credential(123, 9, "credential-secret-marker");
        assert_eq!(credential.kind(), CredentialKind::Installation);
        assert_eq!(credential.app_id(), AppId::new(123));
        assert_eq!(credential.installation_id(), InstallationId::new(9));
        assert_eq!(
            credential.bucket(),
            QuotaBucket::Installation {
                app: AppId::new(123),
                installation: InstallationId::new(9),
            }
        );
        let other = mocks::installation_credential(123, 9, "another-secret-marker");
        assert_ne!(credential.token_id(), other.token_id());
        let rendered = format!("{credential:?} {:?}", credential.bucket());
        assert!(!rendered.contains("secret-marker"), "{rendered}");
        assert_eq!(
            sealed::Sealed::authorization(&credential).value,
            "credential-secret-marker"
        );
    }

    #[test]
    fn app_credential_binds_quota_to_the_app() {
        let credential = mocks::app_credential(123);
        assert_eq!(credential.kind(), CredentialKind::AppJwt);
        assert_eq!(credential.bucket(), QuotaBucket::App(AppId::new(123)));
        let secret = sealed::Sealed::authorization(&credential).value.to_owned();
        assert!(!format!("{credential:?}").contains(&secret));
    }
}
