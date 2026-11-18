//! The emulated world's starting data: users, organizations, teams,
//! repositories, Apps, installations, collaborators and memberships.
//!
//! A [`Seed`] is JSON (`octoemu --seed seed.json`, `POST /__octoemu/seed`) or
//! built in Rust:
//!
//! ```
//! use octoemu::seed::{Access, App, Installation, Org, Plan, Repo, Seed, User};
//!
//! # let pem = include_str!("../tests/fixtures/rsa2048.pub.pem");
//! let seed = Seed::new()
//!     .user(User::new(1, "octocat"))
//!     .org(Org::new(2, "acme").plan(Plan::new("team", 10)))
//!     .repo(Repo::new(3, "acme", "widgets"))
//!     .app(
//!         App::new(4, "invite-bot", "Iv23.example", "fixture-secret")
//!             .public_key(pem)
//!             .permission("members", Access::Write),
//!     )
//!     .installation(Installation::new(5, 4, "acme"));
//! seed.validate().unwrap();
//! ```
//!
//! [`Seed::validate`] checks every reference (an installation's App and
//! account, a collaborator's repository and user, …) and reports every problem
//! at once, each with the JSON path of the offending field.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;

/// The emulated world's data, plus [`Settings`].
///
/// Every list defaults to empty; unknown fields are rejected so a typo in a
/// seed file fails instead of silently seeding less.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Seed {
    /// Emulator settings that travel with the seed.
    #[serde(default)]
    pub settings: Settings,
    /// User accounts.
    #[serde(default)]
    pub users: Vec<User>,
    /// Organization accounts. Logins and IDs share one namespace with users.
    #[serde(default)]
    pub orgs: Vec<Org>,
    /// Teams, each in an organization.
    #[serde(default)]
    pub teams: Vec<Team>,
    /// Repositories, each owned by a user or an organization.
    #[serde(default)]
    pub repos: Vec<Repo>,
    /// Registered GitHub Apps.
    #[serde(default)]
    pub apps: Vec<App>,
    /// App installations on accounts.
    #[serde(default)]
    pub installations: Vec<Installation>,
    /// Pending requests to install an App, made by organization members who
    /// cannot install it themselves.
    #[serde(default)]
    pub installation_requests: Vec<InstallationRequest>,
    /// Users with direct access to repositories.
    #[serde(default)]
    pub collaborators: Vec<Collaborator>,
    /// Organization memberships.
    #[serde(default)]
    pub memberships: Vec<Membership>,
    /// Team memberships.
    #[serde(default)]
    pub team_memberships: Vec<TeamMembership>,
    /// Organizations' custom repository roles.
    #[serde(default)]
    pub repository_roles: Vec<RepositoryRole>,
    /// Organization invitations: pending, failed (`failed_at`) or expired.
    /// Changed in place by the emulator: created by `POST
    /// /orgs/{org}/invitations` and its kin, removed when accepted, declined
    /// or cancelled.
    #[serde(default)]
    pub org_invitations: Vec<OrgInvitation>,
    /// Pending (or expired) repository invitations. Changed in place by the
    /// emulator: created by `PUT …/collaborators/{username}`, removed when
    /// accepted, declined or deleted.
    #[serde(default)]
    pub repository_invitations: Vec<RepositoryInvitation>,
}

/// Emulator settings carried by a seed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Where the virtual clock starts; the real time at start when absent.
    /// Only a starting seed may set it: a merged seed that does is rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<DateTime<Utc>>,
    /// Refuse behaviour the fidelity manifest labels `assumed` (see the
    /// crate docs). A merged seed can turn it on, never off.
    #[serde(default)]
    pub refuse_assumed: bool,
    /// New user access tokens a user may get per App per hour (on the
    /// virtual clock) before authorizing needs the browser again; 10 when
    /// absent. A merged seed may change it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_tokens_per_hour: Option<u32>,
}

/// A user account.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct User {
    /// The account ID, unique across users and organizations.
    pub id: u64,
    /// The login, unique (ignoring case) across users and organizations.
    pub login: String,
    /// Display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Primary email address: shorthand for one primary, verified, public
    /// address when `emails` is empty; otherwise it must be the primary one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Account creation time; the clock's start when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    /// `User` (the default) or `Bot`, reported as `type`.
    #[serde(rename = "type", default)]
    pub kind: UserKind,
    /// The account's email addresses, as `GET /user/emails` lists them:
    /// exactly one primary when any. Without them, `email` stands for one
    /// primary, verified, public address.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub emails: Vec<Email>,
    /// Also list GitHub's noreply address,
    /// `{id}+{login}@users.noreply.github.com` (verified, not primary).
    #[serde(default, skip_serializing_if = "is_false")]
    pub noreply_email: bool,
}

/// One of a user's email addresses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Email {
    /// The address.
    pub email: String,
    /// Whether it is the primary address (exactly one is).
    #[serde(default)]
    pub primary: bool,
    /// Whether it is verified (default true).
    #[serde(default = "yes")]
    pub verified: bool,
    /// `public` or `private`; only the primary address has one.
    #[serde(default)]
    pub visibility: Option<EmailVisibility>,
}

/// Who may see a primary email address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EmailVisibility {
    /// Shown on the profile: `GET /user`'s `email`.
    Public,
    /// Hidden from the profile.
    Private,
}

impl Email {
    /// A verified, non-primary address without a visibility.
    pub fn new(email: impl Into<String>) -> Self {
        Self {
            email: email.into(),
            primary: false,
            verified: true,
            visibility: None,
        }
    }

    /// Makes it the primary address, with `visibility`.
    #[must_use]
    pub fn primary(mut self, visibility: EmailVisibility) -> Self {
        self.primary = true;
        self.visibility = Some(visibility);
        self
    }

    /// Marks it unverified.
    #[must_use]
    pub fn unverified(mut self) -> Self {
        self.verified = false;
        self
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// A user account's `type`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum UserKind {
    /// A person.
    #[default]
    User,
    /// A bot account.
    Bot,
}

impl UserKind {
    /// GitHub's spelling: `User` or `Bot`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "User",
            Self::Bot => "Bot",
        }
    }
}

impl User {
    /// A user with an ID and login and nothing else.
    pub fn new(id: u64, login: impl Into<String>) -> Self {
        Self {
            id,
            login: login.into(),
            name: None,
            email: None,
            created_at: None,
            kind: UserKind::User,
            emails: Vec::new(),
            noreply_email: false,
        }
    }

    /// Adds an email address.
    #[must_use]
    pub fn email_address(mut self, email: Email) -> Self {
        self.emails.push(email);
        self
    }

    /// Also lists GitHub's noreply address.
    #[must_use]
    pub fn noreply_email(mut self) -> Self {
        self.noreply_email = true;
        self
    }

    /// Every address `GET /user/emails` lists: `emails`, or `email` as a
    /// primary, verified, public address; then the noreply address.
    pub fn email_addresses(&self) -> Vec<Email> {
        let mut all = self.emails.clone();
        if all.is_empty()
            && let Some(email) = &self.email
        {
            all.push(Email::new(email.clone()).primary(EmailVisibility::Public));
        }
        if self.noreply_email {
            all.push(Email::new(format!(
                "{}+{}@users.noreply.github.com",
                self.id, self.login
            )));
        }
        all
    }

    /// The primary address when it is public: `GET /user`'s `email`.
    pub fn public_email(&self) -> Option<String> {
        self.email_addresses()
            .into_iter()
            .find(|e| e.primary && e.visibility == Some(EmailVisibility::Public))
            .map(|e| e.email)
    }

    /// Makes the account a bot (`type: "Bot"`).
    #[must_use]
    pub fn bot(mut self) -> Self {
        self.kind = UserKind::Bot;
        self
    }

    /// Sets the display name.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Sets the primary email address (verified and public unless `emails`
    /// say otherwise).
    #[must_use]
    pub fn email(mut self, email: impl Into<String>) -> Self {
        self.email = Some(email.into());
        self
    }

    /// Sets the creation time.
    #[must_use]
    pub fn created_at(mut self, at: DateTime<Utc>) -> Self {
        self.created_at = Some(at);
        self
    }
}

