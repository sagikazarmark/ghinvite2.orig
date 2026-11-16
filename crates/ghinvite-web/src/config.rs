//! Web binary runtime configuration.

// Compatibility path; deployment roots can depend directly on ghinvite-config.
pub use ghinvite_config::apps;

use kekring::Keyring;
use std::{collections::BTreeSet, path::PathBuf, sync::Arc};

/// Configuration parsed at server boot. Production sources fields from
/// Workers vars and secrets; local dev uses [`WebConfig::for_local_dev`].
#[derive(Clone)]
pub struct WebConfig {
    /// Must match the workflow deployment; disabled by default.
    pub reissue_enabled: bool,
    /// Explicit matched bulk composition; production roots parse GH_BULK_ENABLED.
    pub bulk_enabled: bool,
    /// The installed apps (`repo`, and `org` at stage `repo-and-org`) the
    /// deployment builds its [`GithubApps`](ghinvite_github::GithubApps)
    /// from, for setup verification and Console authority reads. Empty
    /// unless parsed from deployment configuration.
    pub github_apps: Vec<apps::InstalledApp>,
    /// GitHub's REST API, web and avatar bases: GitHub's own unless a
    /// non-production deployment sets `GITHUB_API_BASE`, `GITHUB_WEB_BASE`
    /// or `GITHUB_AVATAR_BASE`. `github_apps` and `signin_apps` already use
    /// them; [`WebGithub`](crate::WebGithub) and avatar reads take them from
    /// here.
    pub github: apps::GithubBases,
    /// Each installed app's App ID and webhook secrets, which verify
    /// `/webhooks/{app}`. `None` answers webhooks as unavailable.
    pub webhooks: Option<Arc<apps::InstalledWebhooks>>,
    /// The sign-in app's OAuth credentials, behind `/login` and
    /// `/oauth/callback` (`GH_SIGNIN_CLIENT_ID`/`GH_SIGNIN_CLIENT_SECRET`).
    /// Required in production; `None` (local development without them,
    /// tests) answers sign-in as unavailable.
    pub signin_apps: Option<Arc<apps::SignInApp>>,
    pub organization_delivery_enabled: bool,
    pub access_removal_enabled: bool,
    /// Base URL of THIS web binary (e.g. `https://ghinvite.example`). Used to
    /// build the OAuth redirect URI and absolute URLs in the home page.
    pub base_url: String,
    /// Immutable deployment keyring, shared by sessions and continuations.
    pub keyring: Arc<Keyring>,
    /// Numeric GitHub identities authorized to inspect deployment fingerprints.
    pub operator_ids: BTreeSet<u64>,
    /// Restate ingress URL (e.g. `http://127.0.0.1:8080` for local dev).
    pub restate_ingress: String,
    /// Server-only web-to-Restate credentials; never part of UI props.
    pub restate_auth: crate::restate_client::RestateAuth,
    /// GitHub App install URL — `https://github.com/apps/<app-name>/installations/new`.
    pub github_install_url: String,
    /// Optional separate organization-app install URL; never a setup callback.
    pub github_org_install_url: Option<String>,
    /// Whether to set the `Secure` attribute on session cookies.
    /// Off for local-dev http; on for production https.
    pub cookie_secure: bool,
    /// Directory the native binary serves under `/assets/*`: the Dioxus island
    /// bundle (`scripts/build-island.sh` writes it to `dist/public/assets`).
    /// `None` registers no route. On Workers this is always `None` — Cloudflare
    /// Static Assets serve `/assets/*` before the Worker runs (see
    /// `wrangler/web.toml`). Only read on non-wasm32 builds.
    pub island_assets_dir: Option<PathBuf>,
}

