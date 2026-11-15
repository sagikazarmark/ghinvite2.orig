//! The [`App`] entry point and its builder; the app's key and credential live
//! in submodules.

mod clock;
mod credential;
mod key;

pub(crate) use clock::Clock;
use clock::SystemClock;
pub use key::{AppKeySigner, SigningKey};

use crate::error::{ConfigError, Error};
use crate::id::{AppId, ClientId, InstallationId};
use crate::installation::Installation;
use crate::transport::{self, HttpClient};
pub use credential::AppCredential;
use credential::Jwt;
use reqwest::Url;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderValue, USER_AGENT};
use std::sync::{Arc, Mutex};

/// github.com's REST API base URL.
pub const DEFAULT_BASE_URL: &str = "https://api.github.com";

/// The `X-GitHub-Api-Version` sent with every request.
pub const DEFAULT_API_VERSION: &str = "2022-11-28";

/// The `User-Agent` of the default client.
pub const DEFAULT_USER_AGENT: &str = concat!("octoauth/", env!("CARGO_PKG_VERSION"));

/// How long a signed credential is reused, in local seconds. It expires 540 s
/// after signing, so at least 4 minutes remain when it is last used: more than
/// a request timeout plus a retry middleware's backoff.
const CREDENTIAL_REUSE: i64 = 300;

/// Clock differences up to this many seconds are noise (one-second `Date`
/// resolution, transit time): no correction is applied or reported.
const SKEW_DEAD_BAND: i64 = 30;

/// The largest correction applied. It bounds how far a bogus `Date` can move
/// the credential's validity window, and how long after capture a credential
/// could be replayed (its 10-minute window plus this).
const MAX_SKEW_CORRECTION: i64 = 3600;

/// A GitHub App: its identity, signer and HTTP client.
///
/// ```no_run
/// # async fn example(pem: &str) -> Result<(), Box<dyn std::error::Error>> {
/// use octoauth::{Access, App, OrganizationPermission, Permissions, SigningKey};
///
/// let app = App::new(123, SigningKey::from_pem(pem)?);
/// let installation = app.installation(42);
///
/// let permissions = Permissions::new([(OrganizationPermission::Members, Access::Write)])?;
/// let token = installation.access_token(&permissions).await?;
/// let request = reqwest::Client::new()
///     .get("https://api.github.com/orgs/acme")
///     .bearer_auth(token.expose_secret());
/// # Ok(()) }
/// ```
///
/// Use [`App::builder`] for anything beyond the defaults.
///
/// The app's signed credential is reused for 5 minutes. If GitHub rejects it
/// with a 401 and its `Date` shows the credential could not have been valid at
/// GitHub's time (a skewed host clock, or expiry in flight), the credential is
/// re-signed with the measured correction (up to ±1 hour) and the request
/// retried once; the correction is kept for later credentials until a
/// rejection measures a different one. Each [`Installation::access_token`]
/// call still creates a fresh token.
///
/// Cloning is one reference-count increment; clones share the HTTP client and
/// the credential.
#[derive(Clone)]
pub struct App {
    inner: Arc<Inner>,
}

struct Inner {
    id: AppId,

    /// JWT `iss`: the client ID, or the App ID in decimal.
    issuer: String,
    signer: Arc<dyn AppKeySigner>,
    client: HttpClient,

    /// Always a base (has a host), so path segments can be appended.
    base_url: Url,
    user_agent: Option<HeaderValue>,
    clock: Arc<dyn Clock>,

    /// The last signed credential, shared by clones of the `App`.
    credential: Mutex<Option<CachedCredential>>,
}

/// A signed credential, when (local time) and with which clock correction
/// (seconds added to the local time) it was signed.
struct CachedCredential {
    credential: Arc<Jwt>,
    signed_at: i64,
    correction: i64,
}

