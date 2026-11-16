//! JSON representations of world objects, in GitHub's shapes (the fields
//! ghinvite and octoauth read, plus the common ones around them).

use crate::seed::{
    App, Installation, InstallationRequest, MembershipState, Org, OrgInvitation, OrgRole, Repo,
    RepositoryInvitation, Role, Seed, Team, TeamRole,
};
use crate::state::Shared;
use axum::http::{HeaderMap, header::HOST};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

/// Where the request was sent (`http://host:port`), for absolute URLs in
/// bodies and `Link`: the `Host` header, else the bound address.
pub(crate) fn origin(shared: &Shared, headers: &HeaderMap) -> String {
    headers
        .get(HOST)
        .and_then(|host| host.to_str().ok())
        .filter(|host| !host.is_empty() && !host.contains(['/', ' ', '@']))
        .map_or_else(|| shared.root.clone(), |host| format!("http://{host}"))
}

/// GitHub's timestamp format: `2030-01-01T00:00:00Z`.
pub(crate) fn timestamp(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// A legacy global node ID (`base64("04:User1")`), GitHub's older format.
pub(crate) fn node_id(kind: &str, id: u64) -> String {
    let prefix = match kind {
        "User" => "04",
        "Organization" => "012",
        "Repository" => "010",
        "Integration" => "03",
        "IntegrationInstallation" => "023",
        "Bot" => "04",
        _ => "00",
    };
    STANDARD.encode(format!("{prefix}:{kind}{id}"))
}

/// Rendering context: the world, the origin, the clock's start (the
/// default creation time) and now (for expiry).
pub(crate) struct View<'a> {
    pub(crate) world: &'a Seed,
    pub(crate) origin: &'a str,
    pub(crate) start: DateTime<Utc>,
    pub(crate) now: DateTime<Utc>,
}

/// The ID octoemu gives an App's bot account: this plus the App ID
/// (behaviour `app-bot-account`).
pub(crate) const BOT_ID_OFFSET: u64 = 1_000_000_000;

/// How long a repository invitation stays valid (documented: 7 days).
pub(crate) const INVITATION_LIFETIME: chrono::TimeDelta = chrono::TimeDelta::days(7);

/// Whether `invitation` has expired at `now`.
pub(crate) fn expired(
    invitation: &RepositoryInvitation,
    start: DateTime<Utc>,
    now: DateTime<Utc>,
) -> bool {
    now >= invitation.created_at.unwrap_or(start) + INVITATION_LIFETIME
}

/// Whether organization invitation `invitation` has expired at `now`
/// (documented: seven days).
pub(crate) fn org_invitation_expired(
    invitation: &OrgInvitation,
    start: DateTime<Utc>,
    now: DateTime<Utc>,
) -> bool {
    now >= invitation.created_at.unwrap_or(start) + INVITATION_LIFETIME
}

/// Whether organization invitation `invitation` is still open: neither
/// failed nor expired.
pub(crate) fn org_invitation_open(
    invitation: &OrgInvitation,
    start: DateTime<Utc>,
    now: DateTime<Utc>,
) -> bool {
    invitation.failed_at.is_none() && !org_invitation_expired(invitation, start, now)
}

/// The text `failed_reason` carries for an expired invitation (behaviour
/// `expired-org-invitations-failed`).
pub(crate) const EXPIRED_REASON: &str = "Invitation expired";

/// An organization role as membership objects name it.
pub(crate) fn org_role_name(role: OrgRole) -> &'static str {
    match role {
        OrgRole::Admin => "admin",
        OrgRole::Member => "member",
    }
}

/// A membership state's spelling.
pub(crate) fn state_name(state: MembershipState) -> &'static str {
    match state {
        MembershipState::Active => "active",
        MembershipState::Pending => "pending",
    }
}

/// A team role's spelling.
pub(crate) fn team_role_name(role: TeamRole) -> &'static str {
    match role {
        TeamRole::Member => "member",
        TeamRole::Maintainer => "maintainer",
    }
}

