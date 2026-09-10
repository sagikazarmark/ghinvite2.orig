//! `wasm32-unknown-unknown` compile smoke. The body intentionally references
//! every public surface of this crate so a missing/incompatible `wasm` feature
//! shows up at compile time rather than during deployment.

#![cfg(target_arch = "wasm32")]

use crate::error::Error;
use crate::installation::InstallationClient;
use crate::jwt::AppJwtSigner;
use crate::oauth::{AuthorizeUrl, OAuthConfig, UserApiClient};
use crate::token_cache::TokenCache;
use crate::transport::{HttpTransport, Method, Request, Response};

/// Inert function that exercises every public type without running anything.
/// Exists only so the linker has to resolve all references.
#[allow(dead_code)]
pub fn _wasm_link_check(
    _t: std::sync::Arc<dyn HttpTransport>,
    _signer: AppJwtSigner,
    _ua: UserApiClient,
    _ic: InstallationClient,
    _au: AuthorizeUrl,
    _cfg: OAuthConfig,
    _cache: TokenCache,
    _req: Request,
    _resp: Response,
    _method: Method,
    _err: Error,
) {
}
