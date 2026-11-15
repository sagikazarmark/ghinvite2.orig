//! The route registry: every GitHub route octoemu implements goes through
//! [`Api::route`], which records it (with the credential it accepts) so the
//! fidelity manifest can be checked against it, and wraps it in the
//! route-level middleware: refuse-assumed enforcement, credential
//! verification and permission checks (`crate::auth`).

use crate::auth::Auth;
use crate::repos::{ADMINISTRATION_READ, ADMINISTRATION_WRITE};
use crate::state::SharedState;
use crate::{apps, hooks, repos, users};
use axum::Router;
use axum::handler::Handler;
use axum::http::Method;
use axum::routing::{MethodFilter, on};

/// Cross-cutting behaviours with a `[behaviours.<id>]` entry in
/// `fidelity.toml`. Handlers check one with `Shared::refuse` (or
/// `Shared::allows`, `Shared::message`) before relying on it.
pub(crate) const BEHAVIOURS: &[&str] = &[
    "accept-expired-invitation",
    "app-bot-account",
    "app-jwt",
    "bad-credentials",
    "collaborator-affiliation",
    "collaborator-put-outcomes",
    "custom-role-invitation-permissions",
    "date-header",
    "error-body",
    "expired-invitations-listed",
    "installation-event-sender",
    "installation-events",
    "installation-events-while-suspended",
    "installation-repositories-events",
    "installation-token-expiry",
    "installation-token-format",
    "installation-token-scope",
    "invitation-alias-links",
    "invitation-expiry",
    "invitation-limit",
    "json-content-type",
    "jwt-error-messages",
    "jwt-exp-after-iat",
    "jwt-iat-tolerance",
    "member-added-directly",
    "member-edited-changes",
    "member-events",
    "out-of-scope-repository",
    "pagination",
    "permission-enforcement",
    "permission-without-access",
    "private-resource-404",
    "rate-limit-headers",
    "remove-non-collaborator",
    "repo-error-messages",
    "reput-expired-invitation",
    "reput-pending-invitation",
    "request-id-header",
    "requires-authentication",
    "role-names",
    "suspended-installation",
    "suspended-installation-tokens",
    "suspended-installation-webhooks",
    "token-error-messages",
    "token-implicit-metadata",
    "token-repositories-union",
    "token-revocation-immediate",
    "unknown-user-404",
    "user-repo-permission-ignored",
    "validation-errors",
    "webhook-config-details",
    "webhook-deliveries-cursor",
    "webhook-delivery-headers",
    "webhook-delivery-retention",
    "webhook-hook-id",
    "webhook-no-retry",
    "webhook-ordering",
    "webhook-redelivery",
    "webhook-secret-rotation",
    "webhook-signature",
    "webhook-status-texts",
    "webhook-timeout",
    "wrong-credential-kind",
];

/// GitHub routes (API and web) under construction.
pub(crate) struct Api {
    router: Router<SharedState>,
    routes: Vec<(String, Auth)>,
}

impl Api {
    pub(crate) fn new() -> Self {
        Self {
            router: Router::new(),
            routes: Vec::new(),
        }
    }

    /// Registers `handler` for `method` on `path` (axum's `{param}` syntax),
    /// accepting the callers `auth` describes. The route needs a
    /// `[routes."METHOD /path"]` entry in `fidelity.toml`.
    ///
    /// `auth` is enforced before `handler` runs: the credential is verified,
    /// a caller of the wrong kind gets a 401, and an installation token
    /// without the route's permission a 403 with
    /// `X-Accepted-GitHub-Permissions`. Handlers read the caller with the
    /// `AppCaller` / `InstallationCaller` extractors.
    pub(crate) fn route<H, T>(
        mut self,
        method: Method,
        path: &'static str,
        auth: Auth,
        handler: H,
    ) -> Self
    where
        H: Handler<T, SharedState>,
        T: 'static,
    {
        let filter = MethodFilter::try_from(method.clone()).expect("a standard HTTP method");
        self.router = self.router.route(path, on(filter, handler));
        self.routes.push((format!("{method} {path}"), auth));
        self
    }

    /// `METHOD /pattern` of every registered route, with what it accepts.
    pub(crate) fn routes(&self) -> &[(String, Auth)] {
        &self.routes
    }

    /// `METHOD /pattern` of every registered route.
    pub(crate) fn route_names(&self) -> impl Iterator<Item = &str> {
        self.routes.iter().map(|(route, _)| route.as_str())
    }

    /// The router, with the route-level middleware on every registered route
    /// and a 501 for unregistered methods on registered paths.
    pub(crate) fn into_router(self, shared: SharedState) -> Router<SharedState> {
        if self.routes.is_empty() {
            // axum refuses a route layer on a router without routes.
            return self.router;
        }
        self.router
            .method_not_allowed_fallback(crate::server::fallback)
            .route_layer(axum::middleware::from_fn_with_state(
                shared,
                crate::server::route_context,
            ))
    }
}