/// An organization account.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Org {
    /// The account ID, unique across users and organizations.
    pub id: u64,
    /// The login, unique (ignoring case) across users and organizations.
    pub login: String,
    /// Display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The billing plan (`GET /orgs/{org}` → `plan`).
    #[serde(default)]
    pub plan: Plan,
    /// Creation time; invite budgets depend on the organization's age.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    /// Whether members must have two-factor authentication.
    #[serde(default)]
    pub two_factor_requirement_enabled: bool,
    /// The base permission every member has on the organization's
    /// repositories.
    #[serde(default)]
    pub default_repository_permission: BasePermission,
    /// Whether only owners may invite outside collaborators (GitHub's
    /// "Allow repository administrators to invite outside collaborators"
    /// switched off): an App's `PUT …/collaborators/{username}` for a
    /// non-member is refused (behaviour `outside-collaborator-policy`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub outside_collaborators_restricted: bool,
}

impl Org {
    /// An organization on the free plan, created when the clock starts.
    pub fn new(id: u64, login: impl Into<String>) -> Self {
        Self {
            id,
            login: login.into(),
            name: None,
            plan: Plan::default(),
            created_at: None,
            two_factor_requirement_enabled: false,
            default_repository_permission: BasePermission::default(),
            outside_collaborators_restricted: false,
        }
    }

    /// Sets the display name.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Sets the plan.
    #[must_use]
    pub fn plan(mut self, plan: Plan) -> Self {
        self.plan = plan;
        self
    }

    /// Sets the creation time.
    #[must_use]
    pub fn created_at(mut self, at: DateTime<Utc>) -> Self {
        self.created_at = Some(at);
        self
    }

    /// Sets the members' base repository permission.
    #[must_use]
    pub fn default_repository_permission(mut self, permission: BasePermission) -> Self {
        self.default_repository_permission = permission;
        self
    }

    /// Requires two-factor authentication of members.
    #[must_use]
    pub fn two_factor_required(mut self) -> Self {
        self.two_factor_requirement_enabled = true;
        self
    }

    /// Lets only owners invite outside collaborators.
    #[must_use]
    pub fn restrict_outside_collaborators(mut self) -> Self {
        self.outside_collaborators_restricted = true;
        self
    }
}

/// An organization's billing plan.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    /// The plan name as GitHub reports it (`free`, `team`, …).
    pub name: String,
    /// Paid seats.
    #[serde(default)]
    pub seats: u32,
    /// Seats in use; derived from memberships and invitations when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filled_seats: Option<u32>,
}

impl Plan {
    /// A plan with a name and a seat count.
    pub fn new(name: impl Into<String>, seats: u32) -> Self {
        Self {
            name: name.into(),
            seats,
            filled_seats: None,
        }
    }

    /// Pins the number of filled seats.
    #[must_use]
    pub fn filled_seats(mut self, filled: u32) -> Self {
        self.filled_seats = Some(filled);
        self
    }
}

impl Default for Plan {
    fn default() -> Self {
        Self::new("free", 0)
    }
}

/// An organization's base repository permission.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BasePermission {
    /// No base access.
    None,
    /// Read (GitHub's default).
    #[default]
    Read,
    /// Write.
    Write,
    /// Admin.
    Admin,
}

/// A team in an organization.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Team {
    /// The team ID, unique across teams.
    pub id: u64,
    /// The organization's login.
    pub org: String,
    /// The slug, unique (ignoring case) in the organization. Other entries
    /// name the team as `org/slug`.
    pub slug: String,
    /// Display name; the slug when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Visibility.
    #[serde(default)]
    pub privacy: TeamPrivacy,
    /// The parent team's slug (same organization), if nested. A child team's
    /// members inherit the parent's repository access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// The team's repositories and its role on each; active members (of
    /// this team or a child team) inherit it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repositories: Vec<TeamRepository>,
}

/// A team's role on one of its organization's repositories.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamRepository {
    /// The repository as `owner/name`, owned by the team's organization.
    pub repo: String,
    /// The role members inherit.
    #[serde(default = "pull")]
    pub role: Role,
}

fn pull() -> Role {
    Role::Pull
}

impl Team {
    /// A visible team.
    pub fn new(id: u64, org: impl Into<String>, slug: impl Into<String>) -> Self {
        Self {
            id,
            org: org.into(),
            slug: slug.into(),
            name: None,
            privacy: TeamPrivacy::default(),
            parent: None,
            repositories: Vec::new(),
        }
    }

    /// Nests the team under `parent` (a slug in the same organization).
    #[must_use]
    pub fn parent(mut self, slug: impl Into<String>) -> Self {
        self.parent = Some(slug.into());
        self
    }

    /// Grants the team `role` on `repo` (`owner/name`).
    #[must_use]
    pub fn repository(mut self, repo: impl Into<String>, role: Role) -> Self {
        self.repositories.push(TeamRepository {
            repo: repo.into(),
            role,
        });
        self
    }

    /// The display name: `name`, else the slug.
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.slug)
    }

    /// Sets the display name.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Sets the visibility.
    #[must_use]
    pub fn privacy(mut self, privacy: TeamPrivacy) -> Self {
        self.privacy = privacy;
        self
    }
}

/// A team's visibility.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TeamPrivacy {
    /// Visible to every organization member.
    #[default]
    Closed,
    /// Visible to its members and organization owners only.
    Secret,
}

/// A repository.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repo {
    /// The repository ID, unique across repositories.
    pub id: u64,
    /// The owning account's login (a user or an organization).
    pub owner: String,
    /// The name, unique (ignoring case) per owner. Other entries name the
    /// repository as `owner/name`.
    pub name: String,
    /// Whether the repository is private.
    #[serde(default)]
    pub private: bool,
    /// `public`, `private` or `internal`; derived from `private` when
    /// absent. `private` or `internal` makes the repository private
    /// whatever `private` says; `public` with `private: true` is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,
    /// Creation time; the clock's start when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
}

/// A repository's visibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    /// Anyone can see it.
    Public,
    /// Only those with access.
    Private,
    /// An organization's enterprise members (private to everyone else,
    /// installation tokens included).
    Internal,
}

impl Visibility {
    /// GitHub's spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Private => "private",
            Self::Internal => "internal",
        }
    }
}

impl Repo {
    /// A public repository.
    pub fn new(id: u64, owner: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id,
            owner: owner.into(),
            name: name.into(),
            private: false,
            visibility: None,
            created_at: None,
        }
    }

    /// Makes the repository private.
    #[must_use]
    pub fn private(mut self) -> Self {
        self.private = true;
        self
    }

    /// Sets the visibility (`private` follows it).
    #[must_use]
    pub fn visibility(mut self, visibility: Visibility) -> Self {
        self.private = visibility != Visibility::Public;
        self.visibility = Some(visibility);
        self
    }

    /// The effective visibility.
    pub fn effective_visibility(&self) -> Visibility {
        match self.visibility {
            Some(visibility) => visibility,
            None if self.private => Visibility::Private,
            None => Visibility::Public,
        }
    }

    /// Whether only those with access can see it (`private` or `internal`).
    pub fn is_private(&self) -> bool {
        self.effective_visibility() != Visibility::Public
    }

    /// `owner/name`.
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

/// A registered GitHub App.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct App {
    /// The App ID, unique across Apps.
    pub id: u64,
    /// The slug, unique (ignoring case) across Apps.
    pub slug: String,
    /// Display name; the slug when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The owning account's login, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// The OAuth client ID, unique across Apps.
    pub client_id: String,
    /// The OAuth client secret. Redacted from `GET /__octoemu/state`.
    pub client_secret: String,
    /// The registered RSA public keys (PEM, SPKI or PKCS#1); App JWTs must
    /// verify against one of them. At least one is required.
    #[serde(default)]
    pub public_keys: Vec<String>,
    /// Where webhooks go, if anywhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook: Option<Webhook>,
    /// Registered OAuth callback URLs.
    #[serde(default)]
    pub callback_urls: Vec<String>,
    /// Permissions the App requests, by GitHub's names (`members`,
    /// `administration`, …).
    #[serde(default)]
    pub permissions: BTreeMap<String, Access>,
    /// Webhook events the App subscribes to.
    #[serde(default)]
    pub events: Vec<String>,
    /// Account permissions the App requests of users who sign in
    /// (`email_addresses`, …): what its user access tokens hold besides
    /// `permissions`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub user_permissions: BTreeMap<String, Access>,
    /// Whether user access tokens expire (8 hours, with a refresh token);
    /// GitHub's default, true.
    #[serde(default = "yes")]
    pub expire_user_tokens: bool,
    /// A suspended App: its authorization requests fail with
    /// `application_suspended`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub suspended: bool,
}

