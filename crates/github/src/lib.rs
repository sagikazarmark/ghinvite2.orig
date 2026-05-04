//! GitHub-touching code paths for ghinvite. Two facade clients on top of one
//! transport: [`oauth::UserApiClient`] (user-token, used by the web binary) and
//! [`installation::InstallationClient`] (App-JWT → installation token, used by
//! the Restate Service Worker). [`hmac::verify_signature_256`] is a standalone
//! helper for the webhook receiver; it does not consume the transport.
//!
//! Every HTTP call funnels through [`transport::HttpTransport`] so impls can be
//! swapped (production: [`transport::ReqwestTransport`]; tests:
//! [`mocks::MockTransport`] behind the `test-mock` feature).

pub mod error;
pub mod hmac;
pub mod installation;
pub mod jwt;
pub mod oauth;
pub mod payloads;
pub mod token_cache;
pub mod transport;

#[cfg(any(test, feature = "test-mock"))]
pub mod mocks;

#[cfg(target_arch = "wasm32")]
pub mod wasm_smoke;

// Re-exports filled in as each module gains its public types:
pub use error::{Error, Result};
// pub use installation::InstallationClient;
pub use oauth::{AuthorizeUrl, UserApiClient};
pub use transport::{HttpTransport, Method, Request, Response};
