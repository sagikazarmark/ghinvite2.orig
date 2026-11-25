//! A GitHub App web app: sign in with GitHub, then see the repositories the
//! App can reach that you can access too.
//!
//! It uses all three credentials a GitHub App has, each for what only it can
//! do:
//!
//! 1. **The user's token, once, at sign-in** ([`octologin`]): who signed in
//!    (`GET /user`), and what they may see: the App's installations they can
//!    access (`GET /user/installations`) and the repositories in each they
//!    can access (`GET /user/installations/{id}/repositories`). The session
//!    keeps the IDs. The reads run inside [`SignIn::with_user_token`], which
//!    revokes the token afterwards, before the callback answers.
//! 2. **An installation token, on every page view** ([`octotoken`]):
//!    `GET /installation/repositories`, filtered to the repositories the
//!    user may see. Without the filter, an organization member would see
//!    every repository the App was granted, private ones they cannot open
//!    included.
//! 3. **The App's credential, at startup** ([`octoauth::App::with_credential`]):
//!    `GET /app`, for the App's name and its installation page.
//!
//! Routes:
//!
//! | Route | |
//! |---|---|
//! | `GET /` | Sign-in link, or the repositories (re-read on every load) |
//! | `GET /login` | Start a sign-in (also "re-check access": GitHub skips the consent screen for a user who already authorized the App) |
//! | `GET /callback` | Finish it: the App's callback URL |
//! | `GET /setup` | After installing or changing the App: the App's setup URL |
//! | `POST /logout` | Sign out |
//!
//! Sessions are in memory ([`session`]). Errors reach the browser as fixed
//! codes only.
#![cfg(not(target_arch = "wasm32"))]

mod html;
pub mod session;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use octoauth_octocrab::{authenticated, is_unauthorized, unauthorized};
use octocrab::Octocrab;
use octologin::{AuthorizeOptions, SignIn, SignInError, UserToken};
use octotoken::{
    Access, AppTokenProvider, InstallationId, Permissions, RepositoryPermission, TokenError,
    TokenScope, octoauth, with_token,
};
use session::{Account, InstallationAccess, LOGIN_COOKIE, SESSION_COOKIE, SessionError, Sessions};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Instant;

/// GitHub's maximum page size.
const MAX_PER_PAGE: u8 = 100;

/// A repository, as the page lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repository {
    /// GitHub's repository ID.
    pub id: u64,
    /// `owner/name`.
    pub full_name: String,
    /// Whether it is private.
    pub private: bool,
    /// Its page on GitHub.
    pub html_url: Option<String>,
}

