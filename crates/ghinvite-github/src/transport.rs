//! HTTP transport seam. Every GitHub call funnels through [`HttpTransport`],
//! so the native reqwest adapter, the Workers `fetch` adapter and the test
//! mock are interchangeable.

use crate::error::ApiError;
use std::collections::BTreeMap;
use std::time::Duration;

/// Maximum buffered GitHub response, enforced while reading the body.
pub const MAX_RESPONSE_BYTES: usize = 10 * 1024 * 1024;

/// The transport's deadline cap; a request can lower it, never raise it.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(30);

/// HTTP methods used by the endpoints; retry safety is a separate property.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// Read a resource.
    Get,
    /// Create a resource; never assume retry safety.
    Post,
    /// Apply a partial update.
    Patch,
    /// Replace or set a resource; the method alone does not prove idempotency.
    Put,
    /// Remove a resource or grant.
    Delete,
}

impl Method {
    /// Uppercase HTTP method text.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Patch => "PATCH",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
        }
    }
}

/// Whether a failed transport could have applied a request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TransportError {
    /// No HTTP request was dispatched.
    NotSent,
    /// A request was dispatched but no complete response was obtained; its
    /// upstream effect is unknown.
    SentNoResponse {
        /// Whether the missing response resulted from the transport deadline.
        timed_out: bool,
    },
    /// The response exceeded [`MAX_RESPONSE_BYTES`] and was not exposed.
    Oversize,
}

impl TransportError {
    /// Fixed diagnostic code; connection errors never retain source text.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotSent => "gh:transport:not_sent",
            Self::SentNoResponse { timed_out: true } => "gh:transport:timeout",
            Self::SentNoResponse { timed_out: false } => "gh:transport:no_response",
            Self::Oversize => "gh:transport:oversize",
        }
    }
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for TransportError {}

/// One HTTP request. `Debug` redacts the URL, headers and body: they carry
/// credentials and caller input.
#[derive(Clone)]
#[non_exhaustive]
pub struct Request {
    /// Wire method.
    pub method: Method,
    /// Absolute URL.
    pub url: String,
    /// Lowercased names, sorted for deterministic equality in tests.
    pub headers: BTreeMap<String, String>,
    /// `None` for GET/DELETE.
    pub body: Option<Vec<u8>>,
    /// End-to-end timeout, including response body reads. At most
    /// [`MAX_TIMEOUT`], the default.
    pub timeout: Duration,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("method", &self.method)
            .field("url", &"[REDACTED]")
            .field("headers", &"[REDACTED]")
            .field("body", &"[REDACTED]")
            .finish()
    }
}

impl Request {
    /// An unauthenticated request with the default 30 s deadline; [`crate::rest::Rest`]
    /// adds credentials itself.
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: BTreeMap::new(),
            body: None,
            timeout: MAX_TIMEOUT,
        }
    }

    /// Set a header; the name is lowercased.
    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.insert(name.to_ascii_lowercase(), value.into());
        self
    }

    /// Lower the deadline; the transport's cap cannot be extended.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout.min(MAX_TIMEOUT);
        self
    }

    /// The body as the transport sends it. Adapters must not record, format
    /// or retain these bytes.
    pub fn body_for_transport(&self) -> Option<&[u8]> {
        self.body.as_deref()
    }

    /// Serialize `value` as the JSON body. Nothing has been sent yet, so a
    /// failure keeps only its fact: serde's message would quote caller input.
    pub fn json_body<T: serde::Serialize>(mut self, value: &T) -> Result<Self, ApiError> {
        let body = serde_json::to_vec(value).map_err(|_| ApiError::Decode { what: "request" })?;
        self.body = Some(body);
        self.headers
            .insert("content-type".into(), "application/json".into());
        Ok(self)
    }
}

/// A fully buffered, bounded HTTP response.
#[derive(Clone)]
pub struct Response {
    /// HTTP status, before classification.
    pub status: u16,
    /// Lowercased names, as in [`Request::headers`].
    pub headers: BTreeMap<String, String>,
    /// Buffered body, at most [`MAX_RESPONSE_BYTES`].
    pub body: Vec<u8>,
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .field("headers", &"[REDACTED]")
            .field("body", &"[REDACTED]")
            .finish()
    }
}

/// Single-dispatch HTTP seam. Implementations never follow redirects, enforce
/// the request's deadline through the last body byte, bound the body, and
/// preserve whether a failed request could have reached the server.
#[trait_variant::make(Send)]
pub trait HttpTransport: Send + Sync + 'static {
    /// Dispatch exactly one request.
    async fn send(&self, request: Request) -> Result<Response, TransportError>;
}

/// Object-safe counterpart of [`HttpTransport`], kept behind `Arc`.
/// Implement [`HttpTransport`]; this trait has a blanket implementation.
pub trait HttpTransportDyn: Send + Sync + 'static {
    /// Same contract as [`HttpTransport::send`].
    fn send(&self, request: Request) -> crate::BoxFut<'_, Result<Response, TransportError>>;
}

