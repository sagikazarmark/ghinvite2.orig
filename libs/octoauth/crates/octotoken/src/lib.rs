//! Cached GitHub App installation tokens behind a consumer-facing trait.
//!
//! Consumers ask an [`InstallationTokenProvider`] for a token and never learn
//! how or when it was created or where it is kept. The first implementation,
//! [`AppTokenProvider`], creates them through a [`octoauth::App`] and caches them
//! in memory; shared, encrypted storage for several services comes later
//! behind the same trait.
//!
//! # Using a provider
//!
//! - Get a token per request rather than holding one across a long
//!   operation: a cache hit is cheap, and the provider only returns tokens
//!   with time to spare.
//! - Make the request through [`with_token`]: after GitHub answers **401** it
//!   [`replace`](InstallationTokenProvider::replace)s the rejected token once
//!   and retries once. (Calling [`token`](InstallationTokenProvider::token)
//!   again would return the same rejected token.) Never replace after a 403
//!   (missing permissions, SSO, IP allow list, rate limits): a new token
//!   cannot fix those.
//! - Classify error responses with [`ApiError::classify`], which keeps no
//!   response text, before logging or journaling them.
//! - Never journal, store, log or return a token from a durable step (ADR
//!   0003): obtain it inside the step that uses it.
//!
//! [`with_token`] has a reqwest example; `examples/octocrab.rs` and
//! `examples/reqwest.rs` show complete clients (no automatic retries or
//! redirects, errors converted at the boundary), each tested against a local
//! GitHub stand-in.
//!
//! # Holding a provider
//!
//! - Generic code takes `impl InstallationTokenProvider`; to store any
//!   provider without a type parameter (e.g. a fake in tests), hold an
//!   `Arc<dyn` [`DynInstallationTokenProvider`]`>`, which every provider
//!   implements.
//! - Code that builds its context per request (Cloudflare Workers) gets its
//!   providers from a `static` [`ProviderRegistry`], so tokens outlive the
//!   request.
//!
//! # Platforms
//!
//! Natively, the futures are `Send` and concurrent requests for the same
//! token wait for one creation. On Cloudflare Workers (`wasm32`) they are not
//! `Send`, and a request never waits on another request's creation: concurrent
//! misses may create duplicates, which is harmless. See [`MaybeSend`].
//!
//! # Features
//!
//! - `send-wrapper`: [`DynInstallationTokenProvider`]'s futures are `Send` on
//!   wasm32 too (see there). No effect natively.
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod app_provider;
mod clock;
mod dyn_provider;
mod error;
mod maybe_send;
mod registry;
mod with_token;

#[cfg(test)]
mod tests;

pub use app_provider::{AppTokenProvider, AppTokenProviderBuilder};
pub use dyn_provider::{DynInstallationTokenProvider, DynSendSync, TokenFuture};
pub use error::TokenError;
pub use maybe_send::{MaybeSend, MaybeSendSync};
pub use registry::ProviderRegistry;
pub use with_token::{WithTokenError, with_token};

/// The low-level client, for building an [`App`](octoauth::App).
pub use octoauth;
pub use octoauth::{
    Access, AccountKind, AccountPermission, ApiError, AppCredential, AppId, ConfigError,
    EnterprisePermission, Error, ErrorBody, ForbiddenReason, InstallationAccount, InstallationId,
    InstallationInfo, InstallationToken, OrganizationPermission, Permission, Permissions,
    PermissionsBuilder, RateLimitScope, RepositoryPermission, RepositorySelection, TokenId,
    TokenParts, TokenScope, ValidationCode,
};

use std::future::Future;

/// Provides installation tokens for one GitHub App.
///
/// Returned tokens have at least the provider's minimum validity left on its
/// clock (5 minutes by default for [`AppTokenProvider`]). How tokens are
/// created, cached or shared is the implementation's business.
pub trait InstallationTokenProvider: MaybeSendSync {
    /// A token for `installation` with exactly `scope`. Call per request.
    fn token(
        &self,
        installation: InstallationId,
        scope: &TokenScope,
    ) -> impl Future<Output = Result<InstallationToken, TokenError>> + MaybeSend;

    /// A token for the same installation and scope as `rejected`, newer than
    /// it. Call once after GitHub answers 401 to `rejected`, usually through
    /// [`with_token`]. Concurrent callers replacing the same token get the
    /// same replacement.
    fn replace(
        &self,
        rejected: &InstallationToken,
    ) -> impl Future<Output = Result<InstallationToken, TokenError>> + MaybeSend;
}
