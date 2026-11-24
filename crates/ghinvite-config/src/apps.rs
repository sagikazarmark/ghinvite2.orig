//! Pure configuration for the P1 app split, independent of deployment adapters.
//!
//! Callers select a deployment stage explicitly. Missing org credentials never
//! silently downgrade an org-enabled deployment. The input source owns its
//! original values; each returned secret copy is zeroized by this parser.
//!
//! Three apps, each in its own slot:
//! - `signin`: OAuth only ([`SignInApp`], web only);
//! - `repo`, and `org` at stage `repo-and-org`: installed apps. Each becomes
//!   an [`InstalledApp`] (an `octoauth::App` for `ghinvite_github::GithubApps`)
//!   and, on web, an [`InstalledWebhook`] that verifies its deliveries.
//!
//! Client IDs are unique across all three apps and App IDs across the
//! installed ones. Errors name the variable, never its value.
//!
//! Every parser also reads GitHub's base URLs ([`GithubBases`]): the
//! installed apps create tokens at its API base, and web's sign-in app uses
//! all three. Only a [`Deployment::Development`] root may override them.
//!
//! octoauth signs with one key: the **first** entry of `GH_*_PRIVATE_KEYS`
//! (PD-5); every entry is still validated. To rotate, add the new key in
//! GitHub, deploy it first in the list, then remove the old key in GitHub.

pub use crate::github::{Deployment, GithubBases};
use base64::{Engine as _, engine::general_purpose::STANDARD};
pub use ghinvite_github::apps::AppSlot as InstalledSlot;
pub use ghinvite_github::apps::InstalledApp;
use url::Url;
use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppStage {
    /// M0: a sign-in app on web and the repository installation app.
    Repo,
    /// M1: additionally require the organization installation app.
    RepoAndOrg,
}

impl AppStage {
    /// Native composition roots distinguish absent from non-Unicode values.
    pub fn from_environment() -> Result<Self, AppDeploymentError> {
        match std::env::var("P1_APP_STAGE") {
            Ok(value) => Self::from_config(Some(&value)),
            Err(std::env::VarError::NotPresent) => Self::from_config(None),
            Err(std::env::VarError::NotUnicode(_)) => Err(bad("P1_APP_STAGE")),
        }
    }

    /// Explicit deployment app stage. This does not activate organization
    /// delivery/target features or satisfy their live GitHub gates.
    pub fn from_config(value: Option<&str>) -> Result<Self, AppDeploymentError> {
        match value {
            None | Some("repo") => Ok(Self::Repo),
            Some("repo-and-org") => Ok(Self::RepoAndOrg),
            Some(_) => Err(bad("P1_APP_STAGE")),
        }
    }

    /// The installed apps of this stage.
    fn slots(self) -> &'static [InstalledSlot] {
        match self {
            Self::Repo => &[InstalledSlot::Repo],
            Self::RepoAndOrg => &[InstalledSlot::Repo, InstalledSlot::Org],
        }
    }
}

/// Explicit organization delivery composition. The app stage alone never enables writes.
/// The caller must also successfully parse the complete app configuration for this stage.
pub fn organization_delivery_enabled(
    stage: AppStage,
    value: Option<&str>,
) -> Result<bool, AppDeploymentError> {
    match value {
        None | Some("false") => Ok(false),
        Some("true") if stage == AppStage::RepoAndOrg => Ok(true),
        Some(_) => Err(bad("GH_ORG_DELIVERY_ENABLED")),
    }
}

/// Matched web/workflow M2 capability. App parsing remains mandatory;
/// selected organization targets additionally require organization delivery.
pub fn access_removal_enabled(value: Option<&str>) -> Result<bool, AppDeploymentError> {
    match value {
        None | Some("false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err(bad("GH_ACCESS_REMOVAL_ENABLED")),
    }
}

/// Fixed field names only; no input value or parser source chain is retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AppDeploymentError {
    pub field: &'static str,
    pub kind: DeploymentErrorKind,
}

impl std::fmt::Display for AppDeploymentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            DeploymentErrorKind::Invalid => write!(
                f,
                "invalid or missing deployment configuration: {}",
                self.field
            ),
            DeploymentErrorKind::ProductionOverride => write!(
                f,
                "{} is not accepted in production: GitHub base-URL overrides are for local development and tests only",
                self.field
            ),
        }
    }
}

impl std::error::Error for AppDeploymentError {}

/// Why an [`AppDeploymentError`]'s field was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum DeploymentErrorKind {
    /// Missing, or not in the field's grammar.
    Invalid,
    /// A `GITHUB_*_BASE` override in a production deployment.
    ProductionOverride,
}

/// Web's apps: [`parse_web_apps`].
#[derive(Debug)]
pub struct WebApps {
    pub base_url: Url,
    /// GitHub's base URLs, which `signin` and `installed` already use; the
    /// root builds its other GitHub clients (`WebGithub`) with them too.
    pub github: GithubBases,
    pub signin: SignInApp,
    /// The installed apps (`repo`, and `org` at stage `repo-and-org`) for
    /// `GithubApps`, creating tokens at `github`'s API base.
    pub installed: Vec<InstalledApp>,
    /// The installed apps' App IDs and webhook secrets, for `/webhooks/{app}`.
    pub webhooks: InstalledWebhooks,
}

