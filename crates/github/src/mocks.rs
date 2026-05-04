//! In-process mock for [`crate::transport::HttpTransport`].
//!
//! Behind the `test-mock` feature so it ships in dev-dependencies of consumers
//! without polluting production builds.

use crate::error::Result;
use crate::transport::{HttpTransport, Method, Request, Response};
use async_trait::async_trait;
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::sync::Arc;

/// One scripted expectation: the request the test expects, paired with the
/// response to return on a match.
#[derive(Clone, Debug)]
pub struct Expectation {
    pub method: Method,
    /// Exact URL match. Build with `concat!` if you want to assert against a
    /// known base; query-string matching is exact, so order matters.
    pub url: String,
    /// Subset match. Every header listed here MUST appear on the incoming
    /// request with the listed value (case-insensitive on the key, exact on
    /// the value). The incoming request may carry extra headers.
    pub required_headers: BTreeMap<String, String>,
    /// `None` skips body check; `Some(b)` requires byte-equality.
    pub expected_body: Option<Vec<u8>>,
    pub response: Response,
}

impl Expectation {
    pub fn ok_json(method: Method, url: impl Into<String>, body: serde_json::Value) -> Self {
        Self {
            method,
            url: url.into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 200,
                headers: {
                    let mut h = BTreeMap::new();
                    h.insert("content-type".into(), "application/json".into());
                    h
                },
                body: serde_json::to_vec(&body).unwrap(),
            },
        }
    }

    pub fn status(method: Method, url: impl Into<String>, status: u16) -> Self {
        Self {
            method,
            url: url.into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status,
                headers: BTreeMap::new(),
                body: Vec::new(),
            },
        }
    }

    pub fn require_header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.required_headers
            .insert(name.to_ascii_lowercase(), value.into());
        self
    }

    pub fn require_body(mut self, body: Vec<u8>) -> Self {
        self.expected_body = Some(body);
        self
    }
}

/// In-process mock. Constructs from an ordered list of [`Expectation`]s and
/// pops one per `send()` call. Panics on script exhaustion or mismatch — the
/// goal is *loud* test failures.
#[derive(Clone)]
pub struct MockTransport {
    inner: Arc<Mutex<Vec<Expectation>>>,
}

impl MockTransport {
    pub fn scripted(expectations: Vec<Expectation>) -> Self {
        // Reverse so `pop()` yields in script order.
        let mut v = expectations;
        v.reverse();
        Self {
            inner: Arc::new(Mutex::new(v)),
        }
    }

    /// Returns the count of expectations remaining in the script.
    pub fn remaining(&self) -> usize {
        self.inner.lock().len()
    }

    /// Asserts that the script has been fully consumed.
    pub fn assert_exhausted(&self) {
        let n = self.remaining();
        assert_eq!(n, 0, "MockTransport script not exhausted: {n} expectations remaining");
    }
}

#[async_trait]
impl HttpTransport for MockTransport {
    async fn send(&self, request: Request) -> Result<Response> {
        let next = self
            .inner
            .lock()
            .pop()
            .unwrap_or_else(|| panic!("MockTransport: script exhausted, got unexpected request {request:?}"));

        if next.method != request.method {
            panic!(
                "MockTransport: expected {} but got {}",
                next.method.as_str(),
                request.method.as_str()
            );
        }
        if next.url != request.url {
            panic!(
                "MockTransport: expected URL {:?}, got {:?}",
                next.url, request.url
            );
        }
        for (name, expected_value) in &next.required_headers {
            match request.headers.get(name) {
                Some(actual) if actual == expected_value => (),
                Some(actual) => panic!(
                    "MockTransport: header {name:?} expected {expected_value:?}, got {actual:?}"
                ),
                None => panic!("MockTransport: missing required header {name:?}"),
            }
        }
        if let Some(expected_body) = &next.expected_body {
            let actual_body = request.body.unwrap_or_default();
            if &actual_body != expected_body {
                panic!(
                    "MockTransport: body mismatch.\n  expected: {:?}\n  actual:   {:?}",
                    String::from_utf8_lossy(expected_body),
                    String::from_utf8_lossy(&actual_body)
                );
            }
        }
        // Return the canned response. We deliberately do not let the test
        // observe transport-level errors via the script — return Status
        // responses to model HTTP failures.
        Ok(next.response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn matches_a_scripted_request() {
        let mock = MockTransport::scripted(vec![Expectation::ok_json(
            Method::Get,
            "https://api.github.test/foo",
            serde_json::json!({"k": "v"}),
        )]);
        let resp = mock
            .send(Request::new(Method::Get, "https://api.github.test/foo"))
            .await
            .unwrap();
        assert_eq!(resp.status, 200);
        assert!(resp.body.starts_with(b"{"));
        mock.assert_exhausted();
    }

    #[tokio::test]
    #[should_panic(expected = "missing required header")]
    async fn fails_when_required_header_absent() {
        let mock = MockTransport::scripted(vec![
            Expectation::ok_json(Method::Get, "https://api.github.test/", serde_json::json!({}))
                .require_header("authorization", "Bearer xyz"),
        ]);
        let _ = mock
            .send(Request::new(Method::Get, "https://api.github.test/"))
            .await;
    }

    #[tokio::test]
    #[should_panic(expected = "script exhausted")]
    async fn panics_on_extra_request() {
        let mock = MockTransport::scripted(vec![]);
        let _ = mock
            .send(Request::new(Method::Get, "https://api.github.test/"))
            .await;
    }
}
