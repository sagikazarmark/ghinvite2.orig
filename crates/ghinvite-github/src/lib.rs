//! ghinvite's GitHub REST client, moved from gh-toolkit and trimmed to what
//! ghinvite uses.
//!
//! - [`transport`]: the [`HttpTransport`] seam. `transport::ReqwestTransport`
//!   natively and `worker::WorkerFetchTransport` (wasm32 only) on Cloudflare
//!   Workers never follow redirects and report whether a failed request
//!   provably never left ([`TransportError::NotSent`]) or may have reached
//!   GitHub.
//! - [`credential`]: the sealed [`Credential`] family,
//!   built from octoauth's installation tokens and app credentials. This crate
//!   never mints or caches a token.
//! - [`rest`]: [`Rest`](rest::Rest) and [`RestEnv`](rest::RestEnv), one HTTP
//!   attempt per call, and exhaustive `Link` pagination that fails rather than
//!   returning a prefix.
//! - [`endpoints`]: the typed endpoints ghinvite calls, with their metadata.
//! - [`error`]: secret-free [`ApiError`] classification.
//! - [`apps`]: [`GithubApps`], the installed apps with their token caches
//!   (octotoken providers), and the operations ghinvite calls GitHub with.
//! - [`profiles`]: the token scopes ghinvite requests.
//!
//! Tests use `mocks::MockTransport` (feature `test-mock`) or the native
//! `stub` HTTP fixture (feature `test-stub`).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod apps;
pub mod avatar;
pub mod credential;
pub mod email_invitation;
pub mod endpoints;
pub mod error;
pub mod ids;
mod pagination;
pub mod payloads;
pub mod profiles;
pub mod rest;
pub mod roles;
pub mod transport;

/// Cloudflare Workers `fetch` transport (wasm32 only).
#[cfg(target_arch = "wasm32")]
pub mod worker;

#[cfg(any(test, feature = "test-mock"))]
pub mod mocks;

#[cfg(all(feature = "test-stub", not(target_arch = "wasm32")))]
pub mod stub;

pub use apps::{
    AppSlot, AppUnavailable, GithubApp, GithubApps, InstallationRest, InstalledApp, TokenCallError,
    TokenSource,
};
pub use credential::{AppCredential, Credential, InstallationCredential};
pub use error::{ApiError, CallError};
pub use roles::RepoRole;
pub use transport::{HttpTransport, HttpTransportDyn, Method, Request, Response, TransportError};

/// A boxed `Send` future, as returned by object-safe adapters.
pub type BoxFut<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;
