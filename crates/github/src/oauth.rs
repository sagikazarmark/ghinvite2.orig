//! User-token OAuth flow plus the user-token API client.
//!
//! Two pieces:
//! 1. [`AuthorizeUrl::build`] / [`exchange_code`] are stateless helpers used by
//!    the web binary to drive the OAuth redirect dance.
//! 2. [`UserApiClient`] holds a transport + a user access token and exposes
//!    the two user-side endpoints we call (`/user` and
//!    `/user/memberships/orgs/{login}`).

use crate::error::{Error, Result};
use crate::payloads::GhTokenResponse;
use crate::transport::{HttpTransport, Method, Request};
use std::sync::Arc;
use url::Url;

/// Configuration for the GitHub App's OAuth surface. The web binary owns the
/// `client_secret`; the Restate worker never sees it.
#[derive(Clone, Debug)]
pub struct OAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    /// Must exactly match the redirect URI registered on the GitHub App. v1
    /// hosts a single redirect URI: `<base_url>/oauth/callback`.
    pub redirect_uri: String,
}

/// Output of [`AuthorizeUrl::build`]. The caller stores `state` in a signed
/// session cookie before redirecting the browser to `url`.
#[derive(Clone, Debug)]
pub struct AuthorizeUrl {
    pub url: String,
    pub state: String,
}

impl AuthorizeUrl {
    /// Build a `https://github.com/login/oauth/authorize?...` URL with the given
    /// CSRF `state` token. The caller is responsible for generating `state`
    /// (recommend 32 bytes of CSPRNG → base64url) and persisting it.
    ///
    /// `extra_params` lets callers append anything GitHub supports (e.g.
    /// `allow_signup=false`); `redirect_uri`, `scope`, `state`, `client_id` are
    /// always set by this builder.
    pub fn build(
        cfg: &OAuthConfig,
        state: impl Into<String>,
        extra_params: &[(&str, &str)],
    ) -> Result<Self> {
        let state = state.into();
        let mut url = Url::parse("https://github.com/login/oauth/authorize")
            .map_err(|e| Error::InvalidInput(format!("authorize url: {e}")))?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("client_id", &cfg.client_id);
            q.append_pair("redirect_uri", &cfg.redirect_uri);
            q.append_pair("scope", "read:user");
            q.append_pair("state", &state);
            for (k, v) in extra_params {
                q.append_pair(k, v);
            }
        }
        Ok(Self {
            url: url.into(),
            state,
        })
    }
}

/// Exchange an OAuth authorization code for a user access token. Calls
/// `POST https://github.com/login/oauth/access_token` with `Accept:
/// application/json` so GitHub returns JSON instead of `application/x-www-form-urlencoded`.
pub async fn exchange_code<T: HttpTransport + ?Sized>(
    transport: &T,
    cfg: &OAuthConfig,
    code: &str,
) -> Result<GhTokenResponse> {
    let body = serde_json::json!({
        "client_id": cfg.client_id,
        "client_secret": cfg.client_secret,
        "code": code,
        "redirect_uri": cfg.redirect_uri,
    });
    let req = Request::new(Method::Post, "https://github.com/login/oauth/access_token")
        .header("accept", "application/json")
        .header("user-agent", "ghinvite")
        .json_body(&body)?;
    let resp = transport.send(req).await?.ensure_success()?;

    // GitHub returns 200 with an `error`/`error_description` payload on
    // failure (rather than a 4xx). Parse the body once as an opaque value,
    // sniff for `error`, and only then attempt the success-shape decode.
    let value: serde_json::Value = resp.json()?;
    if let Some(error_code) = value.get("error").and_then(|v| v.as_str()) {
        let desc = value
            .get("error_description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        return Err(Error::OAuth(format!("{error_code}: {desc}")));
    }
    serde_json::from_value(value)
        .map_err(|e| Error::Decode(format!("oauth token response: {e}")))
}

/// User-token API client. Constructed per signed-in session (the access token
/// is encrypted at rest in the session cookie; the web binary decrypts it
/// before instantiating this client per request).
#[derive(Clone)]
#[allow(dead_code)] // fields are read by methods added in Task 9
pub struct UserApiClient {
    transport: Arc<dyn HttpTransport>,
    user_token: String,
    base_url: String,
}

impl UserApiClient {
    /// `transport` typically wraps `ReqwestTransport`. `user_token` is the
    /// `access_token` from [`exchange_code`].
    pub fn new(transport: Arc<dyn HttpTransport>, user_token: String) -> Self {
        Self::with_base(transport, user_token, "https://api.github.com".into())
    }

    /// Construct against an alternate base URL (used by `MockTransport` /
    /// `wiremock` tests). Path is appended verbatim — no trailing slash.
    pub fn with_base(transport: Arc<dyn HttpTransport>, user_token: String, base_url: String) -> Self {
        Self {
            transport,
            user_token,
            base_url,
        }
    }

    #[allow(dead_code)] // used by methods added in Task 9
    fn auth_request(&self, method: Method, path: &str) -> Request {
        Request::new(method, format!("{}{}", self.base_url, path))
            .header("accept", "application/vnd.github+json")
            .header("authorization", format!("Bearer {}", self.user_token))
            .header("user-agent", "ghinvite")
            .header("x-github-api-version", "2022-11-28")
    }
}

#[cfg(test)]
mod url_tests {
    use super::*;

    fn cfg() -> OAuthConfig {
        OAuthConfig {
            client_id: "Iv1.abc".into(),
            client_secret: "secret".into(),
            redirect_uri: "https://example.test/oauth/callback".into(),
        }
    }

    #[test]
    fn authorize_url_includes_required_params() {
        let a = AuthorizeUrl::build(&cfg(), "csrf-token-123", &[]).unwrap();
        assert!(a.url.starts_with("https://github.com/login/oauth/authorize?"));
        assert!(a.url.contains("client_id=Iv1.abc"));
        assert!(a.url.contains("scope=read%3Auser"));
        assert!(a.url.contains("state=csrf-token-123"));
        assert!(a.url.contains("redirect_uri=https%3A%2F%2Fexample.test%2Foauth%2Fcallback"));
        assert_eq!(a.state, "csrf-token-123");
    }

    #[test]
    fn authorize_url_appends_extra_params() {
        let a = AuthorizeUrl::build(&cfg(), "x", &[("allow_signup", "false")]).unwrap();
        assert!(a.url.contains("allow_signup=false"));
    }
}