impl WebConfig {
    /// Production bindings have no unsigned-ingress or legacy-host fallback.
    /// Sign-in is always typed and token-free: the sign-in app's credentials
    /// are required. GitHub's bases cannot be overridden.
    pub fn for_production(
        read: impl FnMut(&'static str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        Self::for_deployment(apps::Deployment::Production, read)
    }

    /// [`for_production`](Self::for_production)'s bindings, for a root that
    /// says whether it serves production: a test harness's Worker build
    /// ([`apps::Deployment::Development`]) also accepts `GITHUB_*_BASE`.
    pub fn for_deployment(
        deployment: apps::Deployment,
        mut read: impl FnMut(&'static str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        let stage = apps::AppStage::from_config(read("P1_APP_STAGE").as_deref())?;
        let organization_delivery_enabled =
            apps::organization_delivery_enabled(stage, read("GH_ORG_DELIVERY_ENABLED").as_deref())?;
        let base = apps::parse_host(&required(&mut read, "P1_HOST")?)?;
        let base_url = base.as_str().trim_end_matches('/').to_owned();
        let ingress = required(&mut read, "RESTATE_INGRESS_URL")?;
        validate_https_url(&ingress, "RESTATE_INGRESS_URL")?;
        let token = required(&mut read, "RESTATE_INGRESS_TOKEN")?;
        if token.len() > 8192 {
            return Err(ConfigError::Field("RESTATE_INGRESS_TOKEN"));
        }
        let restate_auth = crate::restate_client::RestateAuth::from_config(None, Some(&token))
            .map_err(|_| ConfigError::Field("RESTATE_INGRESS_TOKEN"))?;
        let keyring = parse_keyring(&required(&mut read, "P1_KEYRING")?)?;
        let operator_ids = parse_operator_ids(&read("P1_OPERATOR_IDS").unwrap_or_default())?;
        let parsed = apps::parse_web_apps(stage, deployment, &mut read)?;
        let github_install_url = required(&mut read, "GHINVITE_GITHUB_INSTALL_URL")?;
        let install = validate_https_url(&github_install_url, "GHINVITE_GITHUB_INSTALL_URL")?;
        if install.host_str() != Some("github.com")
            || install.port().is_some()
            || !install.path().starts_with("/apps/")
            || !install.path().ends_with("/installations/new")
        {
            return Err(ConfigError::Field("GHINVITE_GITHUB_INSTALL_URL"));
        }
        let github_org_install_url = read("GH_ORG_APP_INSTALL_URL")
            .map(|value| validate_org_install_url(&value).map(|()| value))
            .transpose()?;
        Ok(Self {
            reissue_enabled: ghinvite_config::reissue::enabled(
                read("GH_REISSUE_ENABLED").as_deref(),
            )?,
            bulk_enabled: ghinvite_config::bulk::enabled(read("GH_BULK_ENABLED").as_deref())?,
            github_org_install_url,
            organization_delivery_enabled,
            access_removal_enabled: apps::access_removal_enabled(
                read("GH_ACCESS_REMOVAL_ENABLED").as_deref(),
            )?,
            github_apps: parsed.installed,
            github: parsed.github,
            webhooks: Some(Arc::new(parsed.webhooks)),
            signin_apps: Some(Arc::new(parsed.signin)),
            base_url,
            keyring,
            operator_ids,
            restate_ingress: ingress,
            restate_auth,
            github_install_url,
            cookie_secure: true,
            island_assets_dir: None,
        })
    }

    /// Build native local configuration from environment variables. The session
    /// key is mandatory; other fields have local defaults. Sign-in needs the
    /// sign-in app's `GH_SIGNIN_CLIENT_ID`/`GH_SIGNIN_CLIENT_SECRET`, with
    /// `{GHINVITE_BASE_URL}/oauth/callback` as its callback URL; without
    /// them `/login` answers that sign-in is unavailable. `GITHUB_API_BASE`,
    /// `GITHUB_WEB_BASE` and `GITHUB_AVATAR_BASE` may point every GitHub
    /// client at a stand-in such as the `octoemu` emulator.
    pub fn for_local_dev() -> Result<Self, ConfigError> {
        let descriptor = std::env::var("P1_KEYRING").map_err(|_| SessionKeyError)?;
        let keyring = parse_keyring(&descriptor)?;
        let mut config = Self::with_keyring(keyring);
        for field in ghinvite_config::github::OVERRIDES {
            optional_env(field)?;
        }
        let installed = apps::parse_installed_web(
            apps::AppStage::from_environment()?,
            apps::Deployment::Development,
            |field| std::env::var(field).ok(),
        )?;
        config.github_apps = installed.installed;
        config.github = installed.github;
        config.webhooks = Some(Arc::new(installed.webhooks));
        config.signin_apps =
            local_signin_app(&config.base_url)?.map(|app| Arc::new(app.with_bases(&config.github)));
        config.organization_delivery_enabled = apps::organization_delivery_enabled(
            apps::AppStage::from_environment()?,
            optional_env("GH_ORG_DELIVERY_ENABLED")?.as_deref(),
        )?;
        config.reissue_enabled =
            ghinvite_config::reissue::enabled(optional_env("GH_REISSUE_ENABLED")?.as_deref())?;
        config.bulk_enabled =
            ghinvite_config::bulk::enabled(optional_env("GH_BULK_ENABLED")?.as_deref())?;
        config.access_removal_enabled =
            apps::access_removal_enabled(optional_env("GH_ACCESS_REMOVAL_ENABLED")?.as_deref())?;
        config.github_org_install_url = optional_env("GH_ORG_APP_INSTALL_URL")?
            .map(|value| validate_org_install_url(&value).map(|()| value))
            .transpose()?;
        config.operator_ids =
            parse_operator_ids(&optional_env("P1_OPERATOR_IDS")?.unwrap_or_default())?;
        tracing::info!(fingerprint = %config.keyring.fingerprint(), "P1 keyring loaded");
        let mode = optional_env("GHINVITE_RESTATE_AUTH")?;
        let api_key = optional_env("GHINVITE_RESTATE_API_KEY")?;
        config.restate_auth =
            crate::restate_client::RestateAuth::from_config(mode.as_deref(), api_key.as_deref())?;
        Ok(config)
    }

    /// Local defaults with an explicitly supplied key (also used by tests).
    pub fn for_local_dev_with_secret(secret: [u8; 32]) -> Self {
        Self::with_keyring(Arc::new(
            Keyring::builder("p1")
                .generation(1, secret)
                .current(1)
                .build()
                .unwrap(),
        ))
    }

    pub fn with_keyring(keyring: Arc<Keyring>) -> Self {
        Self {
            reissue_enabled: false,
            bulk_enabled: false,
            github_org_install_url: None,
            organization_delivery_enabled: false,
            access_removal_enabled: false,
            github_apps: Vec::new(),
            github: apps::GithubBases::github(),
            webhooks: None,
            signin_apps: None,
            base_url: std::env::var("GHINVITE_BASE_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8787".into()),
            keyring,
            operator_ids: BTreeSet::new(),
            restate_ingress: std::env::var("GHINVITE_RESTATE_INGRESS")
                .unwrap_or_else(|_| "http://127.0.0.1:8080".into()),
            restate_auth: crate::restate_client::RestateAuth::local_unauthenticated(),
            github_install_url: std::env::var("GHINVITE_GITHUB_INSTALL_URL").unwrap_or_else(|_| {
                "https://github.com/apps/ghinvite-local/installations/new".into()
            }),
            cookie_secure: false,
            // Relative to the working directory: `cargo run -p ghinvite-web`
            // from the repo root, where `scripts/build-island.sh` writes.
            island_assets_dir: Some(PathBuf::from(
                std::env::var("GHINVITE_ISLAND_ASSETS_DIR")
                    .unwrap_or_else(|_| "dist/public/assets".into()),
            )),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid or missing deployment configuration: {0}")]
    Field(&'static str),
    #[error(transparent)]
    Apps(#[from] apps::AppDeploymentError),
    #[error(transparent)]
    SessionKey(#[from] SessionKeyError),
    #[error("P1_OPERATOR_IDS must be comma-separated positive canonical numeric GitHub IDs")]
    OperatorIds,
    #[error(transparent)]
    Restate(#[from] crate::WebError),
    #[error("Invalid encoding in server configuration: {0}")]
    Encoding(&'static str),
}

/// Local development's sign-in app: `GH_SIGNIN_CLIENT_ID` and
/// `GH_SIGNIN_CLIENT_SECRET` (both or neither), calling back to `base_url`.
fn local_signin_app(base_url: &str) -> Result<Option<apps::SignInApp>, ConfigError> {
    let client_id = optional_env("GH_SIGNIN_CLIENT_ID")?;
    let client_secret = optional_env("GH_SIGNIN_CLIENT_SECRET")?;
    let (client_id, client_secret) = match (client_id, client_secret) {
        (Some(id), Some(secret)) => (id, secret),
        (None, None) => {
            tracing::warn!(
                "GH_SIGNIN_CLIENT_ID/GH_SIGNIN_CLIENT_SECRET unset: sign-in unavailable"
            );
            return Ok(None);
        }
        (None, Some(_)) => return Err(ConfigError::Field("GH_SIGNIN_CLIENT_ID")),
        (Some(_), None) => return Err(ConfigError::Field("GH_SIGNIN_CLIENT_SECRET")),
    };
    let redirect_uri = url::Url::parse(&format!("{base_url}/oauth/callback"))
        .map_err(|_| ConfigError::Field("GHINVITE_BASE_URL"))?;
    Ok(Some(apps::SignInApp::new(
        client_id,
        client_secret,
        redirect_uri,
    )))
}

fn required(
    read: &mut impl FnMut(&'static str) -> Option<String>,
    field: &'static str,
) -> Result<String, ConfigError> {
    read(field).ok_or(ConfigError::Field(field))
}

fn validate_https_url(value: &str, field: &'static str) -> Result<url::Url, ConfigError> {
    let invalid = || ConfigError::Field(field);
    let url = url::Url::parse(value).map_err(|_| invalid())?;
    if value.len() > 4096
        || url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || value
            .bytes()
            .any(|b| b.is_ascii_control() || b.is_ascii_whitespace() || b == b'\\')
        || !value.starts_with("https://")
    {
        return Err(invalid());
    }
    if let Some(url::Host::Domain(host)) = url.host() {
        apps::parse_host(host).map_err(|_| invalid())?;
    }
    // Reject hidden URL normalization of the authority (case, IDNs, default
    // ports and numeric IP aliases), while permitting explicit HTTPS ports.
    let authority = value
        .strip_prefix("https://")
        .ok_or_else(invalid)?
        .split('/')
        .next()
        .ok_or_else(invalid)?;
    let canonical = &url[url::Position::BeforeHost..url::Position::AfterPort];
    if authority != canonical {
        return Err(invalid());
    }
    Ok(url)
}

fn optional_env(name: &'static str) -> Result<Option<String>, ConfigError> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(ConfigError::Encoding(name)),
    }
}

impl std::fmt::Debug for WebConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebConfig")
            .field("base_url", &self.base_url)
            .field("keyring", &"[REDACTED]")
            .field("cookie_secure", &self.cookie_secure)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("P1_KEYRING must be a valid kekring1 descriptor")]
pub struct SessionKeyError;

/// Errors name the configuration field, never its secret contents.
pub fn parse_keyring(value: &str) -> Result<Arc<Keyring>, SessionKeyError> {
    Keyring::parse(value)
        .map(Arc::new)
        .map_err(|_| SessionKeyError)
}

pub fn parse_operator_ids(value: &str) -> Result<BTreeSet<u64>, ConfigError> {
    if value.is_empty() {
        return Ok(BTreeSet::new());
    }
    value
        .split(',')
        .map(|part| {
            part.parse::<u64>()
                .ok()
                .filter(|id| *id > 0 && *id <= 9_007_199_254_740_991 && id.to_string() == part)
                .ok_or(ConfigError::OperatorIds)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_configuration_and_redaction() {
        for bad in ["", "ab", "akr1:bad", "kekring1;bad", "secret-value"] {
            let error = parse_keyring(bad).unwrap_err().to_string();
            assert_eq!(error, "P1_KEYRING must be a valid kekring1 descriptor");
        }
        assert_eq!(parse_operator_ids("42,7").unwrap(), BTreeSet::from([7, 42]));
        assert!(parse_operator_ids("").unwrap().is_empty());
        for bad in ["0", "01", "1,", "1, 2", "-1", "18446744073709551616"] {
            assert!(parse_operator_ids(bad).is_err());
        }
        let cfg = WebConfig::for_local_dev_with_secret([0xab; 32]);
        assert!(format!("{cfg:?}").contains("[REDACTED]"));
        assert!(!format!("{cfg:?}").contains(&"ab".repeat(32)));
    }
}

fn validate_org_install_url(value: &str) -> Result<(), ConfigError> {
    let url = validate_https_url(value, "GH_ORG_APP_INSTALL_URL")?;
    let segments: Vec<_> = url.path().split('/').collect();
    if url.host_str() != Some("github.com")
        || url.port().is_some()
        || url.query().is_some()
        || segments.len() != 5
        || segments[1] != "apps"
        || segments[2].is_empty()
        || !segments[2]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        || segments[3] != "installations"
        || segments[4] != "new"
    {
        return Err(ConfigError::Field("GH_ORG_APP_INSTALL_URL"));
    }
    Ok(())
}

#[cfg(test)]
mod organization_install_url_tests {
    use super::*;
    #[test]
    fn org_install_url_is_an_exact_trusted_installation_target() {
        assert!(
            validate_org_install_url("https://github.com/apps/inviter-org/installations/new")
                .is_ok()
        );
        for value in [
            "https://github.com/apps/x/installations/new?return_to=secret",
            "http://github.com/apps/x/installations/new",
            "https://github.com.evil.test/apps/x/installations/new",
            "https://user:secret@github.com/apps/x/installations/new",
            "https://github.com/apps/x/y/installations/new",
            "https://github.com/apps/x/installations/new#secret",
            "https://github.com/apps//installations/new",
        ] {
            let error = validate_org_install_url(value).unwrap_err();
            assert!(error.to_string().contains("GH_ORG_APP_INSTALL_URL"));
            assert!(!error.to_string().contains("secret"));
        }
    }
}
