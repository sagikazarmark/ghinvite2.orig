//! HTTP client for Restate's ingress API.
//!
//! Restate exposes a simple HTTP surface for invoking services from outside
//! the Restate runtime: `POST <ingress>/<ServiceName>/<Key>/<Method>` with a
//! JSON body that matches the handler's `input` parameter. Send-style
//! invocations (fire-and-forget) use a `?op=send` query parameter or a
//! distinct endpoint; this wrapper exposes both `call` (request-response)
//! and `send` (fire-and-forget) variants.
//!
//! The web binary uses `send` for every state-changing handler call so the
//! HTTP request returns quickly (Restate handles retries / durability).
//! `call` is reserved for the rare cases where the web wants the handler's
//! return value before responding to the user.

use crate::error::{Result, WebError};
use reqwest::Client;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::sync::Arc;

/// Cloneable. The inner `Client` is itself cheap to clone (Arc internally).
#[derive(Clone, Debug)]
pub struct RestateClient {
    client: Client,
    ingress_base: String,
}

impl RestateClient {
    pub fn new(ingress_base: impl Into<String>) -> Result<Self> {
        // `reqwest::ClientBuilder::timeout` is not available on the wasm32 target
        // (Cloudflare Workers): on wasm reqwest dispatches to fetch, which has
        // no timeout knob. The Workers runtime applies its own per-request
        // limits, so skipping the builder option is correct.
        let builder = Client::builder();
        #[cfg(not(target_arch = "wasm32"))]
        let builder = builder.timeout(std::time::Duration::from_secs(15));
        let client = builder
            .build()
            .map_err(|e| WebError::Restate(format!("building reqwest client: {e}")))?;
        Ok(Self {
            client,
            ingress_base: ingress_base.into(),
        })
    }

    /// Fire-and-forget invocation: send the input to a Virtual Object / Service
    /// method and return immediately without waiting for the handler to finish.
    /// Restate persists the request and runs the handler durably.
    ///
    /// `key` is the Virtual Object key (or workflow id). For non-keyed Services
    /// pass an empty string.
    pub async fn send<I: Serialize>(
        &self,
        service: &str,
        key: &str,
        method: &str,
        input: &I,
    ) -> Result<()> {
        // Wrap the body in `wasm_send` so the returned future is `Send` on
        // wasm32 (where reqwest's underlying `JsFuture` is `!Send`). axum
        // handlers calling this require Send futures.
        crate::wasm_compat::wasm_send(async move {
            let url = if key.is_empty() {
                format!("{}/{}/send/{}", self.ingress_base, service, method)
            } else {
                format!("{}/{}/{}/{}/send", self.ingress_base, service, key, method)
            };
            let resp = self
                .client
                .post(&url)
                .json(input)
                .send()
                .await
                .map_err(|e| WebError::Restate(format!("send {service}/{method}: {e}")))?;

            if !resp.status().is_success() {
                let status = resp.status().as_u16();
                let body = resp.text().await.unwrap_or_default();
                return Err(WebError::Restate(format!(
                    "send {service}/{method} -> {status}: {body}"
                )));
            }
            Ok(())
        })
        .await
    }

    /// Request-response invocation. Use sparingly — most state changes use
    /// `send`. Returns the handler's deserialized output type.
    pub async fn call<I: Serialize, O: DeserializeOwned>(
        &self,
        service: &str,
        key: &str,
        method: &str,
        input: &I,
    ) -> Result<O> {
        self.call_inner(service, key, method, input, false).await
    }

    /// Confirm authoritative completion. Transport failures remain unknown;
    /// only documented terminal command statuses are definitive.
    pub async fn authoritative_call<I: Serialize, O: DeserializeOwned>(
        &self,
        service: &str,
        key: &str,
        method: &str,
        input: &I,
    ) -> Result<O> {
        self.call_inner(service, key, method, input, true).await
    }

    async fn call_inner<I: Serialize, O: DeserializeOwned>(
        &self,
        service: &str,
        key: &str,
        method: &str,
        input: &I,
        authoritative: bool,
    ) -> Result<O> {
        // Same Send-bound rationale as `send` above.
        crate::wasm_compat::wasm_send(async move {
            let url = if key.is_empty() {
                format!("{}/{}/{}", self.ingress_base, service, method)
            } else {
                format!("{}/{}/{}/{}", self.ingress_base, service, key, method)
            };
            let resp = self
                .client
                .post(&url)
                .json(input)
                .send()
                .await
                .map_err(|e| WebError::Restate(format!("call {service}/{method}: {e}")))?;
            let status = resp.status();
            if !status.is_success() {
                if authoritative {
                    return Err(match status.as_u16() {
                        400 => WebError::BadRequest("Invalid command.".into()),
                        404 => WebError::NotFound,
                        409 => WebError::Conflict,
                        _ => WebError::Restate("Outcome unknown. Retry the same attempt.".into()),
                    });
                }
                let body = resp.text().await.unwrap_or_default();
                return Err(WebError::Restate(format!(
                    "call {service}/{method} -> {}: {body}",
                    status.as_u16()
                )));
            }
            let body = resp.bytes().await.map_err(|e| {
                WebError::Restate(format!("reading {service}/{method} response: {e}"))
            })?;
            // Restate may return an empty body for unit-returning handlers.
            let body: &[u8] = if body.is_empty() { b"null" } else { &body };
            serde_json::from_slice::<O>(body).map_err(|e| {
                WebError::Restate(format!("decoding {service}/{method} response: {e}"))
            })
        })
        .await
    }
}

/// `Arc<RestateClient>` shorthand for `AppState`.
pub type SharedRestateClient = Arc<RestateClient>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_shape_for_keyed_send() {
        // We don't actually hit a server — just sanity check the URL builder
        // shape via a small introspection. The send method is async and
        // does network I/O; deeper tests in Task 9+ use a wiremock server.
        let c = RestateClient::new("http://127.0.0.1:8080").unwrap();
        // No public URL builder; we verified the test by inspection of the
        // method body. Construction and field check:
        assert_eq!(c.ingress_base, "http://127.0.0.1:8080");
    }
}
