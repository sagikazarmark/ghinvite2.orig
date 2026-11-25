//! [`SignIn`]: one GitHub App's OAuth web flow for signing users in.

use crate::authorize::SECRET_LEN;
use crate::transport::{self, Response};
use crate::user::{User, UserId, UserToken, VerifiedEmail};
use crate::{AuthorizeOptions, AuthorizeUrl, OAuthErrorCode, PendingAuthorization, SignInError};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use octoauth::ConfigError;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderValue, LINK, USER_AGENT};
use reqwest::{Method, Url};
use sha2::{Digest, Sha256};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use zeroize::Zeroizing;

/// GitHub's web origin, where the OAuth endpoints live.
pub const DEFAULT_WEB_BASE_URL: &str = "https://github.com";

/// The `User-Agent` sent unless configured otherwise.
pub const DEFAULT_USER_AGENT: &str = concat!("octologin/", env!("CARGO_PKG_VERSION"));

/// Longest access token accepted from the exchange.
const MAX_TOKEN_BYTES: usize = 8 * 1024;

/// Longest authorization code accepted from a callback. GitHub's are 20
/// characters; the bound only stops abuse.
const MAX_CODE_BYTES: usize = 1024;

/// Longest PKCE verifier RFC 7636 allows.
const MAX_VERIFIER_BYTES: usize = 128;

/// Largest error body parsed for an OAuth `error` code.
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

/// One GitHub App's OAuth web flow for signing users in: redirect to GitHub
/// ([`authorize`](Self::authorize)), exchange the callback's code
/// ([`exchange`](Self::exchange)), read who signed in ([`user`](Self::user),
/// [`emails`](Self::emails)), and [`revoke`](Self::revoke) the token.
///
/// Uses PKCE (S256) and an unpredictable `state`, and requests no scopes (a
/// GitHub App's user token carries the app's permissions, never scopes).
pub struct SignIn {
    client_id: String,
    client_secret: Zeroizing<String>,
    redirect_uri: Url,
    web_base: Url,
    api_base: Url,
    user_agent: HeaderValue,
    client: reqwest::Client,
}

impl SignIn {
    /// Configure sign-in for the app with `client_id` and `client_secret`,
    /// whose callback URL is `redirect_uri`.
    pub fn builder(
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        redirect_uri: impl Into<String>,
    ) -> SignInBuilder {
        SignInBuilder {
            client_id: client_id.into(),
            client_secret: Zeroizing::new(client_secret.into()),
            redirect_uri: redirect_uri.into(),
            web_base: None,
            api_base: None,
            user_agent: None,
            client: None,
        }
    }

