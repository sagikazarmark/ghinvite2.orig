//! Session-independent, bounded authority evidence. Storage TTL is only cleanup:
//! freshness is checked from the observation time on every read, including KV.
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Mutex};

pub const TTL_SECONDS: i64 = 300;
const MAX_ENTRIES: usize = 10_000;
const MAX_TOTAL_BYTES: usize = 32 * 1024 * 1024;
const MAX_VALUE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CacheEntry {
    pub observed_at: i64,
    pub value: serde_json::Value,
}
impl CacheEntry {
    pub fn is_fresh_at(&self, now: i64) -> bool {
        self.observed_at <= now && now.saturating_sub(self.observed_at) < TTL_SECONDS
    }
}
#[derive(Clone, Copy, Debug)]
pub struct CacheUnavailable;

#[async_trait::async_trait]
pub trait AuthorityCache: Send + Sync {
    async fn get(&self, key: &str) -> Result<Option<CacheEntry>, CacheUnavailable>;
    async fn put(&self, key: &str, value: CacheEntry) -> Result<(), CacheUnavailable>;
}

pub struct Unconfigured;
#[async_trait::async_trait]
impl AuthorityCache for Unconfigured {
    async fn get(&self, _: &str) -> Result<Option<CacheEntry>, CacheUnavailable> {
        Err(CacheUnavailable)
    }
    async fn put(&self, _: &str, _: CacheEntry) -> Result<(), CacheUnavailable> {
        Err(CacheUnavailable)
    }
}

pub fn valid_key(key: &str) -> bool {
    key.starts_with("authz:")
        && key.len() <= 256
        && key
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b':' | b'_' | b'-'))
}
pub fn valid_entry(value: &CacheEntry, now: i64) -> bool {
    value.is_fresh_at(now)
        && serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() <= MAX_VALUE_BYTES)
}

/// A single native process shares this bounded cache across browser sessions.
#[derive(Default)]
pub struct MemoryAuthorityCache {
    entries: Mutex<MemoryEntries>,
}
#[derive(Default)]
struct MemoryEntries {
    rows: HashMap<String, (CacheEntry, usize)>,
    bytes: usize,
}
impl MemoryEntries {
    fn expire(&mut self, now: i64) {
        self.rows.retain(|_, (entry, size)| {
            if entry.is_fresh_at(now) {
                true
            } else {
                self.bytes -= *size;
                false
            }
        });
    }
    fn remove(&mut self, key: &str) {
        if let Some((_, bytes)) = self.rows.remove(key) {
            self.bytes -= bytes;
        }
    }
}
impl MemoryAuthorityCache {
    fn get_at(&self, key: &str, now: i64) -> Result<Option<CacheEntry>, CacheUnavailable> {
        if !valid_key(key) {
            return Err(CacheUnavailable);
        }
        let mut entries = self.entries.lock().map_err(|_| CacheUnavailable)?;
        entries.expire(now);
        Ok(entries.rows.get(key).map(|(value, _)| value.clone()))
    }
    fn put_at(&self, key: &str, value: CacheEntry, now: i64) -> Result<(), CacheUnavailable> {
        if !valid_key(key) || !valid_entry(&value, now) {
            return Err(CacheUnavailable);
        }
        let size = serde_json::to_vec(&value)
            .map_err(|_| CacheUnavailable)?
            .len()
            + key.len();
        let mut entries = self.entries.lock().map_err(|_| CacheUnavailable)?;
        entries.expire(now);
        entries.remove(key);
        while entries.rows.len() >= MAX_ENTRIES
            || entries.bytes.saturating_add(size) > MAX_TOTAL_BYTES
        {
            // Eviction only discards evidence; the next request must read again.
            let oldest = entries
                .rows
                .iter()
                .min_by_key(|(_, (e, _))| e.observed_at)
                .map(|(k, _)| k.clone())
                .ok_or(CacheUnavailable)?;
            entries.remove(&oldest);
        }
        entries.bytes += size;
        entries.rows.insert(key.to_owned(), (value, size));
        Ok(())
    }
}
#[async_trait::async_trait]
impl AuthorityCache for MemoryAuthorityCache {
    async fn get(&self, key: &str) -> Result<Option<CacheEntry>, CacheUnavailable> {
        self.get_at(key, chrono::Utc::now().timestamp())
    }
    async fn put(&self, key: &str, value: CacheEntry) -> Result<(), CacheUnavailable> {
        self.put_at(key, value, chrono::Utc::now().timestamp())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(at: i64) -> CacheEntry {
        CacheEntry {
            observed_at: at,
            value: serde_json::json!(true),
        }
    }
    #[test]
    fn exact_expiry_future_evidence_and_partitions() {
        let cache = MemoryAuthorityCache::default();
        cache.put_at("authz:repo:1:2:3", entry(1000), 1000).unwrap();
        assert!(cache.get_at("authz:repo:1:2:3", 1299).unwrap().is_some());
        for key in [
            "authz:org:1:2:3",
            "authz:repo:2:2:3",
            "authz:repo:1:3:3",
            "authz:repo:1:2:4",
        ] {
            assert!(cache.get_at(key, 1299).unwrap().is_none());
        }
        assert!(cache.get_at("authz:repo:1:2:3", 1300).unwrap().is_none());
        assert!(cache.put_at("authz:repo:1:2:3", entry(1001), 1000).is_err());
        assert!(!entry(1001).is_fresh_at(1000));
    }
    #[test]
    fn capacity_is_bounded_and_old_evidence_can_be_replaced() {
        let cache = MemoryAuthorityCache::default();
        for id in 0..MAX_ENTRIES + 1 {
            cache
                .put_at(&format!("authz:repo:1:2:{id}"), entry(1000), 1000)
                .unwrap();
        }
        assert_eq!(cache.entries.lock().unwrap().rows.len(), MAX_ENTRIES);
        assert!(
            cache
                .get_at(&format!("authz:repo:1:2:{MAX_ENTRIES}"), 1000)
                .unwrap()
                .is_some()
        );
        cache
            .put_at("authz:repo:1:2:new", entry(1300), 1300)
            .unwrap();
        assert_eq!(cache.entries.lock().unwrap().rows.len(), 1);
    }
    #[test]
    fn aggregate_bytes_are_bounded_even_with_large_repository_pages() {
        let cache = MemoryAuthorityCache::default();
        for id in 0..40 {
            let value = CacheEntry {
                observed_at: 1000,
                value: serde_json::json!("x".repeat(1024 * 1024)),
            };
            cache
                .put_at(&format!("authz:repo:{id}:page"), value, 1000)
                .unwrap();
        }
        let entries = cache.entries.lock().unwrap();
        assert!(entries.bytes <= MAX_TOTAL_BYTES);
        assert!(entries.rows.len() < 40);
    }
}
