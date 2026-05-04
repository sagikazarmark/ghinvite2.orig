//! HTTP transport seam. Every GitHub call funnels through this trait so the
//! reqwest impl can be swapped for `MockTransport` in tests.

use crate::error::{Error, Result};
use async_trait::async_trait;
use std::collections::BTreeMap;

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
            // Truncate to keep error logs sane. Slicing bytes (not the lossy String)
            // avoids any chance of mid-codepoint panic.
            let truncated = if self.body.len() > 4096 {
                format!("{}…", String::from_utf8_lossy(&self.body[..4096]))
            } else {
                String::from_utf8_lossy(&self.body).to_string()
            };
            Err(Error::Status {
                status: self.status,
                body: truncated,
            })
        }
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
/// Native-only: on `wasm32-unknown-unknown`, reqwest's response future is
/// `!Send` (it wraps `js-sys::futures::JsFuture`), which conflicts with the
/// `HttpTransport: Send + Sync + 'static` bound that Restate handlers need on
/// native. Workers builds plug in a different `HttpTransport` impl (e.g. one
/// backed by `worker::Fetch`) — added in Plan 3.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

#[cfg(not(target_arch = "wasm32"))]
impl ReqwestTransport {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| Error::Transport(format!("building reqwest client: {e}")))?;
        Ok(Self { client })
    }

    /// Inject an existing client (e.g. one with a custom timeout middleware).
    pub fn with_client(client: reqwest::Client) -> Self {
        Self { client }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait]
impl HttpTransport for ReqwestTransport {
    async fn send(&self, request: Request) -> Result<Response> {
        let method = match request.method {
            Method::Get => reqwest::Method::GET,
            Method::Post => reqwest::Method::POST,
            Method::Put => reqwest::Method::PUT,
            Method::Delete => reqwest::Method::DELETE,
        };
        let mut builder = self.client.request(method, &request.url);
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
                value
                    .to_str()
                    .unwrap_or("<non-ascii header>")
                    .to_string(),
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
        assert_eq!(r.headers.get("authorization").map(String::as_str), Some("Bearer xyz"));
        assert_eq!(r.headers.get("x-custom").map(String::as_str), Some("v"));
    }

    #[test]
    fn json_body_sets_content_type_and_serializes() {
        let r = Request::new(Method::Post, "https://example.test/")
            .json_body(&serde_json::json!({"k": 1}))
            .unwrap();
        assert_eq!(r.headers.get("content-type").map(String::as_str), Some("application/json"));
        assert_eq!(r.body.as_deref(), Some(b"{\"k\":1}".as_slice()));
    }

    #[test]
    fn response_ensure_success_passes_2xx() {
        let r = Response { status: 201, headers: BTreeMap::new(), body: vec![] };
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
    fn response_ensure_success_truncates_safely_across_utf8_boundary() {
        // Construct a body where byte 4096 sits in the middle of a multi-byte UTF-8 codepoint.
        // 'a' is 1 byte; '€' (U+20AC) encodes as 3 bytes (E2 82 AC).
        // 4095 'a's + '€' puts byte indexes 4095, 4096, 4097 inside the euro sign;
        // the byte slice [..4096] therefore lands mid-codepoint and the OLD code panicked.
        let mut body = vec![b'a'; 4095];
        body.extend_from_slice("€".as_bytes()); // bytes 4095..4098
        body.extend_from_slice(b"trailing"); // bytes 4098..
        let r = Response { status: 500, headers: BTreeMap::new(), body };
        // Must NOT panic. Must produce an Error::Status with body length <= 4096 chars
        // of input plus the ellipsis marker.
        let err = r.ensure_success().unwrap_err();
        match err {
            Error::Status { status, body } => {
                assert_eq!(status, 500);
                assert!(body.ends_with('…'), "expected ellipsis suffix, got {body:?}");
            }
            other => panic!("expected Status, got {other:?}"),
        }
    }
}
