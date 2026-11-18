//! Where scenarios run: an embedded octoemu ([`Target::Emulator`]) or real
//! GitHub ([`Target::Live`]), and the [`World`] each one describes.
//!
//! The emulator target starts a fresh emulator per scenario and seeds it.
//! Seeding is setup, not a scenario step, so it may use the admin API (and
//! a scenario's [`EmulatorSetup`]); the steps use public routes only.
//!
//! The live target reads an env file kept outside the repository, named by
//! `OCTOEMU_LIVE_ENV` (format in the octoemu README).

use octoemu::Emulator;
use octoemu::seed::{
    Access, App, Installation, Membership, Org, OrgRole, Plan, Repo, Seed, Team, User, Webhook,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// Which kind of target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TargetKind {
    /// An embedded octoemu.
    Emulator,
    /// Real GitHub.
    Live,
}

/// The fidelity App: the GitHub App the scenarios act as.
#[derive(Clone, Debug)]
pub struct AppCredentials {
    /// The App ID.
    pub id: u64,
    /// Its client ID (the JWT `iss`; the App ID when absent).
    pub client_id: Option<String>,
    /// Its private key, PEM (PKCS#1 as GitHub issues it, or PKCS#8).
    pub private_key: String,
    /// Its webhook secret, when the env file gives one (reserved for a
    /// signature scenario; not used yet).
    pub webhook_secret: Option<String>,
}

/// The sign-in App's OAuth client credentials (a GitHub App without user
/// permissions).
#[derive(Clone, Debug)]
pub struct OAuthClient {
    /// The client ID.
    pub client_id: String,
    /// The client secret.
    pub client_secret: String,
}

/// An extra installation of the fidelity App with a special account.
#[derive(Clone, Debug)]
pub struct ExtraInstallation {
    /// The installation ID.
    pub installation_id: u64,
    /// The account (organization) login.
    pub account: String,
    /// A repository of it, by name, when the scenario needs one.
    pub repo: Option<String>,
}

/// IDs and names found through public routes when a session starts, for
/// the normaliser.
#[derive(Clone, Debug, Default)]
pub struct Known {
    /// The App's slug.
    pub app_slug: String,
    /// The App's bot user ID.
    pub app_bot_id: Option<u64>,
    /// The organization's ID.
    pub org_id: u64,
    /// The repository's ID.
    pub repo_id: u64,
    /// The team's ID.
    pub team_id: u64,
    /// The invitee's user ID.
    pub invitee_id: u64,
}

/// What the scenarios work with. Field names are the same on both targets;
/// the values are the emulator's seed or the env file's.
#[derive(Clone, Debug)]
pub struct World {
    /// The REST API base (`https://api.github.com`).
    pub api_base: String,
    /// The web base (`https://github.com`).
    pub web_base: String,
    /// The fidelity App.
    pub app: AppCredentials,
    /// Its installation on [`World::org`] (every repository, or at least
    /// [`World::repo`]).
    pub installation_id: u64,
    /// The organization (a fresh free one on live).
    pub org: String,
    /// A repository of the organization, by name.
    pub repo: String,
    /// A team of the organization, by slug.
    pub team: String,
    /// The invitee: a user outside the organization.
    pub invitee: String,
    /// The invitee's classic personal access token.
    pub invitee_token: String,
    /// The sign-in App's OAuth client.
    pub signin: OAuthClient,
    /// A GitHub App user access token (`ghu_`) of the sign-in App for the
    /// invitee, if one is at hand: only the consent screen issues one.
    pub user_token: Option<String>,
    /// An installation on an organization that lets only owners invite
    /// outside collaborators, with a repository.
    pub policy: Option<ExtraInstallation>,
    /// An installation on a paid organization with every seat filled.
    pub no_seat: Option<ExtraInstallation>,
    /// Filled in when the session starts.
    pub known: Known,
}

/// Admin-side preparation a scenario needs on the emulator before its
/// steps: the GitHub-side state a live organization would already be in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmulatorSetup {
    /// GitHub has flagged the organization as spammy.
    OrgSpammed,
}

