//! Cloudflare Workers entry point for the web binary.
//!
//! Wasm32-only: all the wasm-bindgen / `worker` / `storage-d1` APIs we touch
//! here only exist on `wasm32-unknown-unknown`. On native targets the crate
//! is intentionally empty so `cargo check --workspace` (which defaults to the
//! host triple) doesn't drag in JS bindings or try to link a cdylib that
//! depends on them. To touch this code, build with
//! `--target wasm32-unknown-unknown`.
#![cfg(target_arch = "wasm32")]

use std::sync::Arc;
use tower_sessions::{
    SessionStore,
    session::{Id, Record},
    session_store,
};
use worker::{Context, Env, HttpRequest, event};

/// `tower_sessions` requires `Debug + Send + Sync + 'static`. The inner
/// `KvStore` is `!Send + !Sync` upstream because it wraps a JS object — we
/// assert `Send + Sync` here under the same single-threaded-Workers
/// soundness argument used elsewhere in this workspace (see
/// `storage_d1::wasm_impl::WasmSend`).
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
impl SessionStore for KvSessionStore {
    async fn create(&self, record: &mut Record) -> session_store::Result<()> {
        self.save(record).await
    }

    async fn save(&self, record: &Record) -> session_store::Result<()> {
        // Wrap the body in a Send-asserting future so the resulting
        // `Future` returned by `async_trait` (which requires `Send`) is
        // satisfied — the inner `JsFuture` from `worker::kv` is `!Send`.
        web::wasm_compat::wasm_send(async move {
            let key = format!("session:{}", record.id);
            let value = serde_json::to_string(record)
                .map_err(|e| session_store::Error::Encode(e.to_string()))?;
            let put = self
                .kv
                .put(&key, value)
                .map_err(|e| session_store::Error::Backend(e.to_string()))?;
            // `Record::expiry_date` is a non-optional `OffsetDateTime`; KV
            // expects a unix epoch in seconds.
            let unix = record.expiry_date.unix_timestamp() as u64;
            put.expiration(unix)
                .execute()
                .await
                .map_err(|e| session_store::Error::Backend(e.to_string()))
        })
        .await
    }

    async fn load(&self, session_id: &Id) -> session_store::Result<Option<Record>> {
        let key = format!("session:{session_id}");
        web::wasm_compat::wasm_send(async move {
            let raw = self
                .kv
                .get(&key)
                .text()
                .await
                .map_err(|e| session_store::Error::Backend(e.to_string()))?;
            match raw {
                None => Ok(None),
                Some(text) => {
                    let record: Record = serde_json::from_str(&text)
                        .map_err(|e| session_store::Error::Decode(e.to_string()))?;
                    Ok(Some(record))
                }
            }
        })
        .await
    }

    async fn delete(&self, session_id: &Id) -> session_store::Result<()> {
        let key = format!("session:{session_id}");
        web::wasm_compat::wasm_send(async move {
            self.kv
                .delete(&key)
                .await
                .map_err(|e| session_store::Error::Backend(e.to_string()))
        })
        .await
    }
}

fn config_from_env(env: &Env) -> worker::Result<web::WebConfig> {
    use github::oauth::OAuthConfig;

    let base_url = env.var("GHINVITE_BASE_URL")?.to_string();

    let secret_str = env.secret("GHINVITE_SESSION_SECRET")?.to_string();
    let decoded = hex::decode(&secret_str)
        .map_err(|e| worker::Error::RustError(format!("GHINVITE_SESSION_SECRET not valid hex: {e}")))?;
    if decoded.len() < 32 {
        return Err(worker::Error::RustError(
            "GHINVITE_SESSION_SECRET must be at least 32 bytes (64 hex chars)".into(),
        ));
    }
    let mut session_secret = [0u8; 32];
    session_secret.copy_from_slice(&decoded[..32]);

    Ok(web::WebConfig {
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
    })
}

/// Map web/storage/etc errors into `worker::Error` for the fetch entry point.
fn worker_err(e: impl std::fmt::Display) -> worker::Error {
    worker::Error::RustError(e.to_string())
}

#[event(fetch)]
async fn fetch(
    req: HttpRequest,
    env: Env,
    _ctx: Context,
) -> worker::Result<axum::response::Response> {
    use tower::ServiceExt;

    console_error_panic_hook::set_once();
    let _ = tracing_wasm::try_set_as_global_default();

    let config = config_from_env(&env)?;
    let db = env.d1("DB")?;
    let storage: Arc<dyn storage::Storage> = Arc::new(storage_d1::D1Storage::new(db));
    let transport: Arc<dyn github::HttpTransport> =
        Arc::new(github::transport::ReqwestTransport::new().map_err(worker_err)?);
    let restate = Arc::new(web::RestateClient::new(&config.restate_ingress).map_err(worker_err)?);
    let state = web::AppState::new(storage, transport, restate, config);
    let session_store = KvSessionStore::from_env(&env)?;
    let app = web::build_app(state, session_store);

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
