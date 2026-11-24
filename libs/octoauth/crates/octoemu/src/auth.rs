//! Who is calling, and whether they may: every route declares an [`Auth`]
//! when it is registered (`Api::route`), and the route middleware verifies
//! the `Authorization` header against it before the handler runs.
//!
//! - The credential is verified first, on every route: a bad, expired or
//!   revoked credential is a 401 even where none is needed.
//! - Then its kind: an App JWT, an installation token, a user token (a
//!   GitHub App's `ghu_…` or a classic PAT `ghp_…`), an App's client
//!   credentials (`Basic`), or none.
//! - Then, for installation and user tokens, the route's permission: without
//!   it, a 403 `Resource not accessible by integration` with
//!   `X-Accepted-GitHub-Permissions` (documented).
//!
//! Handlers read the verified caller with the [`AppCaller`],
//! [`InstallationCaller`], [`UserCaller`], [`ClientCaller`] and
//! [`AnyCaller`] extractors.
#![allow(
    clippy::result_large_err,
    reason = "a rejection is the response to send, built once per request"
)]

use crate::calls::Identity;
use crate::jwt::{self, JwtError};
use crate::response::{self, REST_DOCS};
use crate::seed::{Access, Installation, Repo, Seed};
use crate::state::Shared;
use crate::tokens::{Rejected, TokenRecord};
use crate::user_tokens::{Grant, UserTokenRecord};
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header::AUTHORIZATION};
use axum::response::Response;
use std::collections::BTreeMap;

/// What a route accepts. Part of every route's registration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Auth {
    /// Anyone; a credential, if sent, must still be valid.
    Public,
    /// An App's JWT (`Authorization: Bearer <jwt>`).
    App,
    /// An installation token, holding `permission` when one is named.
    Installation(Option<Permission>),
    /// A user token: a GitHub App's user access token whose App holds
    /// `permission` (as a user or a repository permission), or a classic
    /// personal access token with a scope that grants it.
    User(Option<Permission>),
    /// An installation token or a user token, each checked as above.
    Token(Option<Permission>),
    /// An App's client credentials (`Authorization: Basic
    /// base64(client_id:client_secret)`).
    Client,
}

/// A permission at a level, by GitHub's names (`metadata`, `members`, …).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Permission {
    pub(crate) name: &'static str,
    pub(crate) access: Access,
}

impl Permission {
    pub(crate) const fn new(name: &'static str, access: Access) -> Self {
        Self { name, access }
    }

    /// `name=level`, the `X-Accepted-GitHub-Permissions` syntax.
    fn header_value(self) -> String {
        format!("{}={}", self.name, self.access)
    }
}

/// The verified caller of a request.
#[derive(Clone, Debug)]
pub(crate) enum Caller {
    /// No credential.
    Anonymous,
    /// An App, by its JWT.
    App { app_id: u64 },
    /// An installation token, as minted.
    Installation(TokenRecord),
    /// A user access token or personal access token, as minted.
    User(UserTokenRecord),
    /// An App, by its client ID and secret.
    Client { app_id: u64 },
}

impl Caller {
    /// What the call log records: identities, never secrets.
    pub(crate) fn identity(&self) -> Option<Identity> {
        match self {
            Self::Anonymous => None,
            Self::App { app_id } => Some(Identity::App { app_id: *app_id }),
            Self::Installation(token) => Some(Identity::Installation {
                app_id: token.app_id,
                installation_id: token.installation_id,
                token_id: token.id.clone(),
            }),
            Self::User(token) => Some(match token.grant {
                Grant::UserToken { app_id } => Identity::UserToken {
                    app_id,
                    user_id: token.user_id,
                    login: token.login.clone(),
                    token_id: token.id.clone(),
                },
                Grant::PersonalAccessToken { .. } => Identity::PersonalAccessToken {
                    user_id: token.user_id,
                    login: token.login.clone(),
                    token_id: token.id.clone(),
                },
            }),
            Self::Client { app_id } => Some(Identity::Client { app_id: *app_id }),
        }
    }
}

/// The App behind a verified JWT, for handlers of [`Auth::App`] routes.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AppCaller(pub(crate) u64);

/// The token of a verified installation token, for handlers of
/// [`Auth::Installation`] routes.
#[derive(Clone, Debug)]
pub(crate) struct InstallationCaller(pub(crate) TokenRecord);

/// The token of a verified user token, for handlers of [`Auth::User`]
/// routes.
#[derive(Clone, Debug)]
pub(crate) struct UserCaller(pub(crate) UserTokenRecord);

