//! Sending one request through reqwest (wrapped in middleware with feature
//! `middleware`) and reading a bounded response.
//!
//! On wasm32 reqwest dispatches through the host's `fetch`, which differs:
//! redirects cannot be disabled (a followed redirect is still caught below, but
//! only after the request was re-sent), and a failure never proves the request
//! was not sent.

use crate::api::{ApiError, ErrorBody};
use http::{HeaderMap, StatusCode};
use std::time::Duration;
use zeroize::Zeroizing;

/// Largest response body accepted from GitHub.
pub(crate) const MAX_RESPONSE_BYTES: usize = 10 * 1024 * 1024;

/// End-to-end deadline per request, set on every request so it also holds for
/// caller-supplied clients.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(30);

/// The client an [`App`](crate::App) sends through.
#[cfg(not(feature = "middleware"))]
pub(crate) type HttpClient = reqwest::Client;

/// The client an [`App`](crate::App) sends through. Plain reqwest clients are
/// wrapped in an empty middleware stack.
#[cfg(feature = "middleware")]
pub(crate) type HttpClient = reqwest_middleware::ClientWithMiddleware;

/// Wrap a plain reqwest client as an [`HttpClient`].
pub(crate) fn from_reqwest(client: reqwest::Client) -> HttpClient {
    #[cfg(feature = "middleware")]
    {
        reqwest_middleware::ClientBuilder::new(client).build()
    }

    #[cfg(not(feature = "middleware"))]
    {
        client
    }
}

/// The default client: `octoauth/<version>` user agent; natively also
/// redirects disabled and a 30 s timeout (on wasm32 the per-request timeout
/// applies and `fetch` follows redirects). `None` if the TLS backend cannot be
/// initialised.
pub(crate) fn default_client() -> Option<HttpClient> {
    let builder = reqwest::Client::builder().user_agent(crate::app::DEFAULT_USER_AGENT);
    #[cfg(not(target_arch = "wasm32"))]
    let builder = builder
        .redirect(reqwest::redirect::Policy::none())
        .timeout(TIMEOUT);
    builder.build().ok().map(from_reqwest)
}

/// Send one request and buffer at most [`MAX_RESPONSE_BYTES`].
pub(crate) async fn send(
    client: &HttpClient,
    request: reqwest::Request,
) -> Result<Response, SendError> {
    let url = request.url().clone();
    #[cfg(not(feature = "middleware"))]
    let response = client.execute(request).await.map_err(send_error);
    #[cfg(feature = "middleware")]
    let response = client.execute(request).await.map_err(|e| match e {
        reqwest_middleware::Error::Reqwest(e) => send_error(e),
        // A middleware may fail before or after dispatching.
        reqwest_middleware::Error::Middleware(_) => SendError::NoResponse,
    });
    let response = response?;
    // A client that follows redirects (and `fetch`, always) may have re-sent
    // the request elsewhere. Never trust a response from another URL.
    if response.url() != &url {
        return Err(SendError::Redirected);
    }
    if response
        .content_length()
        .is_some_and(|len| len > MAX_RESPONSE_BYTES as u64)
    {
        return Err(SendError::Oversize);
    }
    let status = response.status();
    let headers = response.headers().clone();
    let body = read_bounded(response).await?;
    Ok(Response {
        status,
        headers,
        body,
    })
}

/// Read the body chunk by chunk, failing as soon as it exceeds
/// [`MAX_RESPONSE_BYTES`].
#[cfg(not(target_arch = "wasm32"))]
async fn read_bounded(mut response: reqwest::Response) -> Result<Zeroizing<Vec<u8>>, SendError> {
    let mut body = Zeroizing::new(Vec::new());
    while let Some(chunk) = response.chunk().await.map_err(|_| SendError::NoResponse)? {
        push_bounded(&mut body, &chunk)?;
    }
    Ok(body)
}

