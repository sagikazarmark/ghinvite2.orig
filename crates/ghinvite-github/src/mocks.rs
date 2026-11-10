//! In-process test doubles: a scripted [`HttpTransport`], credential
//! builders, and [`FixedTokens`] with [`GithubApps::for_tests`]. Behind the
//! `test-mock` feature, for other crates' tests.

use crate::{
    apps::{AppSlot, GithubApps, InstalledApp, TokenSource},
    credential::{AppCredential, InstallationCredential},
    rest::RestEnv,
    transport::{HttpTransport, Method, Request, Response, TransportError},
};
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// The API base [`env()`] points at.
pub const API_BASE: &str = "https://api.github.test";

/// One scripted expectation: the request the test expects, paired with the
/// response to return on a match.
#[derive(Clone)]
pub struct Expectation {
    /// Exact method.
    pub method: Method,
    /// Exact URL, including the query string in order.
    pub url: String,
    /// Subset match: each listed header must be present with exactly this
    /// value. The request may carry others.
    pub required_headers: BTreeMap<String, String>,
    /// `None` skips the body check; `Some(b)` requires byte equality.
    pub expected_body: Option<Vec<u8>>,
    /// Returned only after method, URL, headers and body match.
    pub response: Response,
}

impl std::fmt::Debug for Expectation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Expectation")
            .field("method", &self.method)
            .field("url", &"[REDACTED]")
            .field("headers", &"[REDACTED]")
            .field("body", &"[REDACTED]")
            .field("response", &self.response)
            .finish()
    }
}

impl Expectation {
    /// Expect `method` and `url`, then answer 200 with this JSON body.
    pub fn ok_json(method: Method, url: impl Into<String>, body: serde_json::Value) -> Self {
        Self {
            method,
            url: url.into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 200,
                headers: [("content-type".to_owned(), "application/json".to_owned())].into(),
                body: serde_json::to_vec(&body).expect("JSON value"),
            },
        }
    }

    /// Expect `method` and `url`, then answer `status` with an empty body.
    pub fn status(method: Method, url: impl Into<String>, status: u16) -> Self {
        Self {
            method,
            url: url.into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status,
                headers: BTreeMap::new(),
                body: Vec::new(),
            },
        }
    }

    /// Require this header value byte for byte; only the name is normalized.
    pub fn require_header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.required_headers
            .insert(name.to_ascii_lowercase(), value.into());
        self
    }

    /// Require the complete request body to equal these bytes.
    pub fn require_body(mut self, body: Vec<u8>) -> Self {
        self.expected_body = Some(body);
        self
    }
}

/// Scripted transport: pops one [`Expectation`] per `send`, in order, and
/// panics on a mismatch or an extra request. Panic messages never render the
/// URL, header values or body.
#[derive(Clone)]
pub struct MockTransport {
    inner: Arc<Mutex<Vec<Expectation>>>,
}

impl MockTransport {
    /// An ordered, single-use script.
    pub fn scripted(expectations: Vec<Expectation>) -> Self {
        let mut v = expectations;
        v.reverse();
        Self {
            inner: Arc::new(Mutex::new(v)),
        }
    }

    /// Expectations not yet consumed.
    pub fn remaining(&self) -> usize {
        self.inner.lock().len()
    }

    /// Assert that the script has been fully consumed.
    pub fn assert_exhausted(&self) {
        let n = self.remaining();
        assert_eq!(
            n, 0,
            "MockTransport script not exhausted: {n} expectations remaining"
        );
    }
}

