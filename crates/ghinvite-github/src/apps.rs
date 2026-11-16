//! ghinvite's GitHub Apps and their tokens: [`GithubApps`].
//!
//! [`GithubApps`] holds, per installed app ([`AppSlot`]), the `octoauth::App`,
//! a [`TokenSource`] (an `octotoken` provider behind octotoken's object-safe
//! `DynInstallationTokenProvider`) and the [`RestEnv`] its calls go through. [`GithubApps::app`] hands out a
//! [`GithubApp`], whose operations replace gh-toolkit's `Broker`:
//!
//! | Operation | Replaces | Behaviour |
//! |---|---|---|
//! | [`read`](GithubApp::read) | `with_token` | token; on [`ApiError::Unauthorized`] `replace` it and run the closure once more (`octotoken::with_token`) |
//! | [`write_once`](GithubApp::write_once) | `call` | token, one call, never replayed; on a 401 the token is `invalidate`d (nothing created) before the error is returned |
//! | [`ensure`](GithubApp::ensure) | `ensure` | `token()` |
//! | [`refresh`](GithubApp::refresh) | `invalidate_installation` + `ensure_fresh` | `invalidate_installation`, then `token()`: one creation |
//! | [`installation`](GithubApp::installation), [`get_installation`](GithubApp::get_installation) | app-JWT `get_installation` | `octoauth::Installation::get` |
//! | [`app_rest`](GithubApp::app_rest) | `list_installation_requests`'s credential | a [`Rest`] with `octoauth::App::credential` |
//!
//! No operation returns a token: closures receive an [`InstallationRest`],
//! which only builds a [`Rest`]. Inside a Restate `ctx.run`, get the token
//! inside the step and return only the outcome (ADR 0003).
//!
//! # Token caches
//!
//! - **Natively** [`GithubApps::new`] creates one `octotoken::AppTokenProvider`
//!   per app. Build the context once at startup and keep it (e.g. in
//!   `AppState`): clones share the providers and their caches, and concurrent
//!   misses for one token wait for one creation.
//! - **On Workers (wasm32)** the providers live in isolate-scope statics, one
//!   `octotoken::ProviderRegistry` per slot, keyed by App ID (PD-4), so a
//!   context built per request still reuses the warm isolate's tokens (and
//!   app credential). The first
//!   `octoauth::App` built for a key in an isolate is the one used; a new
//!   deployment (new code or secrets) runs in new isolates. Concurrent misses
//!   may create duplicate tokens: requests never wait on each other.
//!
//! # `Send`
//!
//! Every future here is `Send` on both targets, so it can run inside Restate's
//! `ctx.run`. On wasm32 the provider's and octoauth's futures hold JavaScript
//! promises and are not `Send`; they are wrapped in
//! `send_wrapper::SendWrapper` (PD-3): by octotoken's `send-wrapper` feature
//! for [`TokenSource`], here for octoauth's. A Worker isolate is
//! single-threaded, so the wrapper's thread check never fires.
//!
//! # Token errors
//!
//! Token creation fails with [`octotoken::TokenError`], which wraps
//! [`octoauth::Error`] ([`TokenError::GitHub`]). gh-toolkit's `MintError`
//! maps onto it as follows; retry only when [`TokenError::is_transient`]
//! (`Transient` and `RateLimited`), as `wf/error.rs` does today:
//!
//! | gh-toolkit `MintError` | `octoauth::Error` |
//! |---|---|
//! | `Transient { kind }` | `Transient { kind }` |
//! | `RateLimited { scope, retry_after }` | `RateLimited { scope, retry_after }` ([`rate_limit`]) |
//! | `PermissionsNotGranted` | `PermissionsNotGranted` |
//! | `InstallationGone` | `InstallationGone` |
//! | `InstallationSuspended` | `InstallationSuspended` |
//! | `MintForbidden { reason }` | `Forbidden { reason }` |
//! | `AppAuth(..)` | `AppCredentialsRejected { .. }`, `Signing(..)` |
//! | `Decode`, `Unexpected` | `Decode`, `Unexpected`, `Redirected` |
//!
//! [`TokenError::ForeignToken`] (a token from another app given to `replace`)
//! cannot happen through this module, which only replaces its own tokens.

