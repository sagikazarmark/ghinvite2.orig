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
        // serde's message renders the value it choked on, and request bodies
        // carry the caller's input (justifications, logins). Only the fact of
        // the failure is safe to keep.
        let body =
            serde_json::to_vec(value).map_err(|_| Error::decode_shape("encoding request body"))?;
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
    /// failure, describing where decoding stopped without quoting the body —
    /// the token endpoints decode through here and their payloads *are*
    /// credentials.
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_slice(&self.body).map_err(|e| Error::decode(&e, self.body.len()))
    }

    /// Returns `Ok(self)` if `status` is 2xx, otherwise the error
    /// [`status_error`](Response::status_error) builds — `Error::RateLimited`
    /// for a throttled response, `Error::Status` for every other non-2xx.
    pub fn ensure_success(self) -> Result<Self> {
        if (200..300).contains(&self.status) {
            Ok(self)
        } else {
            Err(self.status_error())
        }
    }

    /// Build the error for a non-2xx response, summarising the body the same
    /// way [`ensure_success`] does. Callers that match on status manually (e.g.
    /// to special-case 201 vs 204) use this for the unexpected-status arm so
    /// error logs stay consistent across the crate.
    ///
    /// A response carrying [rate-limit evidence](Response::rate_limit) becomes
    /// [`Error::RateLimited`], so throttling is classified once here and no
    /// caller has to re-read headers to tell a throttled 403 from a
    /// permission-denied one.
    ///
    /// Either way the summary keeps GitHub's documented `message` and
    /// validation sub-codes and nothing else; see [`crate::redact`] for why the
    /// body itself never survives. Classification reads the *raw* body first —
    /// [`rate_limit`](Response::rate_limit) looks for GitHub's rate-limit
    /// wording — so sanitizing never costs us the evidence.
    pub fn status_error(&self) -> Error {
        let body = crate::redact::upstream_diagnostic(&self.body);
        match self.rate_limit() {
            Some(rate_limit) => Error::RateLimited {
                status: self.status,
                body,
                rate_limit,
            },
            None => Error::Status {
                status: self.status,
                body,
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
    /// as a non-negative integer of seconds. The header also permits an
    /// HTTP-date, which we read against the response's own `date` so a skewed
    /// local clock cannot shorten the wait. A date already past, an unreadable
    /// value, or a missing `date` to measure it against is no guidance at all.
    pub fn retry_after(&self) -> Option<Duration> {
        let value = self.headers.get("retry-after")?;
        if let Ok(seconds) = value.parse::<u64>() {
            return Some(Duration::from_secs(seconds));
        }
        let retry_at = chrono::DateTime::parse_from_rfc2822(value).ok()?;
        self.after_response_date(retry_at.timestamp())
    }

    /// How long until the primary quota resets, measured as `x-ratelimit-reset`
    /// minus the response's own `date`. Reading both ends off GitHub's clock
    /// keeps a skewed local one from shortening the wait. `None` if either
    /// header is absent or unparseable, or if the reset has already passed.
    pub fn rate_limit_reset_after(&self) -> Option<Duration> {
        self.after_response_date(self.headers.get("x-ratelimit-reset")?.parse().ok()?)
    }

    /// How far `instant` (a Unix timestamp) lies past the `date` this response
    /// was sent at. `None` when there is no readable `date` to measure against,
    /// or when the instant has already passed by GitHub's own reckoning.
    fn after_response_date(&self, instant: i64) -> Option<Duration> {
        let sent = chrono::DateTime::parse_from_rfc2822(self.headers.get("date")?)
            .ok()?
            .timestamp();
        u64::try_from(instant.checked_sub(sent)?)
            .ok()
            .filter(|seconds| *seconds > 0)
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
    /// The scope names the limit GitHub cited, so a `retry-after` or a 429 is
    /// secondary even where the hourly quota has also run out. The reported
    /// wait may still come from that quota's reset: which limit was cited and
    /// when the request can next succeed are different questions.
    ///
    /// Only a response classifies. A transport failure never reaches here, so a
    /// request whose effect is unknown is never mistaken for a refused one.
    pub fn rate_limit(&self) -> Option<RateLimit> {
        if self.status != 403 && self.status != 429 {
            return None;
        }
        // Sending `retry-after` at all is GitHub citing a limit, whether or not
        // the value yields a wait we can use: an unreadable date still says
        // "this is a limit", and dropping that evidence would leave the refusal
        // looking permanent.
        let cited = self.headers.contains_key("retry-after") || self.status == 429;
        let retry_after = self.retry_after();
        let quota_exhausted = self.rate_limit_remaining() == Some(0);
        let named_limit = self.body_names_rate_limit();
        if !(cited || named_limit == Some(true) || (quota_exhausted && named_limit.is_none())) {
            return None;
        }
        // The scope names the limit GitHub cited; the wait follows whichever
        // evidence describes it. A `retry-after` names this request's wait, so
        // it wins outright. Failing that, only an exhausted quota makes the
        // reset this request's wait — the reset rides along on a healthy quota
        // too, and idling out an unrelated hour is far worse than the caller's
        // unguided backoff.
        let scope = if cited || !quota_exhausted {
            RateLimitScope::Secondary
        } else {
            RateLimitScope::Primary
        };
        let retry_after = retry_after.or_else(|| {
            quota_exhausted
                .then(|| self.rate_limit_reset_after())
                .flatten()
        });
        Some(RateLimit { scope, retry_after })
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
    fn response_ensure_success_fails_4xx_with_a_body_summary() {
        let r = Response {
            status: 422,
            headers: BTreeMap::new(),
            body: br#"{"message":"Validation Failed"}"#.to_vec(),
        };
        match r.ensure_success().unwrap_err() {
            Error::Status { status, body } => {
                assert_eq!(status, 422);
                assert!(body.contains("Validation Failed"), "{body}");
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

    /// The token endpoints return credentials in the body. A decode failure
    /// there used to embed the whole payload in the error, which then reached
    /// logs and Restate terminal errors.
    #[test]
    fn response_json_decode_error_never_quotes_the_payload() {
        let token = "ghs_16C7e42F292c6912E7710c838347Ae178B4a";
        let body = format!(r#"{{"token":"{token}","expires_at":1234}}"#);
        let r = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: body.clone().into_bytes(),
        };
        // `expires_at` is an integer where the payload type wants a timestamp
        // string, so serde reports a shape error and names the value it saw.
        let err = r
            .json::<crate::payloads::GhInstallationToken>()
            .unwrap_err();
        let rendered = format!("{err} {err:?}");
        assert!(!rendered.contains(token), "{rendered}");
        assert!(!rendered.contains("body="), "{rendered}");
        assert!(
            rendered.contains(&format!("{}-byte body", body.len())),
            "{rendered}"
        );
    }

    #[test]
    fn status_error_summarises_a_github_error_envelope() {
        let r = Response {
            status: 422,
            headers: BTreeMap::new(),
            body: br#"{"message":"Validation Failed","errors":[{"resource":"RepositoryInvitation","code":"already_exists"}]}"#.to_vec(),
        };
        match r.status_error() {
            Error::Status { status, body } => {
                assert_eq!(status, 422);
                // The 422 sub-code is what tells "already a collaborator" apart
                // from "permission not valid"; it has to survive.
                assert!(body.contains("already_exists"), "{body}");
                assert!(body.contains("Validation Failed"), "{body}");
            }
            other => panic!("expected Status, got {other:?}"),
        }
    }

    #[test]
    fn status_error_drops_a_gateway_body_that_echoes_our_credentials() {
        let jwt = "eyJhbGciOiJSUzI1NiJ9.eyJpc3MiOiIxMjM0NSJ9.c2lnbmF0dXJlLXZhbHVl";
        let r = Response {
            status: 502,
            headers: BTreeMap::new(),
            body: format!("<html>proxy error, upstream sent: Authorization: Bearer {jwt}</html>")
                .into_bytes(),
        };
        let err = r.status_error();
        let rendered = format!("{err} {err:?}");
        assert!(!rendered.contains(jwt), "{rendered}");
        assert!(!rendered.contains("proxy error"), "{rendered}");
        assert_eq!(err.status(), Some(502));
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
    fn retry_after_accepts_the_http_date_form() {
        let response = refusal(
            403,
            &[
                ("retry-after", "Tue, 01 Jan 2030 00:02:00 GMT"),
                ("date", "Tue, 01 Jan 2030 00:00:00 GMT"),
            ],
            "You have exceeded a secondary rate limit",
        );
        assert_eq!(response.retry_after(), Some(Duration::from_secs(120)));

        // A date already past by GitHub's own clock is no guidance, and neither
        // is one with no `date` to measure it against.
        let past = refusal(
            403,
            &[
                ("retry-after", "Mon, 31 Dec 2029 23:58:00 GMT"),
                ("date", "Tue, 01 Jan 2030 00:00:00 GMT"),
            ],
            "You have exceeded a secondary rate limit",
        );
        assert_eq!(past.retry_after(), None);
        let undated = refusal(
            403,
            &[("retry-after", "Tue, 01 Jan 2030 00:02:00 GMT")],
            "You have exceeded a secondary rate limit",
        );
        assert_eq!(undated.retry_after(), None);
    }

    #[test]
    fn an_unusable_retry_after_still_cites_a_limit() {
        // GitHub sent a `retry-after` we cannot turn into a wait — an HTTP-date
        // with no `date` to measure it against — and named no reason in the
        // body. Discarding that evidence would leave this looking like a
        // permanent refusal and settle the invitation on it.
        let limit = refusal(
            403,
            &[("retry-after", "Tue, 01 Jan 2030 00:02:00 GMT")],
            "Something went wrong",
        )
        .rate_limit()
        .expect("a retry-after GitHub sent cites a limit even when unreadable");
        assert_eq!(limit.scope, RateLimitScope::Secondary);
        assert_eq!(limit.retry_after, None);
    }

    #[test]
    fn a_cited_secondary_limit_stays_secondary_on_an_exhausted_quota() {
        // 429 is a secondary citation however the quota reads; the reset is
        // still the soonest this request can succeed, so it supplies the wait.
        let limit = refusal(
            429,
            &[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "1893457800"),
                ("date", "Tue, 01 Jan 2030 00:00:00 GMT"),
            ],
            "Too Many Requests",
        )
        .rate_limit()
        .unwrap();
        assert_eq!(limit.scope, RateLimitScope::Secondary);
        assert_eq!(limit.retry_after, Some(Duration::from_secs(1800)));
    }

    #[test]
    fn retry_after_names_the_wait_even_on_an_exhausted_quota() {
        // GitHub asked for 30s. The hourly reset rides along on the same
        // response and must not replace the wait it actually named.
        let limit = refusal(
            429,
            &[
                ("retry-after", "30"),
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "1893457800"),
                ("date", "Tue, 01 Jan 2030 00:00:00 GMT"),
            ],
            "You have exceeded a secondary rate limit",
        )
        .rate_limit()
        .unwrap();
        assert_eq!(limit.scope, RateLimitScope::Secondary);
        assert_eq!(limit.retry_after, Some(Duration::from_secs(30)));
    }

    #[test]
    fn a_healthy_quotas_reset_is_never_mistaken_for_a_secondary_wait() {
        // A secondary limit with quota to spare: the reset half an hour out
        // belongs to an untouched hourly window, so this reports no wait and
        // the caller falls back to its unguided backoff instead of idling.
        let limit = refusal(
            403,
            &[
                ("x-ratelimit-remaining", "4998"),
                ("x-ratelimit-reset", "1893457800"),
                ("date", "Tue, 01 Jan 2030 00:00:00 GMT"),
            ],
            "You have exceeded a secondary rate limit",
        )
        .rate_limit()
        .unwrap();
        assert_eq!(limit.scope, RateLimitScope::Secondary);
        assert_eq!(limit.retry_after, None);
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
        let limit = response
            .rate_limit()
            .expect("headers are the only evidence");
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
    fn response_ensure_success_bounds_a_huge_body_without_splitting_a_codepoint() {
        // A long `message` whose bound lands mid-codepoint: '€' (U+20AC) is
        // three bytes, so a naive byte slice through it would panic.
        let body = format!(r#"{{"message":"{}€ tail"}}"#, "a".repeat(4096));
        let r = Response {
            status: 500,
            headers: BTreeMap::new(),
            body: body.clone().into_bytes(),
        };
        match r.ensure_success().unwrap_err() {
            Error::Status {
                status,
                body: summary,
            } => {
                assert_eq!(status, 500);
                assert!(summary.len() < body.len() / 4, "{}", summary.len());
                assert!(summary.contains('…'), "{summary}");
            }
            other => panic!("expected Status, got {other:?}"),
        }
    }
}
