//! Authority evidence KV. Logical observation age, not KV expiry, grants access.
use ghinvite_web::authority_cache::{
    AuthorityCache, CacheEntry, CacheUnavailable, valid_entry, valid_key,
};

pub struct KvAuthorityCache(worker::kv::KvStore);
// SAFETY: this adapter is compiled only for the single-threaded Worker runtime,
// like KvSessionStore; JS handles never move to an OS thread.
unsafe impl Send for KvAuthorityCache {}
unsafe impl Sync for KvAuthorityCache {}
impl KvAuthorityCache {
    pub fn from_env(env: &worker::Env) -> worker::Result<Self> {
        Ok(Self(env.kv("AUTHZ_CACHE")?))
    }
}
#[async_trait::async_trait]
impl AuthorityCache for KvAuthorityCache {
    async fn get(&self, key: &str) -> Result<Option<CacheEntry>, CacheUnavailable> {
        if !valid_key(key) {
            return Err(CacheUnavailable);
        }
        ghinvite_web::wasm_compat::wasm_send(async move {
            let value = self
                .0
                .get(key)
                .json::<CacheEntry>()
                .await
                .map_err(|_| CacheUnavailable)?;
            Ok(value.filter(|entry| valid_entry(entry, chrono::Utc::now().timestamp())))
        })
        .await
    }
    async fn put(&self, key: &str, value: CacheEntry) -> Result<(), CacheUnavailable> {
        if !valid_key(key) || !valid_entry(&value, chrono::Utc::now().timestamp()) {
            return Err(CacheUnavailable);
        }
        ghinvite_web::wasm_compat::wasm_send(async move {
            let bytes = serde_json::to_vec(&value).map_err(|_| CacheUnavailable)?;
            self.0
                .put_bytes(key, &bytes)
                .map_err(|_| CacheUnavailable)?
                .expiration_ttl(300)
                .execute()
                .await
                .map_err(|_| CacheUnavailable)
        })
        .await
    }
}
