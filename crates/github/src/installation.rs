//! App-installation client: holds the App-JWT signer + a token cache, and
//! exposes the v1 installation-side endpoints.

use crate::error::Result;
use crate::jwt::AppJwtSigner;
use crate::payloads::GhInstallationToken;
use crate::token_cache::TokenCache;
use crate::transport::{HttpTransport, Method, Request};
use chrono::Utc;
use std::sync::Arc;

const DEFAULT_BASE_URL: &str = "https://api.github.com";

/// Long-lived handle. Construct once per worker invocation (the cache is
/// in-process; restart drops it).
#[derive(Clone)]
pub struct InstallationClient {
    transport: Arc<dyn HttpTransport>,
    signer: AppJwtSigner,
    cache: TokenCache,
    base_url: String,
}

impl InstallationClient {
    pub fn new(transport: Arc<dyn HttpTransport>, signer: AppJwtSigner) -> Self {
        Self {
            transport,
            signer,
            cache: TokenCache::new(),
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }

    /// Test/dev: point at an alternate host (e.g. a wiremock listener).
    pub fn with_base(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Mint an App-JWT and exchange it for an installation token. Hits the
    /// network; callers should prefer `installation_token` which goes through
    /// the cache.
    pub async fn mint_installation_token(
        &self,
        installation_id: u64,
    ) -> Result<GhInstallationToken> {
        let jwt = self.signer.sign(Utc::now())?;
        let url = format!(
            "{}/app/installations/{}/access_tokens",
            self.base_url, installation_id
        );
        let req = Request::new(Method::Post, &url)
            .header("accept", "application/vnd.github+json")
            .header("authorization", format!("Bearer {jwt}"))
            .header("user-agent", "ghinvite")
            .header("x-github-api-version", "2022-11-28");
        self.transport.send(req).await?.ensure_success()?.json()
    }

    /// Returns a non-expired installation token, refreshing through GitHub if
    /// the cached one is gone or within 60s of expiry.
    pub async fn installation_token(&self, installation_id: u64) -> Result<String> {
        if let Some(t) = self.cache.get_fresh(installation_id, Utc::now()) {
            return Ok(t);
        }
        let minted = self.mint_installation_token(installation_id).await?;
        self.cache
            .insert(installation_id, minted.token.clone(), minted.expires_at);
        Ok(minted.token)
    }

    /// Build a request pre-loaded with the auth header for `installation_id`.
    /// Caller adds method/path/body.
    // Used by methods added in Tasks 14-16.
    #[allow(dead_code)]
    pub(crate) async fn auth_request(
        &self,
        installation_id: u64,
        method: Method,
        path: &str,
    ) -> Result<Request> {
        let token = self.installation_token(installation_id).await?;
        Ok(Request::new(method, format!("{}{}", self.base_url, path))
            .header("accept", "application/vnd.github+json")
            .header("authorization", format!("Bearer {token}"))
            .header("user-agent", "ghinvite")
            .header("x-github-api-version", "2022-11-28"))
    }
}

#[cfg(test)]
mod token_mint_tests {
    use super::*;
    use crate::mocks::{Expectation, MockTransport};
    use crate::transport::Response;
    use std::collections::BTreeMap;

    /// Use the same static test key as `crates/github/src/jwt.rs` to keep
    /// signing fast (~milliseconds, not seconds).
    const TEST_KEY_PEM: &str = include_str!("jwt_test_key.pem");

    fn signer() -> AppJwtSigner {
        AppJwtSigner::from_pkcs8_pem(123, TEST_KEY_PEM).unwrap()
    }

    fn token_response_body() -> Vec<u8> {
        // expires far in the future — these tests don't exercise expiry logic.
        br#"{"token":"ghs_xxx","expires_at":"2099-01-01T00:00:00Z"}"#.to_vec()
    }

    #[tokio::test]
    async fn mint_calls_correct_endpoint_and_decodes_token() {
        let mock = MockTransport::scripted(vec![Expectation {
            method: Method::Post,
            url: "https://api.github.test/app/installations/55/access_tokens".into(),
            required_headers: {
                let mut h = BTreeMap::new();
                h.insert("accept".into(), "application/vnd.github+json".into());
                h
            },
            expected_body: None,
            response: Response {
                status: 201,
                headers: BTreeMap::new(),
                body: token_response_body(),
            },
        }]);
        let client = InstallationClient::new(Arc::new(mock.clone()), signer())
            .with_base("https://api.github.test");
        let t = client.mint_installation_token(55).await.unwrap();
        assert_eq!(t.token, "ghs_xxx");
        mock.assert_exhausted();
    }

    #[tokio::test]
    async fn second_call_uses_cache_and_does_not_hit_network() {
        let mock = MockTransport::scripted(vec![Expectation {
            method: Method::Post,
            url: "https://api.github.test/app/installations/55/access_tokens".into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 201,
                headers: BTreeMap::new(),
                body: token_response_body(),
            },
        }]);
        let client = InstallationClient::new(Arc::new(mock.clone()), signer())
            .with_base("https://api.github.test");
        let _ = client.installation_token(55).await.unwrap();
        let t2 = client.installation_token(55).await.unwrap();
        assert_eq!(t2, "ghs_xxx");
        mock.assert_exhausted(); // only ONE network call
    }
}
