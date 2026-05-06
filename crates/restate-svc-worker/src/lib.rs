//! Cloudflare Workers entry point for the Restate service binary.
//!
//! Wasm32-only: all the wasm-bindgen / `worker` / `storage-d1` APIs we touch
//! here only exist on `wasm32-unknown-unknown`. On native targets the crate
//! is intentionally empty so `cargo check --workspace` (which defaults to the
//! host triple) doesn't drag in JS bindings or try to link a cdylib that
//! depends on them. To touch this code, build with
//! `--target wasm32-unknown-unknown`.
#![cfg(target_arch = "wasm32")]

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64_STANDARD;
use http_body_util::BodyExt;
use restate_sdk::endpoint::{HandleOptions, ProtocolMode};
use std::sync::Arc;
use worker::{Body, Context, Env, HttpRequest, event};

/// Map non-`worker::Error` failures into `worker::Error` for the fetch entry
/// point.
fn worker_err(e: impl std::fmt::Display) -> worker::Error {
    worker::Error::RustError(e.to_string())
}

/// Build an [`InstallationClient`] from the wrangler-managed env. Two pieces
/// of secret config are required:
///   * `GHINVITE_GITHUB_APP_ID` — the GitHub App's numeric id, as a `var`.
///   * `GHINVITE_GITHUB_APP_PRIVATE_KEY` — the App's PEM private key,
///     **base64-encoded** because wrangler secrets are single-line strings.
///     Generate with `base64 -w0 private-key.pem | wrangler secret put
///     GHINVITE_GITHUB_APP_PRIVATE_KEY`.
fn github_client_from_env(env: &Env) -> worker::Result<github::InstallationClient> {
    let app_id_str = env.var("GHINVITE_GITHUB_APP_ID")?.to_string();
    let app_id: u64 = app_id_str
        .parse()
        .map_err(|e| worker::Error::RustError(format!("GHINVITE_GITHUB_APP_ID not a u64: {e}")))?;

    let pem_b64 = env.secret("GHINVITE_GITHUB_APP_PRIVATE_KEY")?.to_string();
    let pem_bytes = B64_STANDARD.decode(pem_b64.trim()).map_err(|e| {
        worker::Error::RustError(format!(
            "GHINVITE_GITHUB_APP_PRIVATE_KEY not valid base64: {e}"
        ))
    })?;
    let pem = std::str::from_utf8(&pem_bytes).map_err(|e| {
        worker::Error::RustError(format!(
            "GHINVITE_GITHUB_APP_PRIVATE_KEY decoded bytes are not utf-8: {e}"
        ))
    })?;
    let signer = github::jwt::AppJwtSigner::from_pem(app_id, pem).map_err(worker_err)?;

    let transport: Arc<dyn github::HttpTransport> =
        Arc::new(github::transport::ReqwestTransport::new().map_err(worker_err)?);
    Ok(github::InstallationClient::new(transport, signer))
}

#[event(fetch)]
async fn fetch(req: HttpRequest, env: Env, _ctx: Context) -> worker::Result<http::Response<Body>> {
    console_error_panic_hook::set_once();
    // `tracing_wasm::set_as_global_default` panics on repeat calls within the
    // same wasm instance — Workers can reuse instances across invocations.
    let _ = tracing_wasm::try_set_as_global_default();

    let db = env.d1("DB")?;
    let storage: Arc<dyn storage::Storage> = Arc::new(storage_d1::D1Storage::new(db));
    let github_client = Arc::new(github_client_from_env(&env)?);

    let state = restate_svc::AppState::new(storage, github_client);
    // RESTATE_IDENTITY_KEY is required in production — it is the public key
    // Restate Cloud uses to sign inbound requests. Without it the endpoint
    // accepts any caller. Set via:
    //   wrangler secret put RESTATE_IDENTITY_KEY --config wrangler/restate-svc.toml
    let identity_key = env
        .secret("RESTATE_IDENTITY_KEY")
        .map(|s| s.to_string().trim().to_string())
        .ok();
    if identity_key.is_none() {
        worker::console_warn!(
            "RESTATE_IDENTITY_KEY is not set — endpoint will accept unsigned requests. \
             This is only safe for local dev."
        );
    }
    let endpoint =
        restate_svc::build_endpoint(state, identity_key.as_deref()).map_err(worker_err)?;

    // Cloudflare Workers does not support true bidirectional streaming — it
    // buffers the entire request body before passing it to the worker. We
    // therefore must use `RequestResponse` mode (matching the upstream PoC at
    // https://github.com/sagikazarmark/poc-restate-cloudflare-worker).
    let response = endpoint.handle_with_options(
        req,
        HandleOptions {
            protocol_mode: ProtocolMode::RequestResponse,
        },
    );

    let (parts, body) = response.into_parts();
    let body = Body::from_stream(body.into_data_stream())?;
    Ok(http::Response::from_parts(parts, body))
}