/// Where scenarios run.
#[derive(Debug)]
pub enum Target {
    /// A fresh embedded octoemu per scenario.
    Emulator,
    /// Real GitHub, as the env file describes it.
    Live(Box<LiveConfig>),
}

/// A scenario's connection to its target.
pub struct Session {
    /// The world.
    pub world: World,
    /// The emulator, on the emulator target.
    pub emulator: Option<Emulator>,
}

impl Target {
    /// The emulator target.
    pub fn emulator() -> Self {
        Self::Emulator
    }

    /// The live target, from the env file `OCTOEMU_LIVE_ENV` names.
    pub fn live_from_env() -> Result<Self, String> {
        let path = std::env::var_os("OCTOEMU_LIVE_ENV").ok_or(
            "OCTOEMU_LIVE_ENV is not set: it names the live env file (see the octoemu README)",
        )?;
        LiveConfig::load(Path::new(&path)).map(|config| Self::Live(Box::new(config)))
    }

    /// Which kind.
    pub fn kind(&self) -> TargetKind {
        match self {
            Self::Emulator => TargetKind::Emulator,
            Self::Live(_) => TargetKind::Live,
        }
    }

    /// Opens a session for one scenario: on the emulator, a fresh emulator
    /// seeded with the fidelity world and `setup` applied.
    pub async fn session(&self, setup: Option<EmulatorSetup>) -> Result<Session, String> {
        match self {
            Self::Emulator => {
                let emulator = Emulator::start(emulator_seed())
                    .await
                    .map_err(|error| format!("the emulator did not start: {error}"))?;
                let world = emulator_world(&emulator)?;
                if let Some(EmulatorSetup::OrgSpammed) = setup {
                    admin_post(
                        &emulator,
                        &format!("/orgs/{ORG}/spammed"),
                        r#"{"spammed": true}"#,
                    )
                    .await?;
                }
                Ok(Session {
                    world,
                    emulator: Some(emulator),
                })
            }
            Self::Live(config) => Ok(Session {
                world: config.world(),
                emulator: None,
            }),
        }
    }

    /// Discovery results to reuse across live sessions.
    pub(crate) fn cached_known(&self) -> Option<Known> {
        match self {
            Self::Emulator => None,
            Self::Live(config) => config
                .known
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        }
    }

    pub(crate) fn cache_known(&self, known: &Known) {
        if let Self::Live(config) = self {
            *config.known.lock().unwrap_or_else(PoisonError::into_inner) = Some(known.clone());
        }
    }
}

async fn admin_post(emulator: &Emulator, path: &str, body: &str) -> Result<(), String> {
    let response = reqwest::Client::new()
        .post(format!("{}{path}", emulator.admin_base()))
        .header("content-type", "application/json")
        .body(body.to_owned())
        .send()
        .await
        .map_err(|error| format!("admin {path}: {error}"))?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("admin {path}: {}", response.status()))
    }
}

// --- The emulator's world -----------------------------------------------------

// IDs are far apart, so the normaliser never mistakes an invitation's or a
// delivery's ID (counted from 1) for one of these.
const OWNER: &str = "fidelity-owner";

const INVITEE: &str = "fidelity-invitee";
const ORG: &str = "fidelity-org";
const REPO: &str = "fidelity-sandbox";
const TEAM: &str = "fidelity-team";
const POLICY_ORG: &str = "fidelity-restricted";
const POLICY_REPO: &str = "vault";
const FULL_ORG: &str = "fidelity-full";
const APP_ID: u64 = 9001;
const APP_CLIENT_ID: &str = "Iv23.fidelityapp";
const SIGNIN_APP_ID: u64 = 9002;
const SIGNIN_CLIENT_ID: &str = "Iv23.fidelitysignin";
const SIGNIN_CLIENT_SECRET: &str = "fidelity-signin-secret";
const INSTALLATION_ID: u64 = 9101;
const POLICY_INSTALLATION_ID: u64 = 9102;
const FULL_INSTALLATION_ID: u64 = 9103;

