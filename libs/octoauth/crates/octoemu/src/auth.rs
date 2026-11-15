//! Who is calling, and whether they may: every route declares an [`Auth`]
//! when it is registered (`Api::route`), and the route middleware verifies
//! the `Authorization` header against it before the handler runs.
//!
//! - The credential is verified first, on every route: a bad, expired or
//!   revoked credential is a 401 even where none is needed.
//! - Then its kind: an App JWT, an installation token, or none.
//! - Then, for installation tokens, the route's permission: without it, a 403
//!   `Resource not accessible by integration` with
//!   `X-Accepted-GitHub-Permissions` (documented).
//!
//! Handlers read the verified caller with the [`AppCaller`] and
//! [`InstallationCaller`] extractors.
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
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header::AUTHORIZATION};
use axum::response::Response;

/// What a route accepts. Part of every route's registration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Auth {
    /// Anyone; a credential, if sent, must still be valid.
    Public,
    /// An App's JWT (`Authorization: Bearer <jwt>`).
    App,
    /// An installation token, holding `permission` when one is named.
    Installation(Option<Permission>),
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

/// GitHub's message for a suspended installation (community-reported,
/// behaviour `suspended-installation`).
pub(crate) const SUSPENDED: &str = "This installation has been suspended";

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

/// Verifies the request's credential, whatever the route.
pub(crate) fn authenticate(shared: &Shared, headers: &HeaderMap) -> Result<Caller, Response> {
    let Some(header) = authorization(headers) else {
        return Ok(Caller::Anonymous);
    };
    let Ok((scheme, credential)) = header else {
        return Err(unauthorized(BAD_CREDENTIALS));
    };
    match scheme.as_str() {
        "bearer" if jwt_shaped(&credential) => app_jwt(shared, &credential),
        // Documented: an App's JWT goes in a Bearer header. With `token`
        // GitHub reads it as a token, which matches none.
        "token" if jwt_shaped(&credential) => Err(unauthorized(BAD_CREDENTIALS)),
        "bearer" | "token" if credential.starts_with("ghs_") => {
            installation_token(shared, &credential)
        }
        // User tokens arrive with GE-05; anything else matches no
        // credential.
        _ => Err(unauthorized(BAD_CREDENTIALS)),
    }
}

fn app_jwt(shared: &Shared, credential: &str) -> Result<Caller, Response> {
    let verified = {
        let state = shared.state();
        jwt::verify(&state.world, credential, state.clock.now().timestamp())
    };
    match verified {
        Ok(verified) => {
            if verified.used_iat_tolerance
                && let Some(refusal) = shared.refuse("jwt-iat-tolerance")
            {
                return Err(refusal);
            }
            Ok(Caller::App {
                app_id: verified.app_id,
            })
        }
        Err(error) => {
            if error == JwtError::TooLongAfterIat
                && let Some(refusal) = shared.refuse("jwt-exp-after-iat")
            {
                return Err(refusal);
            }
            Err(unauthorized(
                &shared.message("jwt-error-messages", error.message()),
            ))
        }
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
        Ok((token, Some(false))) => Ok(Caller::Installation(token)),
        // Suspended since the token was minted.
        Ok((_, Some(true))) => Err(shared
            .refuse("suspended-installation-tokens")
            .unwrap_or_else(|| {
                response::error(
                    StatusCode::FORBIDDEN,
                    &shared.message("suspended-installation", SUSPENDED),
                    REST_DOCS,
                )
            })),
        // Uninstalled since, or revoked: stateless tokens might outlive
        // either at GitHub (unmeasured).
        Ok((_, None)) | Err(Rejected::Revoked) => Err(shared
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
        (_, Caller::Anonymous) => Err(unauthorized(REQUIRES_AUTHENTICATION)),
        (Auth::App, _) => wrong_kind(JwtError::Undecodable.message()),
        (Auth::Installation(_), _) => wrong_kind(BAD_CREDENTIALS),
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