impl HttpTransport for MockTransport {
    async fn send(&self, request: Request) -> Result<Response, TransportError> {
        let next = self.inner.lock().pop().unwrap_or_else(|| {
            panic!("MockTransport: script exhausted, got unexpected request {request:?}")
        });
        if next.method != request.method {
            panic!(
                "MockTransport: expected {} but got {}",
                next.method.as_str(),
                request.method.as_str()
            );
        }
        assert!(
            next.url == request.url,
            "MockTransport: URL mismatch (values withheld)"
        );
        for (name, expected_value) in &next.required_headers {
            match request.headers.get(name) {
                Some(actual) if actual == expected_value => (),
                Some(_) => panic!("MockTransport: header mismatch (values withheld)"),
                None => panic!("MockTransport: missing required header (name withheld)"),
            }
        }
        if let Some(expected_body) = &next.expected_body {
            assert!(
                request.body_for_transport().unwrap_or_default() == expected_body,
                "MockTransport: body mismatch (values withheld)"
            );
        }
        // Script HTTP failures as statuses; transport failures need a custom
        // `HttpTransport`.
        Ok(next.response)
    }
}

/// A [`RestEnv`] sending through `mock` to [`API_BASE`].
pub fn env(mock: &MockTransport, user_agent: &str) -> RestEnv {
    RestEnv::new(Arc::new(mock.clone()), API_BASE, user_agent).expect("static test environment")
}

/// An octoauth installation token with `secret`, scoped to metadata read and
/// valid for an hour.
pub fn installation_token(
    app_id: u64,
    installation_id: u64,
    secret: &str,
) -> octoauth::InstallationToken {
    let permissions = octoauth::Permissions::builder()
        .grant(
            octoauth::RepositoryPermission::Metadata,
            octoauth::Access::Read,
        )
        .build()
        .expect("static permissions");
    octoauth::InstallationToken::from_parts(octoauth::TokenParts {
        secret: secret.to_owned(),
        app_id: app_id.into(),
        installation_id: installation_id.into(),
        scope: octoauth::TokenScope::new(permissions),
        expires_at: SystemTime::now() + Duration::from_secs(3600),
        issued_at: None,
    })
    .expect("test token secret must be printable ASCII")
}

/// An [`InstallationCredential`] around [`installation_token`].
pub fn installation_credential(
    app_id: u64,
    installation_id: u64,
    secret: &str,
) -> InstallationCredential {
    InstallationCredential::new(installation_token(app_id, installation_id, secret))
}

/// An [`AppCredential`] signed by a fixed fake signer (not a valid RS256
/// signature; GitHub would refuse it).
pub fn app_credential(app_id: u64) -> AppCredential {
    let app = octoauth::App::new(app_id, FixedSigner);
    AppCredential::new(app.credential().expect("fake signer signs"))
}

/// Signs with a fixed fake signature: the JWT is well formed, but GitHub
/// would refuse it. Local stand-ins (the stub, wiremock) do not check it.
struct FixedSigner;

impl octoauth::AppKeySigner for FixedSigner {
    fn sign_rs256(&self, _: &[u8]) -> Result<Vec<u8>, octoauth::SigningError> {
        Ok(vec![7; 256])
    }
}

/// Where [`app`]s made for [`GithubApps::for_tests`] send octoauth's own
/// requests: a closed loopback port, so a stray app-authenticated request
/// fails fast as transient instead of leaving the machine.
pub const UNREACHABLE_APP_BASE: &str = "http://127.0.0.1:9";

/// An `octoauth::App` with a fake signer (see [`app_credential`]) whose own
/// requests (token creation, `Installation::get`) go to `base_url`, e.g. a
/// stub or wiremock server.
pub fn app(app_id: u64, base_url: &str) -> octoauth::App {
    octoauth::App::builder(app_id, FixedSigner)
        .base_url(base_url)
        .user_agent("ghinvite-test")
        .build()
        .expect("test app configuration")
}

/// A fake `octotoken` provider with predictable tokens, for
/// [`GithubApps::for_tests`] and [`GithubApps::with_sources`].
///
/// - Tokens for one installation and scope are numbered from 1 (a
///   *generation*); the secret is [`FixedTokens::secret`], so scripts can
///   require `authorization: Bearer ghs_fixed_{app}_{installation}_{generation}`
///   ([`FixedTokens::bearer`]). Secrets do not encode the scope.
/// - `token` returns the current generation (creating 1); `replace` of the
///   current token moves to the next one, as `AppTokenProvider` does.
/// - [`fail_next_token`](Self::fail_next_token) and
///   [`fail_next_replace`](Self::fail_next_replace) script errors for the
///   next calls of that operation, in order.
/// - Counts `token` and `replace` calls. Clones share all state.
#[derive(Clone)]
pub struct FixedTokens {
    app_id: octoauth::AppId,
    state: Arc<Mutex<FixedState>>,
}

