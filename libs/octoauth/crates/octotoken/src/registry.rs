//! [`ProviderRegistry`]: isolate-scope providers keyed by App ID.

use crate::AppTokenProvider;
use octoauth::{App, AppId};
use std::sync::{Mutex, MutexGuard};

/// One [`AppTokenProvider`] per App ID, for code that builds its context per
/// request, such as a Cloudflare Worker's `fetch` handler: keep the registry
/// in a `static`, and each request gets the provider (and the app credential)
/// that earlier requests in the same isolate (or process) cached.
///
/// ```no_run
/// use octotoken::{AppTokenProvider, ProviderRegistry, octoauth::App};
///
/// static PROVIDERS: ProviderRegistry = ProviderRegistry::new();
///
/// fn tokens(app: &App) -> AppTokenProvider {
///     PROVIDERS.provider(app)
/// }
/// ```
///
/// The first [`App`] registered for an App ID is kept for the life of the
/// registry: a later `App` with the same ID but another key or base URL gets
/// the first one's provider. On Workers that is harmless, since a new
/// deployment (new code or secrets) runs in new isolates. Providers are never
/// removed; a provider drops its expired tokens itself.
///
/// Natively, prefer creating one [`AppTokenProvider`] per app at startup and
/// passing it around: that keeps tests independent of each other.
pub struct ProviderRegistry {
    providers: Mutex<Vec<(AppId, AppTokenProvider)>>,
}

impl ProviderRegistry {
    /// An empty registry; `const`, so it can initialise a `static`.
    pub const fn new() -> Self {
        Self {
            providers: Mutex::new(Vec::new()),
        }
    }

    /// The provider for `app`'s App ID: the registered one, or a new
    /// [`AppTokenProvider::new`] for `app`, registered. Use the returned
    /// provider's [`app`](AppTokenProvider::app) rather than `app` to reuse
    /// its cached app credential too.
    pub fn provider(&self, app: &App) -> AppTokenProvider {
        let mut providers = self.lock();
        if let Some((_, provider)) = providers.iter().find(|(id, _)| *id == app.id()) {
            return provider.clone();
        }
        let provider = AppTokenProvider::new(app.clone());
        providers.push((app.id(), provider.clone()));
        provider
    }

    /// Lock, recovering from poisoning: a push leaves the list consistent.
    fn lock(&self) -> MutexGuard<'_, Vec<(AppId, AppTokenProvider)>> {
        self.providers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.lock().len()
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for ProviderRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderRegistry")
            .field(
                "apps",
                &self.lock().iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// A registry must be usable as a `static` on every target, wasm32 included.
const _: fn() = || {
    fn sync<T: Sync>() {}
    sync::<ProviderRegistry>();
};
