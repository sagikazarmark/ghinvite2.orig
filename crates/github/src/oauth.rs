//! User-token OAuth flow plus the user-token API client.
//!
//! Two pieces:
//! 1. [`AuthorizeUrl::build`] / [`exchange_code`] are stateless helpers used by
//!    the web binary to drive the OAuth redirect dance.
//! 2. [`UserApiClient`] holds a transport + a user access token and exposes
//!    the two user-side endpoints we call (`/user` and
//!    `/user/memberships/orgs/{login}`).

use crate::error::{Error, Result};
use crate::payloads::{GhMembership, GhTokenResponse, GhUser};
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

    fn auth_request(&self, method: Method, path: &str) -> Request {
        Request::new(method, format!("{}{}", self.base_url, path))
            .header("accept", "application/vnd.github+json")
            .header("authorization", format!("Bearer {}", self.user_token))
            .header("user-agent", "ghinvite")
            .header("x-github-api-version", "2022-11-28")
    }

    /// `GET /user` — returns the signed-in user's basic profile. Used at sign-in
    /// time to upsert the `users` row (the web binary builds a `domain::User`
    /// from this plus `last_seen_at = Utc::now()`).
    ///
    /// **Errors:** `Error::Status` for non-2xx (401 = expired token, etc.),
    /// `Error::Decode` for malformed JSON.
    pub async fn get_user(&self) -> Result<GhUser> {
        let req = self.auth_request(Method::Get, "/user");
        self.transport.send(req).await?.ensure_success()?.json()
    }

    /// `GET /user/memberships/orgs/{login}` — used by the admin recheck path
    /// (spec §10.3). Returns `Error::Status { status: 404, .. }` if the user is
    /// not a member of the org; the caller should map that to "not admin".
    ///
    /// **Errors:** `Error::Status` for non-2xx, `Error::Decode` for malformed JSON.
    pub async fn get_org_membership(&self, org_login: &str) -> Result<GhMembership> {
        // Path-encode the login to defend against odd characters (org renames,
        // etc.). url::form_urlencoded::byte_serialize would be heavier; we use
        // url::Url to build the path safely.
        let path = format!("/user/memberships/orgs/{}", url_path_segment(org_login));
        let req = self.auth_request(Method::Get, &path);
        self.transport.send(req).await?.ensure_success()?.json()
    }
}