use crate::{
    BoxFut,
    credential::{AppCredential, InstallationCredential},
    error::{ApiError, CallError, RateLimitScope},
    ids::{AppId, InstallationId},
    rest::{Rest, RestEnv},
};
use octoauth::{InstallationToken, TokenScope};
use octotoken::TokenError;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

/// An installed GitHub App of ghinvite.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum AppSlot {
    /// The repository app (`repo`).
    Repo,
    /// The organization app (`org`, stage `repo-and-org`).
    Org,
}

impl AppSlot {
    /// The slot's name in configuration and routes.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Repo => "repo",
            Self::Org => "org",
        }
    }

    /// The slot named `name`.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "repo" => Some(Self::Repo),
            "org" => Some(Self::Org),
            _ => None,
        }
    }
}

impl std::fmt::Display for AppSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One installed app as configured: its slot, the `octoauth::App` (signing
/// key, client ID, token-creation base URL) and the user agent its REST calls
/// send.
#[derive(Clone, Debug)]
pub struct InstalledApp {
    /// Which app.
    pub slot: AppSlot,
    /// Creates tokens and signs app credentials.
    pub app: octoauth::App,
    /// `User-Agent` of the app's REST calls, e.g. `p1/repo`.
    pub user_agent: String,
}

/// An object-safe source of installation tokens, so [`GithubApps`] can hold
/// any `octotoken::InstallationTokenProvider` (tests inject a fake, e.g.
/// `mocks::FixedTokens`): octotoken's `DynInstallationTokenProvider`,
/// implemented for every provider. Implement the provider trait rather than
/// this one.
///
/// The futures are `Send` on every target (wrapped in `SendWrapper` on wasm32
/// by octotoken's `send-wrapper` feature).
pub use octotoken::DynInstallationTokenProvider as TokenSource;

/// Box a future as `Send`: natively it is; on wasm32 through `SendWrapper`.
#[cfg(not(target_arch = "wasm32"))]
fn send_box<'a, T>(future: impl Future<Output = T> + Send + 'a) -> BoxFut<'a, T> {
    Box::pin(future)
}

/// Box a future as `Send`: natively it is; on wasm32 through `SendWrapper`.
#[cfg(target_arch = "wasm32")]
fn send_box<'a, T>(future: impl Future<Output = T> + 'a) -> BoxFut<'a, T> {
    Box::pin(send_wrapper::SendWrapper::new(future))
}

/// ghinvite's installed GitHub Apps with their token caches. Cheap to clone.
#[derive(Clone)]
pub struct GithubApps {
    apps: Arc<[GithubApp]>,
}

impl std::fmt::Debug for GithubApps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.apps.iter()).finish()
    }
}

impl GithubApps {
    /// The apps with `octotoken::AppTokenProvider` caches: one per app
    /// natively, the isolate's on wasm32 (see the module docs). `env` gives
    /// the transport and API base of REST calls; each app's
    /// [`InstalledApp::user_agent`] replaces its user agent.
    ///
    /// Fails with `Decode { what: "user_agent" }` for an invalid user agent.
    /// A slot listed twice keeps its last entry.
    pub fn new(
        env: RestEnv,
        apps: impl IntoIterator<Item = InstalledApp>,
    ) -> Result<Self, ApiError> {
        Self::with_sources(env, apps.into_iter().map(provider))
    }

