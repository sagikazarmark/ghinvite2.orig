//! Credential-scoped REST calls. Secrets stay inside opaque credentials.
use crate::{
    credential::Credential,
    error::{ApiError, ForbiddenReason},
    transport::{MAX_RESPONSE_BYTES, Method, Request, Response},
};
use std::sync::Arc;

pub use crate::credential::{CredentialKind, QuotaBucket};
pub use crate::transport::{HttpTransportDyn, TransportError};

mod listing;
pub use listing::{Listing, PageCompletion, PartialInfo, PartialRest};

/// GitHub's REST API origin.
pub const DEFAULT_API_BASE: &str = "https://api.github.com";

/// The REST API version every request sends unless its endpoint names another.
pub const DEFAULT_API_VERSION: &str = "2022-11-28";

/// Transport, API origin and user agent shared by credential-scoped calls.
#[derive(Clone)]
pub struct RestEnv {
    pub(crate) transport: Arc<dyn HttpTransportDyn>,
    pub(crate) api_base: url::Url,
    pub(crate) user_agent: String,
}

impl std::fmt::Debug for RestEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RestEnv")
            .field("api_base", &self.api_base.as_str())
            .field("user_agent", &self.user_agent)
            .finish_non_exhaustive()
    }
}

impl RestEnv {
    /// An environment for `api_base` (e.g. [`DEFAULT_API_BASE`], a local
    /// emulator or a GHES API). The base follows [`parse_base_url`]; the user
    /// agent must be 1–256 printable ASCII characters.
    /// Either failure is `Decode { what: "api_base" | "user_agent" }`.
    pub fn new(
        transport: Arc<dyn HttpTransportDyn>,
        api_base: &str,
        user_agent: impl Into<String>,
    ) -> Result<Self, ApiError> {
        let user_agent = parse_user_agent(user_agent.into())?;
        Ok(Self {
            transport,
            api_base: parse_api_base(api_base)?,
            user_agent,
        })
    }

    /// Replace the user agent, keeping the transport and API base. Same rules
    /// as [`RestEnv::new`].
    pub fn with_user_agent(mut self, user_agent: impl Into<String>) -> Result<Self, ApiError> {
        self.user_agent = parse_user_agent(user_agent.into())?;
        Ok(self)
    }

    /// Replace the API base, keeping the transport and user agent. Same rules
    /// as [`RestEnv::new`].
    pub fn with_api_base(mut self, base: &str) -> Result<Self, ApiError> {
        self.api_base = parse_api_base(base)?;
        Ok(self)
    }

    /// The API base requests are sent to.
    pub fn api_base(&self) -> &str {
        self.api_base.as_str()
    }

    /// The user agent every request sends.
    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }

    /// The transport, e.g. for credential-free requests such as avatars.
    pub fn transport(&self) -> &Arc<dyn HttpTransportDyn> {
        &self.transport
    }

    pub(crate) fn request(
        &self,
        method: Method,
        path: &str,
        credential: &dyn Credential,
        api_version: Option<&str>,
    ) -> Request {
        Request::new(
            method,
            format!("{}{}", self.api_base.as_str().trim_end_matches('/'), path),
        )
        .header("accept", "application/vnd.github+json")
        .header("user-agent", &self.user_agent)
        .header(
            "x-github-api-version",
            api_version.unwrap_or(DEFAULT_API_VERSION),
        )
        .header(
            "authorization",
            format!("Bearer {}", credential.authorization().value),
        )
    }

    pub(crate) async fn send(&self, request: Request) -> Result<Response, ApiError> {
        let response = self.transport.send(request).await.map_err(ApiError::from)?;
        if response.body.len() > MAX_RESPONSE_BYTES {
            return Err(ApiError::Decode { what: "oversize" });
        }
        Ok(response)
    }
}

fn parse_user_agent(user_agent: String) -> Result<String, ApiError> {
    if user_agent.is_empty()
        || user_agent.len() > 256
        || !user_agent
            .bytes()
            .all(|b| b == b' ' || b.is_ascii_graphic())
    {
        return Err(ApiError::Decode { what: "user_agent" });
    }
    Ok(user_agent)
}

fn parse_api_base(base: &str) -> Result<url::Url, ApiError> {
    parse_base_url(base).ok_or(ApiError::Decode { what: "api_base" })
}

