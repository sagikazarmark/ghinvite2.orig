//! [`DynInstallationTokenProvider`]: the object-safe form of
//! [`InstallationTokenProvider`].

use crate::{InstallationTokenProvider, TokenError};
use octoauth::{InstallationId, InstallationToken, TokenScope};
use std::future::Future;
use std::pin::Pin;

/// The future of a [`DynInstallationTokenProvider`] call: `Send` natively and,
/// with the `send-wrapper` feature, on wasm32.
#[cfg(any(not(target_arch = "wasm32"), feature = "send-wrapper"))]
pub type TokenFuture<'a> =
    Pin<Box<dyn Future<Output = Result<InstallationToken, TokenError>> + Send + 'a>>;

/// The future of a [`DynInstallationTokenProvider`] call: `Send` natively and,
/// with the `send-wrapper` feature, on wasm32.
#[cfg(all(target_arch = "wasm32", not(feature = "send-wrapper")))]
pub type TokenFuture<'a> =
    Pin<Box<dyn Future<Output = Result<InstallationToken, TokenError>> + 'a>>;

/// `Send + Sync` natively and, with the `send-wrapper` feature, on wasm32;
/// nothing otherwise. The bound of [`DynInstallationTokenProvider`], so an
/// `Arc<dyn DynInstallationTokenProvider>` is `Send` exactly when its futures
/// are. Implemented for every type that qualifies.
#[cfg(any(not(target_arch = "wasm32"), feature = "send-wrapper"))]
pub trait DynSendSync: Send + Sync {}

#[cfg(any(not(target_arch = "wasm32"), feature = "send-wrapper"))]
impl<T: Send + Sync + ?Sized> DynSendSync for T {}

/// `Send + Sync` natively and, with the `send-wrapper` feature, on wasm32;
/// nothing otherwise. The bound of [`DynInstallationTokenProvider`], so an
/// `Arc<dyn DynInstallationTokenProvider>` is `Send` exactly when its futures
/// are. Implemented for every type that qualifies.
#[cfg(all(target_arch = "wasm32", not(feature = "send-wrapper")))]
pub trait DynSendSync {}

#[cfg(all(target_arch = "wasm32", not(feature = "send-wrapper")))]
impl<T: ?Sized> DynSendSync for T {}

/// [`InstallationTokenProvider`] with boxed futures, so it can be held as
/// `Arc<dyn DynInstallationTokenProvider>`, e.g. to swap a fake in for tests
/// without a type parameter.
///
/// Implemented for every [`InstallationTokenProvider`] (that is `Send + Sync`
/// where [`DynSendSync`] asks for it): implement that trait, not this one.
/// `dyn DynInstallationTokenProvider` implements
/// [`InstallationTokenProvider`] in turn, so it works with
/// [`with_token`](crate::with_token). Since providers implement both traits,
/// method calls are ambiguous where both are in scope: import the one you
/// call, or call it as `DynInstallationTokenProvider::token(&*provider, …)`.
///
/// # wasm32
///
/// The futures are not `Send` on wasm32, like the provider's. With the
/// `send-wrapper` feature they are wrapped in `send_wrapper::SendWrapper`,
/// which makes them `Send` and panics if one is polled or dropped on another
/// thread than the one that created it. A Cloudflare Worker isolate has one
/// thread, so that never happens there; the feature is for hosts that demand
/// `Send` futures, such as Restate's `ctx.run`. Enabling it also requires the
/// provider to be `Send + Sync` on wasm32 ([`AppTokenProvider`] is).
///
/// [`AppTokenProvider`]: crate::AppTokenProvider
pub trait DynInstallationTokenProvider: DynSendSync {
    /// [`InstallationTokenProvider::token`].
    fn token<'a>(&'a self, installation: InstallationId, scope: &'a TokenScope) -> TokenFuture<'a>;

    /// [`InstallationTokenProvider::replace`].
    fn replace<'a>(&'a self, rejected: &'a InstallationToken) -> TokenFuture<'a>;
}

impl<P: InstallationTokenProvider + DynSendSync> DynInstallationTokenProvider for P {
    fn token<'a>(&'a self, installation: InstallationId, scope: &'a TokenScope) -> TokenFuture<'a> {
        boxed(InstallationTokenProvider::token(self, installation, scope))
    }

    fn replace<'a>(&'a self, rejected: &'a InstallationToken) -> TokenFuture<'a> {
        boxed(InstallationTokenProvider::replace(self, rejected))
    }
}

impl InstallationTokenProvider for dyn DynInstallationTokenProvider + '_ {
    async fn token(
        &self,
        installation: InstallationId,
        scope: &TokenScope,
    ) -> Result<InstallationToken, TokenError> {
        DynInstallationTokenProvider::token(self, installation, scope).await
    }

    async fn replace(&self, rejected: &InstallationToken) -> Result<InstallationToken, TokenError> {
        DynInstallationTokenProvider::replace(self, rejected).await
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn boxed<'a>(
    future: impl Future<Output = Result<InstallationToken, TokenError>> + Send + 'a,
) -> TokenFuture<'a> {
    Box::pin(future)
}

#[cfg(all(target_arch = "wasm32", feature = "send-wrapper"))]
fn boxed<'a>(
    future: impl Future<Output = Result<InstallationToken, TokenError>> + 'a,
) -> TokenFuture<'a> {
    Box::pin(send_wrapper::SendWrapper::new(future))
}

#[cfg(all(target_arch = "wasm32", not(feature = "send-wrapper")))]
fn boxed<'a>(
    future: impl Future<Output = Result<InstallationToken, TokenError>> + 'a,
) -> TokenFuture<'a> {
    Box::pin(future)
}
