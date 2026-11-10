//! Signing a browser in through the real `/login` → `/oauth/callback` routes.
//!
//! Tests obtain a session the way a browser does, never by writing session
//! records. Sign-in is typed (octologin, with its own HTTP client): unless a
//! test configures its own sign-in app, [`super::build_app`] points it at a
//! local [`signin_bridge`], which hands each request to the test's scripted
//! GitHub under GitHub's own URL. So one script answers the token exchange,
//! the `/user` lookup and the token's revocation ([`oauth_expectations`]) as
//! well as the installation reads.
#![allow(dead_code)] // Not every test binary that shares these helpers uses all of them.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use ghinvite_core::storage::{AppInstallationReadStorage, AppInstallationStorage};
use ghinvite_core::{Account, AccountType, SelectedRepos};
use ghinvite_github::mocks::{Expectation, MockTransport};
use ghinvite_github::{Method, Response};
use ghinvite_web::config::apps::SignInApp;
use ghinvite_web::{AppState, RestateClient, WebConfig};
use std::sync::Arc;
use tower::ServiceExt;

/// The sign-in app's client ID in tests: tokens are revoked at
/// `DELETE https://api.github.com/applications/Iv1.signin/token`.
pub const SIGNIN_CLIENT_ID: &str = "Iv1.signin";

/// The GitHub account the scripted OAuth exchange signs in as.
#[derive(Clone, Copy, Debug)]
pub struct GithubUser<'a> {
    pub login: &'a str,
    pub id: u64,
    /// The user token the exchange answers with (and sign-in revokes).
    pub token: &'a str,
    /// Legacy caller label only. Authority responses must be scripted explicitly
    /// using installation credentials; OAuth grants no organization authority.
    pub admin_of: Option<(&'a str, u64)>,
}

impl<'a> GithubUser<'a> {
    pub const fn new(login: &'a str, id: u64) -> Self {
        Self {
            login,
            id,
            token: "ghu_xxx",
            admin_of: None,
        }
    }

    pub const fn with_token(self, token: &'a str) -> Self {
        Self { token, ..self }
    }

    pub const fn admin_of(self, organization: &'a str, organization_id: u64) -> Self {
        Self {
            admin_of: Some((organization, organization_id)),
            ..self
        }
    }
}

pub const OCTOCAT: GithubUser<'static> = GithubUser::new("octocat", 42);

/// `octocat`, an active admin of the `acme` organization.
pub const ACME_ADMIN: GithubUser<'static> = OCTOCAT.admin_of("acme", 9001);

/// The GitHub calls one sign-in makes, in order: the token exchange, the
/// `/user` lookup and the token's revocation. Installation authority is
/// verified independently.
pub fn oauth_expectations(user: GithubUser<'_>) -> Vec<Expectation> {
    vec![
        Expectation::ok_json(
            Method::Post,
            "https://github.com/login/oauth/access_token",
            serde_json::json!({"access_token": user.token, "token_type": "bearer"}),
        ),
        Expectation::ok_json(
            Method::Get,
            "https://api.github.com/user",
            serde_json::json!({"id": user.id, "login": user.login}),
        ),
        Expectation::status(
            Method::Delete,
            format!("https://api.github.com/applications/{SIGNIN_CLIENT_ID}/token"),
            204,
        ),
    ]
}

/// Typed sign-in against a local stand-in that forwards each request to
/// `github` with GitHub's own URL (`https://github.com/login/...` for the
/// web flow, `https://api.github.com/...` otherwise), its body and its
/// headers, and answers with whatever `github` answers. A transport error
/// is a 502. The stand-in serves for the rest of the test's runtime.
pub fn signin_bridge(github: Arc<dyn ghinvite_github::HttpTransportDyn>) -> Arc<SignInApp> {
    use axum::response::IntoResponse;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let listener = tokio::net::TcpListener::from_std(listener).unwrap();
    let forward = move |request: axum::extract::Request| {
        let github = github.clone();
        async move {
            let (parts, body) = request.into_parts();
            let body = axum::body::to_bytes(body, 1 << 20).await.unwrap();
            let method = match parts.method.as_str() {
                "GET" => Method::Get,
                "POST" => Method::Post,
                "PATCH" => Method::Patch,
                "PUT" => Method::Put,
                "DELETE" => Method::Delete,
                other => panic!("sign-in bridge: unexpected method {other}"),
            };
            let target = parts.uri.path_and_query().unwrap().as_str();
            let origin = if target.starts_with("/login/") {
                "https://github.com"
            } else {
                "https://api.github.com"
            };
            let mut forwarded = ghinvite_github::Request::new(method, format!("{origin}{target}"));
            for (name, value) in &parts.headers {
                if !matches!(
                    name.as_str(),
                    "host" | "content-length" | "connection" | "accept-encoding"
                ) {
                    forwarded = forwarded.header(name.as_str(), value.to_str().unwrap());
                }
            }
            forwarded.body = (!body.is_empty()).then(|| body.to_vec());
            match github.send(forwarded).await {
                Ok(answer) => {
                    let mut response = axum::response::Response::new(Body::from(answer.body));
                    *response.status_mut() = StatusCode::from_u16(answer.status).unwrap();
                    for (name, value) in answer.headers {
                        response.headers_mut().insert(
                            axum::http::HeaderName::try_from(name).unwrap(),
                            value.parse().unwrap(),
                        );
                    }
                    response
                }
                Err(_) => StatusCode::BAD_GATEWAY.into_response(),
            }
        }
    };
    tokio::spawn(async move {
        axum::serve(listener, axum::Router::new().fallback(forward))
            .await
            .unwrap();
    });
    let mut app = SignInApp::new(
        SIGNIN_CLIENT_ID,
        "signin-test-secret",
        "https://p1.test/oauth/callback".parse().unwrap(),
    );
    app.web_base_url = Some(base.clone());
    app.api_base_url = Some(base);
    Arc::new(app)
}

