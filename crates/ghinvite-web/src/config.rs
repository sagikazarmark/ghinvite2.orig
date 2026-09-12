//! Web binary runtime configuration.

use ghinvite_github::oauth::OAuthConfig;
use std::path::PathBuf;

/// Configuration parsed at server boot. Production sources fields from
/// Workers secrets (Plan 7); local dev uses [`WebConfig::for_local_dev`].
#[derive(Clone)]
pub struct WebConfig {
    /// Base URL of THIS web binary (e.g. `https://ghinvite.example`). Used to
    /// build OAuth redirect URIs and absolute URLs in the home page.
    pub base_url: String,
    /// 32-byte symmetric key for application-level session record encryption.
    /// Production rotates this via Workers secrets.
    pub session_secret: [u8; 32],
    /// Restate ingress URL (e.g. `http://127.0.0.1:8080` for local dev).
    pub restate_ingress: String,
    /// OAuth credentials (client_id, client_secret, redirect_uri).
    pub oauth: OAuthConfig,
    /// GitHub App install URL — `https://github.com/apps/<app-name>/installations/new`.
    pub github_install_url: String,
    /// Whether to set the `Secure` attribute on session cookies.
    /// Off for local-dev http; on for production https.
    pub cookie_secure: bool,
    /// Raw bytes of the GitHub App webhook secret. Used by the webhook handler
    /// to verify `X-Hub-Signature-256`. Loaded from Workers secrets in production;
    /// Empty disables webhook reception (503); local deliveries require
    /// `GHINVITE_WEBHOOK_SECRET` to be set.
    pub webhook_secret: Vec<u8>,
    /// Directory the native binary serves under `/assets/*`: the Dioxus island
    /// bundle (`scripts/build-island.sh` writes it to `dist/public/assets`).
    /// `None` registers no route. On Workers this is always `None` — Cloudflare
    /// Static Assets serve `/assets/*` before the Worker runs (see
    /// `wrangler/web.toml`). Only read on non-wasm32 builds.
    pub island_assets_dir: Option<PathBuf>,
}

impl WebConfig {
    /// Build native local configuration from environment variables. The session
    /// key is mandatory; other fields have local defaults that do not authenticate
    /// against real GitHub without explicit OAuth credentials.
    pub fn for_local_dev() -> Result<Self, SessionKeyError> {
        let secret = std::env::var("GHINVITE_SESSION_SECRET").map_err(|_| SessionKeyError)?;
        Ok(Self::for_local_dev_with_secret(parse_session_secret(
            &secret,
        )?))
    }

    /// Local defaults with an explicitly supplied key (also used by tests).
    pub fn for_local_dev_with_secret(secret: [u8; 32]) -> Self {
        Self {
            base_url: std::env::var("GHINVITE_BASE_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8787".into()),
            session_secret: secret,
            restate_ingress: std::env::var("GHINVITE_RESTATE_INGRESS")
                .unwrap_or_else(|_| "http://127.0.0.1:8080".into()),
            oauth: OAuthConfig {
                client_id: std::env::var("GHINVITE_GITHUB_CLIENT_ID")
                    .unwrap_or_else(|_| "Iv1.local-dev-client-id".into()),
                client_secret: std::env::var("GHINVITE_GITHUB_CLIENT_SECRET")
                    .unwrap_or_else(|_| "local-dev-client-secret".into()),
                redirect_uri: format!(
                    "{}/oauth/callback",
                    std::env::var("GHINVITE_BASE_URL")
                        .unwrap_or_else(|_| "http://127.0.0.1:8787".into())
                ),
            },
            github_install_url: std::env::var("GHINVITE_GITHUB_INSTALL_URL").unwrap_or_else(|_| {
                "https://github.com/apps/ghinvite-local/installations/new".into()
            }),
            cookie_secure: false,
            webhook_secret: std::env::var("GHINVITE_WEBHOOK_SECRET")
                .map(|s| s.into_bytes())
                .unwrap_or_default(),
            // Relative to the working directory: `cargo run -p ghinvite-web`
            // from the repo root, where `scripts/build-island.sh` writes.
            island_assets_dir: Some(PathBuf::from(
                std::env::var("GHINVITE_ISLAND_ASSETS_DIR")
                    .unwrap_or_else(|_| "dist/public/assets".into()),
            )),
        }
    }
}

impl std::fmt::Debug for WebConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebConfig")
            .field("base_url", &self.base_url)
            .field("session_secret", &"[REDACTED]")
            .field("cookie_secure", &self.cookie_secure)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("GHINVITE_SESSION_SECRET must be exactly 64 hexadecimal characters (32 bytes)")]
pub struct SessionKeyError;

/// Shared native/Worker parser; errors never include supplied key material.
pub fn parse_session_secret(value: &str) -> Result<[u8; 32], SessionKeyError> {
    if value.len() != 64 {
        return Err(SessionKeyError);
    }
    let mut key = [0; 32];
    hex::decode_to_slice(value, &mut key).map_err(|_| SessionKeyError)?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_local_key_and_strict_shared_parsing() {
        let key = "ab".repeat(32);
        assert_eq!(parse_session_secret(&key).unwrap(), [0xab; 32]);
        for value in [
            "".to_owned(),
            "a".repeat(63),
            "a".repeat(65),
            "gg".repeat(32),
        ] {
            assert!(parse_session_secret(&value).is_err());
        }
        let cfg = WebConfig::for_local_dev_with_secret([0xab; 32]);
        assert!(!format!("{cfg:?}").contains(&key));
        assert!(format!("{cfg:?}").contains("[REDACTED]"));
        assert!(cfg.base_url.starts_with("http"));
        assert_eq!(cfg.session_secret.len(), 32);
        assert!(cfg.oauth.redirect_uri.ends_with("/oauth/callback"));
    }
}
