//! One request through reqwest, with a bounded response.

use crate::SignInError;
use octoauth::TransientKind;
use std::time::Duration;

/// Largest response accepted. Sign-in responses are a few kilobytes.
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// Per-request deadline.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(30);

/// A buffered response. The body is zeroized when dropped: token responses
/// carry credentials, and error pages may echo the request's.
pub(crate) struct Response {
    pub(crate) status: http::StatusCode,
    pub(crate) headers: http::HeaderMap,
    pub(crate) body: zeroize::Zeroizing<Vec<u8>>,
}

impl Response {
    pub(crate) fn api_error(&self) -> SignInError {
        SignInError::Api(octoauth::ApiError::classify(
            self.status,
            &self.headers,
            octoauth::ErrorBody::Bytes(&self.body),
        ))
    }
}

/// The default client: no redirects natively (on wasm32 `fetch` follows
/// them, and a redirected response is discarded below) and a timeout.
pub(crate) fn default_client() -> Option<reqwest::Client> {
    let builder = reqwest::Client::builder();
    #[cfg(not(target_arch = "wasm32"))]
    let builder = builder
        .redirect(reqwest::redirect::Policy::none())
        .timeout(TIMEOUT);
    builder.build().ok()
}

pub(crate) async fn send(
    client: &reqwest::Client,
    mut request: reqwest::Request,
) -> Result<Response, SignInError> {
    *request.timeout_mut() = Some(TIMEOUT);
    let url = request.url().clone();
    let response = client.execute(request).await.map_err(send_error)?;
    if response.url() != &url {
        return Err(SignInError::Redirected);
    }
    if response
        .content_length()
        .is_some_and(|len| len > MAX_RESPONSE_BYTES as u64)
    {
        return Err(SignInError::Decode);
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

#[cfg(not(target_arch = "wasm32"))]
async fn read_bounded(
    mut response: reqwest::Response,
) -> Result<zeroize::Zeroizing<Vec<u8>>, SignInError> {
    let mut body = zeroize::Zeroizing::new(Vec::new());
    while let Some(chunk) = response.chunk().await.map_err(|_| no_response())? {
        push_bounded(&mut body, &chunk)?;
    }
    Ok(body)
}

/// Read the whole body, then bound it: reqwest's fetch backend streams only
/// with its `stream` feature, which cannot link next to `worker` 0.8. The
/// `Content-Length` check in [`send`] still refuses a declared oversize body
/// before it is read.
#[cfg(target_arch = "wasm32")]
async fn read_bounded(
    response: reqwest::Response,
) -> Result<zeroize::Zeroizing<Vec<u8>>, SignInError> {
    let bytes = response.bytes().await.map_err(|_| no_response())?;
    let mut body = zeroize::Zeroizing::new(Vec::new());
    let pushed = push_bounded(&mut body, &bytes);
    // The buffer may hold a token: zeroize it when nothing else shares it.
    if let Ok(mut bytes) = bytes.try_into_mut() {
        zeroize::Zeroize::zeroize(&mut bytes[..]);
    }
    pushed.map(|()| body)
}

fn push_bounded(body: &mut zeroize::Zeroizing<Vec<u8>>, chunk: &[u8]) -> Result<(), SignInError> {
    if chunk.len() > MAX_RESPONSE_BYTES - body.len() {
        return Err(SignInError::Decode);
    }
    // Grow by hand: a reallocation inside `Vec` would leave the old buffer,
    // which may hold a token, unzeroized.
    if body.capacity() - body.len() < chunk.len() {
        let capacity = (body.len() + chunk.len())
            .max(body.capacity() * 2)
            .min(MAX_RESPONSE_BYTES);
        let mut grown = zeroize::Zeroizing::new(Vec::with_capacity(capacity));
        grown.extend_from_slice(body);
        *body = grown;
    }
    body.extend_from_slice(chunk);
    Ok(())
}

fn no_response() -> SignInError {
    SignInError::Transient {
        kind: TransientKind::NoResponse,
    }
}

/// `NotSent` only when reqwest proves the request never left.
fn send_error(error: reqwest::Error) -> SignInError {
    #[cfg(not(target_arch = "wasm32"))]
    let not_sent = error.is_builder() || error.is_connect();
    #[cfg(target_arch = "wasm32")]
    let not_sent = error.is_builder();
    SignInError::Transient {
        kind: if not_sent {
            TransientKind::NotSent
        } else {
            TransientKind::NoResponse
        },
    }
}

/// An `Authorization` header value, marked sensitive so `Debug` redacts it.
pub(crate) fn authorization(value: &str) -> reqwest::header::HeaderValue {
    let mut header = reqwest::header::HeaderValue::from_str(value)
        .expect("credentials are validated printable ASCII");
    header.set_sensitive(true);
    header
}
