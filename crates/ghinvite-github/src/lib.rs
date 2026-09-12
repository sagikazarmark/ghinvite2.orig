//! GitHub-touching code paths for ghinvite. Two facade clients on top of one
//! transport: [`oauth::UserApiClient`] (user-token, used by the web binary) and
//! [`installation::InstallationClient`] (App-JWT → installation token, used by
//! the Restate Service Worker). Webhook reception lives in the web crate.
//!
//! Every HTTP call funnels through [`transport::HttpTransport`] so impls can be
//! swapped (production native: [`transport::ReqwestTransport`]; tests:
//! [`mocks::MockTransport`] behind the `test-mock` feature). The wasm32 /
//! Cloudflare Workers production transport (`WorkerFetchTransport`) is owned
//! by Plan 3 — reqwest's wasm response future is `!Send` and so cannot satisfy
//! the [`transport::HttpTransport`] `Send + Sync + 'static` bound.

pub mod error;
pub mod installation;
pub mod jwt;
pub mod oauth;
pub mod payloads;
pub mod token_cache;
pub mod transport;
mod util;

#[cfg(any(test, feature = "test-mock"))]
pub mod mocks;

#[cfg(feature = "test-stub")]
pub mod stub;

#[cfg(target_arch = "wasm32")]
pub mod wasm_smoke;

// Re-exports filled in as each module gains its public types:
pub use error::{Error, Result};
pub use installation::InstallationClient;
pub use oauth::{AuthorizeUrl, OAuthConfig, UserApiClient};
pub use transport::{HttpTransport, Method, Request, Response};
