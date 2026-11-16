//! [`AppTokenProvider`]: creates tokens through an [`App`] and caches them in memory.

use crate::clock::{Clock, SystemClock};
use crate::{InstallationTokenProvider, TokenError};
use octoauth::{App, ConfigError, InstallationId, InstallationToken, TokenScope};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Default minimum validity of a returned token.
const DEFAULT_MIN_REMAINING: Duration = Duration::from_secs(5 * 60);

/// Accepted range for the minimum validity: installation tokens live an hour.
const MIN_REMAINING_RANGE: std::ops::RangeInclusive<Duration> =
    Duration::from_secs(30)..=Duration::from_secs(30 * 60);

/// The longest lifetime (`expires_at - issued_at`) believed; GitHub issues
/// one-hour tokens. Anything else means a bogus `Date`.
const MAX_PLAUSIBLE_LIFETIME: i64 = 3600 + 60;

/// Creates installation tokens through an [`App`] and caches them in memory per
/// installation and scope.
///
/// Create one per app and keep it for the life of the process: clones share
/// the cache, while a provider built per request caches nothing. On Cloudflare
/// Workers, keep it in a `static` (e.g. a `OnceLock`) so warm isolates reuse
/// tokens across requests.
///
/// A token is returned only while at least `min_remaining` of its lifetime
/// is left. Freshness uses the token's lifetime as GitHub reports it
/// (`expires_at - issued_at`), counted from when this process received it, so
/// host clock skew does not affect it. Natively, concurrent requests for the
/// same token wait for one creation and share its outcome, errors included.
#[derive(Clone)]
pub struct AppTokenProvider {
    shared: Arc<Shared>,
}

struct Shared {
    app: App,
    min_remaining: i64,
    clock: Arc<dyn Clock>,
    slots: Mutex<HashMap<Key, Arc<Slot>>>,
}

type Key = (InstallationId, TokenScope);

/// Everything cached for one installation and scope.
#[derive(Default)]
struct Slot {
    cached: Mutex<Option<Cached>>,

    /// Held while creating a token, so concurrent callers wait for one.
    #[cfg(not(target_arch = "wasm32"))]
    flight: futures_util::lock::Mutex<()>,

    /// Creations finished so far, numbering their outcomes.
    #[cfg(not(target_arch = "wasm32"))]
    finished: std::sync::atomic::AtomicU64,

    /// The last creation's failure and its number, for callers that waited on it.
    #[cfg(not(target_arch = "wasm32"))]
    failure: Mutex<Option<(u64, TokenError)>>,
}

struct Cached {
    token: InstallationToken,

    /// Local time (provider clock) at which the token expires.
    deadline: i64,
}

impl AppTokenProvider {
    /// A provider for `app` with the default 5-minute minimum validity.
    pub fn new(app: App) -> Self {
        Self::builder(app)
            .build()
            .expect("the default minimum validity is in range")
    }

    /// Configure a provider for `app`.
    pub fn builder(app: App) -> AppTokenProviderBuilder {
        AppTokenProviderBuilder {
            app,
            min_remaining: DEFAULT_MIN_REMAINING,
            clock: Arc::new(SystemClock),
        }
    }

    /// The app tokens are created for.
    pub fn app(&self) -> &App {
        &self.shared.app
    }

    fn slot(&self, key: Key) -> Arc<Slot> {
        let mut slots = lock(&self.shared.slots);
        if let Some(slot) = slots.get(&key) {
            return slot.clone();
        }
        // New keys are rare (installations × scopes): sweep expired slots
        // nobody uses, which also drops their secrets.
        let now = self.shared.clock.now();
        slots.retain(|_, slot| {
            Arc::strong_count(slot) > 1
                || lock(&slot.cached)
                    .as_ref()
                    .is_some_and(|cached| cached.deadline > now)
        });
        slots.entry(key).or_default().clone()
    }

    /// The cached token if fresh and, when replacing, newer than `rejected`.
    fn fresh(
        &self,
        slot: &Slot,
        rejected: Option<&InstallationToken>,
    ) -> Option<InstallationToken> {
        let cached = lock(&slot.cached);
        let cached = cached.as_ref()?;
        if cached.deadline.saturating_sub(self.shared.clock.now()) < self.shared.min_remaining {
            return None;
        }
        if rejected.is_some_and(|rejected| !newer(&cached.token, rejected)) {
            return None;
        }
        Some(cached.token.clone())
    }

    /// Create a token, natively combined with concurrent callers of the same
    /// slot.
    async fn create(
        &self,
        slot: &Slot,
        installation: InstallationId,
        scope: &TokenScope,
        rejected: Option<&InstallationToken>,
    ) -> Result<InstallationToken, TokenError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            use std::sync::atomic::Ordering::SeqCst;
            let arrived = slot.finished.load(SeqCst);
            let _flight = slot.flight.lock().await;
            if let Some(token) = self.fresh(slot, rejected) {
                return Ok(token);
            }
            // A creation that finished while we waited failed: share its outcome
            // rather than queueing another attempt behind it.
            if let Some((number, error)) = &*lock(&slot.failure)
                && *number > arrived
            {
                return Err(error.clone());
            }
            let result = self.create_now(slot, installation, scope, rejected).await;
            let number = slot.finished.load(SeqCst) + 1;
            *lock(&slot.failure) = result.as_ref().err().map(|error| (number, error.clone()));
            slot.finished.store(number, SeqCst);
            result
        }

        // A Worker request must never wait on another request's I/O: create.
        #[cfg(target_arch = "wasm32")]
        {
            self.create_now(slot, installation, scope, rejected).await
        }
    }

    async fn create_now(
        &self,
        slot: &Slot,
        installation: InstallationId,
        scope: &TokenScope,
        rejected: Option<&InstallationToken>,
    ) -> Result<InstallationToken, TokenError> {
        if let Some(rejected) = rejected {
            // Never serve the rejected token again, even if creating fails.
            let mut cached = lock(&slot.cached);
            if cached
                .as_ref()
                .is_some_and(|cached| cached.token.id() == rejected.id())
            {
                *cached = None;
            }
        }
        let result = self
            .shared
            .app
            .installation(installation)
            .access_token(scope.permissions())
            .await
            .map_err(TokenError::from);
        crate::event::created(self.shared.app.id(), installation, rejected, &result);
        let token = result?;
        let deadline = deadline(&token, self.shared.clock.now());
        let mut cached = lock(&slot.cached);
        if cached
            .as_ref()
            .is_none_or(|cached| newer(&token, &cached.token))
        {
            *cached = Some(Cached {
                token: token.clone(),
                deadline,
            });
        }
        // Returned even if it looks stale: a just-created token is the best
        // there is, and creating another would loop.
        Ok(token)
    }

    #[cfg(test)]
    pub(crate) fn slot_count(&self) -> usize {
        lock(&self.shared.slots).len()
    }
}

