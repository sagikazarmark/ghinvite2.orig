//! Cloudflare Workers entry point for the web binary.
//!
//! Wasm32-only: all the wasm-bindgen / `worker` / `storage-d1` APIs we touch
//! here only exist on `wasm32-unknown-unknown`. On native targets the crate
//! is intentionally empty so `cargo check --workspace` (which defaults to the
//! host triple) doesn't drag in JS bindings or try to link a cdylib that
//! depends on them. To touch this code, build with
//! `--target wasm32-unknown-unknown`.
#![cfg(target_arch = "wasm32")]

use ghinvite_web::session_store::{Backend, ProtectedStore};
use std::sync::Arc;
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
            let unix = expires_at.max(chrono::Utc::now().timestamp() + 61) as u64;
            put.expiration(unix).execute().await.map_err(|_| kv_error())
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

fn config_from_env(env: &Env) -> worker::Result<ghinvite_web::WebConfig> {
    use ghinvite_github::oauth::OAuthConfig;

    let base_url = env.var("GHINVITE_BASE_URL")?.to_string();

    let secret_str = env.secret("GHINVITE_SESSION_SECRET")?.to_string();
    let session_secret =
        ghinvite_web::config::parse_session_secret(&secret_str).map_err(worker_err)?;

    Ok(ghinvite_web::WebConfig {
        base_url: base_url.clone(),
        session_secret,
        restate_ingress: env.var("GHINVITE_RESTATE_INGRESS")?.to_string(),
        oauth: OAuthConfig {
            client_id: env.secret("GHINVITE_GITHUB_CLIENT_ID")?.to_string(),
            client_secret: env.secret("GHINVITE_GITHUB_CLIENT_SECRET")?.to_string(),
            redirect_uri: format!("{base_url}/oauth/callback"),
        },
        github_install_url: env.var("GHINVITE_GITHUB_INSTALL_URL")?.to_string(),
        cookie_secure: true,
        webhook_secret: env
            .secret("GHINVITE_WEBHOOK_SECRET")?
            .to_string()
            .into_bytes(),
        // Cloudflare Static Assets serve `/assets/*` before the Worker is
        // invoked (`[assets]` in wrangler/web.toml); no in-Worker route.
        island_assets_dir: None,
    })
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

#[event(fetch)]
async fn fetch(
    req: HttpRequest,
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

    let config = config_from_env(&env)?;
    let db = env.d1("DB")?;
    let storage: Arc<dyn ghinvite_core::storage::Storage> =
        Arc::new(ghinvite_storage_d1::D1Storage::new(db));
    let transport: Arc<dyn ghinvite_github::HttpTransport> =
        Arc::new(ghinvite_github::transport::ReqwestTransport::new().map_err(worker_err)?);
    let restate =
        Arc::new(ghinvite_web::RestateClient::new(&config.restate_ingress).map_err(worker_err)?);
    let commands = Arc::new(ghinvite_web::RestateCommands::new(restate));
    let session_store = ProtectedStore::new(KvSessionStore::from_env(&env)?, config.session_secret);
    let state = ghinvite_web::AppState::new(storage, transport, commands, config);
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