    /// The apps with the given token sources, e.g. fakes in tests. As
    /// [`new`](Self::new) otherwise.
    pub fn with_sources(
        env: RestEnv,
        apps: impl IntoIterator<Item = (InstalledApp, Arc<dyn TokenSource>)>,
    ) -> Result<Self, ApiError> {
        let mut built: Vec<GithubApp> = Vec::new();
        for (installed, tokens) in apps {
            let app = GithubApp {
                slot: installed.slot,
                env: env.clone().with_user_agent(installed.user_agent)?,
                app: installed.app,
                tokens,
            };
            built.retain(|existing| existing.slot != app.slot);
            built.push(app);
        }
        built.sort_by_key(|app| app.slot);
        Ok(Self { apps: built.into() })
    }

    /// The app in `slot`, if configured (`org` only at stage `repo-and-org`).
    pub fn app(&self, slot: AppSlot) -> Result<GithubApp, AppUnavailable> {
        self.apps
            .iter()
            .find(|app| app.slot == slot)
            .cloned()
            .ok_or(AppUnavailable { slot })
    }

    /// The configured slots.
    pub fn slots(&self) -> impl Iterator<Item = AppSlot> + '_ {
        self.apps.iter().map(|app| app.slot)
    }
}

/// An app's token source: a new provider natively.
#[cfg(not(target_arch = "wasm32"))]
fn provider(installed: InstalledApp) -> (InstalledApp, Arc<dyn TokenSource>) {
    let provider = octotoken::AppTokenProvider::new(installed.app.clone());
    (installed, Arc::new(provider))
}

/// An app's token source: the isolate's provider for this slot and App ID
/// (created on first use), with the provider's `App`, so its cached app
/// credential is reused too.
#[cfg(target_arch = "wasm32")]
fn provider(mut installed: InstalledApp) -> (InstalledApp, Arc<dyn TokenSource>) {
    use octotoken::ProviderRegistry;
    // A Worker isolate has one thread: this is isolate scope. A registry per
    // slot keys the providers by slot and App ID.
    static REPO: ProviderRegistry = ProviderRegistry::new();
    static ORG: ProviderRegistry = ProviderRegistry::new();
    let registry = match installed.slot {
        AppSlot::Repo => &REPO,
        AppSlot::Org => &ORG,
    };
    let provider = registry.provider(&installed.app);
    installed.app = provider.app().clone();
    (installed, Arc::new(provider))
}

/// The requested app is not configured at this deployment stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("gh:apps:unavailable")]
pub struct AppUnavailable {
    /// The requested slot.
    pub slot: AppSlot,
}

/// One installed app: its token source, `octoauth::App` and REST
/// environment. From [`GithubApps::app`]; cheap to clone and `'static`, so it
/// can move into a durable step.
#[derive(Clone)]
pub struct GithubApp {
    slot: AppSlot,
    app: octoauth::App,
    tokens: Arc<dyn TokenSource>,
    env: RestEnv,
}

impl std::fmt::Debug for GithubApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GithubApp")
            .field("slot", &self.slot)
            .field("app_id", &self.app.id())
            .field("env", &self.env)
            .finish_non_exhaustive()
    }
}

impl GithubApp {
    /// Which app.
    pub fn slot(&self) -> AppSlot {
        self.slot
    }

    /// The App ID.
    pub fn id(&self) -> AppId {
        self.app.id()
    }

    /// The REST environment of this app's calls.
    pub fn env(&self) -> &RestEnv {
        &self.env
    }

