//! Cloudflare Workers entry point for the Restate service binary.
//!
//! Wasm32-only: all the wasm-bindgen / `worker` / `storage-d1` APIs we touch
//! here only exist on `wasm32-unknown-unknown`. On native targets the crate
//! is intentionally empty so `cargo check --workspace` (which defaults to the
//! host triple) doesn't drag in JS bindings or try to link a cdylib that
//! depends on them. To touch this code, build with
//! `--target wasm32-unknown-unknown`.
#![cfg(target_arch = "wasm32")]

use ghinvite_config::apps::{AppStage, Deployment, organization_delivery_enabled, parse_mint_apps};
use http_body_util::BodyExt;
use restate_sdk::endpoint::{HandleOptions, ProtocolMode};
use std::sync::Arc;
use worker::{Body, Context, Env, HttpRequest, event};

#[cfg(feature = "runtime-tests")]
mod fixture;

/// Map non-`worker::Error` failures into `worker::Error` for the fetch entry
/// point.
fn worker_err(e: impl std::fmt::Display) -> worker::Error {
    worker::Error::RustError(e.to_string())
}

/// Deployed builds talk to GitHub only; the `runtime-tests` harness builds
/// also accept `GITHUB_API_BASE` (and the other `GITHUB_*_BASE`).
const DEPLOYMENT: Deployment = if cfg!(feature = "runtime-tests") {
    Deployment::Development
} else {
    Deployment::Production
};

/// The installed apps' `GithubApps` over the Worker fetch transport. Token
/// providers are isolate-wide (`ghinvite_github::apps`, PD-4).
fn github_from_env(env: &Env) -> worker::Result<(Arc<ghinvite_workflows::RepositoryGithub>, bool)> {
    let stage = AppStage::from_config(
        env.var("P1_APP_STAGE")
            .ok()
            .map(|v| v.to_string())
            .as_deref(),
    )
    .map_err(worker_err)?;
    let organization_enabled = organization_delivery_enabled(
        stage,
        env.var("GH_ORG_DELIVERY_ENABLED")
            .ok()
            .map(|v| v.to_string())
            .as_deref(),
    )
    .map_err(worker_err)?;
    let apps = parse_mint_apps(stage, DEPLOYMENT, |field| {
        if matches!(field, "GH_REPO_PRIVATE_KEYS" | "GH_ORG_PRIVATE_KEYS") {
            env.secret(field).ok().map(|v| v.to_string())
        } else {
            env.var(field).ok().map(|v| v.to_string())
        }
    })
    .map_err(worker_err)?;
    let github_transport = Arc::new(ghinvite_github::worker::WorkerFetchTransport::new());
    let typed = ghinvite_workflows::RepositoryGithub::new(
        github_transport,
        apps.github.api(),
        apps.installed,
    )
    .map_err(worker_err)?;
    Ok((Arc::new(typed), organization_enabled))
}

#[event(fetch)]
async fn fetch(req: HttpRequest, env: Env, _ctx: Context) -> worker::Result<http::Response<Body>> {
    console_error_panic_hook::set_once();
    let _ = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(|| ConsoleWriter)
        .try_init();

    #[cfg(feature = "runtime-tests")]
    if req.uri().path().starts_with("/__fixture/") {
        return fixture::fetch(req, &env).await;
    }

    ghinvite_workflows::delivery_run_fixture::configure(
        env.var("P1_DELIVERY_RUN_RETRY_FIXTURE")
            .ok()
            .map(|v| v.to_string())
            .as_deref(),
    )
    .map_err(worker_err)?;
    let (github_apps, organization_enabled) = github_from_env(&env)?;
    let reissue_setting = env.var("GH_REISSUE_ENABLED").ok().map(|v| v.to_string());
    let reissue_enabled =
        ghinvite_config::reissue::enabled(reissue_setting.as_deref()).map_err(worker_err)?;
    let bulk_setting = env.var("GH_BULK_ENABLED").ok().map(|v| v.to_string());
    let bulk_enabled =
        ghinvite_config::bulk::enabled(bulk_setting.as_deref()).map_err(worker_err)?;
    let email_ring = ghinvite_config::bulk::email_keyring(
        bulk_enabled && organization_enabled,
        env.secret("P1_EMAIL_KEYRING").ok().map(|v| v.to_string()),
    )
    .map_err(worker_err)?;
    let identity = ghinvite_workflows::endpoint_identity::EndpointIdentity::required(
        env.var("RESTATE_IDENTITY_KEYS")
            .ok()
            .map(|v| v.to_string())
            .as_deref(),
    )
    .map_err(worker_err)?;
    let db = env.d1("DB")?;
    let storage = Arc::new(ghinvite_storage_d1::D1Storage::new(db));

    let state = ghinvite_workflows::AppState::new(storage.clone()).with_github_apps(github_apps);
    let state = if organization_enabled {
        state
            .with_organization_delivery(storage.clone())
            .with_pre_activation_cancellation()
    } else {
        state
    };
    #[cfg(feature = "cancellation-fixture")]
    let state = state
        .with_invite_budget(storage.clone())
        .with_pre_activation_cancellation();
    let access_setting = env
        .var("GH_ACCESS_REMOVAL_ENABLED")
        .ok()
        .map(|value| value.to_string());
    let state = if ghinvite_config::apps::access_removal_enabled(access_setting.as_deref())
        .map_err(worker_err)?
    {
        state.with_access_grants(storage.clone())
    } else {
        state
    };
    let state = if reissue_enabled {
        state.with_reissues(storage.clone())
    } else {
        state
    };
    let state = if bulk_enabled {
        state.with_bulk_invites(storage.clone())
    } else {
        state
    };
    let state = if bulk_enabled && organization_enabled {
        let ring = email_ring.ok_or_else(|| worker_err("P1_EMAIL_KEYRING invalid"))?;
        state.with_email_deliveries(
            storage.clone(),
            Arc::new(
                ghinvite_email_vault::EmailVault::new(storage.clone(), ring)
                    .with_review_storage(storage.clone()),
            ),
        )
    } else {
        state
    };
    let endpoint = ghinvite_workflows::build_endpoint_with_identity(state, storage, &identity)
        .map_err(worker_err)?;

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

// workerd has no performance.mark/measure for tracing-wasm's span hooks.
struct ConsoleWriter;
impl std::io::Write for ConsoleWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        worker::console_log!("{}", String::from_utf8_lossy(bytes));
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