impl App {
    /// An App with no keys, webhook, callbacks, permissions or events yet.
    pub fn new(
        id: u64,
        slug: impl Into<String>,
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
    ) -> Self {
        Self {
            id,
            slug: slug.into(),
            name: None,
            owner: None,
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            public_keys: Vec::new(),
            webhook: None,
            callback_urls: Vec::new(),
            permissions: BTreeMap::new(),
            events: Vec::new(),
            user_permissions: BTreeMap::new(),
            expire_user_tokens: true,
            suspended: false,
        }
    }

    /// Requests an account permission of signing-in users.
    #[must_use]
    pub fn user_permission(mut self, name: impl Into<String>, access: Access) -> Self {
        self.user_permissions.insert(name.into(), access);
        self
    }

    /// User access tokens that never expire (and come without a refresh
    /// token).
    #[must_use]
    pub fn non_expiring_user_tokens(mut self) -> Self {
        self.expire_user_tokens = false;
        self
    }

    /// Suspends the App.
    #[must_use]
    pub fn suspended(mut self) -> Self {
        self.suspended = true;
        self
    }

    /// Sets the display name.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Sets the owning account.
    #[must_use]
    pub fn owner(mut self, login: impl Into<String>) -> Self {
        self.owner = Some(login.into());
        self
    }

    /// Registers a public key.
    #[must_use]
    pub fn public_key(mut self, pem: impl Into<String>) -> Self {
        self.public_keys.push(pem.into());
        self
    }

    /// Sets the webhook.
    #[must_use]
    pub fn webhook(mut self, webhook: Webhook) -> Self {
        self.webhook = Some(webhook);
        self
    }

    /// Registers an OAuth callback URL.
    #[must_use]
    pub fn callback_url(mut self, url: impl Into<String>) -> Self {
        self.callback_urls.push(url.into());
        self
    }

    /// Requests a permission.
    #[must_use]
    pub fn permission(mut self, name: impl Into<String>, access: Access) -> Self {
        self.permissions.insert(name.into(), access);
        self
    }

    /// Subscribes to a webhook event.
    #[must_use]
    pub fn event(mut self, event: impl Into<String>) -> Self {
        self.events.push(event.into());
        self
    }
}

/// An App's webhook.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Webhook {
    /// The delivery URL (`http` or `https`).
    pub url: String,
    /// Signing secrets. Deliveries are signed with the first; the rest let
    /// tests model a rotation in progress. Empty means unsigned deliveries.
    /// Redacted from `GET /__octoemu/state`.
    #[serde(default)]
    pub secrets: Vec<String>,
    /// Whether deliveries are sent.
    #[serde(default = "yes")]
    pub active: bool,
}

fn yes() -> bool {
    true
}

impl Webhook {
    /// An active webhook signed with `secret`.
    pub fn new(url: impl Into<String>, secret: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            secrets: vec![secret.into()],
            active: true,
        }
    }

    /// Adds a further secret (a rotation in progress).
    #[must_use]
    pub fn secret(mut self, secret: impl Into<String>) -> Self {
        self.secrets.push(secret.into());
        self
    }
}

/// A permission level. Ordered: `read < write < admin`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    /// Read.
    Read,
    /// Write (implies read).
    Write,
    /// Admin (implies write).
    Admin,
}

impl fmt::Display for Access {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Admin => "admin",
        })
    }
}

/// An App installation on a user or organization account.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Installation {
    /// The installation ID, unique across installations.
    pub id: u64,
    /// The installed App's ID.
    pub app_id: u64,
    /// The account's login. An App installs at most once per account.
    pub account: String,
    /// Whether the installation covers all of the account's repositories.
    #[serde(default)]
    pub repository_selection: RepositorySelection,
    /// The selected repositories as `owner/name`, all owned by the account;
    /// only with `repository_selection: selected`.
    #[serde(default)]
    pub repositories: Vec<String>,
    /// Granted permissions; the App's when absent. Never wider than the
    /// App's (an installation may lag behind an App that asked for more).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<BTreeMap<String, Access>>,
    /// Subscribed events; the App's when absent. A subset of the App's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<Vec<String>>,
    /// When the installation was suspended, if it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspended_at: Option<DateTime<Utc>>,
    /// The user who suspended it (a login), if known; only with
    /// `suspended_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspended_by: Option<String>,
    /// Creation time; the clock's start when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    /// Last change (suspension, for one); `created_at` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
}

impl Installation {
    /// An installation on all of the account's repositories with the App's
    /// permissions and events.
    pub fn new(id: u64, app_id: u64, account: impl Into<String>) -> Self {
        Self {
            id,
            app_id,
            account: account.into(),
            repository_selection: RepositorySelection::All,
            repositories: Vec::new(),
            permissions: None,
            events: None,
            suspended_at: None,
            suspended_by: None,
            created_at: None,
            updated_at: None,
        }
    }

    /// Limits the installation to the given repositories (`owner/name`).
    #[must_use]
    pub fn selected<I, S>(mut self, repositories: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.repository_selection = RepositorySelection::Selected;
        self.repositories = repositories.into_iter().map(Into::into).collect();
        self
    }

    /// Grants exactly these permissions instead of the App's.
    #[must_use]
    pub fn permissions<I, S>(mut self, permissions: I) -> Self
    where
        I: IntoIterator<Item = (S, Access)>,
        S: Into<String>,
    {
        self.permissions = Some(
            permissions
                .into_iter()
                .map(|(name, access)| (name.into(), access))
                .collect(),
        );
        self
    }

    /// Marks the installation suspended.
    #[must_use]
    pub fn suspended_at(mut self, at: DateTime<Utc>) -> Self {
        self.suspended_at = Some(at);
        self
    }

    /// Names the user (a login) who suspended the installation.
    #[must_use]
    pub fn suspended_by(mut self, login: impl Into<String>) -> Self {
        self.suspended_by = Some(login.into());
        self
    }
}

/// A request to install an App on an organization, made by a member who
/// cannot install it (`GET /app/installation-requests`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallationRequest {
    /// The request ID, unique across installation requests.
    pub id: u64,
    /// The requested App's ID.
    pub app_id: u64,
    /// The account (an organization's or user's login) to install on.
    pub account: String,
    /// The requesting user's login.
    pub requester: String,
    /// When it was requested; the clock's start when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
}

impl InstallationRequest {
    /// `requester` asks for App `app_id` on `account`.
    pub fn new(
        id: u64,
        app_id: u64,
        account: impl Into<String>,
        requester: impl Into<String>,
    ) -> Self {
        Self {
            id,
            app_id,
            account: account.into(),
            requester: requester.into(),
            created_at: None,
        }
    }

    /// Sets when it was requested.
    #[must_use]
    pub fn created_at(mut self, at: DateTime<Utc>) -> Self {
        self.created_at = Some(at);
        self
    }
}

/// Which repositories an installation covers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RepositorySelection {
    /// Every repository of the account, current and future.
    #[default]
    All,
    /// Only the listed repositories.
    Selected,
}

/// A user's direct access to a repository.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Collaborator {
    /// The repository as `owner/name`.
    pub repo: String,
    /// The user's login.
    pub user: String,
    /// The role.
    pub role: Role,
}

impl Collaborator {
    /// `user` with `role` on `repo` (`owner/name`).
    pub fn new(repo: impl Into<String>, user: impl Into<String>, role: Role) -> Self {
        Self {
            repo: repo.into(),
            user: user.into(),
            role,
        }
    }
}

