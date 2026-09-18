//! HTTP transport seam. Every GitHub call funnels through this trait so the
//! reqwest impl can be swapped for `MockTransport` in tests.

use crate::error::{Error, RateLimit, RateLimitScope, Result};
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Delete,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Request {
    pub method: Method,
    /// Absolute URL.
    pub url: String,
    /// Sorted by key for deterministic equality in tests.
    pub headers: BTreeMap<String, String>,
    /// `None` for GET/DELETE.
    pub body: Option<Vec<u8>>,
}

impl Request {
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: BTreeMap::new(),
            body: None,
        }
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.insert(name.to_ascii_lowercase(), value.into());
        self
    }

    pub fn json_body<T: serde::Serialize>(mut self, value: &T) -> Result<Self> {
        let body = serde_json::to_vec(value)
            .map_err(|e| Error::Decode(format!("encoding request body: {e}")))?;
        self.body = Some(body);
        self.headers
            .insert("content-type".into(), "application/json".into());
        Ok(self)
    }
}

#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    /// Lowercased keys, matching the convention in `Request::headers`.
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

impl Response {
    /// Decode the body as JSON of `T`. Errors with [`Error::Decode`] on parse
    /// failure (the raw body is included in the error message).
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_slice(&self.body).map_err(|e| {
            let preview = String::from_utf8_lossy(&self.body);
            Error::Decode(format!("{e}: body={preview}"))
        })
    }

    /// Returns `Ok(self)` if `status` is 2xx, otherwise `Err(Error::Status)`.
    pub fn ensure_success(self) -> Result<Self> {
        if (200..300).contains(&self.status) {
            Ok(self)
        } else {
            Err(self.status_error())
        }
    }

    /// Build the error for a non-2xx response, applying the same body
    /// truncation as [`ensure_success`]. Callers that match on status manually
    /// (e.g. to special-case 201 vs 204) use this for the unexpected-status arm
    /// so error logs stay consistent across the crate.
    ///
    /// A response carrying [rate-limit evidence](Response::rate_limit) becomes
    /// [`Error::RateLimited`], so throttling is classified once here and no
    /// caller has to re-read headers to tell a throttled 403 from a
    /// permission-denied one.
    pub fn status_error(&self) -> Error {
        // Truncate to keep error logs sane. Slicing bytes (not the lossy String)
        // avoids any chance of mid-codepoint panic.
        let truncated = if self.body.len() > 4096 {
            format!("{}…", String::from_utf8_lossy(&self.body[..4096]))
        } else {
            String::from_utf8_lossy(&self.body).to_string()
        };
        match self.rate_limit() {
            Some(rate_limit) => Error::RateLimited {
                status: self.status,
                body: truncated,
                rate_limit,
            },
            None => Error::Status {
                status: self.status,
                body: truncated,
            },
        }
    }

    /// GitHub returns the remaining quota in `x-ratelimit-remaining`. Returns
    /// `None` if absent or unparseable. Plan 3's reconciler should back off
    /// before this hits zero.
    pub fn rate_limit_remaining(&self) -> Option<u32> {
        self.headers
            .get("x-ratelimit-remaining")
            .and_then(|v| v.parse().ok())
    }

    /// On 429 / 403-secondary-rate-limit responses, GitHub sends `Retry-After`
    /// as a non-negative integer of seconds. We don't parse the HTTP-date form
    /// (rare; GitHub sends seconds in practice).
    pub fn retry_after(&self) -> Option<Duration> {
        self.headers
            .get("retry-after")
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_secs)
    }

    /// How long until the primary quota resets, measured as `x-ratelimit-reset`
    /// minus the response's own `date`. Reading both ends off GitHub's clock
    /// keeps a skewed local one from shortening the wait. `None` if either
    /// header is absent or unparseable, or if the reset has already passed.
    pub fn rate_limit_reset_after(&self) -> Option<Duration> {
        let reset: i64 = self.headers.get("x-ratelimit-reset")?.parse().ok()?;
        let sent = chrono::DateTime::parse_from_rfc2822(self.headers.get("date")?)
            .ok()?
            .timestamp();
        u64::try_from(reset.checked_sub(sent)?)
            .ok()
            .map(Duration::from_secs)
    }

    /// The documented rate-limit evidence on this response, or `None` when there
    /// is none and the status is therefore GitHub's answer to the request.
    ///
    /// GitHub reports throttling as 403 or 429 and names the limit one of three
    /// documented ways: `retry-after` for a secondary limit, the rate-limit
    /// wording in the response body for either limit, and
    /// `x-ratelimit-remaining: 0` for the exhausted primary quota. The first two
    /// address this request. The quota headers do not — they ride along on every
    /// response, so a permission refusal served as the hour's last request also
    /// reports a remaining quota of zero. An exhausted quota therefore only
    /// counts where GitHub named no other reason, and a 403 with no evidence at
    /// all is a permission refusal. A 429 is a secondary limit by the definition
    /// of the status.
    ///
    /// Only a response classifies. A transport failure never reaches here, so a
    /// request whose effect is unknown is never mistaken for a refused one.
    pub fn rate_limit(&self) -> Option<RateLimit> {
        if self.status != 403 && self.status != 429 {
            return None;
        }
        let retry_after = self.retry_after();
        let quota_exhausted = self.rate_limit_remaining() == Some(0);
        let named_limit = self.body_names_rate_limit();
        if !(retry_after.is_some()
            || self.status == 429
            || named_limit == Some(true)
            || (quota_exhausted && named_limit.is_none()))
        {
            return None;
        }
        Some(RateLimit {
            scope: if quota_exhausted {
                RateLimitScope::Primary
            } else {
                RateLimitScope::Secondary
            },
            // `retry-after` is the more specific instruction when GitHub sends
            // both; the reset stands in for an exhausted quota that sent none.
            retry_after: retry_after.or_else(|| self.rate_limit_reset_after()),
        })
    }

    /// Whether the reason named in the body is a rate limit — the only evidence
    /// a secondary limit leaves when it sends no headers. `None` when the body
    /// names no reason at all, so it neither confirms nor contradicts them.
    fn body_names_rate_limit(&self) -> Option<bool> {
        let body: serde_json::Value = serde_json::from_slice(&self.body).ok()?;
        let message = body.get("message")?.as_str()?.to_ascii_lowercase();
        Some(message.contains("rate limit") || message.contains("abuse detection"))
    }
}

