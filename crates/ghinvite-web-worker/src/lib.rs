//! Cloudflare Workers entry point for the web binary.
//!
//! Wasm32-only: all the wasm-bindgen / `worker` / `storage-d1` APIs we touch
//! here only exist on `wasm32-unknown-unknown`. On native targets the crate
//! is intentionally empty so `cargo check --workspace` (which defaults to the
//! host triple) doesn't drag in JS bindings or try to link a cdylib that
//! depends on them. To touch this code, build with
//! `--target wasm32-unknown-unknown`.
#![cfg(target_arch = "wasm32")]

mod authority_cache;
mod daily_digest;
mod rate_limit;

use ghinvite_web::session_store::{Backend, ProtectedStore};
use std::sync::{Arc, OnceLock};
use tower_sessions::{cookie::time::Duration, session::Id, session_store};
use worker::{Context, Env, HttpRequest, event};

/// `tower_sessions` requires `Debug + Send + Sync + 'static`. The inner
/// `KvStore` is `!Send + !Sync` upstream because it wraps a JS object — we
/// assert `Send + Sync` here under the same single-threaded-Workers
/// soundness argument used elsewhere in this workspace (see
/// `ghinvite_storage_d1::wasm_impl::WasmSend`).
pub struct KvSessionStore {
    kv: worker::kv::KvStore,
}

impl std::fmt::Debug for KvSessionStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KvSessionStore").finish_non_exhaustive()
    }
}

impl Clone for KvSessionStore {
    fn clone(&self) -> Self {
        Self {
            kv: self.kv.clone(),
        }
    }
}

// SAFETY: KvStore wraps JS objects that are !Send + !Sync upstream. This is
// sound only under the single-threaded Cloudflare Workers execution model
// where no OS threads exist and wasm32 has no thread-spawn capability. If
// SharedArrayBuffer-based threads are ever added to the build, these impls
// must be revisited.
unsafe impl Send for KvSessionStore {}
unsafe impl Sync for KvSessionStore {}

impl KvSessionStore {
    fn from_env(env: &Env) -> worker::Result<Self> {
        Ok(Self {
            kv: env.kv("SESSIONS")?,
        })
    }
}

#[async_trait::async_trait]
impl Backend for KvSessionStore {
    async fn insert(
        &self,
        id: &Id,
        payload: &[u8],
        expires_at: i64,
    ) -> session_store::Result<bool> {
        // KV has no atomic insert-if-absent. Random 128-bit IDs make concurrent
        // collisions negligible; refuse known collisions before sealing a new ID.
        if self.get(id).await?.is_some() {
            return Ok(false);
        }
        self.put(id, payload, expires_at).await?;
        Ok(true)
    }

    async fn put(&self, id: &Id, payload: &[u8], expires_at: i64) -> session_store::Result<()> {
        // Wrap the body in a Send-asserting future so the resulting
        // `Future` returned by `async_trait` (which requires `Send`) is
        // satisfied — the inner `JsFuture` from `worker::kv` is `!Send`.
        ghinvite_web::wasm_compat::wasm_send(async move {
            let key = format!("session:{id}");
            let put = self.kv.put_bytes(&key, payload).map_err(|_| kv_error())?;
            // KV requires >=60s retention, even when logical validity is shorter.
            // ProtectedStore enforces the authenticated deadline independently.
            let ttl = ghinvite_web::session_store::session_ttl(
                expires_at,
                chrono::Utc::now().timestamp(),
            );
            put.expiration_ttl(ttl)
                .execute()
                .await
                .map_err(|_| kv_error())
        })
        .await
    }

    async fn get(&self, session_id: &Id) -> session_store::Result<Option<Vec<u8>>> {
        let key = format!("session:{session_id}");
        ghinvite_web::wasm_compat::wasm_send(async move {
            self.kv.get(&key).bytes().await.map_err(|_| kv_error())
        })
        .await
    }

    async fn is_revoked(&self, id: &Id) -> session_store::Result<bool> {
        ghinvite_web::wasm_compat::wasm_send(async move {
            self.kv
                .get(&format!("revoked:{id}"))
                .bytes()
                .await
                .map(|value| value.is_some())
                .map_err(|_| kv_error())
        })
        .await
    }

