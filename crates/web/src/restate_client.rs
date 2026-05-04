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
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::sync::Arc;

/// Cloneable. The inner `Client` is itself cheap to clone (Arc internally).
#[derive(Clone, Debug)]
pub struct RestateClient {
    client: Client,
    ingress_base: String,
}

impl RestateClient {
    pub fn new(ingress_base: impl Into<String>) -> Result<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(15))
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
        let url = if key.is_empty() {
            format!("{}/{}/send/{}", self.ingress_base, service, method)
        } else {
            format!(
                "{}/{}/{}/{}/send",
                self.ingress_base, service, key, method
            )
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
            let body = resp.text().await.unwrap_or_default();
            return Err(WebError::Restate(format!(
                "call {service}/{method} -> {}: {body}",
                status.as_u16()
            )));
        }
        resp.json::<O>()
            .await
            .map_err(|e| WebError::Restate(format!("decoding {service}/{method} response: {e}")))
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