    /// Run `call` with a token for `installation` and `scope`; if it fails
    /// with `CallError::Api(ApiError::Unauthorized)`, replace the token and
    /// run `call` once more (`octotoken::with_token`). A second 401 is
    /// returned as [`TokenCallError::Call`]; other errors are returned
    /// without retrying.
    ///
    /// `call` must make **one** GitHub request, or only requests that are
    /// safe to repeat: a 401 on its third request re-runs the first two. Do
    /// no other side effects in it (e.g. spend a budget before calling).
    pub async fn read<T, E, F, Fut>(
        &self,
        installation: impl Into<InstallationId>,
        scope: &TokenScope,
        mut call: F,
    ) -> Result<T, TokenCallError<E>>
    where
        F: FnMut(InstallationRest) -> Fut,
        Fut: Future<Output = Result<T, CallError<E>>>,
    {
        let token = self
            .tokens
            .token(installation.into(), scope)
            .await
            .map_err(TokenCallError::Token)?;
        match call(self.handle(token.clone())).await {
            Err(CallError::Api(ApiError::Unauthorized)) => {}
            result => return result.map_err(TokenCallError::Call),
        }
        let token = self
            .tokens
            .replace(&token)
            .await
            .map_err(TokenCallError::Token)?;
        call(self.handle(token)).await.map_err(TokenCallError::Call)
    }

    /// Run `call` once with a token for `installation` and `scope`; never
    /// replayed, for writes whose repetition needs the caller's fencing. On
    /// `CallError::Api(ApiError::Unauthorized)` the token is invalidated
    /// before the error is returned, creating nothing: the caller releases
    /// its fence, and its own single retry gets a new token from
    /// [`ensure`](Self::ensure) (P1 spec §9.4). A last 401 costs no token.
    pub async fn write_once<T, E, F, Fut>(
        &self,
        installation: impl Into<InstallationId>,
        scope: &TokenScope,
        call: F,
    ) -> Result<T, TokenCallError<E>>
    where
        F: FnOnce(InstallationRest) -> Fut,
        Fut: Future<Output = Result<T, CallError<E>>>,
    {
        let token = self
            .tokens
            .token(installation.into(), scope)
            .await
            .map_err(TokenCallError::Token)?;
        let result = call(self.handle(token.clone())).await;
        if matches!(result, Err(CallError::Api(ApiError::Unauthorized))) {
            self.tokens.invalidate(&token);
        }
        result.map_err(TokenCallError::Call)
    }

    /// Make sure a token for `installation` and `scope` can be had (cached
    /// or newly created), without returning it.
    pub async fn ensure(
        &self,
        installation: impl Into<InstallationId>,
        scope: &TokenScope,
    ) -> Result<(), TokenError> {
        self.tokens
            .token(installation.into(), scope)
            .await
            .map(drop)
    }

    /// Drop every cached token of `installation`, then create one for
    /// `scope`, e.g. after the installation's permissions changed
    /// (gh-toolkit's `invalidate_installation` + `ensure_fresh`). Creates
    /// exactly one token; the other scopes get theirs when next used.
    pub async fn refresh(
        &self,
        installation: impl Into<InstallationId>,
        scope: &TokenScope,
    ) -> Result<(), TokenError> {
        let installation = installation.into();
        self.tokens.invalidate_installation(installation);
        self.tokens.token(installation, scope).await.map(drop)
    }

    /// The `octoauth::Installation` for app-authenticated operations. Its
    /// futures are not `Send` on wasm32; prefer
    /// [`get_installation`](Self::get_installation).
    pub fn installation(&self, id: impl Into<InstallationId>) -> octoauth::Installation {
        self.app.installation(id)
    }

    /// `octoauth::Installation::get`: the installation's account,
    /// permissions and suspension, authenticated as the app.
    pub async fn get_installation(
        &self,
        id: impl Into<InstallationId>,
    ) -> Result<octoauth::InstallationInfo, octoauth::Error> {
        let installation = self.app.installation(id);
        send_box(installation.get()).await
    }

    /// A [`Rest`] handle authenticated as the app, for app-level endpoints
    /// (`Endpoint::ListInstallationRequests`, `Endpoint::GetInstallation`).
    /// Fails only with `octoauth::Error::Signing`. After a 401 with it, see
    /// `octoauth::AppCredential` (one `get_installation` re-learns clock
    /// skew; then retry once).
    pub fn app_rest(&self) -> Result<AppRest, octoauth::Error> {
        Ok(AppRest {
            env: self.env.clone(),
            credential: AppCredential::new(self.app.credential()?),
        })
    }