/// A repository role: GitHub's five, or a custom role's name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum Role {
    /// `pull` (shown as Read).
    Pull,
    /// `triage`.
    Triage,
    /// `push` (shown as Write).
    Push,
    /// `maintain`.
    Maintain,
    /// `admin`.
    Admin,
    /// An organization's custom repository role.
    Custom(String),
}

impl From<String> for Role {
    fn from(name: String) -> Self {
        match name.as_str() {
            "pull" => Self::Pull,
            "triage" => Self::Triage,
            "push" => Self::Push,
            "maintain" => Self::Maintain,
            "admin" => Self::Admin,
            _ => Self::Custom(name),
        }
    }
}

impl From<Role> for String {
    fn from(role: Role) -> Self {
        match role {
            Role::Pull => "pull".into(),
            Role::Triage => "triage".into(),
            Role::Push => "push".into(),
            Role::Maintain => "maintain".into(),
            Role::Admin => "admin".into(),
            Role::Custom(name) => name,
        }
    }
}

/// An organization membership.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Membership {
    /// The organization's login.
    pub org: String,
    /// The user's login.
    pub user: String,
    /// The role.
    #[serde(default)]
    pub role: OrgRole,
    /// `pending` until the user accepts an invitation.
    #[serde(default)]
    pub state: MembershipState,
}

impl Membership {
    /// An active membership with `role`.
    pub fn new(org: impl Into<String>, user: impl Into<String>, role: OrgRole) -> Self {
        Self {
            org: org.into(),
            user: user.into(),
            role,
            state: MembershipState::Active,
        }
    }

    /// Makes the membership pending.
    #[must_use]
    pub fn pending(mut self) -> Self {
        self.state = MembershipState::Pending;
        self
    }
}

/// An organization role.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrgRole {
    /// A member.
    #[default]
    Member,
    /// An owner.
    Admin,
}

/// Whether a membership is in effect.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MembershipState {
    /// In effect.
    #[default]
    Active,
    /// Invited, not yet accepted.
    Pending,
}

/// A team membership.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamMembership {
    /// The team as `org/slug`.
    pub team: String,
    /// The user's login. The user needs a membership of the team's
    /// organization, an active one for an active team membership.
    pub user: String,
    /// The role.
    #[serde(default)]
    pub role: TeamRole,
    /// `pending` while the organization membership is.
    #[serde(default)]
    pub state: MembershipState,
}

impl TeamMembership {
    /// An active membership with `role` in `team` (`org/slug`).
    pub fn new(team: impl Into<String>, user: impl Into<String>, role: TeamRole) -> Self {
        Self {
            team: team.into(),
            user: user.into(),
            role,
            state: MembershipState::Active,
        }
    }

    /// Makes the membership pending.
    #[must_use]
    pub fn pending(mut self) -> Self {
        self.state = MembershipState::Pending;
        self
    }
}

/// A team role.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TeamRole {
    /// A member.
    #[default]
    Member,
    /// A maintainer.
    Maintainer,
}

/// An invitation to join an organization, addressed to a user or an email
/// address.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrgInvitation {
    /// The invitation ID, unique across organization invitations.
    pub id: u64,
    /// The organization's login.
    pub org: String,
    /// The invited user's login; exactly one of `invitee` and `email`. A
    /// user is invited at most once per organization (while the invitation
    /// is open) and never while a member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invitee: Option<String>,
    /// The invited email address; exactly one of `invitee` and `email`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// The offered role.
    #[serde(default)]
    pub role: InvitationRole,
    /// The inviting user's login, if a user invited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inviter: Option<String>,
    /// The inviting App's ID, if an App invited (its bot account is
    /// reported as the inviter). Not with `inviter`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inviter_app_id: Option<u64>,
    /// Teams (IDs, in the organization) the invitee joins on accepting.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub team_ids: Vec<u64>,
    /// The role in each of `team_ids` when not `member` (by team ID).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub team_roles: BTreeMap<u64, TeamRole>,
    /// When it was sent; the clock's start when absent. It expires seven
    /// days later on the virtual clock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    /// When it failed, if it did (`GET /orgs/{org}/failed_invitations`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_at: Option<DateTime<Utc>>,
    /// Why it failed; only with `failed_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_reason: Option<String>,
}

impl OrgInvitation {
    /// `login` invited to `org` as a direct member.
    pub fn new(id: u64, org: impl Into<String>, login: impl Into<String>) -> Self {
        Self {
            id,
            org: org.into(),
            invitee: Some(login.into()),
            email: None,
            role: InvitationRole::DirectMember,
            inviter: None,
            inviter_app_id: None,
            team_ids: Vec::new(),
            team_roles: BTreeMap::new(),
            created_at: None,
            failed_at: None,
            failed_reason: None,
        }
    }

    /// `email` invited to `org` as a direct member.
    pub fn by_email(id: u64, org: impl Into<String>, email: impl Into<String>) -> Self {
        let mut invitation = Self::new(id, org, "");
        invitation.invitee = None;
        invitation.email = Some(email.into());
        invitation
    }

    /// Sets the offered role.
    #[must_use]
    pub fn role(mut self, role: InvitationRole) -> Self {
        self.role = role;
        self
    }

    /// Names the inviting user.
    #[must_use]
    pub fn inviter(mut self, login: impl Into<String>) -> Self {
        self.inviter = Some(login.into());
        self
    }

    /// Adds a team the invitee joins on accepting.
    #[must_use]
    pub fn team(mut self, id: u64) -> Self {
        self.team_ids.push(id);
        self
    }

    /// Sets when it was sent.
    #[must_use]
    pub fn created_at(mut self, at: DateTime<Utc>) -> Self {
        self.created_at = Some(at);
        self
    }

    /// Marks it failed at `at` for `reason`.
    #[must_use]
    pub fn failed(mut self, at: DateTime<Utc>, reason: impl Into<String>) -> Self {
        self.failed_at = Some(at);
        self.failed_reason = Some(reason.into());
        self
    }
}

/// The role an organization invitation offers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvitationRole {
    /// A member.
    #[default]
    DirectMember,
    /// An owner.
    Admin,
    /// A billing manager (not a member).
    BillingManager,
    /// A hiring manager (not a member).
    HiringManager,
}

impl InvitationRole {
    /// GitHub's spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DirectMember => "direct_member",
            Self::Admin => "admin",
            Self::BillingManager => "billing_manager",
            Self::HiringManager => "hiring_manager",
        }
    }
}

/// An organization's custom repository role: a name over one of GitHub's
/// base roles. Collaborators and invitations on the organization's
/// repositories may name it as their role.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryRole {
    /// The role ID, unique across custom roles.
    pub id: u64,
    /// The organization's login.
    pub org: String,
    /// The name, unique (ignoring case) in the organization and none of
    /// GitHub's role names.
    pub name: String,
    /// The base role it extends.
    pub base_role: BaseRole,
}

impl RepositoryRole {
    /// A custom role `name` in `org` over `base_role`.
    pub fn new(
        id: u64,
        org: impl Into<String>,
        name: impl Into<String>,
        base_role: BaseRole,
    ) -> Self {
        Self {
            id,
            org: org.into(),
            name: name.into(),
            base_role,
        }
    }
}

/// The base of a custom repository role (GitHub allows these four).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BaseRole {
    /// Read.
    Read,
    /// Triage.
    Triage,
    /// Write.
    Write,
    /// Maintain.
    Maintain,
}

impl BaseRole {
    /// The built-in role it corresponds to.
    pub fn role(self) -> Role {
        match self {
            Self::Read => Role::Pull,
            Self::Triage => Role::Triage,
            Self::Write => Role::Push,
            Self::Maintain => Role::Maintain,
        }
    }
}

/// An invitation to collaborate on a repository.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryInvitation {
    /// The invitation ID, unique across repository invitations.
    pub id: u64,
    /// The repository as `owner/name`.
    pub repo: String,
    /// The invited user's login. At most one invitation per user and
    /// repository, and not to a collaborator.
    pub invitee: String,
    /// The inviting user's login, if a user invited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inviter: Option<String>,
    /// The inviting App's ID, if an App invited (its bot account is
    /// reported as the inviter). Not with `inviter`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inviter_app_id: Option<u64>,
    /// The offered role.
    #[serde(default = "push")]
    pub role: Role,
    /// When it was sent; the clock's start when absent. It expires seven
    /// days later on the virtual clock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
}