impl App {
    /// An app on github.com with all defaults: identified by its App ID, the
    /// default client (redirects disabled except on wasm32, 30 s timeout, `User-Agent:`
    /// [`DEFAULT_USER_AGENT`]).
    ///
    /// `signer` is usually a [`SigningKey`](crate::SigningKey); any
    /// [`AppKeySigner`] works, including an `Arc` or `Box` of one.
    ///
    /// # Panics
    ///
    /// If the TLS backend cannot be initialised, like `reqwest::Client::new`.
    /// [`App::builder`] reports that as an error instead.
    pub fn new(id: impl Into<AppId>, signer: impl AppKeySigner + 'static) -> Self {
        Self::builder(id, signer)
            .build()
            .expect("only the TLS backend can fail with defaults")
    }

    /// Configure an app. `signer` as for [`App::new`].
    ///
    /// ```no_run
    /// # fn example(pem: &str) -> Result<(), Box<dyn std::error::Error>> {
    /// use octoauth::{App, SigningKey};
    ///
    /// let app = App::builder(123, SigningKey::from_pem(pem)?)
    ///     .client_id("Iv23liexample")
    ///     .user_agent("my-app/1.0")
    ///     .base_url("https://ghe.example.com/api/v3")
    ///     .build()?;
    /// # Ok(()) }
    /// ```
    pub fn builder(id: impl Into<AppId>, signer: impl AppKeySigner + 'static) -> AppBuilder {
        AppBuilder {
            id: id.into(),
            signer: Arc::new(signer),
            client_id: None,
            client: None,
            base_url: None,
            user_agent: None,
            clock: None,
        }
    }

    /// The numeric App ID.
    pub fn id(&self) -> AppId {
        self.inner.id
    }

    /// A handle for one installation of this app. Cheap (clones the `App`);
    /// performs no I/O.
    pub fn installation(&self, id: impl Into<InstallationId>) -> Installation {
        Installation::new(self.clone(), id.into())
    }

    /// The app's signed credential, for app-level endpoints octoauth does
    /// not wrap: the cached one if younger than 5 minutes, else a new one
    /// (with the clock correction last measured). No I/O. See
    /// [`AppCredential`] for handling a 401.
    ///
    /// Fails only with [`Error::Signing`].
    pub fn credential(&self) -> Result<AppCredential, Error> {
        Ok(AppCredential::new(self.inner.id, self.signed(None)?))
    }

    /// Send a request authenticated as the app to `base_url/<segments>`,
    /// with an optional JSON body.
    ///
    /// A 401 ends in [`Error::AppCredentialsRejected`], but first, if the
    /// response's `Date` shows that GitHub could not have accepted the
    /// credential at its own time (not yet valid, expired, or expiring too far
    /// ahead), and a credential signed now with the measured clock correction
    /// would be accepted, the request is retried once with such a credential.
    /// Retrying is safe: GitHub did not act on a 401. The correction stays with
    /// the cached credential and later re-signs, until a rejection measures a
    /// different one, so a corrected host clock heals after one retry.
    pub(crate) async fn call(
        &self,
        method: reqwest::Method,
        segments: &[&str],
        json: Option<Vec<u8>>,
    ) -> Result<transport::Response, Error> {
        let rejected = self.signed(None)?;
        let response = self
            .send(self.build(method.clone(), segments, json.clone(), &rejected))
            .await?;
        if response.status != 401 {
            return Ok(response);
        }
        let Some((github_now, skew)) = self.observe(&response) else {
            return Err(Error::AppCredentialsRejected { clock_skew: None });
        };
        let correction = if skew.abs() <= SKEW_DEAD_BAND {
            0
        } else {
            skew.clamp(-MAX_SKEW_CORRECTION, MAX_SKEW_CORRECTION)
        };
        let clock_caused = !credential::accepted_at(rejected.window(), github_now);
        // A credential signed now with `correction` (our now is GitHub's minus
        // the skew); if even that would be rejected, retrying cannot help.
        let fixable = github_now
            .checked_sub(skew)
            .and_then(|local_now| local_now.checked_add(correction))
            .and_then(credential::window)
            .is_some_and(|window| credential::accepted_at(window, github_now));
        if !(clock_caused && fixable) {
            return Err(rejected_with(skew));
        }
        crate::event::clock_skew_corrected(self.inner.id, skew, correction);
        let credential = self.signed(Some((correction, &rejected)))?;
        let response = self
            .send(self.build(method, segments, json, &credential))
            .await?;
        if response.status == 401 {
            return Err(match self.observe(&response) {
                Some((_, skew)) => rejected_with(skew),
                None => Error::AppCredentialsRejected { clock_skew: None },
            });
        }
        Ok(response)
    }