/// The sign-in app's OAuth client credentials and callback URL, from which
/// web builds its `octologin::SignIn`.
pub struct SignInApp {
    pub client_id: String,
    pub client_secret: Zeroizing<String>,
    /// `{P1_HOST}/oauth/callback`.
    pub redirect_uri: Url,
    /// GitHub's web origin; `None` is `https://github.com`. Parsed
    /// configuration sets it from [`GithubBases::web`].
    pub web_base_url: Option<String>,
    /// GitHub's REST API base; `None` is `https://api.github.com`. Parsed
    /// configuration sets it from [`GithubBases::api`].
    pub api_base_url: Option<String>,
    /// GitHub's avatar CDN, for the signed-in user's canonical avatar URL;
    /// `None` is `https://avatars.githubusercontent.com`. Parsed
    /// configuration sets it from [`GithubBases::avatar`].
    pub avatar_base_url: Option<String>,
}

impl SignInApp {
    /// Credentials against GitHub itself.
    pub fn new(
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        redirect_uri: Url,
    ) -> Self {
        Self {
            client_id: client_id.into(),
            client_secret: Zeroizing::new(client_secret.into()),
            redirect_uri,
            web_base_url: None,
            api_base_url: None,
            avatar_base_url: None,
        }
    }

    /// Point this app at `github`'s web, API and avatar bases.
    pub fn with_bases(mut self, github: &GithubBases) -> Self {
        self.web_base_url = Some(github.web().to_owned());
        self.api_base_url = Some(github.api().to_owned());
        self.avatar_base_url = Some(github.avatar().to_owned());
        self
    }
}

impl std::fmt::Debug for SignInApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignInApp")
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

/// The workflows' apps: [`parse_mint_apps`].
#[derive(Debug)]
pub struct MintApps {
    pub base_url: Url,
    /// GitHub's base URLs; the root builds its `RepositoryGithub` at the API
    /// base.
    pub github: GithubBases,
    /// The installed apps for `GithubApps`, creating tokens at `github`'s
    /// API base.
    pub installed: Vec<InstalledApp>,
}

/// Web's installed apps without the sign-in app: [`parse_installed_web`].
#[derive(Debug)]
pub struct InstalledWebApps {
    /// GitHub's base URLs.
    pub github: GithubBases,
    /// The apps for `GithubApps`, creating tokens at `github`'s API base.
    pub installed: Vec<InstalledApp>,
    /// Their App IDs and webhook secrets.
    pub webhooks: InstalledWebhooks,
}