fn push() -> Role {
    Role::Push
}

impl RepositoryInvitation {
    /// `invitee` invited to `repo` (`owner/name`) with `role`.
    pub fn new(id: u64, repo: impl Into<String>, invitee: impl Into<String>, role: Role) -> Self {
        Self {
            id,
            repo: repo.into(),
            invitee: invitee.into(),
            inviter: None,
            inviter_app_id: None,
            role,
            created_at: None,
        }
    }

    /// Names the inviting user.
    #[must_use]
    pub fn inviter(mut self, login: impl Into<String>) -> Self {
        self.inviter = Some(login.into());
        self
    }

    /// Sets when it was sent.
    #[must_use]
    pub fn created_at(mut self, at: DateTime<Utc>) -> Self {
        self.created_at = Some(at);
        self
    }
}

macro_rules! adders {
    ($($(#[$doc:meta])* $method:ident($ty:ty) => $field:ident;)*) => {
        $(
            $(#[$doc])*
            #[must_use]
            pub fn $method(mut self, item: $ty) -> Self {
                self.$field.push(item);
                self
            }
        )*
    };
}

impl Seed {
    /// An empty seed.
    pub fn new() -> Self {
        Self::default()
    }

    adders! {
        /// Adds a user.
        user(User) => users;
        /// Adds an organization.
        org(Org) => orgs;
        /// Adds a team.
        team(Team) => teams;
        /// Adds a repository.
        repo(Repo) => repos;
        /// Adds an App.
        app(App) => apps;
        /// Adds an installation.
        installation(Installation) => installations;
        /// Adds an installation request.
        installation_request(InstallationRequest) => installation_requests;
        /// Adds a collaborator.
        collaborator(Collaborator) => collaborators;
        /// Adds an organization membership.
        membership(Membership) => memberships;
        /// Adds a team membership.
        team_membership(TeamMembership) => team_memberships;
        /// Adds a custom repository role.
        repository_role(RepositoryRole) => repository_roles;
        /// Adds a repository invitation.
        repository_invitation(RepositoryInvitation) => repository_invitations;
        /// Adds an organization invitation.
        org_invitation(OrgInvitation) => org_invitations;
    }

    /// Starts the virtual clock at `at`.
    #[must_use]
    pub fn start_time(mut self, at: DateTime<Utc>) -> Self {
        self.settings.start_time = Some(at);
        self
    }

    /// Refuses behaviour labelled `assumed` in the fidelity manifest.
    #[must_use]
    pub fn refuse_assumed(mut self) -> Self {
        self.settings.refuse_assumed = true;
        self
    }

    /// Parses a JSON seed. Structure only: call [`Seed::validate`] (or start
    /// an emulator) to check references.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Checks uniqueness and every reference; reports all problems at once.
    pub fn validate(&self) -> Result<(), SeedError> {
        let mut check = Check::default();
        check.run(self);
        check.finish()
    }

    /// `self` plus `more`, validated as a whole. `more` may not move the
    /// clock; it may turn `refuse_assumed` on.
    pub fn merged(&self, more: Seed) -> Result<Seed, SeedError> {
        let mut problems = Vec::new();
        if more.settings.start_time.is_some() {
            problems.push(Problem::new(
                "settings.start_time",
                "the clock starts once; move it with POST /__octoemu/clock/advance",
            ));
        }
        let mut all = self.clone();
        all.settings.refuse_assumed |= more.settings.refuse_assumed;
        if more.settings.user_tokens_per_hour.is_some() {
            all.settings.user_tokens_per_hour = more.settings.user_tokens_per_hour;
        }
        all.users.extend(more.users);
        all.orgs.extend(more.orgs);
        all.teams.extend(more.teams);
        all.repos.extend(more.repos);
        all.apps.extend(more.apps);
        all.installations.extend(more.installations);
        all.installation_requests.extend(more.installation_requests);
        all.collaborators.extend(more.collaborators);
        all.memberships.extend(more.memberships);
        all.team_memberships.extend(more.team_memberships);
        all.repository_roles.extend(more.repository_roles);
        all.repository_invitations
            .extend(more.repository_invitations);
        all.org_invitations.extend(more.org_invitations);
        if let Err(error) = all.validate() {
            problems.extend(error.problems);
        }
        if problems.is_empty() {
            Ok(all)
        } else {
            Err(SeedError { problems })
        }
    }
}

/// A seed that failed validation: every problem found, in seed order.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub struct SeedError {
    problems: Vec<Problem>,
}

impl SeedError {
    /// The problems, at least one.
    pub fn problems(&self) -> &[Problem] {
        &self.problems
    }
}

impl fmt::Display for SeedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid seed: ")?;
        for (i, problem) in self.problems.iter().enumerate() {
            if i > 0 {
                f.write_str("; ")?;
            }
            write!(f, "{problem}")?;
        }
        Ok(())
    }
}

/// One validation problem.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Problem {
    /// The offending field's JSON path, such as `installations[0].app_id`.
    pub path: String,
    /// What is wrong.
    pub message: String,
}

impl Problem {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AccountKind {
    User,
    Org,
}

/// The validation pass: indexes built as it goes, problems collected.
#[derive(Default)]
struct Check {
    problems: Vec<Problem>,
    /// Lowercased login → kind.
    accounts: HashMap<String, AccountKind>,
    /// Lowercased `owner/name` → owner (lowercased).
    repos: HashMap<String, String>,
    /// Lowercased `org/slug`.
    teams: HashSet<String>,
    apps: HashMap<u64, usize>,
    /// (lowercased org, lowercased user) → state.
    memberships: HashMap<(String, String), MembershipState>,
    /// Lowercased org → lowercased custom role names.
    roles: HashMap<String, HashSet<String>>,
    /// (lowercased `owner/name`, lowercased user) of every collaborator.
    collaborators: HashSet<(String, String)>,
}

impl Check {
    fn problem(&mut self, path: String, message: String) {
        self.problems.push(Problem { path, message });
    }

    fn finish(self) -> Result<(), SeedError> {
        if self.problems.is_empty() {
            Ok(())
        } else {
            Err(SeedError {
                problems: self.problems,
            })
        }
    }

    fn run(&mut self, seed: &Seed) {
        self.accounts(seed);
        self.emails(seed);
        self.teams(seed);
        self.repos(seed);
        self.repository_roles(seed);
        self.apps(seed);
        self.installations(seed);
        self.installation_requests(seed);
        self.collaborators(seed);
        self.memberships(seed);
        self.team_memberships(seed);
        self.repository_invitations(seed);
        self.team_details(seed);
        self.org_invitations(seed);
    }

    fn name(&mut self, path: String, what: &str, value: &str) {
        if value.is_empty() || value.contains('/') || value.chars().any(char::is_whitespace) {
            self.problem(
                path,
                format!("{what} {value:?} must be non-empty, without '/' or whitespace"),
            );
        }
    }

    fn id(&mut self, path: String, id: u64, seen: &mut HashSet<u64>, what: &str) {
        if id == 0 {
            self.problem(path, format!("{what} IDs start at 1"));
        } else if !seen.insert(id) {
            self.problem(path, format!("{what} ID {id} is used twice"));
        }
    }

    /// The account `login` refers to, reporting a problem when it is missing
    /// or of the wrong kind.
    fn account(&mut self, path: String, login: &str, want: Option<AccountKind>) -> bool {
        match (self.accounts.get(&login.to_lowercase()).copied(), want) {
            (None, _) => {
                self.problem(path, format!("no user or organization {login:?}"));
                false
            }
            (Some(AccountKind::User), Some(AccountKind::Org)) => {
                self.problem(path, format!("{login:?} is a user, not an organization"));
                false
            }
            (Some(AccountKind::Org), Some(AccountKind::User)) => {
                self.problem(path, format!("{login:?} is an organization, not a user"));
                false
            }
            _ => true,
        }
    }

