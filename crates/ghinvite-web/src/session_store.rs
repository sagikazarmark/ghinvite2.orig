//! Application-key protection and deny-only revocation shared by native and Workers.
//!
//! Backends see opaque ciphertext, never a Tower record. The requested ID is
//! authenticated independently of the envelope, preventing record substitution.

use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use tower_sessions::cookie::time::{Duration, OffsetDateTime};
use tower_sessions::{
    SessionStore,
    session::{Id, Record},
    session_store::{Error, Result},
};

pub const AUTHENTICATED_LIFETIME: Duration = Duration::days(30);
pub const ANONYMOUS_LIFETIME: Duration = Duration::minutes(30);
pub const REVOCATION_RETENTION: Duration = Duration::days(31);
const LIFETIME_KEY: &str = "ghinvite_lifetime";
const MAGIC: &[u8] = b"ghinvite-session\x01";

/// Minimal opaque persistence boundary. `insert` must not overwrite a known ID;
/// KV may rely on random IDs when concurrent absence checks race. Revocation
/// must be independent of record writes and retained for at least `retention`.
#[async_trait::async_trait]
pub trait Backend: Clone + std::fmt::Debug + Send + Sync + 'static {
    async fn get(&self, id: &Id) -> Result<Option<Vec<u8>>>;
    async fn insert(&self, id: &Id, payload: &[u8], expires_at: i64) -> Result<bool>;
    async fn put(&self, id: &Id, payload: &[u8], expires_at: i64) -> Result<()>;
    async fn is_revoked(&self, id: &Id) -> Result<bool>;
    async fn revoke(&self, id: &Id, retention: Duration) -> Result<()>;
    async fn delete(&self, id: &Id) -> Result<()>;
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
struct Lifetime {
    issued_at: i64,
    deadline: i64,
    authenticated: bool,
}

fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}
fn backend_error(message: &str) -> Error {
    Error::Backend(message.into())
}

fn authenticated(record: &Record) -> bool {
    crate::session::from_record(record).is_some_and(|session| session.is_authenticated())
}

impl Lifetime {
    fn new(authenticated: bool) -> Self {
        let issued_at = now().unix_timestamp();
        let duration = if authenticated {
            AUTHENTICATED_LIFETIME
        } else {
            ANONYMOUS_LIFETIME
        };
        Self {
            issued_at,
            deadline: issued_at + duration.whole_seconds(),
            authenticated,
        }
    }

    fn valid(&self, record: &Record) -> bool {
        let duration = if self.authenticated {
            AUTHENTICATED_LIFETIME
        } else {
            ANONYMOUS_LIFETIME
        };
        crate::session::from_record(record).is_some()
            && self.authenticated == authenticated(record)
            && self.issued_at.checked_add(duration.whole_seconds()) == Some(self.deadline)
            && self.issued_at <= now().unix_timestamp()
            && self.deadline > now().unix_timestamp()
    }
}

/// Called only after successful OAuth establishes fresh identity under a new ID.
pub async fn establish_authenticated_lifetime(
    session: &tower_sessions::Session,
) -> std::result::Result<(), tower_sessions::session::Error> {
    let lifetime = Lifetime::new(true);
    session.set_expiry(Some(tower_sessions::Expiry::AtDateTime(
        OffsetDateTime::from_unix_timestamp(lifetime.deadline).expect("current session deadline"),
    )));
    session.insert(LIFETIME_KEY, lifetime).await
}

/// Preserve the fixed deadline across request-local modifications, including
/// flash-only sessions. Missing authenticated metadata is never repaired here.
pub async fn apply_lifetime(
    session: &tower_sessions::Session,
) -> std::result::Result<(), tower_sessions::session::Error> {
    let lifetime = match session.get::<Lifetime>(LIFETIME_KEY).await? {
        Some(lifetime) => lifetime,
        None => {
            let lifetime = Lifetime::new(false);
            session.insert(LIFETIME_KEY, lifetime).await?;
            lifetime
        }
    };
    // Tower's set_expiry unconditionally marks the record modified. Leave
    // unchanged reads alone: KV permits only one write per key per second.
    if session.is_modified()
        && let Ok(deadline) = OffsetDateTime::from_unix_timestamp(lifetime.deadline)
    {
        session.set_expiry(Some(tower_sessions::Expiry::AtDateTime(
            deadline.min(session.expiry_date()),
        )));
    }
    Ok(())
}

#[derive(Clone)]
pub struct ProtectedStore<B> {
    backend: B,
    cipher: XChaCha20Poly1305,
}

impl<B: std::fmt::Debug> std::fmt::Debug for ProtectedStore<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProtectedStore")
            .field("backend", &self.backend)
            .finish_non_exhaustive()
    }
}

impl<B: Backend> ProtectedStore<B> {
    pub fn new(backend: B, key: [u8; 32]) -> Self {
        Self {
            backend,
            cipher: XChaCha20Poly1305::new((&key).into()),
        }
    }