/// Why a request failed. Every variant prints as a fixed code: none carries
/// upstream text, a token or a credential.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The App could not sign its credential.
    #[error("{}", .0.code())]
    Credential(#[from] octoauth::Error),

    /// No installation token (suspended, uninstalled, rate limited, …).
    #[error("{}", .0.code())]
    Token(#[from] TokenError),

    /// GitHub answered an API call with an error, or did not answer.
    #[error(transparent)]
    GitHub(#[from] octoauth_octocrab::Error),

    /// Signing in failed (state mismatch, code exchange, `GET /user`).
    #[error(transparent)]
    SignIn(#[from] SignInError),

    /// No session could be created.
    #[error(transparent)]
    Session(#[from] SessionError),

    /// The callback came without a pending sign-in: no cookie, expired, or
    /// already used.
    #[error("example:sign_in_expired")]
    SignInExpired,

    /// The user declined on GitHub's consent screen.
    #[error("example:sign_in_declined")]
    SignInDeclined,
}

impl From<octocrab::Error> for Error {
    fn from(error: octocrab::Error) -> Self {
        Self::GitHub(error.into())
    }
}

/// The scope of the installation tokens: `metadata: read`, the least a
/// token carries and enough to list repositories.
pub fn repository_scope() -> TokenScope {
    Permissions::new([(RepositoryPermission::Metadata, Access::Read)])
        .expect("a valid permission set")
        .into()
}

/// The web app; [`router`](Self::router) serves it.
pub struct WebApp {
    tokens: AppTokenProvider,
    sign_in: SignIn,
    github: Octocrab,
    name: String,
    install_url: Option<String>,
    sessions: Sessions,
    secure_cookies: bool,
    per_page: u8,
}

impl WebApp {
    /// `tokens` holds the App, `sign_in` its client ID, secret and callback
    /// URL (`<public URL>/callback`). Reads the App's name with its
    /// credential, so a wrong App ID or key fails here, not on first use.
    pub async fn new(tokens: AppTokenProvider, sign_in: SignIn) -> Result<Self, Error> {
        let github = octoauth_octocrab::builder(tokens.app())?.build()?;
        let about = tokens
            .app()
            .with_credential(unauthorized, |credential| {
                let github = &github;
                async move {
                    authenticated(github, credential.expose_secret())?
                        .current()
                        .app()
                        .await
                }
            })
            .await
            .map_err(|error| error.map(Error::from).into_error())?;
        let install_url = Some(format!(
            "{}/installations/new",
            about.html_url.as_str().trim_end_matches('/')
        ));
        Ok(Self {
            tokens,
            sign_in,
            github,
            name: about.name,
            install_url,
            sessions: Sessions::default(),
            secure_cookies: true,
            per_page: MAX_PER_PAGE,
        })
    }

    /// Whether cookies are `Secure` (default `true`). Turn it off only to
    /// serve plain `http` on localhost.
    pub fn secure_cookies(mut self, secure: bool) -> Self {
        self.secure_cookies = secure;
        self
    }

    /// Page size for GitHub's lists, 1 to 100 (default 100). Smaller pages
    /// only make sense in tests.
    pub fn per_page(mut self, per_page: u8) -> Self {
        self.per_page = per_page.clamp(1, MAX_PER_PAGE);
        self
    }

    /// The routes.
    pub fn router(self) -> Router {
        Router::new()
            .route("/", get(home))
            .route("/login", get(login))
            .route("/callback", get(callback))
            .route("/setup", get(setup))
            .route("/logout", post(logout))
            .with_state(Arc::new(self))
            .layer(axum::middleware::map_response(headers))
    }

    /// What the signed-in user may see, read with their token.
    async fn access(&self, token: &UserToken) -> Result<Vec<InstallationAccess>, Error> {
        let github = authenticated(&self.github, token.expose_secret())?;
        let mut installations = Vec::new();
        // octocrab types this page number as a u8: at most 255 pages.
        for page in 1..=u8::MAX {
            let items = github
                .current()
                .list_app_installations_accessible_to_user()
                .per_page(self.per_page)
                .page(page)
                .send()
                .await?
                .take_items();
            let last = items.len() < usize::from(self.per_page);
            installations.extend(items);
            if last {
                break;
            }
        }
        let mut access = Vec::with_capacity(installations.len());
        for installation in installations {
            let mut repositories = BTreeSet::new();
            let mut listed = 0;
            for page in 1u32.. {
                let listing = github
                    .current()
                    .installation_repositories(installation.id)
                    .per_page(self.per_page)
                    .page(page)
                    .send()
                    .await?;
                listed += listing.repositories.len();
                let empty = listing.repositories.is_empty();
                repositories.extend(listing.repositories.into_iter().map(|r| r.id.0));
                if empty || listed >= listing.total_count {
                    break;
                }
            }
            access.push(InstallationAccess {
                id: InstallationId::new(installation.id.0),
                account: installation.account.login,
                repositories,
            });
        }
        Ok(access)
    }

    /// The repositories `installation` reaches, as the installation.
    async fn repositories(&self, installation: InstallationId) -> Result<Vec<Repository>, Error> {
        let scope = repository_scope();
        let mut repositories = Vec::new();
        for page in 1u32.. {
            let listing = with_token(
                &self.tokens,
                installation,
                &scope,
                is_unauthorized,
                |token| async move {
                    authenticated(&self.github, token.expose_secret())?
                        .apps()
                        .installation_repositories()
                        .per_page(self.per_page)
                        .page(page)
                        .send()
                        .await
                },
            )
            .await
            .map_err(|error| error.map(Error::from).into_error())?;
            let empty = listing.repositories.is_empty();
            repositories.extend(listing.repositories.into_iter().map(|r| Repository {
                id: r.id.0,
                full_name: r.full_name.unwrap_or(r.name),
                private: r.private.unwrap_or(false),
                html_url: r.html_url.map(String::from),
            }));
            if empty || repositories.len() >= listing.total_count {
                break;
            }
        }
        Ok(repositories)
    }

    /// Sign-in after GitHub's callback: verify, exchange, read, revoke.
    async fn finish_sign_in(
        &self,
        pending: Option<octologin::PendingAuthorization>,
        callback: Callback,
    ) -> Result<Account, Error> {
        let pending = pending.ok_or(Error::SignInExpired)?;
        if callback.error.is_some() {
            return Err(Error::SignInDeclined);
        }
        let (Some(code), Some(state)) = (callback.code, callback.state) else {
            return Err(SignInError::InvalidInput.into());
        };
        let verifier = pending.verify(&state)?;
        // The token lives only inside the closure; it is revoked afterwards,
        // whatever the reads did.
        let signed_in = self
            .sign_in
            .with_user_token(&code, &verifier, async |token| {
                let user = self.sign_in.user(token).await?;
                let access = self.access(token).await?;
                Ok::<_, Error>((user, access))
            })
            .await
            .map_err(|error| error.into_error())?;
        // A failed revocation leaves the token to expire (8 hours); the
        // sign-in itself succeeded.
        if let Err(error) = signed_in.revocation {
            eprintln!("could not revoke a user token: {error}");
        }
        let (user, access) = signed_in.value;
        Ok(Account {
            user: user.id,
            login: user.login,
            avatar_url: user.avatar_url,
            access,
            checked: Instant::now(),
        })
    }

    fn page(&self, status: StatusCode, html: String) -> Response {
        (status, Html(html)).into_response()
    }

    fn failure(&self, error: &Error) -> Response {
        let (status, text) = match error {
            Error::SignInExpired => (
                StatusCode::BAD_REQUEST,
                "This sign-in expired or was already used. Please sign in again.",
            ),
            Error::SignInDeclined => (StatusCode::OK, "You did not authorize the App."),
            Error::SignIn(SignInError::StateMismatch | SignInError::InvalidInput) => (
                StatusCode::BAD_REQUEST,
                "This sign-in did not start here. Please sign in again.",
            ),
            Error::Session(SessionError::Full) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "Too many sign-ins at once. Please try again later.",
            ),
            _ => (StatusCode::BAD_GATEWAY, "Signing in failed."),
        };
        self.page(
            status,
            html::message(&self.name, text, Some(&error.to_string())),
        )
    }

    /// `Set-Cookie` for `name`: HttpOnly, SameSite=Lax (sent on GitHub's
    /// redirect back, which is a top-level navigation; Strict would not
    /// be), Secure unless turned off.
    fn cookie(&self, name: &str, value: &str, max_age: u64) -> HeaderValue {
        let secure = if self.secure_cookies { "; Secure" } else { "" };
        HeaderValue::try_from(format!(
            "{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}"
        ))
        .expect("cookie names and base64url IDs are valid header values")
    }
}

type AppState = State<Arc<WebApp>>;

/// The value of cookie `name` in the request.
fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find_map(|(key, value)| (key == name).then_some(value))
}

async fn home(State(app): AppState, headers: HeaderMap) -> Response {
    let Some(account) = cookie(&headers, SESSION_COOKIE).and_then(|id| app.sessions.account(id))
    else {
        return app.page(StatusCode::OK, html::signed_out(&app.name));
    };
    let mut installations = Vec::with_capacity(account.access.len());
    for access in &account.access {
        // Fresh on every load: a repository the App lost is gone at once.
        // One installation failing (suspended, say) does not hide the rest.
        let listing = match app.repositories(access.id).await {
            Ok(repositories) => html::Listing::Repositories(
                repositories
                    .into_iter()
                    .filter(|repository| access.repositories.contains(&repository.id))
                    .collect(),
            ),
            Err(error) => html::Listing::Failed(error.to_string()),
        };
        installations.push((access.account.clone(), listing));
    }
    app.page(
        StatusCode::OK,
        html::signed_in(
            &app.name,
            app.install_url.as_deref(),
            &account,
            &installations,
        ),
    )
}

async fn login(State(app): AppState, headers: HeaderMap) -> Response {
    // One sign-in in flight per browser: a newer one replaces the older.
    if let Some(previous) = cookie(&headers, LOGIN_COOKIE) {
        app.sessions.remove(previous);
    }
    let started = app
        .sign_in
        .authorize(AuthorizeOptions::default())
        .map_err(Error::from)
        .and_then(|(url, pending)| Ok((url, app.sessions.start(pending)?)));
    let (url, id) = match started {
        Ok(started) => started,
        Err(error) => return app.failure(&error),
    };
    let mut response = Redirect::to(url.as_str()).into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        app.cookie(LOGIN_COOKIE, &id, session::LOGIN_TTL.as_secs()),
    );
    response
}

/// GitHub's redirect to the callback URL.
#[derive(Debug, serde::Deserialize)]
struct Callback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    setup_action: Option<String>,
}

