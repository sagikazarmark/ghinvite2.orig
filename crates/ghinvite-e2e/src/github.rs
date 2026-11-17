//! The GitHub world the workflow suites start from, on the octoemu emulator.
//!
//! [`seed`] is a native octoemu seed: the organization `acme` with two
//! private repositories, its owner `creator`, the requester `alice`, and
//! ghinvite's repository and organization Apps installed on it, verifying
//! the fixture key [`crate::installed_apps`] signs with. [`World`] takes
//! the GitHub-side actions a suite needs through octoemu's admin API (an
//! invitee accepts or declines, a user renames, an installation's
//! repositories change) and stages GitHub's answers to the App's
//! collaborator writes as fault rules ([`Outcome`]). Counts come from the
//! emulator's call log.
//!
//! Nothing here is a GitHub fake: every answer is the emulator's, except
//! the fault rules, which are labelled `world: …` and are not GitHub
//! behaviour (see octoemu's README, "Fault rules").

use chrono::{DateTime, Utc};
use octoemu::Emulator;
use octoemu::faults::{Fault, FaultAction, Reply};
use octoemu::seed::{
    Access, App, Collaborator, Installation, Membership, Org, OrgRole, Repo, Role, Seed, User,
};
use serde_json::{Value, json};

/// The repository App's ID (`GH_REPO_APP_ID` in [`crate::installed_apps`]).
pub const REPO_APP_ID: u64 = 123;
/// The repository App's client ID (`GH_REPO_CLIENT_ID`).
pub const REPO_CLIENT_ID: &str = "Iv-p1-runtime-fixture";
/// The organization App's ID (`GH_ORG_APP_ID`).
pub const ORG_APP_ID: u64 = 456;
/// The organization App's client ID (`GH_ORG_CLIENT_ID`).
pub const ORG_CLIENT_ID: &str = "Iv-p1-runtime-fixture-org";
/// The repository App's installation on `acme`.
pub const REPO_INSTALLATION_ID: u64 = 1;
/// The organization App's installation on `acme`.
pub const ORG_INSTALLATION_ID: u64 = 2;
/// `acme`, the installation account (an organization).
pub const ACCOUNT_ID: u64 = 100;
/// `creator`, an owner of `acme` (admin on every repository).
pub const CREATOR_ID: u64 = 7;
/// `alice`, the requester.
pub const ALICE_ID: u64 = 8;
/// `acme/api`.
pub const API_REPO_ID: u64 = 10;
/// `acme/web`.
pub const WEB_REPO_ID: u64 = 11;

/// The public half of `test-fixtures/github-app-rsa.pem`, the key both
/// Apps verify JWTs against.
pub const PUBLIC_KEY: &str =
    include_str!("../../../libs/octoauth/crates/octoemu/tests/fixtures/rsa2048.pub.pem");

/// The suites' world: `acme` (100, an organization created in 2020) with
/// the private `acme/api` (10) and `acme/web` (11); `creator` (7) owns
/// `acme`; `alice` (8); the repository App (123: `administration: write`,
/// `metadata: read`) installed on all of `acme` as installation 1, and the
/// organization App (456: `members: write`, `metadata: read`) as 2, both
/// created 2026-01-01, without webhooks.
pub fn seed() -> Seed {
    seed_with_installation(REPO_INSTALLATION_ID)
}

/// [`seed`] with the repository App installed as `installation_id`.
pub fn seed_with_installation(installation_id: u64) -> Seed {
    let created: DateTime<Utc> = "2020-01-01T00:00:00Z".parse().expect("a timestamp");
    let mut seed = Seed::new()
        .user(User::new(CREATOR_ID, "creator"))
        .user(User::new(ALICE_ID, "alice"))
        .org(Org::new(ACCOUNT_ID, "acme").created_at(created))
        .membership(Membership::new("acme", "creator", OrgRole::Admin))
        .repo(Repo::new(API_REPO_ID, "acme", "api").private())
        .repo(Repo::new(WEB_REPO_ID, "acme", "web").private())
        .app(
            App::new(
                REPO_APP_ID,
                "ghinvite-repo-fixture",
                REPO_CLIENT_ID,
                "repo-fixture-client-secret",
            )
            .public_key(PUBLIC_KEY)
            .permission("administration", Access::Write)
            .permission("metadata", Access::Read),
        )
        .app(
            App::new(
                ORG_APP_ID,
                "ghinvite-org-fixture",
                ORG_CLIENT_ID,
                "org-fixture-client-secret",
            )
            .public_key(PUBLIC_KEY)
            .permission("members", Access::Write)
            .permission("metadata", Access::Read),
        )
        .installation(installed(installation_id, REPO_APP_ID));
    if installation_id != ORG_INSTALLATION_ID {
        seed = seed.installation(installed(ORG_INSTALLATION_ID, ORG_APP_ID));
    }
    seed
}