/// Web's installed apps (setup verification, Console reads, webhook
/// verification) without the sign-in app. Reads only the installed apps'
/// variables.
pub fn parse_installed_web(
    stage: AppStage,
    deployment: Deployment,
    mut read: impl FnMut(&'static str) -> Option<String>,
) -> Result<InstalledWebApps, AppDeploymentError> {
    let github = GithubBases::from_config(deployment, &mut read)?;
    let (installed, webhooks) = installed_apps(stage, true, github.api(), None, &mut read)?;
    Ok(InstalledWebApps {
        github,
        installed,
        webhooks: InstalledWebhooks::new(webhooks),
    })
}

/// The installed apps as `octoauth::App`s for `GithubApps`, creating tokens
/// at `api_base` (e.g. a local stub in tests). Reads only the apps' IDs,
/// client IDs and private keys.
pub fn parse_installed_github_apps(
    stage: AppStage,
    api_base: &str,
    mut read: impl FnMut(&'static str) -> Option<String>,
) -> Result<Vec<InstalledApp>, AppDeploymentError> {
    let (installed, _) = installed_apps(stage, false, api_base, None, &mut read)?;
    Ok(installed)
}

/// Parse only web's app credentials. Session/ingress/binding validation stays in
/// the deployment adapter; successful parsing alone does not validate startup.
pub fn parse_web_apps(
    stage: AppStage,
    deployment: Deployment,
    mut read: impl FnMut(&'static str) -> Option<String>,
) -> Result<WebApps, AppDeploymentError> {
    let github = GithubBases::from_config(deployment, &mut read)?;
    let base_url = host(&mut read)?;
    let client_id = client(&mut read, "GH_SIGNIN_CLIENT_ID")?;
    let client_secret = Zeroizing::new(required(&mut read, "GH_SIGNIN_CLIENT_SECRET")?);
    // octologin's grammar: printable ASCII without spaces, at most 1 KiB.
    if client_secret.len() > 1024 || !client_secret.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(bad("GH_SIGNIN_CLIENT_SECRET"));
    }
    let (installed, webhooks) =
        installed_apps(stage, true, github.api(), Some(&client_id), &mut read)?;
    let redirect_uri = base_url
        .join("oauth/callback")
        .map_err(|_| bad("P1_HOST"))?;
    let signin = SignInApp {
        client_id,
        client_secret,
        redirect_uri,
        web_base_url: None,
        api_base_url: None,
        avatar_base_url: None,
    }
    .with_bases(&github);
    Ok(WebApps {
        base_url,
        github,
        signin,
        installed,
        webhooks: InstalledWebhooks::new(webhooks),
    })
}

/// Parse mint-only credentials. This function never asks the source for OAuth
/// client secrets, webhook secrets, the session keyring, or operator identities.
/// Native and Worker composition roots can use this without a web dependency.
pub fn parse_mint_apps(
    stage: AppStage,
    deployment: Deployment,
    mut read: impl FnMut(&'static str) -> Option<String>,
) -> Result<MintApps, AppDeploymentError> {
    let github = GithubBases::from_config(deployment, &mut read)?;
    let base_url = host(&mut read)?;
    let (installed, _) = installed_apps(stage, false, github.api(), None, &mut read)?;
    Ok(MintApps {
        base_url,
        github,
        installed,
    })
}

/// One installed app's webhook secrets, from `GH_*_WEBHOOK_SECRETS`: every
/// entry verifies, so a secret is rotated by listing the new one beside the
/// old, changing it in GitHub, then removing the old one.
///
/// The grammar: 1 to 25 `;`-separated, distinct entries of 1 to 4096
/// printable ASCII characters without spaces. `Debug` shows nothing of the
/// secrets, and each copy is zeroized on drop.
pub struct WebhookSecrets {
    secrets: Vec<Zeroizing<String>>,
}

impl WebhookSecrets {
    /// Parse a `;`-separated secret list. The error names no value.
    pub fn parse(input: &str) -> Result<Self, InvalidWebhookSecrets> {
        if input.is_empty() || input.len() > 25 * 4097 {
            return Err(InvalidWebhookSecrets);
        }
        let mut secrets: Vec<Zeroizing<String>> = Vec::new();
        for secret in input.split(';') {
            if secrets.len() >= 25
                || secret.is_empty()
                || secret.len() > 4096
                || !secret.bytes().all(|b| b.is_ascii_graphic())
                || secrets.iter().any(|s| s.as_str() == secret)
            {
                return Err(InvalidWebhookSecrets);
            }
            secrets.push(Zeroizing::new(secret.to_owned()));
        }
        Ok(Self { secrets })
    }

    /// The secrets' bytes in configured order; never empty.
    pub fn iter(&self) -> impl Iterator<Item = &[u8]> {
        self.secrets.iter().map(|s| s.as_bytes())
    }

    /// The number of secrets; never zero.
    pub fn len(&self) -> usize {
        self.secrets.len()
    }

    /// Always `false`: a parsed list has at least one secret.
    pub fn is_empty(&self) -> bool {
        self.secrets.is_empty()
    }
}

impl std::fmt::Debug for WebhookSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WebhookSecrets(..)")
    }
}

/// A webhook secret list that does not follow [`WebhookSecrets`]' grammar.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("invalid webhook secret list")]
pub struct InvalidWebhookSecrets;

/// What `/webhooks/{app}` needs of one installed app: its App ID (the
/// expected `app_id` of the installations it reports) and its secrets.
#[derive(Debug)]
pub struct InstalledWebhook {
    pub slot: InstalledSlot,
    pub app_id: u64,
    pub secrets: WebhookSecrets,
}

/// The webhook configuration of each installed app.
#[derive(Debug, Default)]
pub struct InstalledWebhooks {
    apps: Vec<InstalledWebhook>,
}

impl InstalledWebhooks {
    /// The apps' webhooks; a later entry for a slot is ignored.
    pub fn new(apps: impl IntoIterator<Item = InstalledWebhook>) -> Self {
        let mut unique: Vec<InstalledWebhook> = Vec::new();
        for app in apps {
            if !unique.iter().any(|known| known.slot == app.slot) {
                unique.push(app);
            }
        }
        Self { apps: unique }
    }

    /// `slot`'s app, if it is installed.
    pub fn get(&self, slot: InstalledSlot) -> Option<&InstalledWebhook> {
        self.apps.iter().find(|known| known.slot == slot)
    }

    /// The configured slots, in configuration order.
    pub fn slots(&self) -> impl Iterator<Item = InstalledSlot> + '_ {
        self.apps.iter().map(|app| app.slot)
    }
}