/// The session cookie a response sets, or `fallback` when it sets none.
pub fn session_cookie(response: &axum::response::Response, fallback: Option<String>) -> String {
    response
        .headers()
        .get("set-cookie")
        .map(|value| {
            value
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_string()
        })
        .or(fallback)
        .expect("session cookie available")
}

/// The OAuth `state` parameter of a GitHub authorize redirect.
pub fn state_from_location(location: &str) -> &str {
    location
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
}

/// Drive `/login` → `/oauth/callback` and return the signed-in cookie.
pub async fn sign_in(app: &axum::Router) -> String {
    sign_in_with_cookie(app, None).await
}

/// Sign in again from a browser that already holds `cookie`.
pub async fn sign_in_with_cookie(app: &axum::Router, cookie: Option<String>) -> String {
    let mut request = Request::builder().uri("/login");
    if let Some(cookie) = &cookie {
        request = request.header("cookie", cookie);
    }
    let started = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(started.status(), StatusCode::SEE_OTHER);
    let cookie = session_cookie(&started, cookie);
    let location = started.headers().get("location").unwrap().to_str().unwrap();
    let state = state_from_location(location);

    let finished = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/oauth/callback?code=test-code&state={state}"))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(finished.status(), StatusCode::SEE_OTHER);
    session_cookie(&finished, Some(cookie))
}

/// The `acme` organization installation most Console tests administer.
pub fn acme_installation() -> Account {
    Account {
        installation_id: 77,
        account_id: 9001,
        account_login: "acme".into(),
        account_type: AccountType::Organization,
        installed_at: Utc::now(),
        uninstalled_at: None,
        selected_repos: SelectedRepos::All,
    }
}

/// A Restate ingress nothing listens on, for tests that never reach it.
pub fn unreachable_restate() -> Arc<RestateClient> {
    Arc::new(RestateClient::new("http://127.0.0.1:9").unwrap())
}

/// An app over in-memory storage holding `installations`, with GitHub
/// scripted by `expectations` (sign-in first, then whatever the test drives)
/// and Restate reached through `restate`, already signed in.
pub async fn signed_in_app(
    installations: &[Account],
    expectations: Vec<Expectation>,
    restate: Arc<RestateClient>,
) -> (axum::Router, String) {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    for installation in installations {
        storage
            .seed_console_installation(installation)
            .await
            .unwrap();
    }
    let state = app_state(
        storage,
        Arc::new(ConsoleTransport::scripted(expectations)),
        restate,
        console_config(),
    );
    let app = super::build_app(state, tower_sessions::MemoryStore::default());
    let cookie = sign_in(&app).await;
    (app, cookie)
}

/// `state`, signing in through a [`signin_bridge`] to its own scripted
/// GitHub (`state.github_transport`) unless it has a sign-in app already.
pub fn with_signin_bridge(mut state: AppState) -> AppState {
    if state.config.signin_apps.is_none() {
        state.config.signin_apps = Some(signin_bridge(state.github_transport.clone()));
    }
    state
}

/// An app state whose GitHub is `github`: the installed apps (with fixed
/// installation tokens, see [`super::apps::github`]) and avatars call it,
/// and so does sign-in, once [`super::build_app`] adds a [`signin_bridge`].
pub fn app_state(
    storage: Arc<dyn ghinvite_web::WebStorage>,
    github: Arc<dyn ghinvite_github::HttpTransportDyn>,
    restate: Arc<RestateClient>,
    config: WebConfig,
) -> AppState {
    AppState::new(storage, super::apps::github(github), restate, config)
}