/// The App behind verified client credentials, for handlers of
/// [`Auth::Client`] routes.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ClientCaller(pub(crate) u64);

/// Whoever the route admitted, for handlers of [`Auth::Token`] and
/// [`Auth::Public`] routes.
#[derive(Clone, Debug)]
pub(crate) struct AnyCaller(pub(crate) Caller);

/// A handler extracted a caller kind its route does not admit: a
/// registration bug.
fn misdeclared() -> Response {
    response::error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "octoemu: the route's Auth does not admit this caller",
        response::OCTOEMU_DOCS,
    )
}

impl<S: Send + Sync> FromRequestParts<S> for AppCaller {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Response> {
        match parts.extensions.get::<Caller>() {
            Some(Caller::App { app_id }) => Ok(Self(*app_id)),
            _ => Err(misdeclared()),
        }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for InstallationCaller {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Response> {
        match parts.extensions.get::<Caller>() {
            Some(Caller::Installation(token)) => Ok(Self(token.clone())),
            _ => Err(misdeclared()),
        }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for UserCaller {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Response> {
        match parts.extensions.get::<Caller>() {
            Some(Caller::User(token)) => Ok(Self(token.clone())),
            _ => Err(misdeclared()),
        }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for ClientCaller {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Response> {
        match parts.extensions.get::<Caller>() {
            Some(Caller::Client { app_id }) => Ok(Self(*app_id)),
            _ => Err(misdeclared()),
        }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for AnyCaller {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Response> {
        Ok(Self(
            parts
                .extensions
                .get::<Caller>()
                .cloned()
                .unwrap_or(Caller::Anonymous),
        ))
    }
}

/// A 401 in GitHub's shape.
fn unauthorized(message: &str) -> Response {
    response::error(StatusCode::UNAUTHORIZED, message, REST_DOCS)
}

/// GitHub's answer to an invalid, expired or revoked token (documented).
pub(crate) const BAD_CREDENTIALS: &str = "Bad credentials";

/// GitHub's answer to a request without credentials on a route that needs
/// them (documented as the 401's description).
pub(crate) const REQUIRES_AUTHENTICATION: &str = "Requires authentication";

/// GitHub's answer to a token without the route's permission (documented).
pub(crate) const NOT_ACCESSIBLE: &str = "Resource not accessible by integration";

/// GitHub's message when minting a token for a suspended installation
/// (measured, behaviour `suspended-installation`).
pub(crate) const SUSPENDED: &str = "This installation has been suspended";

/// GitHub's message for a token used while its installation is suspended
/// (measured, behaviour `suspended-installation-tokens`).
pub(crate) const SUSPENDED_TOKEN: &str = "This GitHub App installation is currently suspended.";

/// The 403 for a token without `permission`.
pub(crate) fn forbidden(permission: Permission) -> Response {
    let mut response = response::error(StatusCode::FORBIDDEN, NOT_ACCESSIBLE, REST_DOCS);
    if let Ok(value) = HeaderValue::from_str(&permission.header_value()) {
        response.headers_mut().insert(
            HeaderName::from_static("x-accepted-github-permissions"),
            value,
        );
    }
    response
}

/// The `Authorization` header split into scheme (lowercased) and
/// credential; `None` without one.
fn authorization(headers: &HeaderMap) -> Option<Result<(String, String), ()>> {
    let value = headers.get(AUTHORIZATION)?;
    let Ok(value) = value.to_str() else {
        return Some(Err(()));
    };
    let Some((scheme, credential)) = value.trim().split_once(' ') else {
        return Some(Err(()));
    };
    Some(Ok((
        scheme.to_ascii_lowercase(),
        credential.trim().to_owned(),
    )))
}

/// Looks like a JWT: three dot-separated parts, starting with a JSON
/// header's `eyJ`.
fn jwt_shaped(credential: &str) -> bool {
    credential.starts_with("eyJ") && credential.split('.').count() == 3
}

/// Verifies the request's credential, whatever the route; `route` is the
/// route's [`Auth`] when known (it picks the text of some rejections).
pub(crate) fn authenticate(
    shared: &Shared,
    headers: &HeaderMap,
    route: Option<Auth>,
) -> Result<Caller, Response> {
    let Some(header) = authorization(headers) else {
        return Ok(Caller::Anonymous);
    };
    let Ok((scheme, credential)) = header else {
        return Err(unauthorized(BAD_CREDENTIALS));
    };
    match scheme.as_str() {
        "bearer" if jwt_shaped(&credential) => app_jwt(shared, &credential),
        // Documented: an App's JWT goes in a Bearer header. With `token`
        // GitHub reads it as a token, which matches none; on a JWT route
        // it answers as for any credential that is not a JWT (measured).
        "token" if jwt_shaped(&credential) => Err(unauthorized(&match route {
            Some(Auth::App) => {
                shared.message("jwt-error-messages", JwtError::Undecodable.message())
            }
            _ => BAD_CREDENTIALS.into(),
        })),
        "bearer" | "token" if credential.starts_with("ghs_") => {
            installation_token(shared, &credential)
        }
        "bearer" | "token" if credential.starts_with("ghu_") || credential.starts_with("ghp_") => {
            user_token(shared, &credential)
        }
        "basic" => client_credentials(shared, &credential),
        // Anything else (OAuth App tokens, fine-grained PATs, refresh
        // tokens) matches no credential.
        _ => Err(unauthorized(BAD_CREDENTIALS)),
    }
}

/// A `ghu_` or `ghp_` token: looked up on every call; unknown, expired or
/// revoked is 401 `Bad credentials` (documented).
fn user_token(shared: &Shared, credential: &str) -> Result<Caller, Response> {
    let state = shared.state();
    match state.user_tokens.verify(credential, state.clock.now()) {
        Ok(token) => Ok(Caller::User(token.clone())),
        Err(_) => Err(unauthorized(BAD_CREDENTIALS)),
    }
}

/// `Basic base64(client_id:client_secret)`: an App's client credentials.
/// Anything that is not a seeded App's pair is a 401 (behaviour
/// `client-credential-errors`).
fn client_credentials(shared: &Shared, credential: &str) -> Result<Caller, Response> {
    use base64::Engine as _;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(credential)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok());
    let app = decoded.as_deref().and_then(|pair| {
        let (client_id, secret) = pair.split_once(':')?;
        let state = shared.state();
        state
            .world
            .apps
            .iter()
            .find(|app| app.client_id == client_id && app.client_secret == secret)
            .map(|app| app.id)
    });
    match app {
        Some(app_id) => Ok(Caller::Client { app_id }),
        None => Err(shared
            .refuse("client-credential-errors")
            .unwrap_or_else(|| unauthorized(BAD_CREDENTIALS))),
    }
}

fn app_jwt(shared: &Shared, credential: &str) -> Result<Caller, Response> {
    let verified = {
        let state = shared.state();
        jwt::verify(&state.world, credential, state.clock.now().timestamp())
    };
    match verified {
        Ok(verified) => {
            let assumed = [
                (verified.used_iat_tolerance, "jwt-iat-tolerance"),
                (verified.exp_beyond_iat, "jwt-exp-after-iat"),
            ];
            for (relied_on, behaviour) in assumed {
                if relied_on && let Some(refusal) = shared.refuse(behaviour) {
                    return Err(refusal);
                }
            }
            Ok(Caller::App {
                app_id: verified.app_id,
            })
        }
        Err(error) => Err(unauthorized(
            &shared.message("jwt-error-messages", error.message()),
        )),
    }
}

fn installation_token(shared: &Shared, credential: &str) -> Result<Caller, Response> {
    let outcome = {
        let state = shared.state();
        let now = state.clock.now();
        state.tokens.verify(credential, now).cloned().map(|token| {
            let installation = find_installation(&state.world, token.installation_id)
                .map(|installation| installation.suspended_at.is_some());
            (token, installation)
        })
    };
    match outcome {
        // Revoked with `DELETE /installation/token`, still propagating.
        Ok((token, Some(false))) if token.revoked => {
            match shared.refuse("token-revocation-propagation") {
                Some(refusal) => Err(refusal),
                None => Ok(Caller::Installation(token)),
            }
        }
        Ok((token, Some(false))) => Ok(Caller::Installation(token)),
        // Suspended since the token was minted.
        Ok((_, Some(true))) => Err(shared
            .refuse("suspended-installation-tokens")
            .unwrap_or_else(|| {
                response::error(
                    StatusCode::FORBIDDEN,
                    &shared.message("suspended-installation-tokens", SUSPENDED_TOKEN),
                    REST_DOCS,
                )
            })),
        Err(Rejected::Revoked) => Err(shared
            .refuse("token-revocation-propagation")
            .unwrap_or_else(|| unauthorized(BAD_CREDENTIALS))),
        // Uninstalled since: whether a stateless token outlives that at
        // GitHub is unmeasured.
        Ok((_, None)) | Err(Rejected::Uninstalled) => Err(shared
            .refuse("token-revocation-immediate")
            .unwrap_or_else(|| unauthorized(BAD_CREDENTIALS))),
        Err(Rejected::Unknown | Rejected::Expired) => Err(unauthorized(BAD_CREDENTIALS)),
    }
}

/// Checks `caller` against the route's `auth`.
pub(crate) fn admit(shared: &Shared, auth: Auth, caller: &Caller) -> Result<(), Response> {
    let wrong_kind = |message: &str| {
        Err(shared
            .refuse("wrong-credential-kind")
            .unwrap_or_else(|| unauthorized(message)))
    };
    match (auth, caller) {
        (Auth::Public, _) | (Auth::App, Caller::App { .. }) => Ok(()),
        (Auth::Installation(need), Caller::Installation(token)) => match need {
            Some(permission) if !token.has(permission.name, permission.access) => {
                Err(forbidden(permission))
            }
            _ => Ok(()),
        },
        (Auth::Token(need), Caller::Installation(token)) => match need {
            Some(permission) if !token.has(permission.name, permission.access) => {
                Err(forbidden(permission))
            }
            _ => Ok(()),
        },
        (Auth::User(need) | Auth::Token(need), Caller::User(token)) => match need {
            Some(permission) => user_permission(shared, permission, token),
            None => Ok(()),
        },
        (Auth::Client, Caller::Client { .. }) => Ok(()),
        (_, Caller::Anonymous) => Err(unauthorized(REQUIRES_AUTHENTICATION)),
        (Auth::App, _) => wrong_kind(JwtError::Undecodable.message()),
        (Auth::Installation(_) | Auth::User(_) | Auth::Token(_) | Auth::Client, _) => {
            wrong_kind(BAD_CREDENTIALS)
        }
    }
}

/// The permissions a GitHub App's user access token holds: the App's
/// account (user) permissions and its repository and organization
/// permissions (narrowed further by the user's own access on resource
/// routes).
pub(crate) fn user_token_permissions(world: &Seed, app_id: u64) -> BTreeMap<String, Access> {
    let Some(app) = world.apps.iter().find(|app| app.id == app_id) else {
        return BTreeMap::new();
    };
    let mut held = app.permissions.clone();
    for (name, access) in &app.user_permissions {
        let entry = held.entry(name.clone()).or_insert(*access);
        *entry = (*entry).max(*access);
    }
    held
}

/// The classic PAT scopes that grant `permission` (behaviour `pat-scopes`):
/// only email addresses are checked; every other route admits any PAT.
fn scopes_for(permission: &str) -> &'static [&'static str] {
    match permission {
        "email_addresses" => &["user", "user:email"],
        _ => &[],
    }
}

/// Checks a user token against the route's permission.
fn user_permission(
    shared: &Shared,
    permission: Permission,
    token: &UserTokenRecord,
) -> Result<(), Response> {
    match &token.grant {
        Grant::UserToken { app_id } => {
            let held = user_token_permissions(&shared.state().world, *app_id);
            if held
                .get(permission.name)
                .is_some_and(|access| *access >= permission.access)
            {
                Ok(())
            } else {
                Err(forbidden(permission))
            }
        }
        Grant::PersonalAccessToken { scopes } => {
            let accepted = scopes_for(permission.name);
            if accepted.is_empty() || scopes.iter().any(|s| accepted.contains(&s.as_str())) {
                return Ok(());
            }
            if let Some(refusal) = shared.refuse("pat-scopes") {
                return Err(refusal);
            }
            let mut response = response::error(
                StatusCode::FORBIDDEN,
                "Resource not accessible by personal access token",
                REST_DOCS,
            );
            if let Ok(value) = HeaderValue::from_str(&accepted.join(", ")) {
                response
                    .headers_mut()
                    .insert(HeaderName::from_static("x-accepted-oauth-scopes"), value);
            }
            if let Ok(value) = HeaderValue::from_str(&scopes.join(", ")) {
                response
                    .headers_mut()
                    .insert(HeaderName::from_static("x-oauth-scopes"), value);
            }
            Err(response)
        }
    }
}

/// The installation with `id`.
pub(crate) fn find_installation(world: &Seed, id: u64) -> Option<&Installation> {
    world.installations.iter().find(|i| i.id == id)
}

/// Whether `repo` is one of `token`'s repositories: its installation can
/// access it and the token was not narrowed away from it.
pub(crate) fn in_scope(world: &Seed, token: &TokenRecord, repo: &Repo) -> bool {
    find_installation(world, token.installation_id).is_some_and(|installation| {
        crate::apps::installation_repos(world, installation)
            .iter()
            .any(|r| r.id == repo.id)
            && token.covers(repo.id)
    })
}

/// The private-resource rule (documented): a repository the caller cannot
/// see is a 404, never a 403, so its existence does not leak. `Ok` with the
/// repository when `token` reaches it (it is in the token's repositories)
/// or it is public; otherwise the 404 to send. Private and internal
/// repositories are both hidden.
pub(crate) fn visible_repo<'a>(
    world: &'a Seed,
    token: &TokenRecord,
    owner: &str,
    name: &str,
) -> Result<&'a Repo, Response> {
    let repo = world
        .repos
        .iter()
        .find(|r| r.owner.eq_ignore_ascii_case(owner) && r.name.eq_ignore_ascii_case(name));
    let reachable = |repo: &Repo| !repo.is_private() || in_scope(world, token, repo);
    match repo {
        Some(repo) if reachable(repo) => Ok(repo),
        _ => Err(not_found()),
    }
}

/// [`visible_repo`] for a user token (documented: a user access token
/// reaches what both the user and the App can access). A private repository
/// is reachable when the user has access to it and, for a GitHub App's
/// token, a non-suspended installation of the App covers it.
pub(crate) fn visible_repo_for_user<'a>(
    world: &'a Seed,
    token: &UserTokenRecord,
    owner: &str,
    name: &str,
) -> Result<&'a Repo, Response> {
    let repo = world
        .repos
        .iter()
        .find(|r| r.owner.eq_ignore_ascii_case(owner) && r.name.eq_ignore_ascii_case(name));
    let app_covers = |repo: &Repo| match token.app_id() {
        None => true,
        Some(app_id) => world
            .installations
            .iter()
            .filter(|i| i.app_id == app_id && i.suspended_at.is_none())
            .any(|i| {
                crate::apps::installation_repos(world, i)
                    .iter()
                    .any(|r| r.id == repo.id)
            }),
    };
    let reachable = |repo: &Repo| {
        !repo.is_private()
            || (!crate::access::grants(world, repo, &token.login).is_empty() && app_covers(repo))
    };
    match repo {
        Some(repo) if reachable(repo) => Ok(repo),
        _ => Err(not_found()),
    }
}

/// GitHub's 401 for an invalid, expired or revoked credential.
pub(crate) fn bad_credentials() -> Response {
    unauthorized(BAD_CREDENTIALS)
}

/// GitHub's 404.
pub(crate) fn not_found() -> Response {
    response::error(StatusCode::NOT_FOUND, "Not Found", REST_DOCS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seed::{App, Installation, Org, Repo};
    use crate::tokens::Tokens;
    use chrono::DateTime;
    use std::collections::BTreeMap;

    fn world() -> Seed {
        Seed::new()
            .org(Org::new(1, "acme"))
            .repo(Repo::new(10, "acme", "public"))
            .repo(Repo::new(11, "acme", "chosen").private())
            .repo(Repo::new(12, "acme", "other").private())
            .app(App::new(2, "bot", "Iv1.x", "s"))
            .installation(Installation::new(3, 2, "acme").selected(["acme/chosen"]))
    }

    fn token(repository_ids: Option<Vec<u64>>) -> TokenRecord {
        let now = DateTime::from_timestamp(1_900_000_000, 0).unwrap();
        Tokens::default()
            .mint(2, 3, BTreeMap::new(), repository_ids, now)
            .1
    }

    #[test]
    fn private_repositories_out_of_reach_are_404s() {
        let world = world();
        let all = token(None);
        assert_eq!(visible_repo(&world, &all, "acme", "Chosen").unwrap().id, 11);
        assert_eq!(visible_repo(&world, &all, "acme", "public").unwrap().id, 10);
        for (owner, name) in [("acme", "other"), ("acme", "missing"), ("x", "chosen")] {
            let hidden = visible_repo(&world, &all, owner, name).unwrap_err();
            assert_eq!(hidden.status(), StatusCode::NOT_FOUND, "{owner}/{name}");
        }
        let narrowed = token(Some(vec![10]));
        let hidden = visible_repo(&world, &narrowed, "acme", "chosen").unwrap_err();
        assert_eq!(hidden.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn a_missing_permission_names_the_accepted_one() {
        let response = forbidden(Permission::new("members", Access::Write));
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response.headers()["x-accepted-github-permissions"],
            "members=write"
        );
    }
}