async fn callback(
    State(app): AppState,
    headers: HeaderMap,
    Query(callback): Query<Callback>,
) -> Response {
    // With "Request user authorization (OAuth) during installation" on,
    // GitHub sends the installer here with a code but no state: nothing
    // proves this browser started that authorization, so the code is not
    // exchanged. A normal sign-in follows instead (no consent screen, the
    // user just authorized).
    if callback.state.is_none() && callback.setup_action.is_some() {
        return after_setup(&app, callback.setup_action.as_deref());
    }
    let pending = cookie(&headers, LOGIN_COOKIE).and_then(|id| app.sessions.take_pending(id));
    let mut response = match app.finish_sign_in(pending, callback).await {
        Ok(account) => match app.sessions.sign_in(account) {
            Ok(id) => {
                if let Some(previous) = cookie(&headers, SESSION_COOKIE) {
                    app.sessions.remove(previous);
                }
                let mut response = Redirect::to("/").into_response();
                response.headers_mut().append(
                    header::SET_COOKIE,
                    app.cookie(SESSION_COOKIE, &id, session::SESSION_TTL.as_secs()),
                );
                response
            }
            Err(error) => app.failure(&error.into()),
        },
        Err(error) => app.failure(&error),
    };
    response
        .headers_mut()
        .append(header::SET_COOKIE, app.cookie(LOGIN_COOKIE, "", 0));
    response
}