/// An installation on every `acme` repository, created 2026-01-01.
pub fn installed(id: u64, app_id: u64) -> Installation {
    let mut installation = Installation::new(id, app_id, "acme");
    installation.created_at = "2026-01-01T00:00:00Z".parse().ok();
    installation
}

/// GitHub's answer to the App's `PUT /repos/{owner}/{repo}/collaborators/{user}`,
/// staged with [`World::outcome`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// The user is already a direct `pull` collaborator, so a PUT for a
    /// higher role is a role change (204). A world change, not a rule.
    AlreadyCollaborator,
    /// Every PUT answers 422 Validation Failed.
    TerminalFailure,
    /// The next PUT answers 502 and changes nothing.
    TransientOnce,
    /// Every PUT is applied, then its response is lost (502).
    CreatedResponseLost,
    /// Every PUT answers 502 and changes nothing.
    Unanswered,
    /// The next PUT answers 403 `Resource not accessible by integration`.
    AccessLostOnce,
    /// The next PUT answers 403 with `Retry-After: 1` and the secondary
    /// rate limit's message.
    ThrottledOnce,
}

const LABEL: &str = "world: ";

/// GitHub-side actions on an emulator for the workflow suites.
pub struct World<'a> {
    emulator: &'a Emulator,
    client: reqwest::Client,
}

impl<'a> World<'a> {
    /// Actions on `emulator`.
    pub fn new(emulator: &'a Emulator) -> Self {
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("an HTTP client");
        Self { emulator, client }
    }