#[derive(Default)]
struct FixedState {
    generations: std::collections::HashMap<(octoauth::InstallationId, octoauth::TokenScope), u64>,
    token_failures: std::collections::VecDeque<octotoken::TokenError>,
    replace_failures: std::collections::VecDeque<octotoken::TokenError>,
    token_calls: usize,
    replace_calls: usize,
}

impl FixedTokens {
    /// Tokens of App `app_id`.
    pub fn new(app_id: u64) -> Self {
        Self {
            app_id: app_id.into(),
            state: Arc::default(),
        }
    }

    /// The App the tokens belong to.
    pub fn app_id(&self) -> octoauth::AppId {
        self.app_id
    }

    /// The secret of a generation's token.
    pub fn secret(app_id: u64, installation: u64, generation: u64) -> String {
        format!("ghs_fixed_{app_id}_{installation}_{generation}")
    }

    /// The `authorization` header value of a generation's token.
    pub fn bearer(&self, installation: u64, generation: u64) -> String {
        format!(
            "Bearer {}",
            Self::secret(self.app_id.get(), installation, generation)
        )
    }

    /// Fail the next `token` call with `error` (queued in order).
    pub fn fail_next_token(&self, error: octotoken::TokenError) -> &Self {
        self.state.lock().token_failures.push_back(error);
        self
    }

    /// Fail the next `replace` call with `error` (queued in order).
    pub fn fail_next_replace(&self, error: octotoken::TokenError) -> &Self {
        self.state.lock().replace_failures.push_back(error);
        self
    }

    /// `token` calls so far.
    pub fn token_calls(&self) -> usize {
        self.state.lock().token_calls
    }

    /// `replace` calls so far.
    pub fn replace_calls(&self) -> usize {
        self.state.lock().replace_calls
    }

    /// The current generation for `installation` and `scope` (0: none yet).
    pub fn generation(&self, installation: u64, scope: &octoauth::TokenScope) -> u64 {
        let key = (installation.into(), scope.clone());
        self.state
            .lock()
            .generations
            .get(&key)
            .copied()
            .unwrap_or(0)
    }

    fn mint(
        &self,
        installation: octoauth::InstallationId,
        scope: &octoauth::TokenScope,
        generation: u64,
    ) -> octoauth::InstallationToken {
        octoauth::InstallationToken::from_parts(octoauth::TokenParts {
            secret: Self::secret(self.app_id.get(), installation.get(), generation),
            app_id: self.app_id,
            installation_id: installation,
            scope: scope.clone(),
            expires_at: SystemTime::now() + Duration::from_secs(3600),
            issued_at: None,
        })
        .expect("fixed token secret")
    }
}

impl std::fmt::Debug for FixedTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FixedTokens")
            .field("app_id", &self.app_id)
            .finish_non_exhaustive()
    }
}

impl octotoken::InstallationTokenProvider for FixedTokens {
    async fn token(
        &self,
        installation: octoauth::InstallationId,
        scope: &octoauth::TokenScope,
    ) -> Result<octoauth::InstallationToken, octotoken::TokenError> {
        let generation = {
            let mut state = self.state.lock();
            state.token_calls += 1;
            if let Some(error) = state.token_failures.pop_front() {
                return Err(error);
            }
            let generation = state
                .generations
                .entry((installation, scope.clone()))
                .or_insert(0);
            *generation = (*generation).max(1);
            *generation
        };
        Ok(self.mint(installation, scope, generation))
    }