/// Read the whole body, then bound it: reqwest's fetch backend streams only
/// with its `stream` feature, which cannot link next to `worker` 0.8. The
/// `Content-Length` check in [`send`] still refuses a declared oversize body
/// before it is read.
#[cfg(target_arch = "wasm32")]
async fn read_bounded(response: reqwest::Response) -> Result<Zeroizing<Vec<u8>>, SendError> {
    let bytes = response.bytes().await.map_err(|_| SendError::NoResponse)?;
    let mut body = Zeroizing::new(Vec::new());
    let pushed = push_bounded(&mut body, &bytes);
    // The buffer may hold a token: zeroize it when nothing else shares it.
    if let Ok(mut bytes) = bytes.try_into_mut() {
        zeroize::Zeroize::zeroize(&mut bytes[..]);
    }
    pushed.map(|()| body)
}

fn push_bounded(body: &mut Zeroizing<Vec<u8>>, chunk: &[u8]) -> Result<(), SendError> {
    if chunk.len() > MAX_RESPONSE_BYTES - body.len() {
        return Err(SendError::Oversize);
    }
    // Grow by hand: a reallocation inside `Vec` would leave the old buffer,
    // which may hold a token, unzeroized.
    if body.capacity() - body.len() < chunk.len() {
        let capacity = (body.len() + chunk.len())
            .max(body.capacity() * 2)
            .min(MAX_RESPONSE_BYTES);
        let mut grown = Zeroizing::new(Vec::with_capacity(capacity));
        grown.extend_from_slice(body);
        *body = grown;
    }
    body.extend_from_slice(chunk);
    Ok(())
}

/// `NotSent` only when reqwest proves the request never left: a build error,
/// or natively a connect error. `fetch` reports no such distinction.
fn send_error(error: reqwest::Error) -> SendError {
    #[cfg(not(target_arch = "wasm32"))]
    let not_sent = error.is_builder() || error.is_connect();
    #[cfg(target_arch = "wasm32")]
    let not_sent = error.is_builder();
    if not_sent {
        SendError::NotSent
    } else {
        SendError::NoResponse
    }
}

/// `Bearer <secret>`, marked sensitive so reqwest and `http` redact it in `Debug`.
pub(crate) fn bearer(secret: &str) -> reqwest::header::HeaderValue {
    let value = zeroize::Zeroizing::new(format!("Bearer {secret}"));
    let mut header = reqwest::header::HeaderValue::from_str(&value)
        .expect("JWTs and installation tokens are validated printable ASCII");
    header.set_sensitive(true);
    header
}

/// Why no usable response was obtained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SendError {
    /// Provably never sent (request build or connect failure).
    NotSent,

    /// May have reached GitHub; outcome unknown.
    NoResponse,

    /// Body larger than [`MAX_RESPONSE_BYTES`].
    Oversize,

    /// The client followed a redirect.
    Redirected,
}

impl From<SendError> for crate::error::Error {
    fn from(error: SendError) -> Self {
        use crate::error::{Error, TransientKind};
        match error {
            SendError::NotSent => Error::Transient {
                kind: TransientKind::NotSent,
            },
            SendError::NoResponse => Error::Transient {
                kind: TransientKind::NoResponse,
            },
            SendError::Oversize => Error::Decode,
            SendError::Redirected => Error::Redirected,
        }
    }
}

/// A buffered response. The body is zeroized when dropped: a token
/// response carries the token, and error pages may echo the request's
/// credential.
pub(crate) struct Response {
    pub(crate) status: StatusCode,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Zeroizing<Vec<u8>>,
}

impl Response {
    /// The response's `Date` header, as Unix seconds.
    pub(crate) fn date(&self) -> Option<i64> {
        crate::api::response_date(&self.headers)
    }

    /// This response classified as an error.
    pub(crate) fn api_error(&self) -> ApiError {
        ApiError::classify(self.status, &self.headers, ErrorBody::Bytes(&self.body))
    }
}