    async fn revoke(&self, id: &Id, retention: Duration) -> session_store::Result<()> {
        ghinvite_web::wasm_compat::wasm_send(async move {
            // A relative TTL starts at the accepted write, so delayed duplicate
            // writes cannot shorten protection. Never write an "active" marker.
            self.kv
                .put(&format!("revoked:{id}"), "revoked")
                .map_err(|_| kv_error())?
                .expiration_ttl(retention.whole_seconds() as u64)
                .execute()
                .await
                .map_err(|_| kv_error())
        })
        .await
    }

    async fn delete(&self, session_id: &Id) -> session_store::Result<()> {
        let key = format!("session:{session_id}");
        ghinvite_web::wasm_compat::wasm_send(async move {
            self.kv.delete(&key).await.map_err(|_| kv_error())
        })
        .await
    }
}

fn kv_error() -> session_store::Error {
    session_store::Error::Backend("session KV unavailable".into())
}

/// Deployed builds talk to GitHub only; the `runtime-tests` harness builds
/// also accept `GITHUB_API_BASE`, `GITHUB_WEB_BASE` and `GITHUB_AVATAR_BASE`.
const DEPLOYMENT: ghinvite_config::github::Deployment = if cfg!(feature = "runtime-tests") {
    ghinvite_config::github::Deployment::Development
} else {
    ghinvite_config::github::Deployment::Production
};

fn config_from_env(env: &Env) -> worker::Result<ghinvite_web::WebConfig> {
    ghinvite_web::link_secret_entropy::configure(
        env.var("P1_LINK_SECRET_FIXTURE")
            .ok()
            .map(|v| v.to_string())
            .as_deref(),
    )
    .map_err(worker_err)?;
    // A successful configuration is immutable for this isolate. Failed
    // initialization publishes nothing and never falls back to unsigned ingress.
    static CONFIG: OnceLock<ghinvite_web::WebConfig> = OnceLock::new();
    if let Some(config) = CONFIG.get() {
        return Ok(config.clone());
    }
    let config = ghinvite_web::WebConfig::for_deployment(DEPLOYMENT, |field| {
        env.secret(field)
            .ok()
            .map(|v| v.to_string())
            .or_else(|| env.var(field).ok().map(|v| v.to_string()))
    })
    .map_err(worker_err)?;
    tracing::info!(fingerprint = %config.keyring.fingerprint(), "P1 keyring loaded");
    let _ = CONFIG.set(config);
    Ok(CONFIG.get().expect("configuration initialized").clone())
}

/// Map web/storage/etc errors into `worker::Error` for the fetch entry point.
fn worker_err(e: impl std::fmt::Display) -> worker::Error {
    worker::Error::RustError(e.to_string())
}

// workerd does not implement performance.mark/measure, used by tracing-wasm's
// span hooks. Console output with no native timer works on the Worker runtime.
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

#[cfg(feature = "runtime-tests")]
mod fixture;

#[cfg(feature = "runtime-tests")]
mod broker_fixture;
#[cfg(feature = "runtime-tests")]
mod email_review_fixture;
#[cfg(feature = "runtime-tests")]
mod email_vault_fixture;