/// `metadata: read`, which every repository-scoped installation route needs.
use crate::repos::METADATA_READ as METADATA;

/// Every GitHub route octoemu implements; everything else answers 501.
pub(crate) fn github() -> Api {
    Api::new()
        // App (JWT) routes: docs.github.com/rest/apps/apps.
        .route(Method::GET, "/app", Auth::App, apps::get_app)
        .route(
            Method::GET,
            "/app/installations",
            Auth::App,
            apps::list_installations,
        )
        .route(
            Method::GET,
            "/app/installations/{installation_id}",
            Auth::App,
            apps::get_installation,
        )
        .route(
            Method::POST,
            "/app/installations/{installation_id}/access_tokens",
            Auth::App,
            apps::create_token,
        )
        .route(
            Method::PUT,
            "/app/installations/{installation_id}/suspended",
            Auth::App,
            apps::suspend,
        )
        .route(
            Method::DELETE,
            "/app/installations/{installation_id}/suspended",
            Auth::App,
            apps::unsuspend,
        )
        .route(
            Method::GET,
            "/app/installation-requests",
            Auth::App,
            apps::installation_requests,
        )
        // The App's webhook: docs.github.com/rest/apps/webhooks.
        .route(
            Method::GET,
            "/app/hook/config",
            Auth::App,
            hooks::get_config,
        )
        .route(
            Method::PATCH,
            "/app/hook/config",
            Auth::App,
            hooks::patch_config,
        )
        .route(
            Method::GET,
            "/app/hook/deliveries",
            Auth::App,
            hooks::list_deliveries,
        )
        .route(
            Method::GET,
            "/app/hook/deliveries/{delivery_id}",
            Auth::App,
            hooks::get_delivery,
        )
        .route(
            Method::POST,
            "/app/hook/deliveries/{delivery_id}/attempts",
            Auth::App,
            hooks::redeliver,
        )
        // Installation-token routes: docs.github.com/rest/apps/installations.
        .route(
            Method::GET,
            "/installation/repositories",
            Auth::Installation(Some(METADATA)),
            apps::repositories,
        )
        .route(
            Method::DELETE,
            "/installation/token",
            Auth::Installation(None),
            apps::revoke_token,
        )
        // Repositories: docs.github.com/rest/repos/repos.
        .route(
            Method::GET,
            "/repos/{owner}/{repo}",
            Auth::Installation(Some(METADATA)),
            repos::get_repo,
        )
        // Collaborators: docs.github.com/rest/collaborators/collaborators.
        .route(
            Method::GET,
            "/repos/{owner}/{repo}/collaborators",
            Auth::Installation(Some(METADATA)),
            repos::list_collaborators,
        )
        .route(
            Method::PUT,
            "/repos/{owner}/{repo}/collaborators/{username}",
            Auth::Installation(Some(ADMINISTRATION_WRITE)),
            repos::put_collaborator,
        )
        .route(
            Method::DELETE,
            "/repos/{owner}/{repo}/collaborators/{username}",
            Auth::Installation(Some(ADMINISTRATION_WRITE)),
            repos::remove_collaborator,
        )
        .route(
            Method::GET,
            "/repos/{owner}/{repo}/collaborators/{username}/permission",
            Auth::Installation(Some(METADATA)),
            repos::collaborator_permission,
        )
        // Invitations: docs.github.com/rest/collaborators/invitations.
        .route(
            Method::GET,
            "/repos/{owner}/{repo}/invitations",
            Auth::Installation(Some(ADMINISTRATION_READ)),
            repos::list_invitations,
        )
        .route(
            Method::GET,
            "/repositories/{repository_id}/invitations",
            Auth::Installation(Some(ADMINISTRATION_READ)),
            repos::list_invitations_by_id,
        )
        .route(
            Method::PATCH,
            "/repos/{owner}/{repo}/invitations/{invitation_id}",
            Auth::Installation(Some(ADMINISTRATION_WRITE)),
            repos::update_invitation,
        )
        .route(
            Method::DELETE,
            "/repos/{owner}/{repo}/invitations/{invitation_id}",
            Auth::Installation(Some(ADMINISTRATION_WRITE)),
            repos::delete_invitation,
        )
        // Users: docs.github.com/rest/users/users.
        .route(
            Method::GET,
            "/users/{username}",
            Auth::Public,
            users::get_user,
        )
        .route(
            Method::GET,
            "/user/{account_id}",
            Auth::Public,
            users::get_user_by_id,
        )
        // Avatars (avatars.githubusercontent.com).
        .route(Method::GET, "/avatars/u/{id}", Auth::Public, users::avatar)
}