/// The single HTTP-touching trait this crate exposes.
///
/// Impls must be `Send + Sync + 'static` so they can be cloned into long-lived
/// clients held by Restate handlers.
#[async_trait]
pub trait HttpTransport: Send + Sync + 'static {
    async fn send(&self, request: Request) -> Result<Response>;
}

/// Production reqwest impl. Lives behind a thin wrapper so the rest of the
/// crate doesn't have to deal with reqwest types directly.
///
/// On native targets the reqwest future is naturally `Send`. On
/// `wasm32-unknown-unknown` (Cloudflare Workers) reqwest dispatches to
/// `fetch` and its future wraps `JsFuture`, which is `!Send`. We wrap that
/// future in [`WasmSendFut`] so the resulting `async fn` body is `Send`,
/// which is sound: wasm32-unknown-unknown has no OS threads, so values
/// never cross thread boundaries. This lets the same `ReqwestTransport`
/// satisfy `HttpTransport: Send + Sync + 'static` on both targets.
#[derive(Clone, Debug)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    pub fn new() -> Result<Self> {
        // Wasm applies the same deadline per request below. Worker execution
        // limits do not bound wall-clock time spent waiting for upstream I/O.
        let builder = reqwest::Client::builder();
        #[cfg(not(target_arch = "wasm32"))]
        let builder = builder.timeout(std::time::Duration::from_secs(30));
        let client = builder
            .build()
            .map_err(|e| Error::Transport(format!("building reqwest client: {e}")))?;
        Ok(Self { client })
    }

    /// Inject an existing client (e.g. one with a custom timeout middleware).
    pub fn with_client(client: reqwest::Client) -> Self {
        Self { client }
    }
}

/// Wraps a `!Send` future and asserts `Send`. See module docs above for the
/// soundness argument under wasm32-unknown-unknown's single-threaded model.
#[cfg(target_arch = "wasm32")]
struct WasmSendFut<F>(F);

#[cfg(target_arch = "wasm32")]
unsafe impl<F> Send for WasmSendFut<F> {}