    /// GitHub's time from the response's `Date`, and GitHub's clock minus ours.
    fn observe(&self, response: &transport::Response) -> Option<(i64, i64)> {
        let github_now = response.date()?;
        Some((github_now, github_now.checked_sub(self.inner.clock.now())?))
    }

    /// A credential to send.
    ///
    /// - `None`: the cached credential if younger than [`CREDENTIAL_REUSE`],
    ///   else a new one signed with the cached correction (0 at first).
    /// - `Some((correction, rejected))`: a new one signed with `correction`,
    ///   unless a concurrent caller already cached a different, fresh one
    ///   with that correction.
    ///
    /// Signing happens under the lock, so concurrent callers sign once.
    fn signed(&self, retry: Option<(i64, &Arc<Jwt>)>) -> Result<Arc<Jwt>, Error> {
        let inner = &*self.inner;
        let mut cache = inner
            .credential
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = inner.clock.now();
        if let Some(cached) = &*cache
            && (0..CREDENTIAL_REUSE).contains(&now.saturating_sub(cached.signed_at))
        {
            match retry {
                None => return Ok(cached.credential.clone()),
                Some((correction, rejected))
                    if !Arc::ptr_eq(&cached.credential, rejected)
                        && (cached.correction - correction).abs() <= SKEW_DEAD_BAND =>
                {
                    return Ok(cached.credential.clone());
                }
                Some(_) => {}
            }
        }
        let correction = match retry {
            Some((correction, _)) => correction,
            None => cache.as_ref().map_or(0, |cached| cached.correction),
        };
        let signed_now = now
            .checked_add(correction)
            .ok_or(Error::Signing(crate::error::SigningError::InvalidTime))?;
        let credential = Arc::new(
            credential::sign(inner.signer.as_ref(), &inner.issuer, signed_now)
                .map_err(Error::Signing)?,
        );
        *cache = Some(CachedCredential {
            credential: credential.clone(),
            signed_at: now,
            correction,
        });
        crate::event::credential_signed(inner.id, correction);
        Ok(credential)
    }

    /// A request to `base_url/<segments>` with GitHub's standard headers and
    /// `credential`. Segments are percent-encoded.
    fn build(
        &self,
        method: reqwest::Method,
        segments: &[&str],
        json: Option<Vec<u8>>,
        credential: &Jwt,
    ) -> reqwest::Request {
        let mut url = self.inner.base_url.clone();
        url.path_segments_mut()
            .expect("validated base URLs have a host")
            .pop_if_empty()
            .extend(segments);
        let mut request = reqwest::Request::new(method, url);
        *request.timeout_mut() = Some(transport::TIMEOUT);
        let headers = request.headers_mut();
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            "x-github-api-version",
            HeaderValue::from_static(DEFAULT_API_VERSION),
        );
        if let Some(user_agent) = &self.inner.user_agent {
            headers.insert(USER_AGENT, user_agent.clone());
        }
        headers.insert(AUTHORIZATION, credential.authorization());
        if let Some(json) = json {
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            *request.body_mut() = Some(json.into());
        }
        request
    }

    async fn send(&self, request: reqwest::Request) -> Result<transport::Response, Error> {
        transport::send(&self.inner.client, request)
            .await
            .map_err(Error::from)
    }
}

/// A rejection, reporting `skew` (GitHub's clock minus ours) when it is
/// outside the dead band.
fn rejected_with(skew: i64) -> Error {
    Error::AppCredentialsRejected {
        clock_skew: (skew.abs() > SKEW_DEAD_BAND).then_some(skew),
    }
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("id", &self.inner.id)
            .field("issuer", &self.inner.issuer)
            .field("base_url", &self.inner.base_url.as_str())
            .finish_non_exhaustive()
    }
}

