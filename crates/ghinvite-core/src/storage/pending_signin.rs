//! Opaque pending sign-in persistence. Consumption is a durable atomic delete.
use super::{Error, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PendingSignInKey([u8; 32]);
impl PendingSignInKey {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub fn as_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}
impl std::fmt::Debug for PendingSignInKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PendingSignInKey(..)")
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredPendingSignIn {
    /// Base64 envelope, never plaintext OAuth material.
    pub ciphertext: String,
    pub expires_at: i64,
}
impl std::fmt::Debug for StoredPendingSignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredPendingSignIn")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}
impl StoredPendingSignIn {
    pub fn validate(&self) -> Result<()> {
        super::integer::signed(self.expires_at)?;
        if self.expires_at <= 0
            || self.ciphertext.is_empty()
            || self.ciphertext.len() > 8192
            || !self
                .ciphertext
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
        {
            return Err(Error::Corrupt("invalid pending sign-in envelope".into()));
        }
        Ok(())
    }
}
#[async_trait]
pub trait PendingSignInStorage: Send + Sync + 'static {
    async fn replace_pending_signin(
        &self,
        key: PendingSignInKey,
        record: &StoredPendingSignIn,
        now: i64,
    ) -> Result<()>;
    /// Read from the authoritative primary without changing the row.
    async fn read_pending_signin(
        &self,
        key: PendingSignInKey,
    ) -> Result<Option<StoredPendingSignIn>>;
    /// Atomically delete only the exact observed ciphertext and expiry. Return
    /// true only after durable acknowledgement; uncertainty must be an error.
    async fn consume_pending_signin(
        &self,
        key: PendingSignInKey,
        expected: &StoredPendingSignIn,
    ) -> Result<bool>;
}
pub const REPLACE: &str = "INSERT INTO pending_signins(session_key,ciphertext,expires_at) VALUES (?1,?2,?3) ON CONFLICT(session_key) DO UPDATE SET ciphertext=excluded.ciphertext, expires_at=excluded.expires_at";
pub const READ: &str = "SELECT ciphertext,expires_at FROM pending_signins WHERE session_key=?1";
pub const CONSUME: &str = "DELETE FROM pending_signins WHERE session_key=?1 AND ciphertext=?2 AND expires_at=?3 RETURNING 1 AS consumed";
pub const CLEANUP: &str = "DELETE FROM pending_signins WHERE rowid IN (SELECT rowid FROM pending_signins WHERE expires_at<=?1 ORDER BY expires_at LIMIT 100)";
pub fn unavailable() -> Error {
    Error::Database("pending sign-in storage unavailable".into())
}