    fn seal(&self, record: &Record) -> Result<(Vec<u8>, i64)> {
        let lifetime: Lifetime = serde_json::from_value(
            record
                .data
                .get(LIFETIME_KEY)
                .cloned()
                .ok_or_else(|| backend_error("missing session lifetime"))?,
        )
        .map_err(|_| backend_error("invalid session lifetime"))?;
        if !lifetime.valid(record) || record.expiry_date <= now() {
            return Err(backend_error("session expired or invalid"));
        }
        let mut record = record.clone();
        let expiry = record.expiry_date.unix_timestamp().min(lifetime.deadline);
        record.expiry_date = OffsetDateTime::from_unix_timestamp(expiry)
            .map_err(|_| backend_error("invalid expiry"))?;
        let mut envelope = MAGIC.to_vec();
        envelope.extend_from_slice(&lifetime.deadline.to_be_bytes());
        let mut nonce = [0u8; 24];
        OsRng
            .try_fill_bytes(&mut nonce)
            .map_err(|_| backend_error("session randomness unavailable"))?;
        envelope.extend_from_slice(&nonce);
        let mut aad = envelope.clone();
        aad.extend_from_slice(record.id.to_string().as_bytes());
        let plaintext = serde_json::to_vec(&record)
            .map_err(|_| Error::Encode("session encoding failed".into()))?;
        let ciphertext = self
            .cipher
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: &plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| backend_error("session encryption failed"))?;
        envelope.extend_from_slice(&ciphertext);
        Ok((envelope, expiry))
    }

    fn open(&self, id: &Id, envelope: &[u8]) -> Option<Record> {
        let header_len = MAGIC.len() + 8 + 24;
        if envelope.len() < header_len + 16 || !envelope.starts_with(MAGIC) {
            return None;
        }
        let deadline = i64::from_be_bytes(envelope[MAGIC.len()..MAGIC.len() + 8].try_into().ok()?);
        let nonce = &envelope[MAGIC.len() + 8..header_len];
        let mut aad = envelope[..header_len].to_vec();
        aad.extend_from_slice(id.to_string().as_bytes());
        let nonce = XNonce::try_from(nonce).ok()?;
        let plaintext = self
            .cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: &envelope[header_len..],
                    aad: &aad,
                },
            )
            .ok()?;
        let record: Record = serde_json::from_slice(&plaintext).ok()?;
        let lifetime: Lifetime =
            serde_json::from_value(record.data.get(LIFETIME_KEY)?.clone()).ok()?;
        (record.id == *id
            && lifetime.valid(&record)
            && deadline == lifetime.deadline
            && record.expiry_date > now()
            && record.expiry_date.unix_timestamp() <= deadline)
            .then_some(record)
    }
}

#[async_trait::async_trait]
impl<B: Backend> SessionStore for ProtectedStore<B> {
    async fn create(&self, record: &mut Record) -> Result<()> {
        if !record.data.contains_key(LIFETIME_KEY) && !authenticated(record) {
            record.data.insert(
                LIFETIME_KEY.into(),
                serde_json::to_value(Lifetime::new(false)).unwrap(),
            );
        }
        // Collision handling must happen before sealing: ciphertext is ID-bound.
        for _ in 0..8 {
            if self.backend.is_revoked(&record.id).await? {
                return Err(backend_error("session revoked"));
            }
            let (payload, expiry) = self.seal(record)?;
            if self.backend.insert(&record.id, &payload, expiry).await? {
                return Ok(());
            }
            record.id = Id::default();
        }
        Err(backend_error("session ID collision"))
    }

    async fn save(&self, record: &Record) -> Result<()> {
        if self.backend.is_revoked(&record.id).await? {
            return Err(backend_error("session revoked"));
        }
        let (payload, expiry) = self.seal(record)?;
        self.backend.put(&record.id, &payload, expiry).await
    }

    async fn load(&self, id: &Id) -> Result<Option<Record>> {
        if self.backend.is_revoked(id).await? {
            return Ok(None);
        }
        // Invalid ciphertext may belong to an overlapping new-key deployment.
        // Never mutate or revoke it merely because this reader cannot open it.
        Ok(self
            .backend
            .get(id)
            .await?
            .and_then(|raw| self.open(id, &raw)))
    }

    async fn delete(&self, id: &Id) -> Result<()> {
        self.backend.revoke(id, REVOCATION_RETENTION).await?;
        if self.backend.delete(id).await.is_err() {
            tracing::warn!("session ciphertext cleanup failed after revocation");
        }
        Ok(())
    }
}

// Backends live with their deployment shape: `SqliteBackend` in
// `ghinvite-web-server`, the KV backend in `ghinvite-web-worker`. This module
// owns the envelope format they both store opaquely.
