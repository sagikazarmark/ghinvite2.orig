//! GitHub App authentication: installation access tokens.
//!
//! The low-level GitHub App client: one call is one GitHub operation, nothing
//! cached except the app's own credential. [`App`] reuses its signed
//! credential for up to 5 minutes and corrects for a skewed host clock (one
//! retry after a 401, using GitHub's `Date`). Every
//! [`Installation::access_token`] call creates a fresh token; for cached tokens
//! use a provider (crate `octotoken`).
//!
//! [`Installation::get`] reads an installation (account, permissions,
//! suspension). For app-level endpoints the crate does not wrap,
//! [`App::credential`] lends the app's signed credential.
//!
//! ```no_run
//! # async fn example(pem: &str) -> Result<(), Box<dyn std::error::Error>> {
//! use octoauth::{
//!     Access, App, OrganizationPermission, Permissions, RepositoryPermission, SigningKey,
//! };
//!
//! let app = App::new(123, SigningKey::from_pem(pem)?);
//! let permissions = Permissions::builder()
//!     .grant(RepositoryPermission::Metadata, Access::Read)
//!     .grant(OrganizationPermission::Members, Access::Write)
//!     .build()?;
//! let token = app.installation(42).access_token(&permissions).await?;
//!
//! let response = reqwest::Client::new()
//!     .get("https://api.github.com/orgs/acme/invitations")
//!     .bearer_auth(token.expose_secret())
//!     .send()
//!     .await?;
//! # Ok(()) }
//! ```
//!
//! # Using a token with a client
//!
//! The crate wraps no GitHub API client: send the token with any client, as
//! above. Two pieces help with the rest:
//!
//! - [`ApiError::classify`] turns any client's error response (status,
//!   headers, raw or parsed body) into a fixed-code error that keeps GitHub's
//!   rate-limit wait and the reason for a 403 or 422, but never the response's
//!   text, so it can be logged and journaled.
//! - In crate `octotoken`, a provider caches tokens and `with_token` handles
//!   a 401 (one replacement token, one retry).
//!
//! `octotoken`'s `examples/octocrab.rs` and `examples/reqwest.rs` show both
//! clients set up for this: no automatic retries or redirects, errors
//! converted before they are logged.
//!
//! # HTTP
//!
//! The crate's own requests go through reqwest. By default each [`App`]
//! builds a client with redirects disabled; [`AppBuilder::client`] takes your
//! own.
//!
//! # Cloudflare Workers (wasm32)
//!
//! The crate builds for `wasm32-unknown-unknown`, where reqwest uses the
//! host's `fetch` and the app's clock reads JavaScript `Date.now()`. Differences:
//!
//! - `fetch` follows redirects and reqwest cannot turn that off. A redirected
//!   response is still discarded as [`Error::Redirected`], but the request was
//!   already re-sent to the new location (per the Fetch standard, without
//!   `Authorization` if it crosses origins; not verified on workerd).
//! - Transport failures are always [`TransientKind::NoResponse`]: `fetch` never
//!   proves a request was not sent, so [`TransientKind::NotSent`] does not occur.
//! - The futures returned by [`Installation`] methods hold JavaScript promises
//!   and are not `Send`. That is fine for the `worker` crate, which runs them
//!   locally; hosts that demand `Send` futures need a single-threaded wrapper.
//! - The 30 s per-request timeout still applies; reqwest enforces it with an
//!   `AbortSignal`.
//! - A response body is read whole before its size bound applies (a declared
//!   oversize `Content-Length` is still refused unread): reqwest streams
//!   bodies there only with its `stream` feature, which cannot link next to
//!   the `worker` crate.
//!
//! # Features
//!
//! - `middleware`: every request goes through a
//!   `reqwest_middleware::ClientWithMiddleware` (plain clients are wrapped), and
//!   `AppBuilder::middleware_client` accepts your own stack, e.g. with
//!   `reqwest-tracing` for spans or a retry middleware.
//!
//! # Secrets
//!
//! [`SigningKey`], [`InstallationToken`] and [`AppCredential`] implement
//! neither `Serialize` nor `Display`, and their `Debug` output is redacted.
//! `SigningKey` and `AppCredential` are not `Clone`; cloning an
//! `InstallationToken` shares its one zeroized secret. The token and the
//! credential are reachable only through their `expose_secret`. Headers the
//! crate sends are marked sensitive. Errors ([`Error`], [`ApiError`]) carry
//! fixed codes, statuses and durations, never upstream response text.
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod api;
mod app;
mod error;
mod id;
mod installation;
mod permissions;
mod transport;

pub use api::{ApiError, ErrorBody, ValidationCode};
pub use app::{
    App, AppBuilder, AppCredential, AppKeySigner, DEFAULT_API_VERSION, DEFAULT_BASE_URL,
    DEFAULT_USER_AGENT, SigningKey,
};
pub use error::{ConfigError, Error, ForbiddenReason, RateLimitScope, SigningError, TransientKind};
pub use id::{AppId, InstallationId};
pub use installation::{
    AccountKind, Installation, InstallationAccount, InstallationInfo, InstallationToken,
    RepositorySelection, TokenId, TokenParts, TokenScope,
};
pub use permissions::{
    Access, AccountPermission, EnterprisePermission, OrganizationPermission, Permission,
    Permissions, PermissionsBuilder, RepositoryPermission,
};