/// The test key octoauth's and octoemu's tests share (test-only).
pub(crate) const EMULATOR_PRIVATE_KEY: &str =
    include_str!("../../octoemu/tests/fixtures/rsa2048.pem");
const EMULATOR_PUBLIC_KEY: &str = include_str!("../../octoemu/tests/fixtures/rsa2048.pub.pem");
/// A second test-only key the fidelity App does not register.
pub(crate) const UNREGISTERED_PRIVATE_KEY: &str =
    include_str!("../../octoemu/tests/fixtures/rotated2048.pem");

/// The fidelity world: an organization with a repository and a team, the
/// fidelity App installed on it with what ghinvite's Apps hold, an invitee
/// outside it, a sign-in App, an organization that keeps outside
/// collaborators to owners and a paid one with every seat filled.
pub fn emulator_seed() -> Seed {
    let app_permissions = [
        ("administration", Access::Write),
        ("members", Access::Write),
        ("metadata", Access::Read),
    ];
    let mut app = App::new(APP_ID, "fidelity-app", APP_CLIENT_ID, "fidelity-app-secret")
        .owner(ORG)
        .public_key(EMULATOR_PUBLIC_KEY)
        // Nothing listens: deliveries are recorded as failed, and the
        // deliveries API still serves them.
        .webhook(Webhook::new(
            "http://127.0.0.1:9/webhook",
            "fidelity-webhook-secret",
        ));
    for (name, access) in app_permissions {
        app = app.permission(name, access);
    }
    for event in ["member", "organization", "membership"] {
        app = app.event(event);
    }
    Seed::new()
        .user(User::new(5001, OWNER))
        .user(User::new(5002, INVITEE))
        // Named: organization-full's `name` is a non-nullable string.
        .org(Org::new(6001, ORG).name("Fidelity"))
        .org(
            Org::new(6002, POLICY_ORG)
                .name("Fidelity restricted")
                .restrict_outside_collaborators(),
        )
        .org(
            Org::new(6003, FULL_ORG)
                .name("Fidelity full")
                .plan(Plan::new("team", 1).filled_seats(1)),
        )
        .membership(Membership::new(ORG, OWNER, OrgRole::Admin))
        .membership(Membership::new(POLICY_ORG, OWNER, OrgRole::Admin))
        .membership(Membership::new(FULL_ORG, OWNER, OrgRole::Admin))
        .repo(Repo::new(7001, ORG, REPO).private())
        .repo(Repo::new(7002, POLICY_ORG, POLICY_REPO).private())
        .team(Team::new(8001, ORG, TEAM))
        .app(app)
        .app(
            App::new(
                SIGNIN_APP_ID,
                "fidelity-signin",
                SIGNIN_CLIENT_ID,
                SIGNIN_CLIENT_SECRET,
            )
            .owner(ORG)
            .public_key(EMULATOR_PUBLIC_KEY)
            .callback_url("http://127.0.0.1:9/callback"),
        )
        .installation(Installation::new(INSTALLATION_ID, APP_ID, ORG))
        .installation(Installation::new(
            POLICY_INSTALLATION_ID,
            APP_ID,
            POLICY_ORG,
        ))
        .installation(Installation::new(FULL_INSTALLATION_ID, APP_ID, FULL_ORG))
}