    /// The app's client ID.
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// Start a sign-in: the URL to redirect the browser to, and the pending
    /// authorization to keep server side until the callback.
    pub fn authorize(
        &self,
        options: AuthorizeOptions,
    ) -> Result<(AuthorizeUrl, PendingAuthorization), SignInError> {
        let state = random_secret()?;
        let pkce_verifier = random_secret()?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(pkce_verifier.as_bytes()));
        let mut url = endpoint(&self.web_base, &["login", "oauth", "authorize"]);
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("client_id", &self.client_id)
                .append_pair("redirect_uri", self.redirect_uri.as_str())
                .append_pair("state", &state)
                .append_pair("code_challenge", &challenge)
                .append_pair("code_challenge_method", "S256");
            if let Some(allow) = options.allow_signup {
                query.append_pair("allow_signup", if allow { "true" } else { "false" });
            }
            if let Some(login) = &options.login {
                query.append_pair("login", login);
            }
        }
        Ok((
            AuthorizeUrl(url.into()),
            PendingAuthorization {
                state: state.to_string(),
                pkce_verifier,
            },
        ))
    }

    /// Exchange the callback's `code` for a user token, proving the PKCE
    /// verifier from [`PendingAuthorization::verify`].
    ///
    /// Prefer [`with_user_token`](Self::with_user_token), which also revokes
    /// the token; with this, [`revoke`](Self::revoke) it on every path.
    ///
    /// An `error` in GitHub's answer is [`SignInError::Provider`], whatever
    /// the status; the answer's `error_description` and any refresh token
    /// are discarded.
    pub async fn exchange(
        &self,
        code: &str,
        pkce_verifier: &str,
    ) -> Result<UserToken, SignInError> {
        if code.is_empty()
            || code.len() > MAX_CODE_BYTES
            || !code.bytes().all(|b| b.is_ascii_graphic())
            || !(SECRET_LEN..=MAX_VERIFIER_BYTES).contains(&pkce_verifier.len())
            || !pkce_verifier
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
        {
            return Err(SignInError::InvalidInput);
        }
        let pairs = [
            ("client_id", self.client_id.as_str()),
            ("client_secret", self.client_secret.as_str()),
            ("code", code),
            ("code_verifier", pkce_verifier),
            ("redirect_uri", self.redirect_uri.as_str()),
        ];
        // Room for every value percent-encoded (at most three bytes per
        // byte), so the buffer holding the secret never reallocates.
        let capacity = pairs
            .iter()
            .map(|(name, value)| name.len() + 2 + 3 * value.len())
            .sum();
        // The serializer is not `Send`: drop it here, not across the await.
        let body = {
            let mut form = url::form_urlencoded::Serializer::new(String::with_capacity(capacity));
            form.extend_pairs(pairs);
            Zeroizing::new(form.finish().into_bytes())
        };

        let url = endpoint(&self.web_base, &["login", "oauth", "access_token"]);
        let mut request = reqwest::Request::new(Method::POST, url);
        let headers = request.headers_mut();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        headers.insert(USER_AGENT, self.user_agent.clone());
        *request.body_mut() = Some(secret_body(body));
        let response = transport::send(&self.client, request).await?;

        #[derive(serde::Deserialize)]
        struct Answer {
            access_token: Option<Secret>,
            token_type: Option<String>,
            error: Option<String>,
        }
        let answer = if response.body.len() <= MAX_ERROR_BODY_BYTES || response.status.is_success()
        {
            serde_json::from_slice::<Answer>(&response.body).map_err(|_| ())
        } else {
            Err(())
        };
        if let Ok(Answer {
            error: Some(code), ..
        }) = &answer
        {
            return Err(SignInError::Provider {
                code: OAuthErrorCode::from_github(code),
            });
        }
        if !response.status.is_success() {
            return Err(response.api_error());
        }
        let answer = answer.map_err(|_| SignInError::Decode)?;
        let token = answer
            .access_token
            .map(|token| token.0)
            .filter(|token| {
                !token.is_empty()
                    && token.len() <= MAX_TOKEN_BYTES
                    && token.bytes().all(|b| b.is_ascii_graphic())
            })
            .ok_or(SignInError::Decode)?;
        if !answer
            .token_type
            .is_some_and(|kind| kind.eq_ignore_ascii_case("bearer"))
        {
            return Err(SignInError::Decode);
        }
        Ok(UserToken::new(token, self.client_id.clone()))
    }

    /// Who signed in (`GET /user`).
    pub async fn user(&self, token: &UserToken) -> Result<User, SignInError> {
        #[derive(serde::Deserialize)]
        struct Wire {
            id: u64,
            login: String,
            avatar_url: Option<String>,
            created_at: Option<String>,
        }
        let response = self.get(&["user"], token, |_| {}).await?;
        if response.status != http::StatusCode::OK {
            return Err(response.api_error());
        }
        let user: Wire = serde_json::from_slice(&response.body).map_err(|_| SignInError::Decode)?;
        if user.id == 0 || user.login.is_empty() {
            return Err(SignInError::Decode);
        }
        Ok(User {
            id: UserId::new(user.id),
            login: user.login,
            avatar_url: user.avatar_url,
            created_at: user.created_at.as_deref().and_then(system_time),
        })
    }

    /// The signed-in user's **verified** email addresses (`GET /user/emails`);
    /// unverified ones prove nothing and are left out. Fails with
    /// [`SignInError::Incomplete`] rather than return part of a list longer
    /// than one page (100 addresses).
    pub async fn emails(&self, token: &UserToken) -> Result<Vec<VerifiedEmail>, SignInError> {
        let response = self
            .get(&["user", "emails"], token, |url| {
                url.query_pairs_mut().append_pair("per_page", "100");
            })
            .await?;
        if response.status != http::StatusCode::OK {
            return Err(response.api_error());
        }
        if has_next_page(&response) {
            return Err(SignInError::Incomplete);
        }
        #[derive(serde::Deserialize)]
        struct Wire {
            email: String,
            primary: bool,
            verified: bool,
        }
        let emails: Vec<Wire> =
            serde_json::from_slice(&response.body).map_err(|_| SignInError::Decode)?;
        Ok(emails
            .into_iter()
            .filter(|email| email.verified)
            .map(|email| VerifiedEmail {
                email: email.email,
                primary: email.primary,
            })
            .collect())
    }

    /// Revoke the token (`DELETE /applications/{client_id}/token`), as the
    /// app. Only the app that obtained a token can revoke it: a token from
    /// another client ID is refused without sending anything.
    pub async fn revoke(&self, token: UserToken) -> Result<(), SignInError> {
        if token.client_id() != self.client_id {
            return Err(SignInError::ForeignToken);
        }
        #[derive(serde::Serialize)]
        struct Body<'a> {
            access_token: &'a str,
        }
        let body = Zeroizing::new(
            serde_json::to_vec(&Body {
                access_token: token.expose_secret(),
            })
            .expect("a string field serializes"),
        );
        let credentials = concat(&[&self.client_id, ":", &self.client_secret]);
        let mut basic = Zeroizing::new(String::with_capacity(
            "Basic ".len() + credentials.len().div_ceil(3) * 4,
        ));
        basic.push_str("Basic ");
        STANDARD.encode_string(credentials.as_bytes(), &mut basic);
        let url = endpoint(&self.api_base, &["applications", &self.client_id, "token"]);
        let mut request = reqwest::Request::new(Method::DELETE, url);
        let headers = request.headers_mut();
        self.api_headers(headers);
        headers.insert(AUTHORIZATION, transport::authorization(&basic));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        *request.body_mut() = Some(secret_body(body));
        let response = transport::send(&self.client, request).await?;
        if response.status == http::StatusCode::NO_CONTENT {
            Ok(())
        } else {
            Err(response.api_error())
        }
    }

    /// A REST API `GET` with `token`; `query` adds query parameters.
    async fn get(
        &self,
        segments: &[&str],
        token: &UserToken,
        query: impl FnOnce(&mut Url),
    ) -> Result<Response, SignInError> {
        let mut url = endpoint(&self.api_base, segments);
        query(&mut url);
        let mut request = reqwest::Request::new(Method::GET, url);
        let headers = request.headers_mut();
        self.api_headers(headers);
        let bearer = concat(&["Bearer ", token.expose_secret()]);
        headers.insert(AUTHORIZATION, transport::authorization(&bearer));
        transport::send(&self.client, request).await
    }

    fn api_headers(&self, headers: &mut reqwest::header::HeaderMap) {
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            "x-github-api-version",
            HeaderValue::from_static(octoauth::DEFAULT_API_VERSION),
        );
        headers.insert(USER_AGENT, self.user_agent.clone());
    }
}

