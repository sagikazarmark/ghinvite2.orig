//! User-token OAuth flow plus the user-token API client.
//!
//! Two pieces:
//! 1. [`AuthorizeUrl::build`] / [`exchange_code`] are stateless helpers used by
//!    the web binary to drive the OAuth redirect dance.
//! 2. [`UserApiClient`] holds a transport + a user access token and exposes
//!    the two user-side endpoints we call (`/user` and
//!    `/user/memberships/orgs/{login}`).

use crate::error::{Error, Result};
use crate::payloads::{
    GhInstallationRepos, GhMembership, GhTokenResponse, GhUser, GhUserInstallationList,
};
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
            q.append_pair("scope", "read:user read:org");
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
#[tracing::instrument(skip(transport, cfg, code))]
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
    let resp = match transport.send(req).await?.ensure_success() {
        Ok(r) => r,
        Err(err) => {
            if let Some(status) = err.status()
                && (400..500).contains(&status)
            {
                tracing::warn!(status, "oauth token exchange returned 4xx");
            }
            return Err(err);
        }
    };

    // GitHub returns 200 with an `error`/`error_description` payload on
    // failure (rather than a 4xx). Parse the body once as an opaque value,
    // sniff for `error`, and only then attempt the success-shape decode.
    let value: serde_json::Value = resp.json()?;
    if let Some(error_code) = value.get("error").and_then(|v| v.as_str()) {
        // The code is the actionable distinction and is bounded to a documented
        // shape. `error_description` is upstream prose that ends up in logs and
        // in the browser's failure page, so it stops here.
        let error_code =
            crate::redact::bounded_upstream_code(error_code, crate::redact::OAUTH_ERROR_CODES);
        tracing::warn!(
            error_code = %error_code,
            "oauth token exchange returned error payload"
        );
        return Err(Error::OAuth(error_code));
    }
    // A shape mismatch here has the token payload in hand; `serde_json`'s
    // message would quote it.
    let token: GhTokenResponse =
        serde_json::from_value(value).map_err(|_| Error::decode_shape("oauth token response"))?;
    // `scope` is upstream text on the success path — a proxy can return a
    // well-formed token payload whose scope is arbitrarily long or carries a
    // reflected secret. ghinvite requests a fixed scope, so the event itself
    // is the useful part.
    tracing::info!("oauth token exchange succeeded");
    Ok(token)
}

/// User-token API client. Constructed per signed-in session (the access token
/// is encrypted at rest in the server-side session record; the web binary decrypts it
/// before instantiating this client per request).
#[derive(Clone)]
pub struct UserApiClient {
    transport: Arc<dyn HttpTransport>,
    user_token: String,
    base_url: String,
}

impl UserApiClient {
    /// `transport` typically wraps `ReqwestTransport` on native; on wasm /
    /// Cloudflare Workers it wraps Plan 3's Workers-side transport
    /// (`WorkerFetchTransport`), since reqwest's wasm response future is
    /// `!Send`. `user_token` is the `access_token` from [`exchange_code`].
    pub fn new(transport: Arc<dyn HttpTransport>, user_token: String) -> Self {
        Self::with_base(transport, user_token, "https://api.github.com".into())
    }

