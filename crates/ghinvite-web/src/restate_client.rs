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

use crate::error::{IngressFailure, Result, WebError};
use reqwest::Client;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::sync::Arc;

/// Server-only ingress authentication. Deliberately not serializable.
#[derive(Clone, Debug)]
pub struct RestateAuth {
    authorization: Option<HeaderValue>,
}

impl RestateAuth {
    /// Both native environment and Worker bindings use these exact rules.
    /// An omitted mode means `bearer`; missing secrets never disable auth.
    pub fn from_config(mode: Option<&str>, api_key: Option<&str>) -> Result<Self> {
        match mode.unwrap_or("bearer") {
            "local-unauthenticated" if api_key.is_none() => {
                return Ok(Self::local_unauthenticated());
            }
            "bearer" => (),
            _ => {
                return Err(WebError::Restate(IngressFailure::Config(
                    "Invalid GHINVITE_RESTATE_AUTH configuration.",
                )));
            }
        }
        let api_key = api_key
            .filter(|key| !key.is_empty() && key.bytes().all(|b| b.is_ascii_graphic()))
            .ok_or(WebError::Restate(IngressFailure::Config(
                "GHINVITE_RESTATE_API_KEY is required and must be a non-empty token.",
            )))?;
        let mut authorization = HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map_err(|_| WebError::Restate(IngressFailure::Config("Invalid ingress API key.")))?;
        authorization.set_sensitive(true);
        Ok(Self {
            authorization: Some(authorization),
        })
    }

    pub fn local_unauthenticated() -> Self {
        Self {
            authorization: None,
        }
    }
}

/// Cloneable. The inner `Client` is itself cheap to clone (Arc internally).
#[derive(Clone)]
pub struct RestateClient {
    client: Client,
    ingress_base: String,
}

impl std::fmt::Debug for RestateClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RestateClient").finish_non_exhaustive()
    }
}

impl RestateClient {
    /// Credential-free client for local runtimes and tests. Deployment entry
    /// points use `with_auth` with explicit runtime configuration instead.
    pub fn new(ingress_base: impl Into<String>) -> Result<Self> {
        Self::with_auth(ingress_base, RestateAuth::local_unauthenticated())
    }

    pub fn with_auth(ingress_base: impl Into<String>, auth: RestateAuth) -> Result<Self> {
        let ingress_base = ingress_base.into();
        let invalid_url = || {
            WebError::Restate(IngressFailure::Config(
                "Ingress must be an HTTP(S) URL without credentials, query, or fragment.",
            ))
        };
        let url = url::Url::parse(&ingress_base).map_err(|_| invalid_url())?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid_url());
        }
        let loopback = match url.host() {
            Some(url::Host::Domain("localhost")) => true,
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            _ => false,
        };
        if auth.authorization.is_some() && url.scheme() != "https" && !loopback {
            return Err(WebError::Restate(IngressFailure::Config(
                "Authenticated ingress requires HTTPS (except loopback development).",
            )));
        }
        // Wasm uses RequestBuilder::timeout below: Worker execution limits
        // do not impose a wall-clock deadline on upstream fetches.
        let mut headers = HeaderMap::new();
        if let Some(authorization) = auth.authorization {
            headers.insert(AUTHORIZATION, authorization);
        }
        let builder = Client::builder().default_headers(headers);
        #[cfg(not(target_arch = "wasm32"))]
        let builder = builder
            .timeout(std::time::Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none());
        let client = builder.build().map_err(|_| {
            WebError::Restate(IngressFailure::Config("Could not build ingress client."))
        })?;
        Ok(Self {
            client,
            ingress_base: ingress_base.trim_end_matches('/').into(),
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
                .timeout(std::time::Duration::from_secs(15))
                .json(input)
                .send()
                .await
                .map_err(|_| {
                    WebError::Restate(IngressFailure::unreachable("ingress send unreachable"))
                })?;

            if !resp.status().is_success() {
                // A send the ingress refused may still have been persisted
                // before the response went wrong, so the command's effect is
                // not ruled out.
                return Err(WebError::Restate(IngressFailure::OutcomeUnknown {
                    detail: "ingress send rejected",
                    status: Some(resp.status().as_u16()),
                }));
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
                // A single reqwest deadline spans headers and body consumption
                // on both native and Wasm; a timeout never proves no effect.
                .timeout(std::time::Duration::from_secs(15))
                .json(input)
                .send()
                .await
                .map_err(|_| {
                    WebError::Restate(IngressFailure::unreachable("ingress unreachable"))
                })?;
            let status = resp.status();
            if !status.is_success() {
                if authoritative {
                    return Err(match status.as_u16() {
                        400 => WebError::BadRequest("Invalid command.".into()),
                        404 => WebError::NotFound,
                        409 => WebError::Conflict,
                        other => WebError::Restate(IngressFailure::OutcomeUnknown {
                            detail: "ingress returned an unhandled status",
                            status: Some(other),
                        }),
                    });
                }
                return Err(WebError::Restate(IngressFailure::Rejected {
                    status: status.as_u16(),
                }));
            }
            let body = resp.bytes().await.map_err(|_| {
                WebError::Restate(IngressFailure::OutcomeUnknown {
                    detail: "ingress response body unreadable",
                    status: Some(status.as_u16()),
                })
            })?;
            // Restate may return an empty body for unit-returning handlers.
            let body: &[u8] = if body.is_empty() { b"null" } else { &body };
            // Deserializer errors can quote untrusted response values, including
            // reflected credentials. Never pass them to logs or browser errors.
            serde_json::from_slice::<O>(body).map_err(|_| {
                // The ingress answered; we could not use it. Keep the status it
                // answered with — the outcome is unknown, not statusless.
                WebError::Restate(IngressFailure::OutcomeUnknown {
                    detail: "ingress response could not be decoded",
                    status: Some(status.as_u16()),
                })
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
        // does network I/O; `tests/restate_ingress.rs` uses a wiremock server.
        let c = RestateClient::new("http://127.0.0.1:8080").unwrap();
        // No public URL builder; we verified the test by inspection of the
        // method body. Construction and field check:
        assert_eq!(c.ingress_base, "http://127.0.0.1:8080");
    }
}