/// Builder for [`App`], from [`App::builder`]. Setters only record values;
/// [`build`](Self::build) validates them.
#[must_use]
pub struct AppBuilder {
    id: AppId,
    signer: Arc<dyn AppKeySigner>,
    client_id: Option<String>,
    client: Option<HttpClient>,
    base_url: Option<String>,
    user_agent: Option<String>,
    clock: Option<Arc<dyn Clock>>,
}

impl AppBuilder {
    /// Identify the app by its client ID (`Iv23…`) when authenticating, as
    /// GitHub recommends.
    /// Default: the App ID. 1–255 ASCII letters, digits, `_`, `-` or `.`.
    pub fn client_id(mut self, client_id: impl Into<String>) -> Self {
        self.client_id = Some(client_id.into());
        self
    }

    /// Send through your own client (proxy, TLS roots, connection pool).
    ///
    /// Build it with `redirect(reqwest::redirect::Policy::none())`: if it
    /// follows a redirect, the response is discarded as
    /// [`Error::Redirected`]. Every request still gets a 30 s timeout and a
    /// 10 MiB body limit. GitHub rejects requests without a `User-Agent`; set
    /// one on the client or with [`user_agent`](Self::user_agent).
    pub fn client(mut self, client: reqwest::Client) -> Self {
        self.client = Some(transport::from_reqwest(client));
        self
    }

    /// Send through a middleware stack (retries, `reqwest-tracing`, …). The
    /// same requirements as [`client`](Self::client) apply.
    #[cfg(feature = "middleware")]
    pub fn middleware_client(mut self, client: reqwest_middleware::ClientWithMiddleware) -> Self {
        self.client = Some(client);
        self
    }

    /// Send this `User-Agent` on every request, overriding the client's.
    /// 1–256 printable ASCII characters.
    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = Some(user_agent.into());
        self
    }

    /// Use another REST API base URL, e.g. `https://ghe.example.com/api/v3` or
    /// a test server. Accepts a string or a `reqwest::Url`. Must be `https`, or
    /// `http` on a loopback host; no credentials, query or fragment.
    /// Default: [`DEFAULT_BASE_URL`].
    pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    /// Replace the wall clock (tests only).
    #[cfg(test)]
    pub(crate) fn clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = Some(clock);
        self
    }

    /// Validate and build. Errors name the field, never the value:
    ///
    /// | Field | Reason |
    /// |---|---|
    /// | `client_id` | `identifier` |
    /// | `user_agent` | `printable_ascii` |
    /// | `base_url` | `url` |
    /// | `client` | `tls` (the default client could not initialise TLS) |
    pub fn build(self) -> Result<App, ConfigError> {
        let issuer = match self.client_id {
            Some(client_id) => ClientId::new(client_id)?.as_str().to_owned(),
            None => self.id.to_string(),
        };
        let user_agent = self
            .user_agent
            .as_deref()
            .map(validate_user_agent)
            .transpose()?;
        let base_url = validate_base_url(self.base_url.as_deref().unwrap_or(DEFAULT_BASE_URL))?;
        let client = match self.client {
            Some(client) => client,
            None => transport::default_client().ok_or(ConfigError::new("client", "tls"))?,
        };
        Ok(App {
            inner: Arc::new(Inner {
                id: self.id,
                issuer,
                signer: self.signer,
                client,
                base_url,
                user_agent,
                clock: self.clock.unwrap_or_else(|| Arc::new(SystemClock)),
                credential: Mutex::new(None),
            }),
        })
    }
}

impl std::fmt::Debug for AppBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppBuilder")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

fn validate_user_agent(user_agent: &str) -> Result<HeaderValue, ConfigError> {
    let invalid = || ConfigError::new("user_agent", "printable_ascii");
    if user_agent.is_empty()
        || user_agent.len() > 256
        || !user_agent.bytes().all(|b| (0x20..=0x7e).contains(&b))
    {
        return Err(invalid());
    }
    HeaderValue::from_str(user_agent).map_err(|_| invalid())
}