/// Percent-encode a single path segment. We allow only the characters GitHub
/// uses in logins; everything else is encoded.
fn url_path_segment(s: &str) -> String {
    const SAFE: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_.~";
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if SAFE.contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
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

#[cfg(test)]
mod exchange_tests {
    use super::*;
    use crate::error::Error;
    use crate::mocks::{Expectation, MockTransport};
    use crate::transport::{Method, Response};
    use std::collections::BTreeMap;

    fn cfg() -> OAuthConfig {
        OAuthConfig {
            client_id: "Iv1.abc".into(),
            client_secret: "secret".into(),
            redirect_uri: "https://example.test/oauth/callback".into(),
        }
    }

    #[tokio::test]
    async fn happy_path_returns_token() {
        let mock = MockTransport::scripted(vec![Expectation {
            method: Method::Post,
            url: "https://github.com/login/oauth/access_token".into(),
            required_headers: {
                let mut h = BTreeMap::new();
                h.insert("accept".into(), "application/json".into());
                h.insert("content-type".into(), "application/json".into());
                h.insert("user-agent".into(), "ghinvite".into());
                h
            },
            expected_body: None,
            response: Response {
                status: 200,
                headers: BTreeMap::new(),
                body: br#"{"access_token":"u_xxx","token_type":"bearer","scope":"read:user"}"#.to_vec(),
            },
        }]);
        let token = exchange_code(&mock, &cfg(), "auth-code-1").await.unwrap();
        assert_eq!(token.access_token, "u_xxx");
        assert_eq!(token.scope, "read:user");
        mock.assert_exhausted();
    }

    #[tokio::test]
    async fn github_error_payload_surfaces_as_oauth_error() {
        let mock = MockTransport::scripted(vec![Expectation {
            method: Method::Post,
            url: "https://github.com/login/oauth/access_token".into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 200, // GitHub uses 200 + error payload, not a 4xx.
                headers: BTreeMap::new(),
                body: br#"{"error":"bad_verification_code","error_description":"The code passed is incorrect or expired."}"#.to_vec(),
            },
        }]);
        let err = exchange_code(&mock, &cfg(), "expired").await.unwrap_err();
        match err {
            Error::OAuth(msg) => {
                assert!(msg.contains("bad_verification_code"));
                assert!(msg.contains("expired"));
            }
            other => panic!("expected OAuth error, got {other:?}"),
        }
        mock.assert_exhausted();
    }

    #[tokio::test]
    async fn http_5xx_surfaces_as_status_error() {
        let mock = MockTransport::scripted(vec![Expectation::status(
            Method::Post,
            "https://github.com/login/oauth/access_token",
            502,
        )]);
        let err = exchange_code(&mock, &cfg(), "any").await.unwrap_err();
        assert_eq!(err.status(), Some(502));
        mock.assert_exhausted();
    }

    #[tokio::test]
    async fn malformed_json_body_surfaces_as_decode_error() {
        let mock = MockTransport::scripted(vec![Expectation {
            method: Method::Post,
            url: "https://github.com/login/oauth/access_token".into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 200,
                headers: BTreeMap::new(),
                body: b"not-json".to_vec(),
            },
        }]);
        let err = exchange_code(&mock, &cfg(), "any").await.unwrap_err();
        assert!(matches!(err, Error::Decode(_)), "expected Decode, got {err:?}");
        mock.assert_exhausted();
    }

    #[tokio::test]
    async fn unexpected_json_shape_surfaces_as_decode_error() {
        let mock = MockTransport::scripted(vec![Expectation {
            method: Method::Post,
            url: "https://github.com/login/oauth/access_token".into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 200,
                headers: BTreeMap::new(),
                body: br#"{"foo":"bar"}"#.to_vec(),
            },
        }]);
        let err = exchange_code(&mock, &cfg(), "any").await.unwrap_err();
        assert!(matches!(err, Error::Decode(_)), "expected Decode, got {err:?}");
        mock.assert_exhausted();
    }
}

#[cfg(test)]
mod user_api_tests {
    use super::*;
    use crate::mocks::{Expectation, MockTransport};
    use crate::transport::{Method, Response};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    fn client_with(mock: MockTransport) -> UserApiClient {
        UserApiClient::with_base(
            Arc::new(mock),
            "u_xxx".into(),
            "https://api.github.test".into(),
        )
    }

    #[tokio::test]
    async fn get_user_sends_authorization_and_returns_payload() {
        let mock = MockTransport::scripted(vec![Expectation {
            method: Method::Get,
            url: "https://api.github.test/user".into(),
            required_headers: {
                let mut h = BTreeMap::new();
                h.insert("authorization".into(), "Bearer u_xxx".into());
                h.insert("accept".into(), "application/vnd.github+json".into());
                h
            },
            expected_body: None,
            response: Response {
                status: 200,
                headers: BTreeMap::new(),
                body: br#"{"id":42,"login":"octocat","avatar_url":"https://a/u/42"}"#.to_vec(),
            },
        }]);
        let mock_clone = mock.clone();
        let user = client_with(mock).get_user().await.unwrap();
        assert_eq!(user.id, 42);
        assert_eq!(user.login, "octocat");
        mock_clone.assert_exhausted();
    }

    #[tokio::test]
    async fn get_org_membership_path_encodes_login() {
        let mock = MockTransport::scripted(vec![Expectation::ok_json(
            Method::Get,
            "https://api.github.test/user/memberships/orgs/acme%20corp",
            serde_json::json!({"role": "admin", "state": "active"}),
        )]);
        let m = client_with(mock).get_org_membership("acme corp").await.unwrap();
        assert_eq!(m.role, "admin");
        assert_eq!(m.state, "active");
    }

    #[tokio::test]
    async fn get_org_membership_404_surfaces_status() {
        let mock = MockTransport::scripted(vec![Expectation::status(
            Method::Get,
            "https://api.github.test/user/memberships/orgs/private",
            404,
        )]);
        let err = client_with(mock).get_org_membership("private").await.unwrap_err();
        assert_eq!(err.status(), Some(404));
    }
}
