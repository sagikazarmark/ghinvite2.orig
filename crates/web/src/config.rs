//! Web binary runtime configuration.

use github::oauth::OAuthConfig;

/// Configuration parsed at server boot. Production sources fields from
/// Workers secrets (Plan 7); local dev uses [`WebConfig::for_local_dev`].
#[derive(Clone, Debug)]
pub struct WebConfig {
    /// Base URL of THIS web binary (e.g. `https://ghinvite.example`). Used to
    /// build OAuth redirect URIs and absolute URLs in the home page.
    pub base_url: String,
    /// 32-byte symmetric key for tower-sessions cookie encryption.
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
}

impl WebConfig {
    /// Build a config suitable for `cargo run -p web` local dev. Reads from
    /// environment variables; falls back to safe defaults that won't actually
    /// authenticate against real GitHub.
    pub fn for_local_dev() -> Self {
        let secret = match std::env::var("GHINVITE_SESSION_SECRET") {
            Ok(s) if s.len() >= 32 => {
                let mut b = [0u8; 32];
                b.copy_from_slice(&s.as_bytes()[..32]);
                b
            }
            _ => *b"insecure-local-dev-session-key!!", // 32 bytes
        };
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_dev_default_does_not_panic() {
        // SAFETY: tests should not depend on the env, but for_local_dev reads
        // it; we rely on the unwrap_or fallbacks. Cleaner would be to inject
        // env, but acceptable for this smoke.
        let cfg = WebConfig::for_local_dev();
        assert!(cfg.base_url.starts_with("http"));
        assert_eq!(cfg.session_secret.len(), 32);
        assert!(cfg.oauth.redirect_uri.ends_with("/oauth/callback"));
    }
}