fn validate_base_url(base_url: &str) -> Result<Url, ConfigError> {
    let invalid = || ConfigError::new("base_url", "url");
    let url = Url::parse(base_url).map_err(|_| invalid())?;
    let host = url.host_str().ok_or_else(invalid)?;
    let loopback = host == "localhost"
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    let scheme_ok = url.scheme() == "https" || (url.scheme() == "http" && loopback);
    if !scheme_ok
        || url.cannot_be_a_base()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid());
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEM: &str = include_str!("../tests/fixtures/rsa2048.pem");

    fn builder() -> AppBuilder {
        App::builder(7, SigningKey::from_pem(PEM).unwrap())
    }

    fn field(builder: AppBuilder) -> (&'static str, &'static str) {
        let error = builder.build().unwrap_err();
        (error.field(), error.what())
    }

    fn url(app: &App, segments: &[&str]) -> String {
        let credential = app.signed(None).unwrap();
        app.build(reqwest::Method::GET, segments, None, &credential)
            .url()
            .to_string()
    }

    #[test]
    fn issuer() {
        assert_eq!(builder().build().unwrap().inner.issuer, "7");
        assert_eq!(
            builder()
                .client_id("Iv23.repo")
                .build()
                .unwrap()
                .inner
                .issuer,
            "Iv23.repo"
        );
        assert_eq!(
            field(builder().client_id("has space")),
            ("client_id", "identifier")
        );
    }

    #[test]
    fn user_agent_validation() {
        assert!(
            builder()
                .user_agent("app/1.0 (+https://x.test)")
                .build()
                .is_ok()
        );
        for bad in [
            String::new(),
            "tab\there".into(),
            "é".into(),
            "x".repeat(257),
        ] {
            assert_eq!(
                field(builder().user_agent(bad)),
                ("user_agent", "printable_ascii")
            );
        }
    }

    #[test]
    fn base_url_validation() {
        for good in [
            "https://ghe.example.com/api/v3/",
            "http://127.0.0.1:8080",
            "http://localhost:1",
            "http://[::1]:2",
        ] {
            assert!(builder().base_url(good).build().is_ok(), "{good}");
        }
        for bad in [
            "http://ghe.example.com",
            "https://user:pw@ghe.example.com",
            "https://ghe.example.com/?q=1",
            "https://ghe.example.com/#f",
            "ftp://ghe.example.com",
            "mailto:x@ghe.example.com",
            "not a url",
        ] {
            assert_eq!(field(builder().base_url(bad)), ("base_url", "url"), "{bad}");
        }
    }

    #[test]
    fn request_urls_append_segments() {
        let segments = ["app", "installations", "42"];
        for (base, expected) in [
            (
                DEFAULT_BASE_URL,
                "https://api.github.com/app/installations/42",
            ),
            (
                "https://ghe.example.com/api/v3",
                "https://ghe.example.com/api/v3/app/installations/42",
            ),
            (
                "https://ghe.example.com/api/v3/",
                "https://ghe.example.com/api/v3/app/installations/42",
            ),
        ] {
            let app = builder().base_url(base).build().unwrap();
            assert_eq!(url(&app, &segments), expected);
        }
        let parsed = Url::parse("https://ghe.example.com/api/v3").unwrap();
        let app = builder().base_url(parsed).build().unwrap();
        assert_eq!(
            url(&app, &segments),
            "https://ghe.example.com/api/v3/app/installations/42"
        );
    }

    #[test]
    fn debug_shows_identity_only() {
        let app = App::new(7, SigningKey::from_pem(PEM).unwrap());
        assert_eq!(
            format!("{app:?}"),
            r#"App { id: AppId(7), issuer: "7", base_url: "https://api.github.com/", .. }"#
        );
        assert_eq!(
            format!("{:?}", app.installation(42)),
            "Installation { app: AppId(7), id: InstallationId(42) }"
        );
        assert_eq!(
            format!("{:?}", builder()),
            "AppBuilder { id: AppId(7), .. }"
        );
    }
}