#[cfg(target_arch = "wasm32")]
impl<F: std::future::Future> std::future::Future for WasmSendFut<F> {
    type Output = F::Output;
    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        // SAFETY: structural pin projection to the only field.
        unsafe { self.map_unchecked_mut(|s| &mut s.0) }.poll(cx)
    }
}

#[cfg(target_arch = "wasm32")]
fn wasm_send<F: std::future::Future>(f: F) -> WasmSendFut<F> {
    WasmSendFut(f)
}

#[cfg(not(target_arch = "wasm32"))]
fn wasm_send<F: std::future::Future>(f: F) -> F {
    f
}

#[async_trait]
impl HttpTransport for ReqwestTransport {
    async fn send(&self, request: Request) -> Result<Response> {
        wasm_send(async move {
            let method = match request.method {
                Method::Get => reqwest::Method::GET,
                Method::Post => reqwest::Method::POST,
                Method::Put => reqwest::Method::PUT,
                Method::Delete => reqwest::Method::DELETE,
            };
            let mut builder = self.client.request(method, &request.url);
            // reqwest's Wasm AbortGuard retains this timer through bytes(),
            // bounding headers and body together and cancelling fetch on drop.
            #[cfg(target_arch = "wasm32")]
            {
                builder = builder.timeout(std::time::Duration::from_secs(30));
            }
            for (k, v) in &request.headers {
                builder = builder.header(k, v);
            }
            if let Some(body) = request.body {
                builder = builder.body(body);
            }
            let resp = builder
                .send()
                .await
                .map_err(|e| Error::Transport(e.to_string()))?;

            let status = resp.status().as_u16();
            let mut headers = BTreeMap::new();
            for (name, value) in resp.headers() {
                headers.insert(
                    name.as_str().to_ascii_lowercase(),
                    value.to_str().unwrap_or("<non-ascii header>").to_string(),
                );
            }
            let body = resp
                .bytes()
                .await
                .map_err(|e| Error::Transport(format!("reading body: {e}")))?
                .to_vec();
            Ok(Response {
                status,
                headers,
                body,
            })
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_builder_lowers_header_keys() {
        let r = Request::new(Method::Get, "https://example.test/")
            .header("Authorization", "Bearer xyz")
            .header("X-Custom", "v");
        assert_eq!(
            r.headers.get("authorization").map(String::as_str),
            Some("Bearer xyz")
        );
        assert_eq!(r.headers.get("x-custom").map(String::as_str), Some("v"));
    }

    #[test]
    fn json_body_sets_content_type_and_serializes() {
        let r = Request::new(Method::Post, "https://example.test/")
            .json_body(&serde_json::json!({"k": 1}))
            .unwrap();
        assert_eq!(
            r.headers.get("content-type").map(String::as_str),
            Some("application/json")
        );
        assert_eq!(r.body.as_deref(), Some(b"{\"k\":1}".as_slice()));
    }

    #[test]
    fn response_ensure_success_passes_2xx() {
        let r = Response {
            status: 201,
            headers: BTreeMap::new(),
            body: vec![],
        };
        assert_eq!(r.clone().ensure_success().unwrap().status, 201);
    }

    #[test]
    fn response_ensure_success_fails_4xx_with_body() {
        let r = Response {
            status: 422,
            headers: BTreeMap::new(),
            body: b"validation failed".to_vec(),
        };
        match r.ensure_success().unwrap_err() {
            Error::Status { status, body } => {
                assert_eq!(status, 422);
                assert!(body.contains("validation failed"));
            }
            other => panic!("expected Status, got {other:?}"),
        }
    }

    #[test]
    fn response_json_decodes_well_formed_payload() {
        let r = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: br#"{"x": 1}"#.to_vec(),
        };
        let parsed: serde_json::Value = r.json().unwrap();
        assert_eq!(parsed["x"], 1);
    }

    #[test]
    fn response_json_returns_decode_error_for_garbage() {
        let r = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: b"not json".to_vec(),
        };
        let err = r.json::<serde_json::Value>().unwrap_err();
        assert!(matches!(err, Error::Decode(_)));
    }

    #[test]
    fn rate_limit_remaining_parses_header() {
        let mut headers = BTreeMap::new();
        headers.insert("x-ratelimit-remaining".into(), "4998".into());
        let r = Response {
            status: 200,
            headers,
            body: vec![],
        };
        assert_eq!(r.rate_limit_remaining(), Some(4998));
    }

    #[test]
    fn rate_limit_remaining_absent_or_garbage_is_none() {
        let r = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: vec![],
        };
        assert!(r.rate_limit_remaining().is_none());

        let mut bad = BTreeMap::new();
        bad.insert("x-ratelimit-remaining".into(), "not a number".into());
        let r = Response {
            status: 200,
            headers: bad,
            body: vec![],
        };
        assert!(r.rate_limit_remaining().is_none());
    }

