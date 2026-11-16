//! JSON representations of world objects, in GitHub's shapes (the fields
//! ghinvite and octoauth read, plus the common ones around them).

use crate::seed::{App, Installation, InstallationRequest, Repo, RepositoryInvitation, Role, Seed};
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

impl View<'_> {
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