impl<T: HttpTransport> HttpTransportDyn for T {
    fn send(&self, request: Request) -> crate::BoxFut<'_, Result<Response, TransportError>> {
        Box::pin(HttpTransport::send(self, request))
    }
}

/// Native reqwest transport: redirects disabled, 30 s deadline, bounded body.
/// On Workers use `worker::WorkerFetchTransport`: reqwest's fetch
/// backend cannot disable redirects.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

#[cfg(not(target_arch = "wasm32"))]
impl ReqwestTransport {
    /// Build the transport. Fails only if the TLS backend cannot initialize.
    pub fn new() -> Result<Self, TransportError> {
        let client = reqwest::Client::builder()
            .timeout(MAX_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| TransportError::NotSent)?;
        Ok(Self { client })
    }

    /// Fixture seam. The caller must disable redirects: a client's redirect
    /// policy cannot be inspected after construction.
    #[cfg(any(test, feature = "test-mock", feature = "test-stub"))]
    pub fn with_client(client: reqwest::Client) -> Self {
        Self { client }
    }

    async fn execute(&self, request: Request) -> Result<Response, TransportError> {
        let method = match request.method {
            Method::Get => reqwest::Method::GET,
            Method::Post => reqwest::Method::POST,
            Method::Patch => reqwest::Method::PATCH,
            Method::Put => reqwest::Method::PUT,
            Method::Delete => reqwest::Method::DELETE,
        };
        let mut builder = self
            .client
            .request(method, &request.url)
            .timeout(request.timeout.min(MAX_TIMEOUT));
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = request.body {
            builder = builder.body(body);
        }
        // Build before dispatch so malformed URLs/headers never look ambiguous.
        let built = builder.build().map_err(|_| TransportError::NotSent)?;
        let mut response = self.client.execute(built).await.map_err(|error| {
            if error.is_connect() {
                TransportError::NotSent
            } else {
                TransportError::SentNoResponse {
                    timed_out: error.is_timeout(),
                }
            }
        })?;
        if response
            .content_length()
            .is_some_and(|len| len > MAX_RESPONSE_BYTES as u64)
        {
            return Err(TransportError::Oversize);
        }
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_ascii_lowercase(),
                    value.to_str().unwrap_or("<non-ascii header>").to_string(),
                )
            })
            .collect();
        let mut body = Vec::new();
        while let Some(chunk) =
            response
                .chunk()
                .await
                .map_err(|error| TransportError::SentNoResponse {
                    timed_out: error.is_timeout(),
                })?
        {
            if chunk.len() > MAX_RESPONSE_BYTES - body.len() {
                return Err(TransportError::Oversize);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Response {
            status,
            headers,
            body,
        })
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl HttpTransport for ReqwestTransport {
    async fn send(&self, request: Request) -> Result<Response, TransportError> {
        self.execute(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn native_adapter_transmits_the_body() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/write", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let expected = br#"{"permission":"push"}"#;
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut received = Vec::new();
            loop {
                let mut chunk = [0; 4096];
                let n = socket.read(&mut chunk).await.unwrap();
                assert!(n > 0, "request ended before body");
                received.extend_from_slice(&chunk[..n]);
                assert!(received.len() < 8192, "unexpected request size");
                if let Some(end) = received.windows(4).position(|w| w == b"\r\n\r\n") {
                    let end = end + 4;
                    if received.len() >= end + expected.len() {
                        assert!(received[end..] == *expected, "body wire mismatch");
                        break;
                    }
                }
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .await
                .unwrap();
        });
        let request = Request::new(Method::Put, url)
            .json_body(&serde_json::json!({"permission":"push"}))
            .unwrap();
        assert_eq!(
            HttpTransport::send(&ReqwestTransport::new().unwrap(), request)
                .await
                .unwrap()
                .status,
            200
        );
        server.await.unwrap();
    }

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
    fn timeout_can_be_lowered_but_never_raised() {
        let r = Request::new(Method::Get, "https://example.test/");
        assert_eq!(r.timeout, MAX_TIMEOUT);
        assert_eq!(
            r.clone().timeout(Duration::from_secs(5)).timeout,
            Duration::from_secs(5)
        );
        assert_eq!(r.timeout(Duration::from_secs(60)).timeout, MAX_TIMEOUT);
    }

    #[test]
    fn debug_never_renders_url_headers_or_body() {
        let secret = "debug-secret-marker";
        let request = Request::new(Method::Post, format!("https://example.test/{secret}"))
            .header("authorization", secret)
            .json_body(&secret)
            .unwrap();
        let response = Response {
            status: 200,
            headers: [("x".to_owned(), secret.to_owned())].into(),
            body: secret.as_bytes().to_vec(),
        };
        let rendered = format!("{request:?} {response:?}");
        assert!(!rendered.contains(secret), "{rendered}");
    }
}