    fn accounts(&mut self, seed: &Seed) {
        let mut ids = HashSet::new();
        let entries = seed
            .users
            .iter()
            .enumerate()
            .map(|(i, u)| (format!("users[{i}]"), u.id, &u.login, AccountKind::User))
            .chain(
                seed.orgs
                    .iter()
                    .enumerate()
                    .map(|(i, o)| (format!("orgs[{i}]"), o.id, &o.login, AccountKind::Org)),
            )
            .collect::<Vec<_>>();
        for (at, id, login, kind) in entries {
            self.id(format!("{at}.id"), id, &mut ids, "account");
            self.name(format!("{at}.login"), "login", login);
            if self.accounts.insert(login.to_lowercase(), kind).is_some() {
                self.problem(
                    format!("{at}.login"),
                    format!("login {login:?} is used twice (logins ignore case)"),
                );
            }
        }
        for (i, org) in seed.orgs.iter().enumerate() {
            if org.plan.name.is_empty() {
                self.problem(format!("orgs[{i}].plan.name"), "is empty".into());
            }
        }
    }

    fn emails(&mut self, seed: &Seed) {
        for (i, user) in seed.users.iter().enumerate() {
            let at = format!("users[{i}]");
            let mut seen = HashSet::new();
            for (e, email) in user.emails.iter().enumerate() {
                let address = &email.email;
                if address.is_empty()
                    || !address.contains('@')
                    || address.chars().any(char::is_whitespace)
                {
                    self.problem(
                        format!("{at}.emails[{e}].email"),
                        format!("{address:?} is not an email address"),
                    );
                } else if !seen.insert(address.to_lowercase()) {
                    self.problem(
                        format!("{at}.emails[{e}].email"),
                        format!("{address:?} is listed twice"),
                    );
                }
                if !email.primary && email.visibility.is_some() {
                    self.problem(
                        format!("{at}.emails[{e}].visibility"),
                        "only the primary address has a visibility".into(),
                    );
                }
            }
            let primaries: Vec<&Email> = user.emails.iter().filter(|e| e.primary).collect();
            if !user.emails.is_empty() && primaries.len() != 1 {
                self.problem(
                    format!("{at}.emails"),
                    format!("exactly one address is primary, not {}", primaries.len()),
                );
            }
            if let (Some(email), [primary]) = (&user.email, primaries.as_slice())
                && !email.eq_ignore_ascii_case(&primary.email)
            {
                self.problem(
                    format!("{at}.email"),
                    format!("{email:?} is not the primary address in emails"),
                );
            }
        }
    }

    fn teams(&mut self, seed: &Seed) {
        let mut ids = HashSet::new();
        for (i, team) in seed.teams.iter().enumerate() {
            let at = format!("teams[{i}]");
            self.id(format!("{at}.id"), team.id, &mut ids, "team");
            self.name(format!("{at}.slug"), "slug", &team.slug);
            self.account(format!("{at}.org"), &team.org, Some(AccountKind::Org));
            let key = format!("{}/{}", team.org, team.slug).to_lowercase();
            if !self.teams.insert(key) {
                self.problem(
                    format!("{at}.slug"),
                    format!("{}/{} is used twice", team.org, team.slug),
                );
            }
        }
    }

    fn repos(&mut self, seed: &Seed) {
        let mut ids = HashSet::new();
        for (i, repo) in seed.repos.iter().enumerate() {
            let at = format!("repos[{i}]");
            self.id(format!("{at}.id"), repo.id, &mut ids, "repository");
            self.name(format!("{at}.name"), "name", &repo.name);
            self.account(format!("{at}.owner"), &repo.owner, None);
            if repo.visibility == Some(Visibility::Public) && repo.private {
                self.problem(
                    format!("{at}.visibility"),
                    "a public repository cannot be private: true".into(),
                );
            }
            if repo.visibility == Some(Visibility::Internal)
                && self.accounts.get(&repo.owner.to_lowercase()) != Some(&AccountKind::Org)
            {
                self.problem(
                    format!("{at}.visibility"),
                    "only an organization's repositories can be internal".into(),
                );
            }
            let full_name = repo.full_name();
            if self
                .repos
                .insert(full_name.to_lowercase(), repo.owner.to_lowercase())
                .is_some()
            {
                self.problem(
                    format!("{at}.name"),
                    format!("{full_name} is used twice (names ignore case)"),
                );
            }
        }
    }

    fn repository_roles(&mut self, seed: &Seed) {
        let mut ids = HashSet::new();
        for (i, role) in seed.repository_roles.iter().enumerate() {
            let at = format!("repository_roles[{i}]");
            self.id(format!("{at}.id"), role.id, &mut ids, "repository role");
            self.account(format!("{at}.org"), &role.org, Some(AccountKind::Org));
            let name = role.name.to_lowercase();
            if role.name.trim().is_empty() {
                self.problem(format!("{at}.name"), "is empty".into());
            } else if matches!(
                name.as_str(),
                "pull" | "triage" | "push" | "maintain" | "admin" | "read" | "write" | "none"
            ) {
                self.problem(
                    format!("{at}.name"),
                    format!("{:?} is one of GitHub's own role names", role.name),
                );
            } else if !self
                .roles
                .entry(role.org.to_lowercase())
                .or_default()
                .insert(name)
            {
                self.problem(
                    format!("{at}.name"),
                    format!("{:?} is used twice in {:?}", role.name, role.org),
                );
            }
        }
    }

    /// Checks that a custom `role` on `repo` (lowercased `owner/name`) is
    /// one of the owning organization's.
    fn role(&mut self, path: String, repo: &str, role: &Role) {
        let Role::Custom(name) = role else { return };
        if name.is_empty() {
            self.problem(path, "is empty".into());
            return;
        }
        let Some(owner) = self.repos.get(repo).cloned() else {
            return;
        };
        if self.accounts.get(&owner) != Some(&AccountKind::Org) {
            self.problem(
                path,
                format!("{name:?}: custom roles exist only on organization repositories"),
            );
        } else if !self
            .roles
            .get(&owner)
            .is_some_and(|roles| roles.contains(&name.to_lowercase()))
        {
            self.problem(
                path,
                format!(
                    "{name:?} is neither a GitHub role nor a custom repository role of {owner:?}"
                ),
            );
        }
    }

    fn apps(&mut self, seed: &Seed) {
        let mut ids = HashSet::new();
        let mut slugs = HashSet::new();
        let mut client_ids = HashSet::new();
        for (i, app) in seed.apps.iter().enumerate() {
            let at = format!("apps[{i}]");
            self.id(format!("{at}.id"), app.id, &mut ids, "App");
            self.apps.insert(app.id, i);
            self.name(format!("{at}.slug"), "slug", &app.slug);
            if !slugs.insert(app.slug.to_lowercase()) {
                self.problem(
                    format!("{at}.slug"),
                    format!("App slug {:?} is used twice", app.slug),
                );
            }
            if let Some(owner) = &app.owner {
                self.account(format!("{at}.owner"), owner, None);
            }
            self.name(format!("{at}.client_id"), "client ID", &app.client_id);
            if !client_ids.insert(app.client_id.clone()) {
                self.problem(
                    format!("{at}.client_id"),
                    format!("client ID {:?} is used twice", app.client_id),
                );
            }
            if app.client_secret.is_empty() {
                self.problem(format!("{at}.client_secret"), "is empty".into());
            }
            if app.public_keys.is_empty() {
                self.problem(
                    format!("{at}.public_keys"),
                    "an App needs at least one RSA public key".into(),
                );
            }
            for (k, pem) in app.public_keys.iter().enumerate() {
                if !is_rsa_public_key(pem) {
                    self.problem(
                        format!("{at}.public_keys[{k}]"),
                        "not an RSA public key in PEM (SPKI `PUBLIC KEY` or PKCS#1 `RSA PUBLIC KEY`)"
                            .into(),
                    );
                }
            }
            if let Some(webhook) = &app.webhook {
                if !is_http_url(&webhook.url) {
                    self.problem(
                        format!("{at}.webhook.url"),
                        format!("{:?} is not an http(s) URL", webhook.url),
                    );
                }
                for (s, secret) in webhook.secrets.iter().enumerate() {
                    if secret.is_empty() {
                        self.problem(format!("{at}.webhook.secrets[{s}]"), "is empty".into());
                    }
                }
            }
            for (c, url) in app.callback_urls.iter().enumerate() {
                if !is_http_url(url) {
                    self.problem(
                        format!("{at}.callback_urls[{c}]"),
                        format!("{url:?} is not an http(s) URL"),
                    );
                }
            }
            for (field, permissions) in [
                ("permissions", &app.permissions),
                ("user_permissions", &app.user_permissions),
            ] {
                for name in permissions.keys() {
                    if !is_identifier(name) {
                        self.problem(
                            format!("{at}.{field}"),
                            format!("permission name {name:?} is not snake_case"),
                        );
                    }
                }
            }
            let mut events = HashSet::new();
            for (e, event) in app.events.iter().enumerate() {
                if !is_identifier(event) {
                    self.problem(
                        format!("{at}.events[{e}]"),
                        format!("event {event:?} is not snake_case"),
                    );
                } else if !events.insert(event) {
                    self.problem(
                        format!("{at}.events[{e}]"),
                        format!("event {event:?} is listed twice"),
                    );
                }
            }
        }
    }