/// `state` with its GitHub (installed apps, avatars, sign-in) replaced by
/// `github`, as [`app_state`] builds it.
pub fn with_github(
    mut state: AppState,
    github: Arc<dyn ghinvite_github::HttpTransportDyn>,
) -> AppState {
    let replaced = super::apps::github(github);
    state.github_transport = replaced.transport;
    state.github_apps = replaced.apps;
    state
}

/// Scripted GitHub matched by method and URL, in any order across
/// resources. Installation reads must carry a fixed installation token
/// ([`super::apps::github`]); none of them creates a token.
pub struct ConsoleTransport {
    expectations: std::sync::Mutex<Vec<Expectation>>,
}
impl ConsoleTransport {
    pub fn scripted(expectations: Vec<Expectation>) -> Self {
        Self {
            expectations: std::sync::Mutex::new(expectations),
        }
    }
}
impl ghinvite_github::HttpTransport for ConsoleTransport {
    async fn send(
        &self,
        request: ghinvite_github::Request,
    ) -> std::result::Result<Response, ghinvite_github::TransportError> {
        assert!(
            !request.url.ends_with("/access_tokens"),
            "installation tokens come from the fixed token source"
        );
        if request.url.contains("/repos/")
            || request.url.contains("/installation/")
            || request.url.contains("/orgs/")
        {
            assert!(
                request
                    .headers
                    .get("authorization")
                    .is_some_and(|value| value.starts_with("Bearer ghs_fixed_")),
                "authority reads use installation credentials"
            );
        }
        // Match independent resources by URL; repeated reads of one resource
        // consume their scripted responses in order (failure then recovery).
        let next = {
            let mut values = self.expectations.lock().unwrap();
            values
                .iter()
                .position(|e| e.method == request.method && e.url == request.url)
                .map(|i| values.remove(i))
        };
        let Some(next) = next else {
            return Err(ghinvite_github::TransportError::SentNoResponse { timed_out: false });
        };
        MockTransport::scripted(vec![next]).send(request).await
    }
}
pub fn console_config() -> WebConfig {
    WebConfig::for_local_dev_with_secret([7; 32])
}
/// Projection fixture representing a signed installer event already processed.
/// This does not run setup or manufacture authorization cache entries.
pub trait ConsoleInstallationFixture {
    fn seed_console_installation(
        &self,
        account: &Account,
    ) -> impl std::future::Future<Output = ghinvite_core::storage::Result<()>> + Send;
    fn uninstall_console_installation(
        &self,
        installation_id: u64,
        at: chrono::DateTime<Utc>,
    ) -> impl std::future::Future<Output = ghinvite_core::storage::Result<()>> + Send;
}
impl ConsoleInstallationFixture for ghinvite_storage_sqlx::SqlxStorage {
    async fn seed_console_installation(
        &self,
        account: &Account,
    ) -> ghinvite_core::storage::Result<()> {
        let mut value = ghinvite_core::account::Installation::repo(account.clone());
        value.installed_by_user_id = Some(42);
        value.installed_by_login = Some("octocat".into());
        value.permissions.insert(
            "metadata".into(),
            ghinvite_core::account::InstallationPermission::Read,
        );
        value.permissions.insert(
            "administration".into(),
            ghinvite_core::account::InstallationPermission::Write,
        );
        self.put_app_installation(&value).await
    }
    async fn uninstall_console_installation(
        &self,
        id: u64,
        at: chrono::DateTime<Utc>,
    ) -> ghinvite_core::storage::Result<()> {
        let mut value = self
            .app_installation(ghinvite_core::account::App::Repo, id)
            .await?
            .ok_or(ghinvite_core::storage::Error::NotFound)?;
        value.revision += 1;
        value.account.uninstalled_at = Some(at);
        self.put_app_installation(&value).await
    }
}
pub async fn seed_org_authority(storage: &ghinvite_storage_sqlx::SqlxStorage, account: &Account) {
    let mut value = ghinvite_core::account::Installation::repo(account.clone());
    value.app = ghinvite_core::account::App::Org;
    value.permissions.insert(
        "members".into(),
        ghinvite_core::account::InstallationPermission::Read,
    );
    storage.put_app_installation(&value).await.unwrap();
}
pub fn repo_identity(id: u64, full_name: &str, account_id: u64) -> Expectation {
    Expectation::ok_json(
        Method::Get,
        format!("https://api.github.com/repos/{full_name}"),
        serde_json::json!({"id":id,"full_name":full_name,"owner":{"id":account_id,"login":full_name.split('/').next().unwrap()}}),
    )
}
pub fn repo_admin(full_name: &str, user_id: u64, login: &str) -> Expectation {
    Expectation::ok_json(
        Method::Get,
        format!("https://api.github.com/repos/{full_name}/collaborators/{login}/permission"),
        serde_json::json!({"user":{"id":user_id,"login":login},"role_name":"admin"}),
    )
}
