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
use worker::{Context, Env, Request, Response, Result, event};

pub struct KvSessionStore {
    kv: worker::kv::KvStore,
}

// SAFETY: D1Database and KvStore wrap JS objects that are !Send + !Sync upstream.
// This is sound only under the single-threaded Cloudflare Workers execution model
// where no OS threads exist and wasm32 has no thread-spawn capability. If
// SharedArrayBuffer-based threads are ever added to the build, these impls must
// be revisited.
unsafe impl Send for KvSessionStore {}
unsafe impl Sync for KvSessionStore {}

impl KvSessionStore {
    fn from_env(env: &Env) -> Result<Self> {
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
        let key = format!("session:{}", record.id);
        let value = serde_json::to_string(record)
            .map_err(|e| session_store::Error::Encode(e.to_string()))?;
        let mut put = self
            .kv
            .put(&key, value)
            .map_err(|e| session_store::Error::Backend(e.to_string()))?;
        if let Some(expiry) = record.expiry_date {
            let unix = expiry.unix_timestamp() as u64;
            put = put.expiration(unix);
        }
        put.execute()
            .await
            .map_err(|e| session_store::Error::Backend(e.to_string()))
    }

    async fn load(&self, session_id: &Id) -> session_store::Result<Option<Record>> {
        let key = format!("session:{session_id}");
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
    }

    async fn delete(&self, session_id: &Id) -> session_store::Result<()> {
        let key = format!("session:{session_id}");
        self.kv
            .delete(&key)
            .await
            .map_err(|e| session_store::Error::Backend(e.to_string()))
    }
}

fn config_from_env(env: &Env) -> Result<web::WebConfig> {
    use github::oauth::OAuthConfig;

    let base_url = env.var("GHINVITE_BASE_URL")?.to_string();

    let secret_str = env.secret("GHINVITE_SESSION_SECRET")?.to_string();
    let mut session_secret = [0u8; 32];
    let bytes = secret_str.as_bytes();
    session_secret[..bytes.len().min(32)].copy_from_slice(&bytes[..bytes.len().min(32)]);

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

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    console_error_panic_hook::set_once();
    tracing_wasm::set_as_global_default();

    let config = config_from_env(&env)?;
    let db = env.d1("DB")?;
    let storage: Arc<dyn storage::Storage> = Arc::new(storage_d1::D1Storage::new(db));
    let transport: Arc<dyn github::HttpTransport> =
        Arc::new(github::transport::ReqwestTransport::new()?);
    let restate = Arc::new(web::RestateClient::new(&config.restate_ingress)?);
    let state = web::AppState::new(storage, transport, restate, config);
    let session_store = KvSessionStore::from_env(&env)?;
    let app = web::build_app(state, session_store);

    worker::axum::run(app, req).await
}