/// A GitHub base URL (REST API, web or avatar origin): `https`, or `http` on
/// a loopback host (`localhost`, `127.0.0.0/8`, `::1`), with a host and no
/// credentials, query or fragment. The same rule as octoauth's
/// `AppBuilder::base_url` and octologin's base URLs, so every GitHub client
/// accepts the same bases.
pub fn parse_base_url(value: &str) -> Option<url::Url> {
    let url = url::Url::parse(value).ok()?;
    let host = url.host_str()?;
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
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    Some(url)
}

/// HTTP status and typed response body; no raw headers or bytes escape.
#[derive(Debug)]
pub struct ApiResponse<T> {
    /// HTTP status of the successful response.
    pub status: u16,
    /// Decoded body.
    pub body: T,
}

/// Path construction where a filled segment can never change the URL's
/// structure.
#[derive(Clone, Debug)]
pub struct PathTemplate {
    template: &'static str,
    segments: Vec<String>,
    valid: bool,
}

impl PathTemplate {
    /// Declare a path without queries, fragments or literal dot traversal.
    pub const fn new(template: &'static str) -> Self {
        let b = template.as_bytes();
        assert!(!b.is_empty() && b[0] == b'/');
        let mut i = 0;
        while i < b.len() {
            assert!(b[i] != b'?' && b[i] != b'#' && b[i] != b'%' && b[i] != b'\\');
            if i > 0 {
                assert!(b[i] != b'.' || b[i - 1] != b'.');
            }
            i += 1;
        }
        Self {
            template,
            segments: Vec::new(),
            valid: true,
        }
    }

    /// Fill the next placeholder, percent-encoding everything but unreserved
    /// characters. Empty and dot segments fail when the path is built.
    pub fn seg(mut self, s: &str) -> Self {
        if s.is_empty() || s == "." || s == ".." {
            self.valid = false;
        }
        let mut encoded = String::new();
        for b in s.bytes() {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'~') {
                encoded.push(b as char);
            } else {
                use std::fmt::Write;
                let _ = write!(encoded, "%{b:02X}");
            }
        }
        self.segments.push(encoded);
        self
    }

    pub(crate) fn build(&self) -> Result<String, ApiError> {
        if !self.valid {
            return Err(ApiError::Decode { what: "path" });
        }
        let mut path = self.template.to_owned();
        for segment in &self.segments {
            let Some(at) = path.find("{}") else {
                return Err(ApiError::Decode { what: "path" });
            };
            path.replace_range(at..at + 2, segment);
        }
        if path.contains(['{', '}'])
            || path.starts_with("//")
            || path.split('/').any(|part| part == ".")
            || path.bytes().any(|b| b.is_ascii_control() || b == b' ')
        {
            return Err(ApiError::Decode { what: "path" });
        }
        Ok(path)
    }
}

/// A borrowed REST client tied to its credential's lifetime.
pub struct Rest<'a> {
    pub(crate) env: &'a RestEnv,
    pub(crate) cred: &'a dyn Credential,
}

impl<'a> Rest<'a> {
    /// A client sending `cred` through `env`.
    pub fn new(env: &'a RestEnv, cred: &'a dyn Credential) -> Self {
        Self { env, cred }
    }

    pub(crate) fn request(&self, method: Method, path: &str, api_version: Option<&str>) -> Request {
        self.env.request(method, path, self.cred, api_version)
    }

    /// One HTTP attempt to `path`, decoding a 2xx body as `Resp` (an empty
    /// body decodes as JSON `null`). Retries belong to the caller.
    ///
    /// Repository-administration writes (repository PATCH/DELETE, rulesets,
    /// branch protection) are refused before dispatch with
    /// `Forbidden { reason: Disabled }`.
    pub async fn call<Req: serde::Serialize + Sync, Resp: serde::de::DeserializeOwned>(
        &self,
        method: Method,
        path: PathTemplate,
        query: &[(&str, &str)],
        body: Option<&Req>,
    ) -> Result<ApiResponse<Resp>, ApiError> {
        self.call_with_version(method, path, query, body, None)
            .await
    }