pub(crate) fn bad(field: &'static str) -> AppDeploymentError {
    AppDeploymentError {
        field,
        kind: DeploymentErrorKind::Invalid,
    }
}
pub(crate) fn production_override(field: &'static str) -> AppDeploymentError {
    AppDeploymentError {
        field,
        kind: DeploymentErrorKind::ProductionOverride,
    }
}
fn required(
    read: &mut impl FnMut(&'static str) -> Option<String>,
    field: &'static str,
) -> Result<String, AppDeploymentError> {
    read(field).ok_or_else(|| bad(field))
}
/// An OAuth client ID: 1 to 255 ASCII letters, digits, `_`, `-` or `.`.
fn client(
    read: &mut impl FnMut(&'static str) -> Option<String>,
    field: &'static str,
) -> Result<String, AppDeploymentError> {
    let value = required(read, field)?;
    if value.is_empty()
        || value.len() > 255
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
    {
        return Err(bad(field));
    }
    Ok(value)
}
fn host(read: &mut impl FnMut(&'static str) -> Option<String>) -> Result<Url, AppDeploymentError> {
    parse_host(&required(read, "P1_HOST")?)
}

/// Validate a bare canonical DNS host shared by both deployment roots.
pub fn parse_host(value: &str) -> Result<Url, AppDeploymentError> {
    // A canonical ASCII DNS host, not a URL, port, path, or userinfo. IDNs must
    // be supplied in their DNS A-label form. URL normalization cannot hide input.
    if value.is_empty()
        || value.len() > 253
        || value.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
    {
        return Err(bad("P1_HOST"));
    }
    let url = Url::parse(&format!("https://{value}/")).map_err(|_| bad("P1_HOST"))?;
    if !matches!(url.host(), Some(url::Host::Domain(_))) || url.host_str() != Some(value) {
        return Err(bad("P1_HOST"));
    }
    Ok(url)
}

/// The installed apps of `stage`, and with `web` their webhooks. Client IDs
/// must be unique, also against the sign-in app's `signin`, and App IDs
/// too; checked once every variable has parsed.
fn installed_apps(
    stage: AppStage,
    web: bool,
    api_base: &str,
    signin: Option<&str>,
    read: &mut impl FnMut(&'static str) -> Option<String>,
) -> Result<(Vec<InstalledApp>, Vec<InstalledWebhook>), AppDeploymentError> {
    let mut apps = Vec::new();
    let mut webhooks = Vec::new();
    let mut client_ids: Vec<String> = signin.into_iter().map(str::to_owned).collect();
    for &slot in stage.slots() {
        let (app, client_id, webhook) = installed(slot, web, api_base, read)?;
        apps.push(app);
        client_ids.push(client_id);
        webhooks.extend(webhook);
    }
    unique(&client_ids, &apps)?;
    Ok((apps, webhooks))
}

/// The variables of one installed app.
struct InstalledFields {
    app_id: &'static str,
    client_id: &'static str,
    private_keys: &'static str,
    webhook_secrets: &'static str,
}

fn fields(slot: InstalledSlot) -> InstalledFields {
    match slot {
        InstalledSlot::Repo => InstalledFields {
            app_id: "GH_REPO_APP_ID",
            client_id: "GH_REPO_CLIENT_ID",
            private_keys: "GH_REPO_PRIVATE_KEYS",
            webhook_secrets: "GH_REPO_WEBHOOK_SECRETS",
        },
        InstalledSlot::Org => InstalledFields {
            app_id: "GH_ORG_APP_ID",
            client_id: "GH_ORG_CLIENT_ID",
            private_keys: "GH_ORG_PRIVATE_KEYS",
            webhook_secrets: "GH_ORG_WEBHOOK_SECRETS",
        },
    }
}

fn installed(
    slot: InstalledSlot,
    web: bool,
    api_base: &str,
    read: &mut impl FnMut(&'static str) -> Option<String>,
) -> Result<(InstalledApp, String, Option<InstalledWebhook>), AppDeploymentError> {
    let fields = fields(slot);
    let id = required(read, fields.app_id)?;
    let id = id
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0 && n.to_string() == id)
        .ok_or_else(|| bad(fields.app_id))?;
    let client_id = client(read, fields.client_id)?;
    let input = Zeroizing::new(required(read, fields.private_keys)?);
    let key = signing_key(&input).map_err(|()| bad(fields.private_keys))?;
    let user_agent = format!("p1/{}", slot.as_str());
    let app = octoauth::App::builder(id, key)
        .client_id(&client_id)
        .user_agent(&user_agent)
        .base_url(api_base)
        .build()
        .map_err(|error| match error.field() {
            "client_id" => bad(fields.client_id),
            "signing_key" => bad(fields.private_keys),
            "base_url" => bad("GitHub API base"),
            _ => bad("GitHub app client"),
        })?;
    let webhook = if web {
        let input = Zeroizing::new(required(read, fields.webhook_secrets)?);
        let secrets = WebhookSecrets::parse(&input).map_err(|_| bad(fields.webhook_secrets))?;
        Some(InstalledWebhook {
            slot,
            app_id: id,
            secrets,
        })
    } else {
        None
    };
    let app = InstalledApp {
        slot,
        app,
        user_agent,
    };
    Ok((app, client_id, webhook))
}

/// The signing key of `GH_*_PRIVATE_KEYS`: 1 to 25 `;`-separated
/// `kid:base64(PEM)` entries with distinct key IDs (1 to 64 ASCII letters,
/// digits, `_`, `-` or `.`), each a PKCS#8 RSA key of 2048 to 4096 bits.
/// Every entry is validated; the first one signs (PD-5).
fn signing_key(input: &str) -> Result<octoauth::SigningKey, ()> {
    // Bound before allocating decoded key buffers.
    if input.is_empty() || input.len() > 512 * 1024 {
        return Err(());
    }
    let mut kids = Vec::new();
    let mut first = None;
    for item in input.split(';') {
        if kids.len() >= 25 {
            return Err(());
        }
        let (kid, encoded) = item.split_once(':').ok_or(())?;
        if kid.is_empty()
            || kid.len() > 64
            || !kid
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
            || kids.contains(&kid)
        {
            return Err(());
        }
        kids.push(kid);
        if encoded.len() > 24 * 1024 {
            return Err(());
        }
        let decoded = Zeroizing::new(STANDARD.decode(encoded).map_err(|_| ())?);
        let pem = std::str::from_utf8(&decoded).map_err(|_| ())?.trim();
        // The deployment contract is PKCS#8, even though octoauth also
        // accepts PKCS#1 PEMs.
        if !pem.starts_with("-----BEGIN PRIVATE KEY-----")
            || !pem.ends_with("-----END PRIVATE KEY-----")
        {
            return Err(());
        }
        let key = octoauth::SigningKey::from_pem(pem).map_err(|_| ())?;
        first.get_or_insert(key);
    }
    first.ok_or(())
}

/// Client IDs are unique across the sign-in app and the installed apps, and
/// App IDs across the installed apps. Names the fields, never the values.
fn unique(client_ids: &[String], apps: &[InstalledApp]) -> Result<(), AppDeploymentError> {
    for (index, client_id) in client_ids.iter().enumerate() {
        if client_ids[..index].contains(client_id) {
            return Err(bad(
                "GH_SIGNIN_CLIENT_ID/GH_REPO_CLIENT_ID/GH_ORG_CLIENT_ID",
            ));
        }
    }
    for (index, app) in apps.iter().enumerate() {
        if apps[..index]
            .iter()
            .any(|previous| previous.app.id() == app.app.id())
        {
            return Err(bad("GH_REPO_APP_ID/GH_ORG_APP_ID"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod organization_delivery_config_tests {
    use super::*;
    #[test]
    fn organization_delivery_requires_explicit_true_and_org_registry_stage() {
        for stage in [AppStage::Repo, AppStage::RepoAndOrg] {
            assert_eq!(organization_delivery_enabled(stage, None), Ok(false));
            assert_eq!(
                organization_delivery_enabled(stage, Some("false")),
                Ok(false)
            );
            for value in ["", "TRUE", "1", "secret-marker"] {
                assert_eq!(
                    organization_delivery_enabled(stage, Some(value))
                        .unwrap_err()
                        .field,
                    "GH_ORG_DELIVERY_ENABLED"
                );
            }
        }
        assert!(organization_delivery_enabled(AppStage::Repo, Some("true")).is_err());
        assert_eq!(
            organization_delivery_enabled(AppStage::RepoAndOrg, Some("true")),
            Ok(true)
        );
    }
    #[test]
    fn access_removal_capability_is_explicit_and_diagnostics_never_echo_values() {
        assert_eq!(access_removal_enabled(None), Ok(false));
        assert_eq!(access_removal_enabled(Some("false")), Ok(false));
        assert_eq!(access_removal_enabled(Some("true")), Ok(true));
        for value in ["", "1", "TRUE", "ACCESS_SECRET_MARKER"] {
            let error = access_removal_enabled(Some(value)).unwrap_err();
            assert_eq!(error.field, "GH_ACCESS_REMOVAL_ENABLED");
            assert!(!format!("{error:?}").contains("ACCESS_SECRET_MARKER"));
        }
    }
}

#[cfg(test)]
mod installed_app_tests {
    use super::*;
    use rsa::pkcs1v15::{Signature, VerifyingKey};
    use rsa::pkcs8::DecodePrivateKey;
    use rsa::signature::Verifier;
    use std::collections::BTreeMap;

    const KEY_1: &str = include_str!("../../../test-fixtures/github-app-rsa.pem");
    const KEY_2: &str = include_str!("../../../test-fixtures/github-app-rsa-2.pem");

    fn entry(kid: &str, pem: &str) -> String {
        format!("{kid}:{}", STANDARD.encode(pem))
    }

    fn values(keys: &str) -> BTreeMap<&'static str, String> {
        BTreeMap::from([
            ("P1_HOST", "p1.example.com".into()),
            ("GH_SIGNIN_CLIENT_ID", "Iv.signin".into()),
            ("GH_SIGNIN_CLIENT_SECRET", "SIGNIN_SECRET_MARKER".into()),
            ("GH_REPO_APP_ID", "123".into()),
            ("GH_REPO_CLIENT_ID", "Iv.repo".into()),
            ("GH_REPO_PRIVATE_KEYS", keys.into()),
            ("GH_REPO_WEBHOOK_SECRETS", "REPO_SECRET".into()),
            ("GH_ORG_APP_ID", "456".into()),
            ("GH_ORG_CLIENT_ID", "Iv.org".into()),
            ("GH_ORG_PRIVATE_KEYS", entry("only", KEY_1)),
            ("GH_ORG_WEBHOOK_SECRETS", "ORG_SECRET".into()),
        ])
    }

    fn reader<'a>(
        values: &'a BTreeMap<&'static str, String>,
    ) -> impl FnMut(&'static str) -> Option<String> + 'a {
        |key| values.get(key).cloned()
    }

    /// The decoded JWT claims and whether `pem`'s key signed it.
    fn signed_by(app: &octoauth::App, pem: &str) -> (serde_json::Value, bool) {
        let credential = app.credential().unwrap();
        let jwt = credential.expose_secret();
        let (input, signature) = jwt.rsplit_once('.').unwrap();
        let claims = input.split('.').nth(1).unwrap();
        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(claims)
            .unwrap();
        let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(signature)
            .unwrap();
        let public = rsa::PrivateKey::from_pkcs8_pem(pem)
            .unwrap()
            .to_public_key();
        let verified = VerifyingKey::<rsa::sha2::Sha256>::new(public)
            .verify(
                input.as_bytes(),
                &Signature::try_from(signature.as_slice()).unwrap(),
            )
            .is_ok();
        (serde_json::from_slice(&claims).unwrap(), verified)
    }

    #[test]
    fn the_fixture_keys_are_accepted_by_octoauth() {
        for pem in [KEY_1, KEY_2] {
            octoauth::SigningKey::from_pem(pem).unwrap();
        }
    }

    #[test]
    fn the_first_configured_key_signs() {
        for (keys, first, second) in [
            (
                format!("{};{}", entry("new", KEY_1), entry("old", KEY_2)),
                KEY_1,
                KEY_2,
            ),
            (
                format!("{};{}", entry("new", KEY_2), entry("old", KEY_1)),
                KEY_2,
                KEY_1,
            ),
        ] {
            let values = values(&keys);
            let apps =
                parse_mint_apps(AppStage::Repo, Deployment::Production, reader(&values)).unwrap();
            let [repo] = apps.installed.as_slice() else {
                panic!("one installed app");
            };
            let (claims, by_first) = signed_by(&repo.app, first);
            assert!(by_first);
            assert!(!signed_by(&repo.app, second).1);
            // The client ID is the JWT issuer.
            assert_eq!(claims["iss"], "Iv.repo");
        }
    }

    #[test]
    fn installed_apps_are_built_for_each_stage() {
        let values = values(&entry("only", KEY_1));
        for (stage, expected) in [
            (AppStage::Repo, vec![(InstalledSlot::Repo, 123, "p1/repo")]),
            (
                AppStage::RepoAndOrg,
                vec![
                    (InstalledSlot::Repo, 123, "p1/repo"),
                    (InstalledSlot::Org, 456, "p1/org"),
                ],
            ),
        ] {
            let summary = |apps: &[InstalledApp]| {
                apps.iter()
                    .map(|app| (app.slot, app.app.id().get(), app.user_agent.clone()))
                    .collect::<Vec<_>>()
            };
            let expected: Vec<_> = expected
                .into_iter()
                .map(|(slot, id, agent)| (slot, id, agent.to_owned()))
                .collect();
            let mint = parse_mint_apps(stage, Deployment::Production, reader(&values)).unwrap();
            assert_eq!(summary(&mint.installed), expected);
            let web = parse_web_apps(stage, Deployment::Production, reader(&values)).unwrap();
            assert_eq!(summary(&web.installed), expected);
            let apps = parse_installed_github_apps(stage, "http://127.0.0.1:3001", reader(&values))
                .unwrap();
            assert_eq!(summary(&apps), expected);
            assert!(format!("{:?}", apps[0].app).contains("http://127.0.0.1:3001/"));
            assert!(format!("{:?}", mint.installed[0].app).contains("https://api.github.com/"));
        }
    }

    #[test]
    fn every_parser_plumbs_the_github_bases_outside_production_only() {
        let mut values = values(&entry("only", KEY_1));
        values.insert("GITHUB_API_BASE", "http://127.0.0.1:3001".into());
        values.insert("GITHUB_WEB_BASE", "http://127.0.0.1:3001/".into());
        values.insert("GITHUB_AVATAR_BASE", "http://127.0.0.1:3001/avatars".into());
        let web = parse_web_apps(AppStage::Repo, Deployment::Development, reader(&values)).unwrap();
        assert_eq!(web.github.api(), "http://127.0.0.1:3001/");
        assert_eq!(
            web.signin.api_base_url.as_deref(),
            Some("http://127.0.0.1:3001/")
        );
        assert_eq!(
            web.signin.web_base_url.as_deref(),
            Some("http://127.0.0.1:3001/")
        );
        assert_eq!(
            web.signin.avatar_base_url.as_deref(),
            Some("http://127.0.0.1:3001/avatars")
        );
        assert!(format!("{:?}", web.installed[0].app).contains("http://127.0.0.1:3001/"));
        let mint =
            parse_mint_apps(AppStage::Repo, Deployment::Development, reader(&values)).unwrap();
        assert_eq!(mint.github, web.github);
        assert!(format!("{:?}", mint.installed[0].app).contains("http://127.0.0.1:3001/"));
        let installed =
            parse_installed_web(AppStage::Repo, Deployment::Development, reader(&values)).unwrap();
        assert_eq!(installed.github, web.github);
        for error in [
            parse_web_apps(AppStage::Repo, Deployment::Production, reader(&values)).unwrap_err(),
            parse_mint_apps(AppStage::Repo, Deployment::Production, reader(&values)).unwrap_err(),
            parse_installed_web(AppStage::Repo, Deployment::Production, reader(&values))
                .unwrap_err(),
        ] {
            assert_eq!(error.field, "GITHUB_API_BASE");
            assert_eq!(error.kind, DeploymentErrorKind::ProductionOverride);
        }
        // Without overrides both deployments use GitHub.
        for key in crate::github::OVERRIDES {
            values.remove(key);
        }
        for deployment in [Deployment::Production, Deployment::Development] {
            let web = parse_web_apps(AppStage::Repo, deployment, reader(&values)).unwrap();
            assert_eq!(web.github, GithubBases::github());
            assert_eq!(
                web.signin.web_base_url.as_deref(),
                Some("https://github.com/")
            );
        }
    }

    #[test]
    fn invalid_keys_name_the_field_never_the_value() {
        let marker = "KEY_SECRET_MARKER";
        let bad_keys = [
            // Not base64.
            format!("k:{marker}"),
            // Not a PEM.
            entry(
                "k",
                &format!("-----BEGIN PRIVATE KEY-----\n{marker}\n-----END PRIVATE KEY-----"),
            ),
            // A valid key after an invalid first one.
            format!("{};{}", entry("k", marker), entry("k2", KEY_1)),
        ];
        for keys in bad_keys {
            let values = values(&keys);
            for error in [
                parse_mint_apps(AppStage::Repo, Deployment::Production, reader(&values))
                    .unwrap_err(),
                parse_installed_github_apps(
                    AppStage::Repo,
                    crate::github::DEFAULT_API_BASE,
                    reader(&values),
                )
                .unwrap_err(),
            ] {
                assert_eq!(error.field, "GH_REPO_PRIVATE_KEYS");
                let rendered = format!("{error} {error:?}");
                assert!(!rendered.contains(marker), "{rendered}");
            }
        }
        let mut values = values(&entry("only", KEY_1));
        values.insert("GH_REPO_CLIENT_ID", "Iv repo".into());
        assert_eq!(
            parse_installed_github_apps(
                AppStage::Repo,
                crate::github::DEFAULT_API_BASE,
                reader(&values)
            )
            .unwrap_err()
            .field,
            "GH_REPO_CLIENT_ID"
        );
    }

    #[test]
    fn the_api_base_is_validated() {
        let values = values(&entry("only", KEY_1));
        assert_eq!(
            parse_installed_github_apps(AppStage::Repo, "http://example.com", reader(&values))
                .unwrap_err()
                .field,
            "GitHub API base"
        );
    }

    #[test]
    fn webhooks_carry_each_installed_apps_id_and_secrets_in_order() {
        let mut values = values(&entry("only", KEY_1));
        values.insert("GH_REPO_WEBHOOK_SECRETS", "new-secret;old-secret".into());
        let repo_only =
            parse_installed_web(AppStage::Repo, Deployment::Production, reader(&values)).unwrap();
        let repo = repo_only.webhooks.get(InstalledSlot::Repo).unwrap();
        assert_eq!(repo.app_id, 123);
        let secrets: Vec<_> = repo.secrets.iter().collect();
        assert_eq!(secrets, [b"new-secret".as_slice(), b"old-secret"]);
        assert!(repo_only.webhooks.get(InstalledSlot::Org).is_none());
        let both = parse_installed_web(
            AppStage::RepoAndOrg,
            Deployment::Production,
            reader(&values),
        )
        .unwrap();
        let org = both.webhooks.get(InstalledSlot::Org).unwrap();
        assert_eq!(org.app_id, 456);
        assert_eq!(
            org.secrets.iter().collect::<Vec<_>>(),
            [b"ORG_SECRET".as_slice()]
        );
        let web = parse_web_apps(
            AppStage::RepoAndOrg,
            Deployment::Production,
            reader(&values),
        )
        .unwrap();
        assert_eq!(
            web.webhooks.slots().collect::<Vec<_>>(),
            [InstalledSlot::Repo, InstalledSlot::Org]
        );
        let printed = format!("{:?} {web:?}", both.webhooks);
        assert_eq!(printed.matches("WebhookSecrets(..)").count(), 4);
        assert!(!printed.contains("SECRET"), "{printed}");
        // Workflows never read webhook secrets.
        for parsed in [
            parse_mint_apps(AppStage::RepoAndOrg, Deployment::Production, |key| {
                assert!(!key.contains("WEBHOOK"), "{key}");
                values.get(key).cloned()
            })
            .map(|apps| apps.installed),
            parse_installed_github_apps(
                AppStage::RepoAndOrg,
                crate::github::DEFAULT_API_BASE,
                |key| {
                    assert!(!key.contains("WEBHOOK"), "{key}");
                    values.get(key).cloned()
                },
            ),
        ] {
            assert_eq!(parsed.unwrap().len(), 2);
        }
    }

    #[test]
    fn invalid_webhook_secrets_name_the_field_never_the_value() {
        let marker = "HOOK_SECRET_MARKER";
        for input in [
            String::new(),
            format!("{marker};"),
            format!("{marker};{marker}"),
            format!("{marker} spaced"),
            "x".repeat(4097),
            vec![marker; 26].join(";"),
        ] {
            let mut values = values(&entry("only", KEY_1));
            values.insert("GH_ORG_WEBHOOK_SECRETS", input);
            let error = parse_installed_web(
                AppStage::RepoAndOrg,
                Deployment::Production,
                reader(&values),
            )
            .unwrap_err();
            assert_eq!(error.field, "GH_ORG_WEBHOOK_SECRETS");
            assert!(!format!("{error} {error:?}").contains(marker));
        }
        let mut values = values(&entry("only", KEY_1));
        values.remove("GH_REPO_WEBHOOK_SECRETS");
        assert_eq!(
            parse_installed_web(AppStage::Repo, Deployment::Production, reader(&values))
                .unwrap_err()
                .field,
            "GH_REPO_WEBHOOK_SECRETS"
        );
    }

    #[test]
    fn every_key_entry_is_validated_though_only_the_first_signs() {
        let pkcs1 =
            "-----BEGIN RSA PRIVATE KEY-----\nKEY_SECRET_MARKER\n-----END RSA PRIVATE KEY-----";
        let bad_lists = [
            // A malformed second key.
            format!("{};{}", entry("a", KEY_1), entry("b", pkcs1)),
            format!("{};b:KEY_SECRET_MARKER", entry("a", KEY_1)),
            // Duplicate, empty or malformed key IDs.
            format!("{};{}", entry("a", KEY_1), entry("a", KEY_2)),
            entry("", KEY_1),
            entry("bad kid", KEY_1),
            entry(&"k".repeat(65), KEY_1),
            // No `kid:` prefix, or nothing at all.
            STANDARD.encode(KEY_1),
            String::new(),
            // More than 25 entries.
            (0..26)
                .map(|i| entry(&format!("k{i}"), KEY_1))
                .collect::<Vec<_>>()
                .join(";"),
        ];
        for keys in bad_lists {
            let values = values(&keys);
            let error = parse_mint_apps(AppStage::Repo, Deployment::Production, reader(&values))
                .unwrap_err();
            assert_eq!(error.field, "GH_REPO_PRIVATE_KEYS");
            assert!(!format!("{error} {error:?}").contains("MARKER"));
        }
        let keys = (0..25)
            .map(|i| entry(&format!("k{i}"), if i == 0 { KEY_2 } else { KEY_1 }))
            .collect::<Vec<_>>()
            .join(";");
        let apps = parse_mint_apps(
            AppStage::Repo,
            Deployment::Production,
            reader(&values(&keys)),
        )
        .unwrap();
        assert!(signed_by(&apps.installed[0].app, KEY_2).1);
    }

    #[test]
    fn client_ids_follow_the_identifier_grammar() {
        for (field, value) in [
            ("GH_REPO_CLIENT_ID", ""),
            ("GH_REPO_CLIENT_ID", "Iv/repo"),
            ("GH_ORG_CLIENT_ID", "Iv org"),
            ("GH_SIGNIN_CLIENT_ID", "Iv.signin\n"),
        ] {
            let mut input = values(&entry("only", KEY_1));
            input.insert(field, value.into());
            assert_eq!(
                parse_web_apps(AppStage::RepoAndOrg, Deployment::Production, reader(&input))
                    .unwrap_err()
                    .field,
                field
            );
        }
        let mut input = values(&entry("only", KEY_1));
        input.insert("GH_REPO_CLIENT_ID", "x".repeat(256));
        assert!(parse_mint_apps(AppStage::Repo, Deployment::Production, reader(&input)).is_err());
        input.insert("GH_REPO_CLIENT_ID", "Iv1_a-b.C".into());
        parse_mint_apps(AppStage::Repo, Deployment::Production, reader(&input)).unwrap();
    }

    #[test]
    fn duplicate_identities_are_rejected_by_every_parser() {
        let mut input = values(&entry("only", KEY_1));
        input.insert("GH_ORG_APP_ID", "123".into());
        for error in [
            parse_mint_apps(AppStage::RepoAndOrg, Deployment::Production, reader(&input))
                .unwrap_err(),
            parse_installed_web(AppStage::RepoAndOrg, Deployment::Production, reader(&input))
                .unwrap_err(),
            parse_installed_github_apps(
                AppStage::RepoAndOrg,
                crate::github::DEFAULT_API_BASE,
                reader(&input),
            )
            .unwrap_err(),
        ] {
            assert_eq!(error.field, "GH_REPO_APP_ID/GH_ORG_APP_ID");
        }
        let mut input = values(&entry("only", KEY_1));
        input.insert("GH_ORG_CLIENT_ID", "Iv.repo".into());
        assert_eq!(
            parse_mint_apps(AppStage::RepoAndOrg, Deployment::Production, reader(&input))
                .unwrap_err()
                .field,
            "GH_SIGNIN_CLIENT_ID/GH_REPO_CLIENT_ID/GH_ORG_CLIENT_ID"
        );
        // The sign-in app's client ID only collides on web.
        let mut input = values(&entry("only", KEY_1));
        input.insert("GH_REPO_CLIENT_ID", "Iv.signin".into());
        parse_mint_apps(AppStage::Repo, Deployment::Production, reader(&input)).unwrap();
        assert_eq!(
            parse_web_apps(AppStage::Repo, Deployment::Production, reader(&input))
                .unwrap_err()
                .field,
            "GH_SIGNIN_CLIENT_ID/GH_REPO_CLIENT_ID/GH_ORG_CLIENT_ID"
        );
    }
}