    fn installations(&mut self, seed: &Seed) {
        let mut ids = HashSet::new();
        let mut pairs = HashSet::new();
        for (i, installation) in seed.installations.iter().enumerate() {
            let at = format!("installations[{i}]");
            self.id(
                format!("{at}.id"),
                installation.id,
                &mut ids,
                "installation",
            );
            let app = match self.apps.get(&installation.app_id) {
                Some(&index) => Some(&seed.apps[index]),
                None => {
                    self.problem(
                        format!("{at}.app_id"),
                        format!("no App with ID {}", installation.app_id),
                    );
                    None
                }
            };
            let account = installation.account.to_lowercase();
            if self.account(format!("{at}.account"), &installation.account, None)
                && !pairs.insert((installation.app_id, account.clone()))
            {
                self.problem(
                    format!("{at}.account"),
                    format!(
                        "App {} is already installed on {:?}",
                        installation.app_id, installation.account
                    ),
                );
            }
            match installation.repository_selection {
                RepositorySelection::All if !installation.repositories.is_empty() => {
                    self.problem(
                        format!("{at}.repositories"),
                        "only an installation with repository_selection \"selected\" lists repositories"
                            .into(),
                    );
                }
                _ => {}
            }
            let mut listed = HashSet::new();
            for (r, full_name) in installation.repositories.iter().enumerate() {
                let path = format!("{at}.repositories[{r}]");
                match self.repos.get(&full_name.to_lowercase()) {
                    None => self.problem(path, format!("no repository {full_name:?}")),
                    Some(owner) if *owner != account => self.problem(
                        path,
                        format!(
                            "{full_name:?} is not owned by the installation's account {:?}",
                            installation.account
                        ),
                    ),
                    Some(_) if !listed.insert(full_name.to_lowercase()) => {
                        self.problem(path, format!("{full_name:?} is listed twice"));
                    }
                    Some(_) => {}
                }
            }
            if let Some(by) = &installation.suspended_by {
                if installation.suspended_at.is_none() {
                    self.problem(
                        format!("{at}.suspended_by"),
                        "only a suspended installation (with suspended_at) names who suspended it"
                            .into(),
                    );
                }
                self.account(format!("{at}.suspended_by"), by, Some(AccountKind::User));
            }
            let Some(app) = app else { continue };
            for (name, access) in installation.permissions.iter().flatten() {
                match app.permissions.get(name) {
                    Some(granted) if granted >= access => {}
                    Some(granted) => self.problem(
                        format!("{at}.permissions.{name}"),
                        format!(
                            "{access} is wider than App {}'s {granted}",
                            installation.app_id
                        ),
                    ),
                    None => self.problem(
                        format!("{at}.permissions.{name}"),
                        format!("App {} does not request {name:?}", installation.app_id),
                    ),
                }
            }
            for (e, event) in installation.events.iter().flatten().enumerate() {
                if !app.events.contains(event) {
                    self.problem(
                        format!("{at}.events[{e}]"),
                        format!(
                            "App {} does not subscribe to {event:?}",
                            installation.app_id
                        ),
                    );
                }
            }
        }
    }

    fn installation_requests(&mut self, seed: &Seed) {
        let mut ids = HashSet::new();
        for (i, request) in seed.installation_requests.iter().enumerate() {
            let at = format!("installation_requests[{i}]");
            self.id(
                format!("{at}.id"),
                request.id,
                &mut ids,
                "installation request",
            );
            if !self.apps.contains_key(&request.app_id) {
                self.problem(
                    format!("{at}.app_id"),
                    format!("no App with ID {}", request.app_id),
                );
            }
            self.account(format!("{at}.account"), &request.account, None);
            self.account(
                format!("{at}.requester"),
                &request.requester,
                Some(AccountKind::User),
            );
        }
    }

    fn collaborators(&mut self, seed: &Seed) {
        let mut pairs = HashSet::new();
        for (i, collaborator) in seed.collaborators.iter().enumerate() {
            let at = format!("collaborators[{i}]");
            let repo = collaborator.repo.to_lowercase();
            if !self.repos.contains_key(&repo) {
                self.problem(
                    format!("{at}.repo"),
                    format!("no repository {:?}", collaborator.repo),
                );
            }
            let user_ok = self.account(
                format!("{at}.user"),
                &collaborator.user,
                Some(AccountKind::User),
            );
            if user_ok && !pairs.insert((repo.clone(), collaborator.user.to_lowercase())) {
                self.problem(
                    format!("{at}.user"),
                    format!(
                        "{:?} is already a collaborator on {:?}",
                        collaborator.user, collaborator.repo
                    ),
                );
            }
            self.role(format!("{at}.role"), &repo, &collaborator.role);
        }
        self.collaborators = pairs;
    }

    fn repository_invitations(&mut self, seed: &Seed) {
        let mut ids = HashSet::new();
        let mut pairs = HashSet::new();
        for (i, invitation) in seed.repository_invitations.iter().enumerate() {
            let at = format!("repository_invitations[{i}]");
            self.id(
                format!("{at}.id"),
                invitation.id,
                &mut ids,
                "repository invitation",
            );
            let repo = invitation.repo.to_lowercase();
            if !self.repos.contains_key(&repo) {
                self.problem(
                    format!("{at}.repo"),
                    format!("no repository {:?}", invitation.repo),
                );
            }
            let invitee_ok = self.account(
                format!("{at}.invitee"),
                &invitation.invitee,
                Some(AccountKind::User),
            );
            let pair = (repo.clone(), invitation.invitee.to_lowercase());
            if invitee_ok && self.collaborators.contains(&pair) {
                self.problem(
                    format!("{at}.invitee"),
                    format!(
                        "{:?} is already a collaborator on {:?}",
                        invitation.invitee, invitation.repo
                    ),
                );
            } else if invitee_ok && !pairs.insert(pair) {
                self.problem(
                    format!("{at}.invitee"),
                    format!(
                        "{:?} already has an invitation to {:?}",
                        invitation.invitee, invitation.repo
                    ),
                );
            }
            if let Some(inviter) = &invitation.inviter {
                self.account(format!("{at}.inviter"), inviter, Some(AccountKind::User));
            }
            if let Some(app) = invitation.inviter_app_id {
                if invitation.inviter.is_some() {
                    self.problem(
                        format!("{at}.inviter_app_id"),
                        "an invitation has one inviter: a user or an App".into(),
                    );
                }
                if !self.apps.contains_key(&app) {
                    self.problem(
                        format!("{at}.inviter_app_id"),
                        format!("no App with ID {app}"),
                    );
                }
            }
            self.role(format!("{at}.role"), &repo, &invitation.role);
        }
    }

    fn memberships(&mut self, seed: &Seed) {
        for (i, membership) in seed.memberships.iter().enumerate() {
            let at = format!("memberships[{i}]");
            let org_ok = self.account(format!("{at}.org"), &membership.org, Some(AccountKind::Org));
            let user_ok = self.account(
                format!("{at}.user"),
                &membership.user,
                Some(AccountKind::User),
            );
            let key = (
                membership.org.to_lowercase(),
                membership.user.to_lowercase(),
            );
            if !(org_ok && user_ok) {
                continue;
            }
            if let std::collections::hash_map::Entry::Vacant(entry) = self.memberships.entry(key) {
                entry.insert(membership.state);
            } else {
                self.problem(
                    format!("{at}.user"),
                    format!(
                        "{:?} already has a membership of {:?}",
                        membership.user, membership.org
                    ),
                );
            }
        }
    }