impl View<'_> {
    /// An organization as `GET /orgs/{org}` reports it (an
    /// `organization-full` subset). `plan` only with `plan` (the caller
    /// holds `organization_plan`), the owner-only settings only with
    /// `admin_fields`.
    pub(crate) fn full_org(&self, org: &Org, plan: bool, admin_fields: bool) -> Value {
        let mut body = self.organization(&org.login).unwrap_or_default();
        let origin = self.origin;
        let login = &org.login;
        let created = timestamp(org.created_at.unwrap_or(self.start));
        let public_repos = self
            .world
            .repos
            .iter()
            .filter(|r| r.owner.eq_ignore_ascii_case(login) && !r.is_private())
            .count();
        for (field, value) in [
            ("name", org.name.clone().map_or(Value::Null, Value::from)),
            ("company", Value::Null),
            ("blog", "".into()),
            ("location", Value::Null),
            ("email", Value::Null),
            ("twitter_username", Value::Null),
            ("is_verified", false.into()),
            ("has_organization_projects", true.into()),
            ("has_repository_projects", true.into()),
            ("public_repos", public_repos.into()),
            ("public_gists", 0.into()),
            ("followers", 0.into()),
            ("following", 0.into()),
            ("html_url", format!("{origin}/{login}").into()),
            ("created_at", created.clone().into()),
            ("updated_at", created.into()),
            ("archived_at", Value::Null),
            ("type", "Organization".into()),
        ] {
            body[field] = value;
        }
        if plan {
            body["plan"] = json!({
                "name": org.plan.name,
                "space": 976_562_499,
                "private_repos": 999_999,
                "filled_seats": crate::organization::filled_seats(self.world, org, self.start, self.now),
                "seats": org.plan.seats,
            });
        }
        if admin_fields {
            let private = self
                .world
                .repos
                .iter()
                .filter(|r| r.owner.eq_ignore_ascii_case(login) && r.is_private())
                .count();
            body["two_factor_requirement_enabled"] = org.two_factor_requirement_enabled.into();
            body["default_repository_permission"] =
                serde_json::to_value(org.default_repository_permission).unwrap_or_default();
            body["members_can_create_repositories"] = true.into();
            body["total_private_repos"] = private.into();
            body["owned_private_repos"] = private.into();
        }
        body
    }

    /// An organization membership (`org-membership`).
    pub(crate) fn membership(
        &self,
        org: &str,
        login: &str,
        role: &str,
        state: MembershipState,
    ) -> Value {
        let origin = self.origin;
        let org_login = self
            .world
            .orgs
            .iter()
            .find(|o| o.login.eq_ignore_ascii_case(org))
            .map_or(org, |o| o.login.as_str());
        let user = self.account(login).unwrap_or_default();
        let user_login = user["login"].as_str().unwrap_or(login).to_owned();
        json!({
            "url": format!("{origin}/orgs/{org_login}/memberships/{user_login}"),
            "state": state_name(state),
            "role": role,
            "organization_url": format!("{origin}/orgs/{org_login}"),
            "organization": self.organization(org_login),
            "user": user,
        })
    }

    /// A team (`team`, as lists show it).
    pub(crate) fn team(&self, team: &Team) -> Value {
        let origin = self.origin;
        let org = &team.org;
        let slug = &team.slug;
        let parent = team
            .parent
            .as_deref()
            .and_then(|p| crate::access::team(self.world, org, p))
            .map(|parent| self.team_simple(parent));
        let mut body = self.team_simple(team);
        body["parent"] = parent.unwrap_or(Value::Null);
        body["members_url"] = format!("{origin}/orgs/{org}/teams/{slug}/members{{/member}}").into();
        body
    }

    /// A team as `parent` shows it (`team-simple`).
    fn team_simple(&self, team: &Team) -> Value {
        let origin = self.origin;
        let org = &team.org;
        let slug = &team.slug;
        json!({
            "id": team.id,
            "node_id": STANDARD.encode(format!("04:Team{}", team.id)),
            "url": format!("{origin}/organizations/{}/team/{}", self.organization(org).map_or(Value::Null, |o| o["id"].clone()), team.id),
            "html_url": format!("{origin}/orgs/{org}/teams/{slug}"),
            "name": team.display_name(),
            "slug": slug,
            "description": null,
            "privacy": serde_json::to_value(team.privacy).unwrap_or_default(),
            "notification_setting": "notifications_enabled",
            "permission": "pull",
            "members_url": format!("{origin}/orgs/{org}/teams/{slug}/members{{/member}}"),
            "repositories_url": format!("{origin}/orgs/{org}/teams/{slug}/repos"),
        })
    }

    /// A team as `GET /orgs/{org}/teams/{team_slug}` reports it
    /// (`team-full`).
    pub(crate) fn full_team(&self, team: &Team) -> Value {
        let mut body = self.team(team);
        let key = crate::access::team_key(team);
        let members = self
            .world
            .team_memberships
            .iter()
            .filter(|m| m.team.eq_ignore_ascii_case(&key) && m.state == MembershipState::Active)
            .count();
        let created = timestamp(self.start);
        body["members_count"] = members.into();
        body["repos_count"] = team.repositories.len().into();
        body["created_at"] = created.clone().into();
        body["updated_at"] = created.into();
        body["organization"] = self
            .world
            .orgs
            .iter()
            .find(|o| o.login.eq_ignore_ascii_case(&team.org))
            .map_or(Value::Null, |org| self.full_org(org, false, false));
        body
    }

    /// A team membership (`team-membership`).
    pub(crate) fn team_membership(
        &self,
        team: &Team,
        login: &str,
        role: TeamRole,
        state: MembershipState,
    ) -> Value {
        let origin = self.origin;
        let login = self
            .account(login)
            .and_then(|a| a["login"].as_str().map(str::to_owned))
            .unwrap_or_else(|| login.to_owned());
        let org_id = self
            .organization(&team.org)
            .map_or(Value::Null, |o| o["id"].clone());
        json!({
            "url": format!("{origin}/organizations/{org_id}/team/{}/memberships/{login}", team.id),
            "role": team_role_name(role),
            "state": state_name(state),
        })
    }

    /// An organization invitation (`organization-invitation`). An expired
    /// one shows its expiry as `failed_at` with [`EXPIRED_REASON`].
    pub(crate) fn org_invitation(&self, invitation: &OrgInvitation) -> Value {
        let origin = self.origin;
        let inviter = match (&invitation.inviter, invitation.inviter_app_id) {
            (Some(login), _) => self.account(login).unwrap_or_default(),
            (None, Some(app_id)) => self.app_bot(app_id),
            (None, None) => Value::Null,
        };
        let created = invitation.created_at.unwrap_or(self.start);
        let (failed_at, failed_reason) = match (&invitation.failed_at, &invitation.failed_reason) {
            (Some(at), reason) => (
                Value::from(timestamp(*at)),
                reason.clone().map_or(Value::Null, Value::from),
            ),
            (None, _) if org_invitation_expired(invitation, self.start, self.now) => (
                timestamp(created + INVITATION_LIFETIME).into(),
                EXPIRED_REASON.into(),
            ),
            (None, _) => (Value::Null, Value::Null),
        };
        let login = invitation.invitee.as_deref().map(|login| {
            self.account(login)
                .and_then(|a| a["login"].as_str().map(str::to_owned))
                .unwrap_or_else(|| login.to_owned())
        });
        let org_id = self
            .organization(&invitation.org)
            .map_or(Value::Null, |o| o["id"].clone());
        json!({
            "id": invitation.id,
            "node_id": STANDARD.encode(format!("011:OrganizationInvitation{}", invitation.id)),
            "login": login,
            "email": invitation.email,
            "role": invitation.role.as_str(),
            "created_at": timestamp(created),
            "failed_at": failed_at,
            "failed_reason": failed_reason,
            "inviter": inviter,
            "team_count": invitation.team_ids.len(),
            "invitation_teams_url": format!("{origin}/organizations/{org_id}/invitations/{}/teams", invitation.id),
            "invitation_source": "member",
        })
    }

    /// A user or organization as a `simple-user`; `None` for an unknown
    /// login.
    pub(crate) fn account(&self, login: &str) -> Option<Value> {
        let origin = self.origin;
        let (id, login, kind) = if let Some(user) = self
            .world
            .users
            .iter()
            .find(|u| u.login.eq_ignore_ascii_case(login))
        {
            (user.id, user.login.as_str(), user.kind.as_str())
        } else {
            let org = self
                .world
                .orgs
                .iter()
                .find(|o| o.login.eq_ignore_ascii_case(login))?;
            (org.id, org.login.as_str(), "Organization")
        };
        Some(json!({
            "login": login,
            "id": id,
            "node_id": node_id(kind, id),
            "avatar_url": format!("{origin}/avatars/u/{id}?v=4"),
            "gravatar_id": "",
            "url": format!("{origin}/users/{login}"),
            "html_url": format!("{origin}/{login}"),
            "type": kind,
            "site_admin": false,
        }))
    }

    /// The account's numeric ID and type, for `target_id`/`target_type`.
    fn target(&self, login: &str) -> (Value, Value) {
        self.account(login)
            .map(|a| (a["id"].clone(), a["type"].clone()))
            .unwrap_or_default()
    }

    /// A repository as a `minimal-repository` subset.
    pub(crate) fn repo(&self, repo: &Repo) -> Value {
        let origin = self.origin;
        let full_name = repo.full_name();
        let created = timestamp(repo.created_at.unwrap_or(self.start));
        json!({
            "id": repo.id,
            "node_id": node_id("Repository", repo.id),
            "name": repo.name,
            "full_name": full_name,
            "private": repo.is_private(),
            "visibility": repo.effective_visibility().as_str(),
            "owner": self.account(&repo.owner),
            "html_url": format!("{origin}/{full_name}"),
            "description": null,
            "fork": false,
            "url": format!("{origin}/repos/{full_name}"),
            "archived": false,
            "disabled": false,
            "created_at": created,
            "updated_at": created,
        })
    }

    /// A repository as webhook lists carry it (`repositories_added`,
    /// `installation.repositories`): `{id, node_id, name, full_name,
    /// private}`.
    pub(crate) fn repo_ref(&self, repo: &Repo) -> Value {
        json!({
            "id": repo.id,
            "node_id": node_id("Repository", repo.id),
            "name": repo.name,
            "full_name": repo.full_name(),
            "private": repo.is_private(),
        })
    }

    /// A repository as `GET /repos/{owner}/{repo}` reports it (a
    /// `full-repository` subset).
    pub(crate) fn full_repo(&self, repo: &Repo) -> Value {
        let mut body = self.repo(repo);
        let origin = self.origin;
        let full_name = repo.full_name();
        body["default_branch"] = "main".into();
        body["homepage"] = Value::Null;
        body["language"] = Value::Null;
        body["is_template"] = false.into();
        body["collaborators_url"] =
            format!("{origin}/repos/{full_name}/collaborators{{/collaborator}}").into();
        body["pushed_at"] = body["created_at"].clone();
        if crate::access::org_owned(self.world, repo) {
            body["organization"] = self.account(&repo.owner).unwrap_or_default();
        }
        body
    }

    /// A user or organization as `GET /users/{username}` reports it (a
    /// `public-user` subset); `None` for an unknown login.
    pub(crate) fn full_account(&self, login: &str) -> Option<Value> {
        let mut body = self.account(login)?;
        let user = crate::access::user(self.world, login);
        let org = self
            .world
            .orgs
            .iter()
            .find(|o| o.login.eq_ignore_ascii_case(login));
        let name = user
            .and_then(|u| u.name.clone())
            .or_else(|| org.and_then(|o| o.name.clone()));
        let created = user
            .and_then(|u| u.created_at)
            .or_else(|| org.and_then(|o| o.created_at))
            .unwrap_or(self.start);
        let public_repos = self
            .world
            .repos
            .iter()
            .filter(|r| r.owner.eq_ignore_ascii_case(login) && !r.is_private())
            .count();
        for (field, value) in [
            ("name", name.map_or(Value::Null, Value::from)),
            ("company", Value::Null),
            ("blog", "".into()),
            ("location", Value::Null),
            (
                "email",
                user.and_then(crate::seed::User::public_email)
                    .map_or(Value::Null, Value::from),
            ),
            ("hireable", Value::Null),
            ("bio", Value::Null),
            ("twitter_username", Value::Null),
            ("public_repos", public_repos.into()),
            ("public_gists", 0.into()),
            ("followers", 0.into()),
            ("following", 0.into()),
            ("created_at", timestamp(created).into()),
            ("updated_at", timestamp(created).into()),
        ] {
            body[field] = value;
        }
        Some(body)
    }

    /// An App's bot account (`{slug}[bot]`, `type: "Bot"`), the actor of
    /// what an installation token does (behaviour `app-bot-account`).
    pub(crate) fn app_bot(&self, app_id: u64) -> Value {
        let origin = self.origin;
        let slug = self
            .world
            .apps
            .iter()
            .find(|a| a.id == app_id)
            .map_or_else(|| format!("app-{app_id}"), |a| a.slug.clone());
        let login = format!("{slug}[bot]");
        let id = BOT_ID_OFFSET + app_id;
        json!({
            "login": login,
            "id": id,
            "node_id": node_id("Bot", id),
            "avatar_url": format!("{origin}/avatars/u/{id}?v=4"),
            "gravatar_id": "",
            "url": format!("{origin}/users/{login}"),
            "html_url": format!("{origin}/apps/{slug}"),
            "type": "Bot",
            "site_admin": false,
        })
    }

    /// A collaborator: the account plus `permissions` and `role_name`.
    pub(crate) fn collaborator(&self, repo: &Repo, login: &str, role: &Role) -> Value {
        let mut body = self.account(login).unwrap_or_default();
        body["permissions"] = crate::access::permission_flags(self.world, repo, role);
        body["role_name"] = crate::access::role_name(self.world, repo, role).into();
        body
    }

    /// A repository invitation (`repository-invitation`).
    pub(crate) fn invitation(&self, invitation: &RepositoryInvitation) -> Value {
        let origin = self.origin;
        let repo = self
            .world
            .repos
            .iter()
            .find(|r| r.full_name().eq_ignore_ascii_case(&invitation.repo));
        let inviter = match (&invitation.inviter, invitation.inviter_app_id) {
            (Some(login), _) => self.account(login).unwrap_or_default(),
            (None, Some(app_id)) => self.app_bot(app_id),
            (None, None) => Value::Null,
        };
        let full_name = repo.map_or_else(|| invitation.repo.clone(), Repo::full_name);
        json!({
            "id": invitation.id,
            "node_id": STANDARD.encode(format!("09:RepositoryInvitation{}", invitation.id)),
            "repository": repo.map(|r| self.repo(r)),
            "invitee": self.account(&invitation.invitee),
            "inviter": inviter,
            "permissions": repo.map(|r| crate::access::role_name(self.world, r, &invitation.role)),
            "created_at": timestamp(invitation.created_at.unwrap_or(self.start)),
            "expired": expired(invitation, self.start, self.now),
            "url": format!("{origin}/user/repository_invitations/{}", invitation.id),
            "html_url": format!("{origin}/{full_name}/invitations"),
        })
    }

    /// An organization as webhook payloads carry it.
    pub(crate) fn organization(&self, login: &str) -> Option<Value> {
        let origin = self.origin;
        let org = self
            .world
            .orgs
            .iter()
            .find(|o| o.login.eq_ignore_ascii_case(login))?;
        Some(json!({
            "login": org.login,
            "id": org.id,
            "node_id": node_id("Organization", org.id),
            "url": format!("{origin}/orgs/{}", org.login),
            "avatar_url": format!("{origin}/avatars/u/{}?v=4", org.id),
            "description": null,
        }))
    }

    /// An installation, as `GET /app/installations/{id}` reports it.
    pub(crate) fn installation(&self, installation: &Installation) -> Value {
        let origin = self.origin;
        let app = self.world.apps.iter().find(|a| a.id == installation.app_id);
        let permissions = crate::apps::granted(self.world, installation);
        let events = installation
            .events
            .clone()
            .or_else(|| app.map(|a| a.events.clone()))
            .unwrap_or_default();
        let created = installation.created_at.unwrap_or(self.start);
        let (target_id, target_type) = self.target(&installation.account);
        json!({
            "id": installation.id,
            "client_id": app.map(|a| a.client_id.as_str()),
            "account": self.account(&installation.account),
            "repository_selection": installation.repository_selection,
            "access_tokens_url": format!("{origin}/app/installations/{}/access_tokens", installation.id),
            "repositories_url": format!("{origin}/installation/repositories"),
            "html_url": format!("{origin}/settings/installations/{}", installation.id),
            "app_id": installation.app_id,
            "app_slug": app.map(|a| a.slug.as_str()),
            "target_id": target_id,
            "target_type": target_type,
            "permissions": permissions,
            "events": events,
            "created_at": timestamp(created),
            "updated_at": timestamp(installation.updated_at.unwrap_or(created)),
            "single_file_name": null,
            "has_multiple_single_files": false,
            "single_file_paths": [],
            "suspended_by": installation.suspended_by.as_deref().and_then(|login| self.account(login)),
            "suspended_at": installation.suspended_at.map(timestamp),
        })
    }

    /// An App, as `GET /app` reports it.
    pub(crate) fn app(&self, app: &App) -> Value {
        let origin = self.origin;
        let created = timestamp(self.start);
        let installations = self
            .world
            .installations
            .iter()
            .filter(|i| i.app_id == app.id)
            .count();
        json!({
            "id": app.id,
            "slug": app.slug,
            "node_id": node_id("Integration", app.id),
            "client_id": app.client_id,
            "owner": app.owner.as_deref().and_then(|login| self.account(login)),
            "name": app.name.as_deref().unwrap_or(&app.slug),
            "description": "",
            "external_url": format!("{origin}/apps/{}", app.slug),
            "html_url": format!("{origin}/apps/{}", app.slug),
            "created_at": created,
            "updated_at": created,
            "permissions": app.permissions,
            "events": app.events,
            "installations_count": installations,
        })
    }

    /// An installation request.
    pub(crate) fn installation_request(&self, request: &InstallationRequest) -> Value {
        json!({
            "id": request.id,
            "node_id": STANDARD.encode(format!("028:IntegrationInstallationRequest{}", request.id)),
            "account": self.account(&request.account),
            "requester": self.account(&request.requester),
            "created_at": timestamp(request.created_at.unwrap_or(self.start)),
        })
    }
}