#[event(fetch)]
async fn fetch(
    mut req: HttpRequest,
    env: Env,
    _ctx: Context,
) -> worker::Result<axum::response::Response> {
    use tower::ServiceExt;

    console_error_panic_hook::set_once();
    let _ = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(|| ConsoleWriter)
        .try_init();

    #[cfg(feature = "runtime-tests")]
    if req.uri().path().starts_with("/__fixture/rate-limit/") {
        return Ok(rate_limit::fixture(req, &env).await);
    }

    #[cfg(feature = "runtime-tests")]
    if req.uri().path().starts_with("/__fixture/broker/") {
        return broker_fixture::fetch(req, &env).await;
    }

    #[cfg(feature = "runtime-tests")]
    if req.uri().path().starts_with("/__fixture/email-review/") {
        return email_review_fixture::fetch(req, &env).await;
    }
    #[cfg(feature = "runtime-tests")]
    if req.uri().path().starts_with("/__fixture/email-vault/") {
        return email_vault_fixture::fetch(req, &env).await;
    }

    #[cfg(feature = "runtime-tests")]
    if req.uri().path().starts_with("/__fixture/") {
        return fixture::fetch(req, &env).await;
    }

    let config = match config_from_env(&env) {
        Ok(config) => config,
        Err(error) => {
            // All configuration parsers return fixed field names and bounded
            // diagnostics, never their input values or parser source chains.
            tracing::error!(
                alert = "p1_configuration",
                configuration = %error,
                "Web startup configuration invalid"
            );
            use axum::response::IntoResponse;
            return Ok((
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "Service configuration unavailable",
            )
                .into_response());
        }
    };
    let email_enabled = config.bulk_enabled && config.organization_delivery_enabled;
    let email_ring = match ghinvite_config::bulk::email_keyring(
        email_enabled,
        env.secret("P1_EMAIL_KEYRING").ok().map(|v| v.to_string()),
    ) {
        Ok(ring) => ring,
        Err(error) => {
            tracing::error!(alert="p1_configuration",configuration=%error,"Web startup configuration invalid");
            use axum::response::IntoResponse;
            return Ok((
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "Service configuration unavailable",
            )
                .into_response());
        }
    };
    let edge_limits = match rate_limit::WorkerRateLimiter::from_env(&env) {
        Ok(limiter) => Arc::new(limiter),
        Err(_) => {
            use axum::response::IntoResponse;
            tracing::error!(
                alert = "p1_configuration",
                "Required rate-limit bindings unavailable"
            );
            return Ok((
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "Service configuration unavailable",
            )
                .into_response());
        }
    };
    if let Some(ip) = rate_limit::client_ip(&req) {
        req.extensions_mut().insert(ip);
    }
    let authority_cache = match authority_cache::KvAuthorityCache::from_env(&env) {
        Ok(cache) => Arc::new(cache),
        Err(_) => {
            use axum::response::IntoResponse;
            tracing::error!(
                alert = "p1_configuration",
                "Required authority cache binding unavailable"
            );
            return Ok((
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "Service configuration unavailable",
            )
                .into_response());
        }
    };
    let db = env.d1("DB")?;
    let storage = Arc::new(ghinvite_storage_d1::D1Storage::new(db));
    // Built per request: the apps' installation-token caches are
    // isolate-scope statics inside `GithubApps` (PD-4).
    let github = match ghinvite_web::WebGithub::new(
        Arc::new(ghinvite_github::worker::WorkerFetchTransport::new()),
        &config.github,
        config.github_apps.clone(),
    ) {
        Ok(github) => github,
        Err(error) => {
            tracing::error!(
                alert = "p1_configuration",
                code = error.code(),
                "GitHub client configuration invalid"
            );
            use axum::response::IntoResponse;
            return Ok((
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "Service configuration unavailable",
            )
                .into_response());
        }
    };
    let restate = Arc::new(
        ghinvite_web::RestateClient::with_auth(
            &config.restate_ingress,
            config.restate_auth.clone(),
        )
        .map_err(worker_err)?,
    );
    let session_store =
        ProtectedStore::new(KvSessionStore::from_env(&env)?, config.keyring.clone());
    let email_vault = email_ring.filter(|_| email_enabled).map(|ring| {
        Arc::new(
            ghinvite_email_vault::EmailVault::new(storage.clone(), ring)
                .with_review_storage(storage.clone()),
        )
    });
    let state = ghinvite_web::AppState::new(storage, github, restate, config)
        .with_edge_limits(edge_limits)
        .with_authority_cache(authority_cache);
    let state = if let Some(vault) = email_vault {
        state.with_bulk_email_vault(vault)
    } else {
        state
    };
    let app = ghinvite_web::build_app(state, session_store);

    // `worker::axum::run` does not exist in worker 0.8. The axum `Router`
    // implements `tower::Service<http::Request<B>>` directly, so we call
    // `.call(req).await` and return the `axum::response::Response`. Worker's
    // `IntoResponse` impl for `http::Response<B: http_body::Body<Data=Bytes>>`
    // (gated on the `http` feature) converts that to a `web_sys::Response`
    // for us.
    let resp = match app.oneshot(req).await {
        Ok(r) => r,
        Err(e) => match e {}, // `Infallible`: no value can construct this arm.
    };
    Ok(resp)
}