    fn team_memberships(&mut self, seed: &Seed) {
        let mut pairs = HashSet::new();
        for (i, membership) in seed.team_memberships.iter().enumerate() {
            let at = format!("team_memberships[{i}]");
            let team = membership.team.to_lowercase();
            let team_ok = self.teams.contains(&team);
            if !team_ok {
                self.problem(
                    format!("{at}.team"),
                    format!("no team {:?} (teams are named org/slug)", membership.team),
                );
            }
            let user_ok = self.account(
                format!("{at}.user"),
                &membership.user,
                Some(AccountKind::User),
            );
            if !(team_ok && user_ok) {
                continue;
            }
            let user = membership.user.to_lowercase();
            if !pairs.insert((team.clone(), user.clone())) {
                self.problem(
                    format!("{at}.user"),
                    format!(
                        "{:?} is already in team {:?}",
                        membership.user, membership.team
                    ),
                );
            }
            let org = team.split('/').next().unwrap_or_default().to_owned();
            match self.memberships.get(&(org, user)) {
                None => self.problem(
                    format!("{at}.user"),
                    format!(
                        "{:?} has no membership of the team's organization",
                        membership.user
                    ),
                ),
                Some(MembershipState::Pending) if membership.state == MembershipState::Active => {
                    self.problem(
                        format!("{at}.state"),
                        format!(
                            "an active team membership needs an active organization membership; {:?}'s is pending",
                            membership.user
                        ),
                    );
                }
                Some(_) => {}
            }
        }
    }
}

impl Check {
    /// Team parents and repositories (after repositories and roles).
    fn team_details(&mut self, seed: &Seed) {
        for (i, team) in seed.teams.iter().enumerate() {
            let at = format!("teams[{i}]");
            if let Some(parent) = &team.parent {
                let key = format!("{}/{}", team.org, parent).to_lowercase();
                if parent.eq_ignore_ascii_case(&team.slug) {
                    self.problem(
                        format!("{at}.parent"),
                        "a team cannot be its own parent".into(),
                    );
                } else if !self.teams.contains(&key) {
                    self.problem(
                        format!("{at}.parent"),
                        format!("no team {parent:?} in {:?}", team.org),
                    );
                } else if team_cycle(seed, team) {
                    self.problem(format!("{at}.parent"), "team parents form a cycle".into());
                }
            }
            let mut listed = HashSet::new();
            for (r, grant) in team.repositories.iter().enumerate() {
                let path = format!("{at}.repositories[{r}]");
                let repo = grant.repo.to_lowercase();
                match self.repos.get(&repo) {
                    None => self.problem(
                        format!("{path}.repo"),
                        format!("no repository {:?}", grant.repo),
                    ),
                    Some(owner) if *owner != team.org.to_lowercase() => self.problem(
                        format!("{path}.repo"),
                        format!(
                            "{:?} is not owned by the team's organization {:?}",
                            grant.repo, team.org
                        ),
                    ),
                    Some(_) if !listed.insert(repo.clone()) => {
                        self.problem(
                            format!("{path}.repo"),
                            format!("{:?} is listed twice", grant.repo),
                        );
                    }
                    Some(_) => {}
                }
                self.role(format!("{path}.role"), &repo, &grant.role);
            }
        }
    }

    fn org_invitations(&mut self, seed: &Seed) {
        let mut ids = HashSet::new();
        let mut open = HashSet::new();
        for (i, invitation) in seed.org_invitations.iter().enumerate() {
            let at = format!("org_invitations[{i}]");
            self.id(
                format!("{at}.id"),
                invitation.id,
                &mut ids,
                "organization invitation",
            );
            let org_ok = self.account(format!("{at}.org"), &invitation.org, Some(AccountKind::Org));
            let org = invitation.org.to_lowercase();
            match (&invitation.invitee, &invitation.email) {
                (Some(login), None) => {
                    if self.account(format!("{at}.invitee"), login, Some(AccountKind::User))
                        && org_ok
                    {
                        let user = login.to_lowercase();
                        if self.memberships.contains_key(&(org.clone(), user.clone())) {
                            self.problem(
                                format!("{at}.invitee"),
                                format!(
                                    "{login:?} already has a membership of {:?}",
                                    invitation.org
                                ),
                            );
                        } else if invitation.failed_at.is_none()
                            && !open.insert((org.clone(), user))
                        {
                            self.problem(
                                format!("{at}.invitee"),
                                format!(
                                    "{login:?} already has an open invitation to {:?}",
                                    invitation.org
                                ),
                            );
                        }
                    }
                }
                (None, Some(email)) => {
                    if email.is_empty()
                        || !email.contains('@')
                        || email.chars().any(char::is_whitespace)
                    {
                        self.problem(
                            format!("{at}.email"),
                            format!("{email:?} is not an email address"),
                        );
                    }
                }
                _ => self.problem(
                    format!("{at}.invitee"),
                    "an invitation names exactly one of invitee and email".into(),
                ),
            }
            if let Some(inviter) = &invitation.inviter {
                self.account(format!("{at}.inviter"), inviter, Some(AccountKind::User));
            }
            if let Some(app) = invitation.inviter_app_id {
                if invitation.inviter.is_some() {
                    self.problem(
                        format!("{at}.inviter_app_id"),
                        "an invitation has one inviter: a user or an App".into(),
                    );
                }
                if !self.apps.contains_key(&app) {
                    self.problem(
                        format!("{at}.inviter_app_id"),
                        format!("no App with ID {app}"),
                    );
                }
            }
            let mut teams = HashSet::new();
            for (t, id) in invitation.team_ids.iter().enumerate() {
                let found = seed
                    .teams
                    .iter()
                    .any(|team| team.id == *id && team.org.eq_ignore_ascii_case(&invitation.org));
                if !found {
                    self.problem(
                        format!("{at}.team_ids[{t}]"),
                        format!("no team with ID {id} in {:?}", invitation.org),
                    );
                } else if !teams.insert(*id) {
                    self.problem(
                        format!("{at}.team_ids[{t}]"),
                        format!("team {id} is listed twice"),
                    );
                }
            }
            for id in invitation.team_roles.keys() {
                if !invitation.team_ids.contains(id) {
                    self.problem(
                        format!("{at}.team_roles"),
                        format!("team {id} is not one of team_ids"),
                    );
                }
            }
            if invitation.failed_reason.is_some() && invitation.failed_at.is_none() {
                self.problem(
                    format!("{at}.failed_reason"),
                    "only a failed invitation (with failed_at) has a reason".into(),
                );
            }
        }
    }
}

/// Whether following `team`'s parents ever returns to it.
fn team_cycle(seed: &Seed, team: &Team) -> bool {
    let mut current = team.parent.clone();
    for _ in 0..=seed.teams.len() {
        let Some(slug) = current else { return false };
        if slug.eq_ignore_ascii_case(&team.slug) {
            return true;
        }
        current = seed
            .teams
            .iter()
            .find(|t| t.org.eq_ignore_ascii_case(&team.org) && t.slug.eq_ignore_ascii_case(&slug))
            .and_then(|t| t.parent.clone());
    }
    true
}

fn is_rsa_public_key(pem: &str) -> bool {
    use rsa::pkcs1::DecodeRsaPublicKey;
    use rsa::pkcs8::DecodePublicKey;
    rsa::RsaPublicKey::from_public_key_pem(pem.trim()).is_ok()
        || rsa::RsaPublicKey::from_pkcs1_pem(pem.trim()).is_ok()
}

fn is_http_url(url: &str) -> bool {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"));
    rest.is_some_and(|rest| {
        !rest.is_empty() && !rest.starts_with('/') && !rest.chars().any(char::is_whitespace)
    })
}

/// GitHub's permission and event names: `snake_case` ASCII.
fn is_identifier(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}