    /// Endpoint metadata selects the version; generic calls keep the default.
    pub(crate) async fn call_with_version<
        Req: serde::Serialize + Sync,
        Resp: serde::de::DeserializeOwned,
    >(
        &self,
        method: Method,
        path: PathTemplate,
        query: &[(&str, &str)],
        body: Option<&Req>,
        api_version: Option<&str>,
    ) -> Result<ApiResponse<Resp>, ApiError> {
        let built = path.build()?;
        if repo_admin_path(path.template, method) || repo_admin_path(&built, method) {
            return Err(ApiError::Forbidden {
                reason: ForbiddenReason::Disabled,
            });
        }
        let mut request = self.request(method, &built, api_version);
        if !query.is_empty() {
            let mut url =
                url::Url::parse(&request.url).map_err(|_| ApiError::Decode { what: "path" })?;
            url.query_pairs_mut().extend_pairs(query.iter().copied());
            request.url = url.into();
        }
        if let Some(body) = body {
            request = request.json_body(body)?;
        }
        let response = self.env.send(request).await?;
        if !(200..300).contains(&response.status) {
            return Err(ApiError::from_response(&response));
        }
        if let Some(partial) = PartialInfo::from_response(&response) {
            return Err(ApiError::Partial {
                reason: partial.reason,
                org_ids: partial.org_ids,
            });
        }
        let decoded = serde_json::from_slice(if response.body.is_empty() {
            b"null"
        } else {
            &response.body
        })
        .map_err(|_| ApiError::Decode { what: "shape" })?;
        Ok(ApiResponse {
            status: response.status,
            body: decoded,
        })
    }

    /// The credential class, checked by endpoint metadata.
    pub fn credential_kind(&self) -> CredentialKind {
        self.cred.kind()
    }

    /// Quota identity; contains no credential material.
    pub fn bucket(&self) -> QuotaBucket {
        self.cred.bucket()
    }
}