fn emulator_world(emulator: &Emulator) -> Result<World, String> {
    let invitee_token = emulator
        .mint_personal_access_token(INVITEE, &["repo", "user", "admin:org"])
        .ok_or("the emulator minted no PAT")?;
    let user_token = emulator.mint_user_token(INVITEE, SIGNIN_APP_ID);
    Ok(World {
        api_base: emulator.api_base().to_owned(),
        web_base: emulator.web_base().to_owned(),
        app: AppCredentials {
            id: APP_ID,
            client_id: Some(APP_CLIENT_ID.to_owned()),
            private_key: EMULATOR_PRIVATE_KEY.to_owned(),
            webhook_secret: Some("fidelity-webhook-secret".to_owned()),
        },
        installation_id: INSTALLATION_ID,
        org: ORG.to_owned(),
        repo: REPO.to_owned(),
        team: TEAM.to_owned(),
        invitee: INVITEE.to_owned(),
        invitee_token,
        signin: OAuthClient {
            client_id: SIGNIN_CLIENT_ID.to_owned(),
            client_secret: SIGNIN_CLIENT_SECRET.to_owned(),
        },
        user_token,
        policy: Some(ExtraInstallation {
            installation_id: POLICY_INSTALLATION_ID,
            account: POLICY_ORG.to_owned(),
            repo: Some(POLICY_REPO.to_owned()),
        }),
        no_seat: Some(ExtraInstallation {
            installation_id: FULL_INSTALLATION_ID,
            account: FULL_ORG.to_owned(),
            repo: None,
        }),
        known: Known::default(),
    })
}

// --- The live env file ---------------------------------------------------------

/// The live target's configuration, from the env file.
#[derive(Debug)]
pub struct LiveConfig {
    values: BTreeMap<String, String>,
    private_key: String,
    known: Mutex<Option<Known>>,
}

/// Every key the env file may hold; the required ones first.
pub const LIVE_KEYS: &[(&str, bool)] = &[
    ("OCTOEMU_LIVE_APP_ID", true),
    ("OCTOEMU_LIVE_APP_PRIVATE_KEY_PATH", true),
    ("OCTOEMU_LIVE_INSTALLATION_ID", true),
    ("OCTOEMU_LIVE_ORG", true),
    ("OCTOEMU_LIVE_REPO", true),
    ("OCTOEMU_LIVE_TEAM", true),
    ("OCTOEMU_LIVE_INVITEE_LOGIN", true),
    ("OCTOEMU_LIVE_INVITEE_PAT", true),
    ("OCTOEMU_LIVE_SIGNIN_CLIENT_ID", true),
    ("OCTOEMU_LIVE_SIGNIN_CLIENT_SECRET", true),
    ("OCTOEMU_LIVE_APP_CLIENT_ID", false),
    ("OCTOEMU_LIVE_APP_WEBHOOK_SECRET", false),
    ("OCTOEMU_LIVE_USER_TOKEN", false),
    ("OCTOEMU_LIVE_POLICY_INSTALLATION_ID", false),
    ("OCTOEMU_LIVE_POLICY_ORG", false),
    ("OCTOEMU_LIVE_POLICY_REPO", false),
    ("OCTOEMU_LIVE_NO_SEAT_INSTALLATION_ID", false),
    ("OCTOEMU_LIVE_NO_SEAT_ORG", false),
    ("OCTOEMU_LIVE_API_BASE", false),
    ("OCTOEMU_LIVE_WEB_BASE", false),
];

impl LiveConfig {
    /// Reads and checks the env file at `path`, which must lie outside the
    /// repository.
    pub fn load(path: &Path) -> Result<Self, String> {
        let canonical =
            std::fs::canonicalize(path).map_err(|error| format!("{}: {error}", path.display()))?;
        if let Some(root) = repository_root()
            && canonical.starts_with(&root)
        {
            return Err(format!(
                "{} is inside the repository ({}): keep the live env file outside it",
                path.display(),
                root.display()
            ));
        }
        let text = std::fs::read_to_string(&canonical)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let values = parse_env(&text)?;
        Self::from_values(values)
    }