impl InstallationTokenProvider for AppTokenProvider {
    async fn token(
        &self,
        installation: InstallationId,
        scope: &TokenScope,
    ) -> Result<InstallationToken, TokenError> {
        let slot = self.slot((installation, scope.clone()));
        if let Some(token) = self.fresh(&slot, None) {
            return Ok(token);
        }
        self.create(&slot, installation, scope, None).await
    }

    async fn replace(&self, rejected: &InstallationToken) -> Result<InstallationToken, TokenError> {
        if rejected.app_id() != self.shared.app.id() {
            let error = TokenError::ForeignToken;
            let installation = rejected.installation_id();
            crate::event::failed(self.shared.app.id(), installation, Some(rejected), &error);
            return Err(error);
        }
        let installation = rejected.installation_id();
        let scope = rejected.scope();
        let slot = self.slot((installation, scope.clone()));
        if let Some(token) = self.fresh(&slot, Some(rejected)) {
            return Ok(token);
        }
        self.create(&slot, installation, scope, Some(rejected))
            .await
    }

    fn invalidate(&self, rejected: &InstallationToken) {
        let key = (rejected.installation_id(), rejected.scope().clone());
        // No slot is created: a foreign or uncached token has nothing to drop.
        let Some(slot) = lock(&self.shared.slots).get(&key).cloned() else {
            return;
        };
        let mut cached = lock(&slot.cached);
        if rejected.app_id() == self.shared.app.id()
            && cached
                .as_ref()
                .is_some_and(|cached| cached.token.id() == rejected.id())
        {
            *cached = None;
        }
    }

    fn invalidate_installation(&self, installation: InstallationId) {
        for ((id, _), slot) in lock(&self.shared.slots).iter() {
            if *id == installation {
                *lock(&slot.cached) = None;
            }
        }
    }
}

impl std::fmt::Debug for AppTokenProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppTokenProvider")
            .field("app", &self.shared.app.id())
            .field("min_remaining", &self.shared.min_remaining)
            .finish_non_exhaustive()
    }
}

/// Builder for [`AppTokenProvider`], from [`AppTokenProvider::builder`].
#[must_use]
pub struct AppTokenProviderBuilder {
    app: App,
    min_remaining: Duration,
    clock: Arc<dyn Clock>,
}

impl AppTokenProviderBuilder {
    /// The least validity a returned token has left. Default 5 minutes;
    /// 30 seconds to 30 minutes. Larger means more tokens created.
    pub fn min_remaining(mut self, min_remaining: Duration) -> Self {
        self.min_remaining = min_remaining;
        self
    }

    /// Replace the clock (tests only).
    #[cfg(test)]
    pub(crate) fn clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Validate and build. Fails with `("min_remaining", "range")` outside
    /// 30 seconds to 30 minutes.
    pub fn build(self) -> Result<AppTokenProvider, ConfigError> {
        if !MIN_REMAINING_RANGE.contains(&self.min_remaining) {
            return Err(ConfigError::new("min_remaining", "range"));
        }
        Ok(AppTokenProvider {
            shared: Arc::new(Shared {
                app: self.app,
                min_remaining: self.min_remaining.as_secs() as i64,
                clock: self.clock,
                slots: Mutex::default(),
            }),
        })
    }
}

impl std::fmt::Debug for AppTokenProviderBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppTokenProviderBuilder")
            .field("app", &self.app.id())
            .field("min_remaining", &self.min_remaining)
            .finish_non_exhaustive()
    }
}

/// Whether `a` is a different, later token than `b`.
fn newer(a: &InstallationToken, b: &InstallationToken) -> bool {
    a.id() != b.id() && (a.expires_at(), a.issued_at()) > (b.expires_at(), b.issued_at())
}

/// The local time at which `token`, received at local time `received`,
/// expires: `received` plus its lifetime per GitHub's clock when known and
/// plausible, else GitHub's `expires_at` read against the local clock.
fn deadline(token: &InstallationToken, received: i64) -> i64 {
    let expires_at = unix_seconds(token.expires_at());
    let lifetime = token
        .issued_at()
        .map(|issued_at| expires_at - unix_seconds(issued_at))
        .filter(|lifetime| (1..=MAX_PLAUSIBLE_LIFETIME).contains(lifetime));
    match lifetime {
        Some(lifetime) => received.saturating_add(lifetime),
        None => expires_at,
    }
}

fn unix_seconds(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

/// Lock a std mutex, recovering from poisoning: every critical section
/// leaves the data consistent.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