    /// The emulator.
    pub fn emulator(&self) -> &'a Emulator {
        self.emulator
    }

    /// `POST {admin}{path}` with an optional JSON body; panics unless 2xx.
    async fn admin(&self, path: &str, body: Option<Value>) {
        let request = self
            .client
            .post(format!("{}{path}", self.emulator.admin_base()));
        let request = match body {
            Some(body) => request.json(&body),
            None => request,
        };
        let response = request.send().await.expect("the emulator's admin API");
        let status = response.status();
        assert!(
            status.is_success(),
            "POST {path}: {status} {}",
            response.text().await.unwrap_or_default()
        );
    }

    /// The world as the emulator holds it now.
    fn world(&self) -> Value {
        self.emulator.state()["world"].take()
    }

    /// The full names of `owner`'s repositories.
    fn repositories(&self, owner: &str) -> Vec<String> {
        let world = self.world();
        world["repos"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|repo| repo["owner"].as_str() == Some(owner))
            .map(|repo| format!("{owner}/{}", repo["name"].as_str().unwrap_or_default()))
            .collect()
    }

    /// `login`'s open invitations as `(id, repository)`.
    pub fn invitations(&self, login: &str) -> Vec<(u64, String)> {
        let world = self.world();
        world["repository_invitations"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|invitation| {
                invitation["invitee"]
                    .as_str()
                    .is_some_and(|invitee| invitee.eq_ignore_ascii_case(login))
            })
            .filter_map(|invitation| {
                Some((
                    invitation["id"].as_u64()?,
                    invitation["repo"].as_str()?.to_owned(),
                ))
            })
            .collect()
    }

    /// The repositories `login` is a direct collaborator on.
    pub fn collaborations(&self, login: &str) -> Vec<String> {
        let world = self.world();
        world["collaborators"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|collaborator| {
                collaborator["user"]
                    .as_str()
                    .is_some_and(|user| user.eq_ignore_ascii_case(login))
            })
            .filter_map(|collaborator| Some(collaborator["repo"].as_str()?.to_owned()))
            .collect()
    }

    /// The invitee accepts invitation `id`: a collaborator with its role.
    pub async fn accept(&self, login: &str, id: u64) {
        self.admin(
            &format!("/act/{login}/repository_invitations/{id}/accept"),
            None,
        )
        .await;
    }

    /// The invitee declines invitation `id`.
    pub async fn decline(&self, login: &str, id: u64) {
        self.admin(
            &format!("/act/{login}/repository_invitations/{id}/decline"),
            None,
        )
        .await;
    }

    /// A collaborator leaves `repo` (`owner/name`).
    pub async fn leave(&self, login: &str, repo: &str) {
        self.admin(&format!("/act/{login}/repos/{repo}/leave"), None)
            .await;
    }

    /// The user changes their login; ID, collaborations and invitations stay
    /// theirs.
    pub async fn rename(&self, login: &str, to: &str) {
        self.admin(
            &format!("/users/{login}/rename"),
            Some(json!({"login": to})),
        )
        .await;
    }

    /// Installation `installation` now selects exactly these repositories
    /// (`installation_repositories` events follow).
    pub async fn select(&self, installation: u64, repository_ids: &[u64]) {
        let world = self.world();
        let names: Vec<String> = repository_ids
            .iter()
            .map(|id| {
                world["repos"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|repo| repo["id"].as_u64() == Some(*id))
                    .map(|repo| {
                        format!(
                            "{}/{}",
                            repo["owner"].as_str().unwrap_or_default(),
                            repo["name"].as_str().unwrap_or_default()
                        )
                    })
                    .unwrap_or_else(|| panic!("no repository {id}"))
            })
            .collect();
        self.admin(
            &format!("/installations/{installation}/repositories"),
            Some(json!({"repository_selection": "selected", "repositories": names})),
        )
        .await;
    }

    /// Installation `installation` selects all of its account's repositories
    /// again.
    pub async fn select_all(&self, installation: u64) {
        self.admin(
            &format!("/installations/{installation}/repositories"),
            Some(json!({"repository_selection": "all", "repositories": []})),
        )
        .await;
    }

    /// The invitation's 7 days are over.
    pub async fn expire(&self, invitation: u64) {
        self.admin(
            &format!("/repository_invitations/{invitation}/expire"),
            None,
        )
        .await;
    }

    /// `login` is a direct collaborator with `role` on every `acme`
    /// repository: they accept their open invitations there, leave a
    /// collaboration with another role, and are added with `role`.
    pub async fn collaborate(&self, login: &str, role: Role) {
        for (id, repo) in self.invitations(login) {
            if repo.starts_with("acme/") {
                self.accept(login, id).await;
            }
        }
        let current = self.collaborations(login);
        let mut added = Seed::new();
        for repo in self.repositories("acme") {
            if current.contains(&repo) {
                self.leave(login, &repo).await;
            }
            added = added.collaborator(Collaborator::new(repo, login, role.clone()));
        }
        self.emulator.seed(added).expect("collaborators");
    }

    /// `login` leaves every repository they are a direct collaborator on;
    /// their open invitations stay.
    pub async fn revoke(&self, login: &str) {
        for repo in self.collaborations(login) {
            self.leave(login, &repo).await;
        }
    }

    /// `login`'s open invitations declined and their direct access removed.
    pub async fn forget(&self, login: &str) {
        for (id, _) in self.invitations(login) {
            self.decline(login, id).await;
        }
        self.revoke(login).await;
    }

    /// An injected answer GitHub stays too consistent to give on its own: `GET
    /// /users/{login}` names account `id`, as if the login had been recycled
    /// between the App's reads of the user by ID and by login. Returns the
    /// rule's ID ([`Emulator::remove_fault`] ends it).
    pub fn recycled_login(&self, login: &str, id: u64) -> u64 {
        let body = json!({
            "id": id, "login": login, "type": "User",
            "node_id": format!("U_recycled{id}"),
            "created_at": "2020-01-01T00:00:00Z",
        });
        self.emulator
            .add_fault(
                Fault::on(
                    "GET",
                    format!("/users/{login}"),
                    FaultAction::respond_json(200, body),
                )
                .label(format!("{LABEL}recycled login {login}")),
            )
            .expect("a fault rule")
    }

    /// Stages GitHub's answer to the App's collaborator PUTs for `login` on
    /// `repo` (`owner/name`). Returns the fault rule's ID, if it is one.
    pub async fn outcome(&self, repo: &str, login: &str, outcome: Outcome) -> Option<u64> {
        let path = format!("/repos/{repo}/collaborators/{login}");
        let rule = |action: FaultAction, once: bool, name: &str| {
            let fault = Fault::on("PUT", path.clone(), action).label(format!("{LABEL}{name}"));
            let fault = if once { fault.once() } else { fault };
            self.emulator.add_fault(fault).expect("a fault rule")
        };
        Some(match outcome {
            Outcome::AlreadyCollaborator => {
                for (id, invited) in self.invitations(login) {
                    if invited.eq_ignore_ascii_case(repo) {
                        self.decline(login, id).await;
                    }
                }
                if !self
                    .collaborations(login)
                    .iter()
                    .any(|c| c.eq_ignore_ascii_case(repo))
                {
                    self.emulator
                        .seed(Seed::new().collaborator(Collaborator::new(repo, login, Role::Pull)))
                        .expect("a collaborator");
                }
                return None;
            }
            Outcome::TerminalFailure => rule(
                FaultAction::respond_json(
                    422,
                    json!({
                        "message": "Validation Failed",
                        "errors": [{"resource": "Repository", "code": "custom", "message": "fixture refusal"}],
                        "documentation_url": "https://docs.github.com/rest",
                    }),
                ),
                false,
                "terminal failure",
            ),
            Outcome::TransientOnce => rule(
                FaultAction::Respond(Reply::new(502).text("fixture: bad gateway")),
                true,
                "transient once",
            ),
            Outcome::CreatedResponseLost => rule(
                FaultAction::ApplyThenLose { status: Some(502) },
                false,
                "created, response lost",
            ),
            Outcome::Unanswered => rule(
                FaultAction::Respond(Reply::new(502).text("fixture: bad gateway")),
                false,
                "unanswered",
            ),
            Outcome::AccessLostOnce => rule(
                FaultAction::respond_json(
                    403,
                    json!({
                        "message": "Resource not accessible by integration",
                        "documentation_url": "https://docs.github.com/rest",
                    }),
                ),
                true,
                "access lost once",
            ),
            Outcome::ThrottledOnce => rule(
                FaultAction::Respond(
                    Reply::new(403).header("retry-after", "1").json(json!({
                        "message": "You have exceeded a secondary rate limit. Please wait a few minutes before you try again.",
                        "documentation_url": "https://docs.github.com/rest/overview/rate-limits-for-the-rest-api#about-secondary-rate-limits",
                    })),
                ),
                true,
                "throttled once",
            ),
        })
    }

    /// Adds `fault`, labelled `world: {name}`, so [`World::clear_rules`] and
    /// [`World::reset`] remove it. Returns its ID.
    pub fn rule(&self, name: &str, fault: Fault) -> u64 {
        self.emulator
            .add_fault(fault.label(format!("{LABEL}{name}")))
            .expect("a fault rule")
    }

    /// Removes every `world: …` fault rule (other rules stay).
    pub fn clear_rules(&self) {
        for fault in self.emulator.faults() {
            if fault.label.as_deref().is_some_and(|l| l.starts_with(LABEL)) {
                self.emulator.remove_fault(fault.id);
            }
        }
    }

    /// A fresh start for the next scenario: no `world: …` rules, no open
    /// repository invitations (each invitee declines), no direct
    /// collaborators (each leaves), and an empty call log. Logins,
    /// installations, the clock and other fault rules stay.
    pub async fn reset(&self) {
        self.clear_rules();
        let world = self.world();
        for invitation in world["repository_invitations"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if let (Some(id), Some(invitee)) =
                (invitation["id"].as_u64(), invitation["invitee"].as_str())
            {
                self.decline(invitee, id).await;
            }
        }
        for collaborator in world["collaborators"].as_array().into_iter().flatten() {
            if let (Some(repo), Some(user)) =
                (collaborator["repo"].as_str(), collaborator["user"].as_str())
            {
                self.leave(user, repo).await;
            }
        }
        self.emulator.clear_calls();
    }

    /// The App's collaborator PUTs in the call log.
    pub fn writes(&self) -> usize {
        self.write_statuses().len()
    }

    /// The statuses the App's collaborator PUTs were answered with, in
    /// order (0: no response).
    pub fn write_statuses(&self) -> Vec<u16> {
        self.emulator
            .calls()
            .iter()
            .filter(|call| {
                let parts: Vec<&str> = call
                    .path
                    .split('?')
                    .next()
                    .unwrap_or("")
                    .split('/')
                    .collect();
                call.method == "PUT"
                    && matches!(parts.as_slice(), ["", "repos", _, _, "collaborators", _])
            })
            .map(|call| call.status)
            .collect()
    }

    /// The `permission` each PUT to `path` asked for, in order.
    pub fn write_permissions(&self, path: &str) -> Vec<Option<String>> {
        self.emulator
            .calls()
            .iter()
            .filter(|call| call.method == "PUT" && call.path.split('?').next() == Some(path))
            .map(|call| {
                call.body_fields
                    .as_ref()
                    .and_then(|fields| fields.get("permission"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect()
    }

    /// Calls with `method` whose path (without the query) satisfies `path`.
    pub fn count(&self, method: &str, path: impl Fn(&str) -> bool) -> usize {
        self.emulator
            .calls()
            .iter()
            .filter(|call| call.method == method && path(call.path.split('?').next().unwrap_or("")))
            .count()
    }

    /// Whether the call log has a `method` call to exactly `path`.
    pub fn called(&self, method: &str, path: &str) -> bool {
        self.count(method, |p| p == path) > 0
    }

    /// The App's deletions of collaborators and invitations in the call log.
    pub fn deletions(&self) -> usize {
        self.count("DELETE", |path| {
            let parts: Vec<&str> = path.split('/').collect();
            matches!(
                parts.as_slice(),
                ["", "repos", _, _, "collaborators" | "invitations", _]
            )
        })
    }
}