impl std::fmt::Debug for SignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignIn")
            .field("client_id", &self.client_id)
            .field("redirect_uri", &self.redirect_uri.as_str())
            .finish_non_exhaustive()
    }
}

/// Builder for [`SignIn`], from [`SignIn::builder`]. Setters only record
/// values; [`build`](Self::build) validates them.
#[must_use]
pub struct SignInBuilder {
    client_id: String,
    client_secret: Zeroizing<String>,
    redirect_uri: String,
    web_base: Option<String>,
    api_base: Option<String>,
    user_agent: Option<String>,
    client: Option<reqwest::Client>,
}

impl SignInBuilder {
    /// Another web origin than `https://github.com`, e.g. a test server.
    pub fn web_base_url(mut self, url: impl Into<String>) -> Self {
        self.web_base = Some(url.into());
        self
    }

    /// Another REST API base than `https://api.github.com`, e.g. a test server.
    pub fn api_base_url(mut self, url: impl Into<String>) -> Self {
        self.api_base = Some(url.into());
        self
    }

    /// The `User-Agent` to send. Default: `octologin/<version>`.
    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = Some(user_agent.into());
        self
    }

    /// Send through this client. Configure it with
    /// `reqwest::redirect::Policy::none()`: a redirected response is
    /// discarded as [`SignInError::Redirected`], but only after the request
    /// was re-sent.
    pub fn client(mut self, client: reqwest::Client) -> Self {
        self.client = Some(client);
        self
    }

    /// Validate and build. Errors name the field, never the value:
    ///
    /// | Field | Reason |
    /// |---|---|
    /// | `client_id` | `identifier` |
    /// | `client_secret` | `printable_ascii` |
    /// | `redirect_uri`, `web_base_url`, `api_base_url` | `url` |
    /// | `user_agent` | `printable_ascii` |
    /// | `client` | `tls` (the default client could not initialise TLS) |
    ///
    /// URLs must be `https`, or `http` on a loopback host, without
    /// credentials or a fragment; the base URLs also without a query.
    pub fn build(self) -> Result<SignIn, ConfigError> {
        if self.client_id.is_empty()
            || self.client_id.len() > 255
            || !self
                .client_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        {
            return Err(ConfigError::new("client_id", "identifier"));
        }
        if self.client_secret.is_empty()
            || self.client_secret.len() > 1024
            || !self.client_secret.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(ConfigError::new("client_secret", "printable_ascii"));
        }
        let redirect_uri = url(&self.redirect_uri, "redirect_uri", true)?;
        let web_base = url(
            self.web_base.as_deref().unwrap_or(DEFAULT_WEB_BASE_URL),
            "web_base_url",
            false,
        )?;
        let api_base = url(
            self.api_base
                .as_deref()
                .unwrap_or(octoauth::DEFAULT_BASE_URL),
            "api_base_url",
            false,
        )?;
        let user_agent = self.user_agent.as_deref().unwrap_or(DEFAULT_USER_AGENT);
        let user_agent = (!user_agent.is_empty()
            && user_agent.len() <= 256
            && user_agent.bytes().all(|b| (0x20..=0x7e).contains(&b)))
        .then(|| HeaderValue::from_str(user_agent).ok())
        .flatten()
        .ok_or(ConfigError::new("user_agent", "printable_ascii"))?;
        let client = match self.client {
            Some(client) => client,
            None => transport::default_client().ok_or(ConfigError::new("client", "tls"))?,
        };
        Ok(SignIn {
            client_id: self.client_id,
            client_secret: self.client_secret,
            redirect_uri,
            web_base,
            api_base,
            user_agent,
            client,
        })
    }
}

