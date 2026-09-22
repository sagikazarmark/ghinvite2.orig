//! GitHub-touching code paths for ghinvite. Two facade clients on top of one
//! transport: [`oauth::UserApiClient`] (user-token, used by the web binary) and
//! [`installation::InstallationClient`] (App-JWT → installation token, used by
//! the Restate Service Worker). Webhook reception lives in the web crate.
//!
//! Every HTTP call funnels through [`transport::HttpTransport`] so impls can be
//! swapped (production, native and Cloudflare Workers:
//! [`transport::ReqwestTransport`]; tests: [`mocks::MockTransport`] behind the
//! `test-mock` feature).

pub mod error;
pub mod installation;
pub mod jwt;
pub mod oauth;
mod pagination;
pub mod payloads;
mod redact;
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
pub use error::{Error, RateLimit, RateLimitScope, Result};
pub use installation::{
    CollaboratorAddition, CollaboratorRole, InstallationClient, InvitationDeletion,
};
pub use oauth::{AuthorizeUrl, OAuthConfig, OrgMembership, UserApiClient};
// `redact` itself stays private — bodies are summarised inside this crate —
// but the rule for bounding an upstream-supplied code is needed by the web
// crate too, which reads the same kind of value off GitHub's callback query
// string and off the installations it lists.
pub use redact::{OAUTH_ERROR_CODES, UNRECOGNIZED, bounded_upstream_code};
pub use transport::{HttpTransport, Method, Request, Response};
