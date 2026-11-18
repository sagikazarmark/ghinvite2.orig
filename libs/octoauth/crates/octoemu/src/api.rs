//! The route registry: every GitHub route octoemu implements goes through
//! [`Api::route`], which records it (with the credential it accepts) so the
//! fidelity manifest can be checked against it, and wraps it in the
//! route-level middleware: refuse-assumed enforcement, credential
//! verification and permission checks (`crate::auth`).

use crate::auth::Auth;
use crate::orgs::{MEMBERS_READ, MEMBERS_WRITE};
use crate::repos::{ADMINISTRATION_READ, ADMINISTRATION_WRITE};
use crate::state::SharedState;
use crate::users::EMAIL_ADDRESSES_READ;
use crate::{applications, apps, hooks, invitee, oauth, orgs, repos, users};
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
    "applications-errors",
    "authorize-page-errors",
    "bad-credentials",
    "bad-refresh-token-text",
    "client-credential-errors",
    "collaborator-affiliation",
    "collaborator-put-outcomes",
    "custom-role-invitation-permissions",
    "date-header",
    "email-invitation-accept",
    "error-body",
    "expired-invitations-listed",
    "expired-org-invitations-failed",
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
    "invited-team-membership-pending",
    "json-content-type",
    "jwt-error-messages",
    "jwt-exp-after-iat",
    "jwt-iat-tolerance",
    "member-added-directly",
    "member-edited-changes",
    "member-events",
    "noreply-email",
    "oauth-authorize-errors",
    "oauth-code",
    "oauth-error-status",
    "oauth-token-errors",
    "oauth-token-response",
    "org-accept-team-events",
    "org-admin-fields",
    "org-error-messages",
    "org-invitation-cancel",
    "org-invitation-decline",
    "org-invitation-expiry",
    "org-invitation-limit",
    "org-invitation-limit-response",
    "org-invitation-roles",
    "org-member-events",
    "org-membership-pending",
    "org-no-seat",
    "org-out-of-scope",
    "org-plan-visibility",
    "org-rename",
    "org-reput-pending",
    "org-spammed",
    "out-of-scope-repository",
    "pagination",
    "pat-scopes",
    "permission-enforcement",
    "permission-without-access",
    "pkce",
    "pkce-exchange-errors",
    "private-resource-404",
    "rate-limit-headers",
    "redirect-uri-loopback-port",
    "redirect-uri-match",
    "refresh-token-rotation",
    "remove-non-collaborator",
    "remove-org-non-member",
    "remove-team-non-member",
    "repo-error-messages",
    "reput-expired-invitation",
    "reput-pending-invitation",
    "request-id-header",
    "requires-authentication",
    "role-names",
    "suspended-installation",
    "suspended-installation-tokens",
    "suspended-installation-webhooks",
    "team-membership-events",
    "team-put-non-member",
    "team-repository-access",
    "token-error-messages",
    "token-implicit-metadata",
    "token-repositories-union",
    "token-revocation-immediate",
    "unknown-user-404",
    "unsupported-grant-type",
    "user-installation-access",
    "user-installation-repository-selection",
    "user-installations-pat",
    "user-public-email",
    "user-rename",
    "user-repo-permission-ignored",
    "user-token-cap",
    "user-token-expiry",
    "user-token-hourly-limit",
    "user-token-invitations",
    "user-token-org-membership-list",
    "user-token-permissions",
    "user-token-revocation",
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
        // The user's view of an installation: docs.github.com/rest/apps/installations.
        .route(
            Method::GET,
            "/user/installations/{installation_id}/repositories",
            Auth::User(None),
            apps::user_repositories,
        )
        // Repositories: docs.github.com/rest/repos/repos.
        .route(
            Method::GET,
            "/repos/{owner}/{repo}",
            Auth::Token(Some(METADATA)),
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
        // The signed-in user: docs.github.com/rest/users/users and
        // docs.github.com/rest/users/emails.
        .route(
            Method::GET,
            "/user",
            Auth::User(None),
            users::get_authenticated,
        )
        .route(
            Method::GET,
            "/user/emails",
            Auth::User(Some(EMAIL_ADDRESSES_READ)),
            users::emails,
        )
        // The invitee: docs.github.com/rest/collaborators/invitations.
        .route(
            Method::GET,
            "/user/repository_invitations",
            Auth::User(None),
            invitee::list,
        )
        .route(
            Method::PATCH,
            "/user/repository_invitations/{invitation_id}",
            Auth::User(None),
            invitee::accept,
        )
        .route(
            Method::DELETE,
            "/user/repository_invitations/{invitation_id}",
            Auth::User(None),
            invitee::decline,
        )
        // Organizations: docs.github.com/rest/orgs/orgs and
        // docs.github.com/rest/orgs/members.
        .route(
            Method::GET,
            "/orgs/{org}",
            Auth::Installation(None),
            orgs::get_org,
        )
        .route(
            Method::GET,
            "/orgs/{org}/members",
            Auth::Installation(Some(MEMBERS_READ)),
            orgs::list_members,
        )
        .route(
            Method::GET,
            "/orgs/{org}/memberships/{username}",
            Auth::Installation(Some(MEMBERS_READ)),
            orgs::get_membership,
        )
        .route(
            Method::PUT,
            "/orgs/{org}/memberships/{username}",
            Auth::Installation(Some(MEMBERS_WRITE)),
            orgs::put_membership,
        )
        .route(
            Method::DELETE,
            "/orgs/{org}/memberships/{username}",
            Auth::Installation(Some(MEMBERS_WRITE)),
            orgs::remove_membership,
        )
        .route(
            Method::GET,
            "/orgs/{org}/invitations",
            Auth::Installation(Some(MEMBERS_READ)),
            orgs::list_invitations,
        )
        .route(
            Method::POST,
            "/orgs/{org}/invitations",
            Auth::Installation(Some(MEMBERS_WRITE)),
            orgs::create_invitation,
        )
        .route(
            Method::DELETE,
            "/orgs/{org}/invitations/{invitation_id}",
            Auth::Installation(Some(MEMBERS_WRITE)),
            orgs::cancel_invitation,
        )
        .route(
            Method::GET,
            "/orgs/{org}/invitations/{invitation_id}/teams",
            Auth::Installation(Some(MEMBERS_READ)),
            orgs::invitation_teams,
        )
        .route(
            Method::GET,
            "/orgs/{org}/failed_invitations",
            Auth::Installation(Some(MEMBERS_READ)),
            orgs::list_failed_invitations,
        )
        // Teams: docs.github.com/rest/teams/teams and
        // docs.github.com/rest/teams/members.
        .route(
            Method::GET,
            "/orgs/{org}/teams",
            Auth::Installation(Some(MEMBERS_READ)),
            orgs::list_teams,
        )
        .route(
            Method::GET,
            "/orgs/{org}/teams/{team_slug}",
            Auth::Installation(Some(MEMBERS_READ)),
            orgs::get_team,
        )
        .route(
            Method::GET,
            "/orgs/{org}/teams/{team_slug}/memberships/{username}",
            Auth::Installation(Some(MEMBERS_READ)),
            orgs::get_team_membership,
        )
        .route(
            Method::PUT,
            "/orgs/{org}/teams/{team_slug}/memberships/{username}",
            Auth::Installation(Some(MEMBERS_WRITE)),
            orgs::put_team_membership,
        )
        .route(
            Method::DELETE,
            "/orgs/{org}/teams/{team_slug}/memberships/{username}",
            Auth::Installation(Some(MEMBERS_WRITE)),
            orgs::remove_team_membership,
        )
        // The user's organization memberships: docs.github.com/rest/orgs/members.
        .route(
            Method::GET,
            "/user/memberships/orgs",
            Auth::User(None),
            orgs::list_user_memberships,
        )
        .route(
            Method::GET,
            "/user/memberships/orgs/{org}",
            Auth::User(Some(MEMBERS_READ)),
            orgs::get_user_membership,
        )
        .route(
            Method::PATCH,
            "/user/memberships/orgs/{org}",
            Auth::User(Some(MEMBERS_WRITE)),
            orgs::accept_membership,
        )
        // An App's OAuth tokens: docs.github.com/rest/apps/oauth-applications.
        .route(
            Method::POST,
            "/applications/{client_id}/token",
            Auth::Client,
            applications::check_token,
        )
        .route(
            Method::PATCH,
            "/applications/{client_id}/token",
            Auth::Client,
            applications::reset_token,
        )
        .route(
            Method::DELETE,
            "/applications/{client_id}/token",
            Auth::Client,
            applications::delete_token,
        )
        .route(
            Method::DELETE,
            "/applications/{client_id}/grant",
            Auth::Client,
            applications::delete_grant,
        )
        // github.com's OAuth web flow (web_base).
        .route(
            Method::GET,
            "/login/oauth/authorize",
            Auth::Public,
            oauth::authorize,
        )
        .route(
            Method::POST,
            "/login/oauth/authorize",
            Auth::Public,
            oauth::consent,
        )
        .route(
            Method::POST,
            "/login/oauth/access_token",
            Auth::Public,
            oauth::access_token,
        )
        // Avatars (avatars.githubusercontent.com).
        .route(Method::GET, "/avatars/u/{id}", Auth::Public, users::avatar)
}