    /// Checks parsed values and reads the private key.
    pub fn from_values(values: BTreeMap<String, String>) -> Result<Self, String> {
        let mut problems = Vec::new();
        for key in values.keys() {
            if !LIVE_KEYS.iter().any(|(known, _)| known == key) {
                problems.push(format!("unknown key {key}"));
            }
        }
        for (key, required) in LIVE_KEYS {
            if *required && values.get(*key).is_none_or(String::is_empty) {
                problems.push(format!("{key} is required"));
            }
        }
        for key in [
            "OCTOEMU_LIVE_APP_ID",
            "OCTOEMU_LIVE_INSTALLATION_ID",
            "OCTOEMU_LIVE_POLICY_INSTALLATION_ID",
            "OCTOEMU_LIVE_NO_SEAT_INSTALLATION_ID",
        ] {
            if let Some(value) = values.get(key)
                && value.parse::<u64>().is_err()
            {
                problems.push(format!("{key} must be a number"));
            }
        }
        let pairs = [
            (
                "OCTOEMU_LIVE_POLICY_INSTALLATION_ID",
                "OCTOEMU_LIVE_POLICY_ORG",
            ),
            (
                "OCTOEMU_LIVE_NO_SEAT_INSTALLATION_ID",
                "OCTOEMU_LIVE_NO_SEAT_ORG",
            ),
        ];
        for (a, b) in pairs {
            if values.contains_key(a) != values.contains_key(b) {
                problems.push(format!("{a} and {b} go together"));
            }
        }
        if !problems.is_empty() {
            return Err(format!("live env file: {}", problems.join("; ")));
        }
        let key_path = PathBuf::from(&values["OCTOEMU_LIVE_APP_PRIVATE_KEY_PATH"]);
        let private_key = std::fs::read_to_string(&key_path)
            .map_err(|error| format!("{}: {error}", key_path.display()))?;
        Ok(Self {
            values,
            private_key,
            known: Mutex::new(None),
        })
    }

    fn get(&self, key: &str) -> Option<String> {
        self.values.get(key).filter(|v| !v.is_empty()).cloned()
    }

    fn number(&self, key: &str) -> Option<u64> {
        self.get(key).and_then(|v| v.parse().ok())
    }

    fn world(&self) -> World {
        let required = |key: &str| self.get(key).unwrap_or_default();
        World {
            api_base: self
                .get("OCTOEMU_LIVE_API_BASE")
                .unwrap_or_else(|| "https://api.github.com".into()),
            web_base: self
                .get("OCTOEMU_LIVE_WEB_BASE")
                .unwrap_or_else(|| "https://github.com".into()),
            app: AppCredentials {
                id: self.number("OCTOEMU_LIVE_APP_ID").unwrap_or_default(),
                client_id: self.get("OCTOEMU_LIVE_APP_CLIENT_ID"),
                private_key: self.private_key.clone(),
                webhook_secret: self.get("OCTOEMU_LIVE_APP_WEBHOOK_SECRET"),
            },
            installation_id: self
                .number("OCTOEMU_LIVE_INSTALLATION_ID")
                .unwrap_or_default(),
            org: required("OCTOEMU_LIVE_ORG"),
            repo: required("OCTOEMU_LIVE_REPO"),
            team: required("OCTOEMU_LIVE_TEAM"),
            invitee: required("OCTOEMU_LIVE_INVITEE_LOGIN"),
            invitee_token: required("OCTOEMU_LIVE_INVITEE_PAT"),
            signin: OAuthClient {
                client_id: required("OCTOEMU_LIVE_SIGNIN_CLIENT_ID"),
                client_secret: required("OCTOEMU_LIVE_SIGNIN_CLIENT_SECRET"),
            },
            user_token: self.get("OCTOEMU_LIVE_USER_TOKEN"),
            policy: self
                .number("OCTOEMU_LIVE_POLICY_INSTALLATION_ID")
                .map(|installation_id| ExtraInstallation {
                    installation_id,
                    account: required("OCTOEMU_LIVE_POLICY_ORG"),
                    repo: self.get("OCTOEMU_LIVE_POLICY_REPO"),
                }),
            no_seat: self
                .number("OCTOEMU_LIVE_NO_SEAT_INSTALLATION_ID")
                .map(|installation_id| ExtraInstallation {
                    installation_id,
                    account: required("OCTOEMU_LIVE_NO_SEAT_ORG"),
                    repo: None,
                }),
            known: Known::default(),
        }
    }
}