    /// Construct against an alternate base URL (used by `MockTransport` /
    /// `wiremock` tests). Path is appended verbatim — no trailing slash.
    pub fn with_base(
        transport: Arc<dyn HttpTransport>,
        user_token: String,
        base_url: String,
    ) -> Self {
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
    /// time to upsert the `users` row (the web binary builds a `ghinvite_core::User`
    /// from this plus `last_seen_at = Utc::now()`).
    ///
    /// **Errors:** `Error::Status` for non-2xx (401 = expired token, etc.),
    /// `Error::RateLimited` for a throttled 403/429, `Error::Decode` for
    /// malformed JSON.
    #[tracing::instrument(skip(self), fields(method = "get_user"))]
    pub async fn get_user(&self) -> Result<GhUser> {
        tracing::debug!("calling GET /user");
        let req = self.auth_request(Method::Get, "/user");
        let resp = self.transport.send(req).await?;
        if !(200..300).contains(&resp.status) {
            tracing::warn!(status = resp.status, "github GET /user returned non-2xx");
        }
        resp.ensure_success()?.json()
    }

    /// `GET /user/memberships/orgs/{login}` — used by the admin recheck path
    /// (spec §10.3). Returns `Error::Status { status: 404, .. }` if the user is
    /// not a member of the org; the caller should map that to "not admin".
    ///
    /// **Errors:** `Error::Status` for non-2xx, `Error::RateLimited` for a
    /// throttled 403/429 — which is not evidence of non-membership — and
    /// `Error::Decode` for malformed JSON.
    #[tracing::instrument(skip(self), fields(method = "get_org_membership", org_login))]
    pub async fn get_org_membership(&self, org_login: &str) -> Result<GhMembership> {
        // Path-encode the login to defend against odd characters (org renames,
        // etc.).
        let path = format!(
            "/user/memberships/orgs/{}",
            crate::util::path_segment(org_login)
        );
        tracing::debug!("calling GET /user/memberships/orgs/{{org_login}}");
        let req = self.auth_request(Method::Get, &path);
        let resp = self.transport.send(req).await?;
        if !(200..300).contains(&resp.status) {
            tracing::warn!(
                status = resp.status,
                "github GET /user/memberships/orgs returned non-2xx"
            );
        }
        resp.ensure_success()?.json()
    }

    /// `GET /user/installations` — installations of this GitHub App that the
    /// signed-in user can access. Used by setup-return handling to verify the
    /// untrusted `installation_id` query parameter from GitHub.
    #[tracing::instrument(skip(self), fields(method = "list_user_installations"))]
    pub async fn list_user_installations(&self) -> Result<GhUserInstallationList> {
        let req = self.auth_request(Method::Get, "/user/installations?per_page=100");
        let resp = self.transport.send(req).await?;
        if !(200..300).contains(&resp.status) {
            tracing::warn!(
                status = resp.status,
                "github GET /user/installations returned non-2xx"
            );
        }
        resp.ensure_success()?.json()
    }

    /// `GET /user/installations/{installation_id}/repositories` — repos the
    /// signed-in user can see through this app installation. Used by the
    /// console's link-create form to render a repo-picker.
    ///
    /// Paginates with `?per_page=100` (v1 simplification; installations with
    /// >100 repos need paging in v1.1).
    #[tracing::instrument(
        skip(self),
        fields(method = "list_user_installation_repos", installation_id)
    )]
    pub async fn list_user_installation_repos(
        &self,
        installation_id: u64,
    ) -> Result<GhInstallationRepos> {
        let path = format!("/user/installations/{installation_id}/repositories?per_page=100");
        let req = self.auth_request(Method::Get, &path);
        let resp = self.transport.send(req).await?;
        if !(200..300).contains(&resp.status) {
            tracing::warn!(
                status = resp.status,
                "github GET /user/installations/{installation_id}/repositories returned non-2xx"
            );
        }
        resp.ensure_success()?.json()
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
        assert!(
            a.url
                .starts_with("https://github.com/login/oauth/authorize?")
        );
        assert!(a.url.contains("client_id=Iv1.abc"));
        assert!(a.url.contains("scope=read%3Auser+read%3Aorg"));
        assert!(a.url.contains("state=csrf-token-123"));
        assert!(
            a.url
                .contains("redirect_uri=https%3A%2F%2Fexample.test%2Foauth%2Fcallback")
        );
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
                body: br#"{"access_token":"u_xxx","token_type":"bearer","scope":"read:user"}"#
                    .to_vec(),
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
        match &err {
            // The code survives so callers can still tell an expired code from
            // a declined authorization; the prose does not.
            Error::OAuth(code) => assert_eq!(code, "bad_verification_code"),
            other => panic!("expected OAuth error, got {other:?}"),
        }
        assert!(!format!("{err} {err:?}").contains("incorrect or expired"));
        mock.assert_exhausted();
    }

    /// An error payload whose `error` is prose rather than a documented code —
    /// or that smuggles a credential into that slot — must not be carried.
    #[tokio::test]
    async fn unrecognized_error_payload_is_reduced_to_a_placeholder() {
        let mock = MockTransport::scripted(vec![Expectation {
            method: Method::Post,
            url: "https://github.com/login/oauth/access_token".into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 200,
                headers: BTreeMap::new(),
                body: br#"{"error":"token ghs_16C7e42F292c6912E7710c838347Ae178B4a rejected"}"#
                    .to_vec(),
            },
        }]);
        let err = exchange_code(&mock, &cfg(), "any").await.unwrap_err();
        let rendered = format!("{err} {err:?}");
        assert!(!rendered.contains("ghs_"), "{rendered}");
        assert!(rendered.contains(crate::redact::UNRECOGNIZED), "{rendered}");
        mock.assert_exhausted();
    }

    /// The success-shape decode runs with the token payload in hand.
    #[tokio::test]
    async fn token_shape_mismatch_never_quotes_the_payload() {
        let token = "gho_16C7e42F292c6912E7710c838347Ae178B4a";
        let mock = MockTransport::scripted(vec![Expectation {
            method: Method::Post,
            url: "https://github.com/login/oauth/access_token".into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 200,
                headers: BTreeMap::new(),
                body: format!(r#"{{"access_token":"{token}","scope":42}}"#).into_bytes(),
            },
        }]);
        let err = exchange_code(&mock, &cfg(), "any").await.unwrap_err();
        let rendered = format!("{err} {err:?}");
        assert!(matches!(err, Error::Decode(_)), "{rendered}");
        assert!(!rendered.contains(token), "{rendered}");
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
        assert!(
            matches!(err, Error::Decode(_)),
            "expected Decode, got {err:?}"
        );
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
        assert!(
            matches!(err, Error::Decode(_)),
            "expected Decode, got {err:?}"
        );
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
            serde_json::json!({"role": "admin", "state": "active", "organization": {"id": 9001}}),
        )]);
        let m = client_with(mock)
            .get_org_membership("acme corp")
            .await
            .unwrap();
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
        let err = client_with(mock)
            .get_org_membership("private")
            .await
            .unwrap_err();
        assert_eq!(err.status(), Some(404));
    }

    #[tokio::test]
    async fn list_user_installations_decodes_response() {
        let mock = MockTransport::scripted(vec![Expectation {
            method: Method::Get,
            url: "https://api.github.test/user/installations?per_page=100".into(),
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
                body: br#"{
                    "total_count": 1,
                    "installations": [{
                        "id": 77,
                        "account": {"id": 9001, "login": "acme", "type": "Organization"},
                        "repository_selection": "selected",
                        "target_type": "Organization",
                        "target_id": 9001
                    }]
                }"#
                .to_vec(),
            },
        }]);
        let resp = client_with(mock).list_user_installations().await.unwrap();
        assert_eq!(resp.total_count, 1);
        assert_eq!(resp.installations[0].id, 77);
        assert_eq!(resp.installations[0].account.login, "acme");
    }

    #[tokio::test]
    async fn list_user_installation_repos_decodes_response() {
        let mock = MockTransport::scripted(vec![Expectation {
            method: Method::Get,
            url: "https://api.github.test/user/installations/77/repositories?per_page=100".into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 200,
                headers: BTreeMap::new(),
                body: br#"{
                    "total_count": 2,
                    "repositories": [
                        {"id": 10, "full_name": "acme/api", "private": false},
                        {"id": 11, "full_name": "acme/web", "private": true}
                    ]
                }"#
                .to_vec(),
            },
        }]);
        let resp = client_with(mock)
            .list_user_installation_repos(77)
            .await
            .unwrap();
        assert_eq!(resp.total_count, 2);
        assert_eq!(resp.repositories.len(), 2);
        assert_eq!(resp.repositories[0].full_name, "acme/api");
    }
}
