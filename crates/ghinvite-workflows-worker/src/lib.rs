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
use restate_sdk::endpoint::{Endpoint, HandleOptions, ProtocolMode};
use std::cell::RefCell;
use std::sync::Arc;
use worker::{Body, Context, Env, HttpRequest, event, wasm_bindgen::JsValue};

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

/// One var or secret that composing the endpoint read, with its value.
struct EnvRead {
    secret: bool,
    name: &'static str,
    value: Option<String>,
}

impl EnvRead {
    fn current(env: &Env, secret: bool, name: &str) -> Option<String> {
        if secret {
            env.secret(name).ok().map(|v| v.to_string())
        } else {
            env.var(name).ok().map(|v| v.to_string())
        }
    }
}

/// Reads vars and secrets for one composition and records every read, so a
/// later request can check that its env still has the same values.
/// Composition is a pure function of these values and the `DB` binding:
/// equal values take the same branches and so read the same names.
struct EnvReader<'a> {
    env: &'a Env,
    reads: RefCell<Vec<EnvRead>>,
}

impl EnvReader<'_> {
    fn var(&self, name: &'static str) -> Option<String> {
        self.read(false, name)
    }

    fn secret(&self, name: &'static str) -> Option<String> {
        self.read(true, name)
    }

    fn read(&self, secret: bool, name: &'static str) -> Option<String> {
        let value = EnvRead::current(self.env, secret, name);
        self.reads.borrow_mut().push(EnvRead {
            secret,
            name,
            value: value.clone(),
        });
        value
    }
}

/// The installed apps' `GithubApps` over the Worker fetch transport. Token
/// providers are isolate-wide (`ghinvite_github::apps`, PD-4).
fn github_from_env(
    env: &EnvReader,
) -> worker::Result<(Arc<ghinvite_workflows::RepositoryGithub>, bool)> {
    let stage = AppStage::from_config(env.var("P1_APP_STAGE").as_deref()).map_err(worker_err)?;
    let organization_enabled =
        organization_delivery_enabled(stage, env.var("GH_ORG_DELIVERY_ENABLED").as_deref())
            .map_err(worker_err)?;
    let apps = parse_mint_apps(stage, DEPLOYMENT, |field| {
        if matches!(field, "GH_REPO_PRIVATE_KEYS" | "GH_ORG_PRIVATE_KEYS") {
            env.secret(field)
        } else {
            env.var(field)
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

/// Compose every service from the env. Returns the endpoint and the `DB`
/// binding object its storage holds.
fn compose(env: &EnvReader) -> worker::Result<(Endpoint, JsValue)> {
    ghinvite_workflows::delivery_run_fixture::configure(
        env.var("P1_DELIVERY_RUN_RETRY_FIXTURE").as_deref(),
    )
    .map_err(worker_err)?;
    let (github_apps, organization_enabled) = github_from_env(env)?;
    let reissue_setting = env.var("GH_REISSUE_ENABLED");
    let reissue_enabled =
        ghinvite_config::reissue::enabled(reissue_setting.as_deref()).map_err(worker_err)?;
    let bulk_setting = env.var("GH_BULK_ENABLED");
    let bulk_enabled =
        ghinvite_config::bulk::enabled(bulk_setting.as_deref()).map_err(worker_err)?;
    let email_ring = ghinvite_config::bulk::email_keyring(
        bulk_enabled && organization_enabled,
        env.secret("P1_EMAIL_KEYRING"),
    )
    .map_err(worker_err)?;
    let identity = ghinvite_workflows::endpoint_identity::EndpointIdentity::required(
        env.var("RESTATE_IDENTITY_KEYS").as_deref(),
    )
    .map_err(worker_err)?;
    let db = env.env.d1("DB")?;
    let db_binding = AsRef::<JsValue>::as_ref(&db).clone();
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
    let access_setting = env.var("GH_ACCESS_REMOVAL_ENABLED");
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
    Ok((endpoint, db_binding))
}

/// An endpoint and what it was composed from: every env read and the `DB`
/// binding object its storage holds.
struct Composed {
    reads: Vec<EnvRead>,
    db: JsValue,
    endpoint: Endpoint,
}

impl Composed {
    /// The same `DB` object (`===`) and the same value for every read.
    fn serves(&self, env: &Env) -> bool {
        env.d1("DB")
            .is_ok_and(|db| *AsRef::<JsValue>::as_ref(&db) == self.db)
            && self
                .reads
                .iter()
                .all(|read| EnvRead::current(env, read.secret, read.name) == read.value)
    }
}

thread_local! {
    // A Worker isolate runs one thread, so this is the isolate's endpoint.
    static ENDPOINT: RefCell<Option<Composed>> = const { RefCell::new(None) };
}

/// The isolate's endpoint: composed on first use, and again whenever a
/// request's env differs from the one it was composed for.
///
/// Composing binds every service, and binding builds its discovery metadata
/// (the schemars schemas of every handler): ~11.5 ms per request in debug
/// Wasm, on each of the ~45 Restate request-response rounds of an admission.
/// In production an isolate's env does not change (a deploy or secret change
/// starts new isolates), but `env` still arrives per request and fixtures
/// swap `env.DB` for a fault-injecting proxy, so the cache is keyed on
/// everything composition read rather than assumed constant: a swapped `DB`
/// or a changed var recomposes and replaces the entry, and an invalid env
/// fails every request and is never cached. Composition is synchronous, so
/// concurrent requests cannot interleave inside it, and requests in flight
/// keep the endpoint they started with. Restate identity verification is
/// part of `Endpoint::handle`, so it still runs on every request.
fn endpoint(env: &Env) -> worker::Result<Endpoint> {
    let cached = ENDPOINT.with_borrow(|cached| {
        cached
            .as_ref()
            .filter(|cached| cached.serves(env))
            .map(|cached| cached.endpoint.clone())
    });
    if let Some(endpoint) = cached {
        return Ok(endpoint);
    }
    let reader = EnvReader {
        env,
        reads: RefCell::default(),
    };
    let (endpoint, db) = compose(&reader)?;
    ENDPOINT.set(Some(Composed {
        reads: reader.reads.into_inner(),
        db,
        endpoint: endpoint.clone(),
    }));
    Ok(endpoint)
}

/// `true` for a path the pinned restate-sdk (8a39311, `endpoint/mod.rs:256`)
/// cannot parse without `parts.len() - 3` underflowing: fewer than three
/// `/`-separated parts, not ending in `health` or `discover`. Once such a
/// request's signature verifies, debug builds panic there, which traps and
/// poisons the whole Wasm instance (endpoint cache included); release builds
/// answer 400. No such path can name a handler, so the Worker answers 404
/// itself and never hands it to the endpoint.
fn sdk_cannot_route(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    parts.len() < 3 && !matches!(parts.last(), Some(&"health" | &"discover"))
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

    let endpoint = endpoint(&env)?;
    if sdk_cannot_route(req.uri().path()) {
        let mut response = http::Response::new(Body::empty());
        *response.status_mut() = http::StatusCode::NOT_FOUND;
        return Ok(response);
    }

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