    fn handle(&self, token: InstallationToken) -> InstallationRest {
        InstallationRest {
            env: self.env.clone(),
            credential: InstallationCredential::new(token),
        }
    }
}

/// An installation token bound to a REST environment, handed to
/// [`GithubApp::read`] and [`GithubApp::write_once`] closures. It only builds
/// a [`Rest`]; the token stays inside.
pub struct InstallationRest {
    env: RestEnv,
    credential: InstallationCredential,
}

impl InstallationRest {
    /// The REST client for this token.
    pub fn rest(&self) -> Rest<'_> {
        Rest::new(&self.env, &self.credential)
    }

    /// The credential's identity (app, installation, token ID); no secret.
    pub fn credential(&self) -> &InstallationCredential {
        &self.credential
    }
}

impl std::fmt::Debug for InstallationRest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("InstallationRest")
            .field(&self.credential)
            .finish()
    }
}

/// The app's signed credential bound to a REST environment, from
/// [`GithubApp::app_rest`]. Get one per request.
pub struct AppRest {
    env: RestEnv,
    credential: AppCredential,
}

impl AppRest {
    /// The REST client for the app credential.
    pub fn rest(&self) -> Rest<'_> {
        Rest::new(&self.env, &self.credential)
    }
}

impl std::fmt::Debug for AppRest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("AppRest").field(&self.credential).finish()
    }
}

/// Why [`GithubApp::read`] or [`GithubApp::write_once`] failed.
#[derive(Clone, Debug)]
pub enum TokenCallError<E = std::convert::Infallible> {
    /// No token (or replacement) could be had; the closure did not run
    /// (again).
    Token(TokenError),
    /// The closure failed: any error, or for `read` a second 401.
    Call(CallError<E>),
}

impl<E> TokenCallError<E> {
    /// Fixed code; never formats the application's payload.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Token(error) => error.code(),
            Self::Call(error) => error.code(),
        }
    }

    /// Whether GitHub answered the (last) call with 401.
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, Self::Call(CallError::Api(ApiError::Unauthorized)))
    }
}

impl<E> std::fmt::Display for TokenCallError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Token(error) => error.fmt(f),
            Self::Call(error) => error.fmt(f),
        }
    }
}

impl<E: std::fmt::Debug> std::error::Error for TokenCallError<E> {}

/// The `octoauth::Error` behind a token failure, if any.
pub fn github_error(error: &TokenError) -> Option<&octoauth::Error> {
    match error {
        TokenError::GitHub(error) => Some(error),
        _ => None,
    }
}

/// The limit and GitHub's requested wait of a rate-limited token creation
/// (gh-toolkit's `MintError::RateLimited`).
pub fn rate_limit(error: &TokenError) -> Option<(RateLimitScope, Option<Duration>)> {
    match github_error(error)? {
        octoauth::Error::RateLimited { scope, retry_after } => Some((*scope, *retry_after)),
        _ => None,
    }
}

/// Compile-time proof, on every target, that the operations' futures are
/// `Send` (Restate's `ctx.run` requires it) given a `Send` closure.
#[allow(dead_code)]
fn futures_are_send(app: &GithubApp, scope: &TokenScope) {
    fn send<T: Send>(_: T) {}
    send(app.clone());
    send(app.ensure(1, scope));
    send(app.refresh(1, scope));
    send(app.get_installation(1));
    send(app.read(1, scope, |handle| async move {
        crate::endpoints::get_user_by_id(&handle.rest(), 1)
            .await
            .map_err(CallError::<()>::from)
    }));
    send(app.write_once(1, scope, |handle| async move {
        crate::endpoints::remove_collaborator(
            &handle.rest(),
            crate::endpoints::Repository {
                owner: "o",
                name: "r",
            },
            "u",
        )
        .await
        .map_err(CallError::<()>::from)
    }));
}

#[cfg(test)]
mod tests;