/// GitHub's redirect to the setup URL, after an installation is created,
/// changed or requested.
#[derive(Debug, serde::Deserialize)]
struct Setup {
    setup_action: Option<String>,
}

async fn setup(State(app): AppState, Query(setup): Query<Setup>) -> Response {
    after_setup(&app, setup.setup_action.as_deref())
}

/// The `installation_id` GitHub adds is not used: anyone can put any ID in
/// the URL. What changed is read back from GitHub by signing in again.
fn after_setup(app: &WebApp, action: Option<&str>) -> Response {
    match action {
        Some("request") => app.page(
            StatusCode::OK,
            html::message(
                &app.name,
                "Installation requested. An owner of the account has to approve it.",
                None,
            ),
        ),
        _ => Redirect::to("/login").into_response(),
    }
}

/// `SameSite=Lax` keeps other sites from posting here with the cookie.
async fn logout(State(app): AppState, headers: HeaderMap) -> Response {
    if let Some(id) = cookie(&headers, SESSION_COOKIE) {
        app.sessions.remove(id);
    }
    let mut response = Redirect::to("/").into_response();
    response
        .headers_mut()
        .append(header::SET_COOKIE, app.cookie(SESSION_COOKIE, "", 0));
    response
}

/// Pages show private repository names, and `/login`'s redirect carries the
/// state: nothing is cached. No scripts, no framing, forms post here only.
async fn headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; img-src https:; style-src 'unsafe-inline'; \
             form-action 'self'; frame-ancestors 'none'; base-uri 'none'",
        ),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}