/// Repository-administration writes this crate never sends.
fn repo_admin_path(path: &str, method: Method) -> bool {
    if matches!(method, Method::Get) {
        return false;
    }
    let parts: Vec<_> = path.split('/').filter(|s| !s.is_empty()).collect();
    (parts.len() == 3 && parts[0] == "repos" && matches!(method, Method::Patch | Method::Delete))
        || (parts.len() >= 4 && parts[0] == "repos" && parts[3] == "rulesets")
        || (parts.len() >= 3 && parts[0] == "orgs" && parts[2] == "rulesets")
        || (parts.len() >= 6
            && parts[0] == "repos"
            && parts[3] == "branches"
            && parts[5] == "protection"
            && matches!(method, Method::Put | Method::Delete))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mocks::{self, Expectation, MockTransport};

    #[test]
    fn segments_cannot_change_origin_or_path_structure() {
        assert_eq!(
            PathTemplate::new("/repos/{}/{}")
                .seg("o/name")
                .seg("r.%")
                .build()
                .unwrap(),
            "/repos/o%2Fname/r%2E%25"
        );
        for segment in ["", ".", ".."] {
            assert!(PathTemplate::new("/repos/{}").seg(segment).build().is_err());
        }
        assert_eq!(
            PathTemplate::new("/{}")
                .seg("//evil.test/#?")
                .build()
                .unwrap(),
            "/%2F%2Fevil%2Etest%2F%23%3F"
        );
        assert!(PathTemplate::new("/./repos/o/r").build().is_err());
        assert!(PathTemplate::new("//evil.test").build().is_err());
        assert!(PathTemplate::new("/repos/{}").build().is_err());
        assert!(PathTemplate::new("/repos").seg("extra").build().is_err());
    }

    #[test]
    fn environment_rejects_unsafe_bases_and_user_agents() {
        let transport: Arc<dyn HttpTransportDyn> = Arc::new(MockTransport::scripted(vec![]));
        for base in [
            "ftp://api.github.test",
            "https://user:pass@api.github.test",
            "https://api.github.test/?q=1",
            "https://api.github.test/#f",
            "http://api.github.test",
            "http://10.0.0.1:3001",
            "not a url",
        ] {
            assert_eq!(
                RestEnv::new(transport.clone(), base, "ua").unwrap_err(),
                ApiError::Decode { what: "api_base" },
                "{base}"
            );
        }
        for agent in ["", "line\nbreak", &"x".repeat(257)] {
            assert_eq!(
                RestEnv::new(transport.clone(), DEFAULT_API_BASE, agent).unwrap_err(),
                ApiError::Decode { what: "user_agent" }
            );
        }
        let env = RestEnv::new(transport, DEFAULT_API_BASE, "ghinvite").unwrap();
        assert_eq!(env.api_base(), "https://api.github.com/");
        assert_eq!(env.user_agent(), "ghinvite");
        let env = env.with_api_base("http://127.0.0.1:3001").unwrap();
        assert_eq!(env.api_base(), "http://127.0.0.1:3001/");
        for loopback in ["http://localhost:1", "http://[::1]:2/api/v3/"] {
            assert!(parse_base_url(loopback).is_some(), "{loopback}");
        }
    }

    #[tokio::test]
    async fn generic_call_sends_credential_headers_query_and_body_once() {
        let mock = MockTransport::scripted(vec![
            Expectation::ok_json(
                Method::Post,
                "https://api.github.test/orgs/acme/things?x=a+b",
                serde_json::json!({"id":1}),
            )
            .require_header("authorization", "Bearer rest-secret")
            .require_header("accept", "application/vnd.github+json")
            .require_header("user-agent", "rest-contract")
            .require_header("x-github-api-version", DEFAULT_API_VERSION)
            .require_header("content-type", "application/json")
            .require_body(br#"{"k":1}"#.to_vec()),
        ]);
        let env = mocks::env(&mock, "rest-contract");
        let cred = mocks::installation_credential(1, 2, "rest-secret");
        let rest = Rest::new(&env, &cred);
        assert_eq!(rest.credential_kind(), CredentialKind::Installation);
        let response: ApiResponse<serde_json::Value> = rest
            .call(
                Method::Post,
                PathTemplate::new("/orgs/{}/things").seg("acme"),
                &[("x", "a b")],
                Some(&serde_json::json!({"k":1})),
            )
            .await
            .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body["id"], 1);
        mock.assert_exhausted();
    }

    #[tokio::test]
    async fn generic_call_refuses_repository_administration_before_dispatch() {
        let mock = MockTransport::scripted(vec![]);
        let env = mocks::env(&mock, "rest-contract");
        let cred = mocks::installation_credential(1, 2, "rest-secret");
        let rest = Rest::new(&env, &cred);
        for (method, path) in [
            (Method::Delete, PathTemplate::new("/repos/{}/{}")),
            (Method::Patch, PathTemplate::new("/repos/{}/{}")),
            (Method::Post, PathTemplate::new("/repos/{}/{}/rulesets")),
            (
                Method::Put,
                PathTemplate::new("/repos/{}/{}/branches/main/protection"),
            ),
        ] {
            let result = rest
                .call::<(), serde_json::Value>(method, path.seg("o").seg("r"), &[], None)
                .await;
            assert!(matches!(
                result,
                Err(ApiError::Forbidden {
                    reason: ForbiddenReason::Disabled
                })
            ));
        }
        assert!(matches!(
            rest.call::<(), serde_json::Value>(
                Method::Post,
                PathTemplate::new("/orgs/{}/rulesets").seg("o"),
                &[],
                None
            )
            .await,
            Err(ApiError::Forbidden {
                reason: ForbiddenReason::Disabled
            })
        ));
        mock.assert_exhausted();
    }

    #[tokio::test]
    async fn non_success_statuses_are_classified_and_bodies_never_decoded() {
        let mut moved = Expectation::ok_json(
            Method::Get,
            "https://api.github.test/repos/o/r/collaborators",
            serde_json::json!({"private":"PRIVATE_BODY"}),
        );
        moved.response.status = 302;
        moved
            .response
            .headers
            .insert("location".into(), "https://private.test/".into());
        let mock = MockTransport::scripted(vec![moved]);
        let env = mocks::env(&mock, "rest-contract");
        let cred = mocks::installation_credential(1, 2, "rest-secret");
        let error = Rest::new(&env, &cred)
            .call::<(), serde_json::Value>(
                Method::Get,
                PathTemplate::new("/repos/{}/{}/collaborators")
                    .seg("o")
                    .seg("r"),
                &[],
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(error, ApiError::Moved { status: 302 });
        assert!(!format!("{error} {error:?}").contains("private"));
        mock.assert_exhausted();
    }
}