impl std::fmt::Debug for SignInBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignInBuilder")
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

/// A deserialized string moved straight into zeroizing storage.
struct Secret(Zeroizing<String>);

impl<'de> serde::Deserialize<'de> for Secret {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(|s| Self(Zeroizing::new(s)))
    }
}

/// `parts` joined into one zeroizing string, allocated once.
fn concat(parts: &[&str]) -> Zeroizing<String> {
    let mut joined = Zeroizing::new(String::with_capacity(parts.iter().map(|p| p.len()).sum()));
    for part in parts {
        joined.push_str(part);
    }
    joined
}

/// 32 random bytes, base64url: an OAuth `state` or PKCE verifier.
fn random_secret() -> Result<Zeroizing<String>, SignInError> {
    let mut bytes = Zeroizing::new([0u8; 32]);
    getrandom_02::getrandom(&mut bytes[..]).map_err(|_| SignInError::Randomness)?;
    Ok(Zeroizing::new(URL_SAFE_NO_PAD.encode(&bytes[..])))
}

/// A request body that is zeroized however the request ends.
fn secret_body(body: Zeroizing<Vec<u8>>) -> reqwest::Body {
    bytes::Bytes::from_owner(body).into()
}

/// `base/<segments>`, each segment percent-encoded.
fn endpoint(base: &Url, segments: &[&str]) -> Url {
    let mut url = base.clone();
    url.path_segments_mut()
        .expect("validated base URLs have a host")
        .pop_if_empty()
        .extend(segments);
    url
}

/// Whether the `Link` header names a next page.
fn has_next_page(response: &Response) -> bool {
    response.headers.get_all(LINK).iter().any(|value| {
        value.to_str().map_or(true, |value| {
            value.split(',').any(|link| {
                link.split(';').skip(1).any(|param| {
                    param
                        .trim()
                        .split_once('=')
                        .filter(|(name, _)| name.trim().eq_ignore_ascii_case("rel"))
                        .is_some_and(|(_, rels)| {
                            rels.trim()
                                .trim_matches('"')
                                .split_ascii_whitespace()
                                .any(|rel| rel.eq_ignore_ascii_case("next"))
                        })
                })
            })
        })
    })
}

/// An RFC 3339 timestamp as a `SystemTime`, if it parses and is after 1970.
fn system_time(value: &str) -> Option<SystemTime> {
    let seconds = chrono::DateTime::parse_from_rfc3339(value)
        .ok()?
        .timestamp();
    Some(UNIX_EPOCH + Duration::from_secs(u64::try_from(seconds).ok()?))
}

/// Validate a URL: `https`, or `http` on a loopback host; a host; no
/// credentials or fragment; for a base (`!callback`) no query either.
fn url(value: &str, field: &'static str, callback: bool) -> Result<Url, ConfigError> {
    let invalid = || ConfigError::new(field, "url");
    let url = Url::parse(value).map_err(|_| invalid())?;
    let host = url.host_str().ok_or_else(invalid)?;
    let loopback = host == "localhost"
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    let scheme_ok = url.scheme() == "https" || (url.scheme() == "http" && loopback);
    if !scheme_ok
        || url.cannot_be_a_base()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || (!callback && url.query().is_some())
    {
        return Err(invalid());
    }
    Ok(url)
}