    #[test]
    fn retry_after_parses_seconds() {
        let mut headers = BTreeMap::new();
        headers.insert("retry-after".into(), "60".into());
        let r = Response {
            status: 429,
            headers,
            body: vec![],
        };
        assert_eq!(r.retry_after(), Some(Duration::from_secs(60)));
    }

    /// A response with the given headers and `{"message": ...}` body, the shape
    /// GitHub returns for every refusal.
    fn refusal(status: u16, headers: &[(&str, &str)], message: &str) -> Response {
        Response {
            status,
            headers: headers
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            body: serde_json::json!({ "message": message }).to_string().into(),
        }
    }

    #[test]
    fn retry_after_classifies_a_secondary_limit_on_403() {
        let limit = refusal(
            403,
            &[("retry-after", "60"), ("x-ratelimit-remaining", "4998")],
            "You have exceeded a secondary rate limit",
        )
        .rate_limit()
        .expect("retry-after is documented secondary-limit evidence");
        assert_eq!(limit.scope, RateLimitScope::Secondary);
        assert_eq!(limit.retry_after, Some(Duration::from_secs(60)));
    }

    #[test]
    fn exhausted_quota_takes_its_wait_from_the_reset_header() {
        let limit = refusal(
            403,
            &[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "1893457800"),
                ("date", "Tue, 01 Jan 2030 00:00:00 GMT"),
            ],
            "API rate limit exceeded for installation ID 9.",
        )
        .rate_limit()
        .expect("an exhausted quota is documented primary-limit evidence");
        assert_eq!(limit.scope, RateLimitScope::Primary);
        // 1893457800 is 1800s after the `date` above.
        assert_eq!(limit.retry_after, Some(Duration::from_secs(1800)));
    }

    #[test]
    fn reset_is_measured_against_githubs_clock_not_ours() {
        // Reset already passed by GitHub's own reckoning: no wait to report,
        // and a local clock behind GitHub's cannot invent one.
        let response = refusal(
            403,
            &[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "1893454200"),
                ("date", "Tue, 01 Jan 2030 00:00:00 GMT"),
            ],
            "API rate limit exceeded",
        );
        assert_eq!(response.rate_limit_reset_after(), None);
        let limit = response.rate_limit().unwrap();
        assert_eq!(limit.scope, RateLimitScope::Primary);
        assert_eq!(limit.retry_after, None);
    }

    #[test]
    fn body_wording_classifies_a_limit_that_sent_no_headers() {
        let limit = refusal(403, &[], "You have exceeded a secondary rate limit")
            .rate_limit()
            .expect("the documented body wording is evidence on its own");
        assert_eq!(limit.scope, RateLimitScope::Secondary);
        assert_eq!(limit.retry_after, None);
    }

    #[test]
    fn malformed_or_absent_headers_fall_back_to_the_other_evidence() {
        // Unparseable numbers are no evidence at all, so the body still decides.
        let limit = refusal(
            403,
            &[
                ("retry-after", "in a little while"),
                ("x-ratelimit-remaining", "plenty"),
                ("x-ratelimit-reset", "soon"),
                ("date", "whenever"),
            ],
            "API rate limit exceeded for installation ID 9.",
        )
        .rate_limit()
        .expect("body wording survives unparseable headers");
        assert_eq!(limit.scope, RateLimitScope::Secondary);
        assert_eq!(limit.retry_after, None);

        // A 429 is evidence by itself, with or without readable headers.
        let limit = refusal(429, &[("retry-after", "?")], "Too Many Requests")
            .rate_limit()
            .expect("429 is a secondary limit by definition");
        assert_eq!(limit.scope, RateLimitScope::Secondary);
        assert_eq!(limit.retry_after, None);
    }

    #[test]
    fn genuine_forbidden_response_is_not_throttling() {
        let response = refusal(
            403,
            &[
                ("x-ratelimit-remaining", "4998"),
                ("x-ratelimit-reset", "1893457800"),
                ("date", "Tue, 01 Jan 2030 00:00:00 GMT"),
            ],
            "Resource not accessible by integration",
        );
        assert_eq!(response.rate_limit(), None);
        match response.status_error() {
            Error::Status { status, body } => {
                assert_eq!(status, 403);
                assert!(body.contains("not accessible"));
            }
            other => panic!("expected Status, got {other:?}"),
        }
    }

    #[test]
    fn genuine_forbidden_response_is_not_throttling_on_the_hours_last_request() {
        // The quota headers report the hour, not this answer: a refusal served
        // as the last permitted request carries `remaining: 0` too. GitHub named
        // a different reason, so the exhausted quota cannot overrule it — a
        // permission refusal must still settle rather than retry.
        let response = refusal(
            403,
            &[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "1893457800"),
                ("date", "Tue, 01 Jan 2030 00:00:00 GMT"),
            ],
            "Resource not accessible by integration",
        );
        assert_eq!(response.rate_limit(), None);
        assert!(matches!(response.status_error(), Error::Status { .. }));
    }

    #[test]
    fn exhausted_quota_is_evidence_when_github_names_no_other_reason() {
        // No readable reason in the body, so the headers stand: retrying an
        // unexplained refusal costs one request, settling it costs the invitation.
        let response = Response {
            status: 403,
            headers: [("x-ratelimit-remaining".to_owned(), "0".to_owned())]
                .into_iter()
                .collect(),
            body: b"<html>upstream error</html>".to_vec(),
        };
        let limit = response.rate_limit().expect("headers are the only evidence");
        assert_eq!(limit.scope, RateLimitScope::Primary);
    }

    #[test]
    fn non_throttling_status_never_classifies_as_a_limit() {
        // Quota headers ride along on every response; only 403/429 report a limit.
        assert_eq!(
            refusal(
                404,
                &[("x-ratelimit-remaining", "0"), ("retry-after", "60")],
                "Not Found"
            )
            .rate_limit(),
            None
        );
    }

    #[test]
    fn status_error_reports_a_throttled_response_as_rate_limited() {
        let err = refusal(
            403,
            &[("retry-after", "30")],
            "You have exceeded a secondary rate limit",
        )
        .status_error();
        match err {
            Error::RateLimited {
                status,
                body,
                rate_limit,
            } => {
                assert_eq!(status, 403);
                assert!(body.contains("secondary rate limit"));
                assert_eq!(rate_limit.scope, RateLimitScope::Secondary);
                assert_eq!(rate_limit.retry_after, Some(Duration::from_secs(30)));
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn response_ensure_success_truncates_safely_across_utf8_boundary() {
        // Construct a body where byte 4096 sits in the middle of a multi-byte UTF-8 codepoint.
        // 'a' is 1 byte; '€' (U+20AC) encodes as 3 bytes (E2 82 AC).
        // 4095 'a's + '€' puts byte indexes 4095, 4096, 4097 inside the euro sign;
        // the byte slice [..4096] therefore lands mid-codepoint and the OLD code panicked.
        let mut body = vec![b'a'; 4095];
        body.extend_from_slice("€".as_bytes()); // bytes 4095..4098
        body.extend_from_slice(b"trailing"); // bytes 4098..
        let r = Response {
            status: 500,
            headers: BTreeMap::new(),
            body,
        };
        // Must NOT panic. Must produce an Error::Status with body length <= 4096 chars
        // of input plus the ellipsis marker.
        let err = r.ensure_success().unwrap_err();
        match err {
            Error::Status { status, body } => {
                assert_eq!(status, 500);
                assert!(
                    body.ends_with('…'),
                    "expected ellipsis suffix, got {body:?}"
                );
            }
            other => panic!("expected Status, got {other:?}"),
        }
    }
}