/// The git work tree this crate sits in, if any.
fn repository_root() -> Option<PathBuf> {
    let manifest = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR")).ok()?;
    manifest
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

/// `KEY=VALUE` lines; blank lines and `#` comments skipped (also after an
/// unquoted value); `export ` and one pair of matching quotes around the
/// value allowed.
pub fn parse_env(text: &str) -> Result<BTreeMap<String, String>, String> {
    let mut values = BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("line {}: not KEY=VALUE", index + 1));
        };
        let key = key.trim();
        let mut value = value.trim();
        let quoted = ['"', '\'']
            .into_iter()
            .find(|quote| value.len() >= 2 && value.starts_with(*quote) && value.ends_with(*quote));
        match quoted {
            Some(_) => value = &value[1..value.len() - 1],
            // A comment after an unquoted value.
            None => {
                if let Some(at) = value.find(" #").or_else(|| value.find("\t#")) {
                    value = value[..at].trim_end();
                }
            }
        }
        if values.insert(key.to_owned(), value.to_owned()).is_some() {
            return Err(format!("line {}: {key} given twice", index + 1));
        }
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_env_file_parses_and_is_checked() {
        let values = parse_env(
            "# fidelity\nexport OCTOEMU_LIVE_ORG=\"example-org\"\nOCTOEMU_LIVE_REPO='sandbox'\n\
             OCTOEMU_LIVE_TEAM=team  # a comment\nOCTOEMU_LIVE_INVITEE_LOGIN=\"a #b\"\n",
        )
        .unwrap();
        assert_eq!(values["OCTOEMU_LIVE_ORG"], "example-org");
        assert_eq!(values["OCTOEMU_LIVE_REPO"], "sandbox");
        assert_eq!(values["OCTOEMU_LIVE_TEAM"], "team");
        assert_eq!(values["OCTOEMU_LIVE_INVITEE_LOGIN"], "a #b");
        assert!(parse_env("NOPE").is_err());
        assert!(parse_env("A=1\nA=2").is_err());

        let error = LiveConfig::from_values(BTreeMap::from([
            ("OCTOEMU_LIVE_APP_ID".to_owned(), "x".to_owned()),
            ("OCTOEMU_LIVE_SURPRISE".to_owned(), "1".to_owned()),
            ("OCTOEMU_LIVE_POLICY_ORG".to_owned(), "o".to_owned()),
        ]))
        .unwrap_err();
        for expected in [
            "unknown key OCTOEMU_LIVE_SURPRISE",
            "OCTOEMU_LIVE_ORG is required",
            "OCTOEMU_LIVE_APP_ID must be a number",
            "go together",
        ] {
            assert!(error.contains(expected), "{error}");
        }
    }

    #[test]
    fn the_readme_example_names_every_key_and_only_known_ones() {
        let readme = include_str!("../../octoemu/README.md");
        let start = readme
            .find("**The live env file**")
            .expect("the README documents the env file");
        let block = readme[start..]
            .split("```sh\n")
            .nth(1)
            .and_then(|rest| rest.split("```").next())
            .expect("an sh block");
        let values = parse_env(block).unwrap();
        let documented: Vec<&str> = values.keys().map(String::as_str).collect();
        let known: Vec<&str> = LIVE_KEYS.iter().map(|(key, _)| *key).collect();
        let mut sorted = known.clone();
        sorted.sort_unstable();
        assert_eq!(documented, sorted);
        for value in values.values() {
            assert!(!value.contains(" #"), "{value}");
        }
    }

    #[test]
    fn an_env_file_inside_the_repository_is_refused() {
        let inside = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let error = LiveConfig::load(&inside).unwrap_err();
        assert!(error.contains("inside the repository"), "{error}");
    }

    #[test]
    fn the_emulator_seed_is_valid() {
        emulator_seed().validate().unwrap();
    }
}