    async fn replace(
        &self,
        rejected: &octoauth::InstallationToken,
    ) -> Result<octoauth::InstallationToken, octotoken::TokenError> {
        let installation = rejected.installation_id();
        let scope = rejected.scope();
        let generation = {
            let mut state = self.state.lock();
            state.replace_calls += 1;
            if let Some(error) = state.replace_failures.pop_front() {
                return Err(error);
            }
            if rejected.app_id() != self.app_id {
                return Err(octotoken::TokenError::ForeignToken);
            }
            let generation = state
                .generations
                .entry((installation, scope.clone()))
                .or_insert(0);
            if *generation == 0 || self.mint(installation, scope, *generation).id() == rejected.id()
            {
                *generation += 1;
            }
            *generation
        };
        Ok(self.mint(installation, scope, generation))
    }
}

impl GithubApps {
    /// Apps for tests: REST calls go through `mock` to [`API_BASE`] with user
    /// agent `p1/{slot}`, tokens come from the given [`FixedTokens`], and the
    /// `octoauth::App`s have a fake signer and [`UNREACHABLE_APP_BASE`].
    /// For app-authenticated requests against a stand-in, build the apps with
    /// [`app`] and [`GithubApps::with_sources`].
    pub fn for_tests(
        mock: &MockTransport,
        apps: impl IntoIterator<Item = (AppSlot, FixedTokens)>,
    ) -> Self {
        let env = env(mock, "ghinvite-test");
        let apps = apps.into_iter().map(|(slot, tokens)| {
            let installed = InstalledApp {
                slot,
                app: app(tokens.app_id().get(), UNREACHABLE_APP_BASE),
                user_agent: format!("p1/{slot}"),
            };
            (installed, Arc::new(tokens) as Arc<dyn TokenSource>)
        });
        Self::with_sources(env, apps).expect("static test apps")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt;

    #[tokio::test]
    async fn matches_a_scripted_request() {
        let mock = MockTransport::scripted(vec![Expectation::ok_json(
            Method::Get,
            "https://api.github.test/foo",
            serde_json::json!({"k": "v"}),
        )]);
        let resp = mock
            .send(Request::new(Method::Get, "https://api.github.test/foo"))
            .await
            .unwrap();
        assert_eq!(resp.status, 200);
        assert!(resp.body.starts_with(b"{"));
        mock.assert_exhausted();
    }

    #[tokio::test]
    #[should_panic(expected = "missing required header")]
    async fn fails_when_required_header_absent() {
        let mock = MockTransport::scripted(vec![
            Expectation::ok_json(
                Method::Get,
                "https://api.github.test/",
                serde_json::json!({}),
            )
            .require_header("authorization", "Bearer xyz"),
        ]);
        let _ = mock
            .send(Request::new(Method::Get, "https://api.github.test/"))
            .await;
    }

    #[tokio::test]
    #[should_panic(expected = "script exhausted")]
    async fn panics_on_extra_request() {
        let mock = MockTransport::scripted(vec![]);
        let _ = mock
            .send(Request::new(Method::Get, "https://api.github.test/"))
            .await;
    }

    #[tokio::test]
    async fn mismatch_diagnostics_never_render_auth_body_or_url() {
        let secret = "mismatch-secret-marker";
        for mismatch in 0..3 {
            let url = format!("https://test.example/{secret}");
            let mut expectation = Expectation::status(Method::Post, &url, 200);
            match mismatch {
                0 => expectation.url = "https://test.example/other".into(),
                1 => expectation = expectation.require_header("authorization", "different"),
                _ => expectation = expectation.require_body(b"different".to_vec()),
            }
            assert!(!format!("{expectation:?}").contains(secret));
            let mock = MockTransport::scripted(vec![expectation]);
            let request = Request::new(Method::Post, url)
                .header("authorization", secret)
                .json_body(&secret)
                .unwrap();
            let panic = std::panic::AssertUnwindSafe(mock.send(request))
                .catch_unwind()
                .await
                .unwrap_err();
            let message = panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap();
            assert!(!message.contains(secret));
            assert!(message.contains("withheld"));
        }
    }
}
