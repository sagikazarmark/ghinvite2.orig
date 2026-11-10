//! Atomic session-lineage sign-in reservations. Keys are domain-separated
//! SHA-256 digests, never raw session IDs, cookies, OAuth codes or tokens.
use crate::{RequestId, SecretHash};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SigninBudgetKey(SecretHash);
impl SigninBudgetKey {
    pub const fn from_digest(digest: [u8; 32]) -> Self {
        Self(SecretHash::from_bytes(digest))
    }
    pub fn as_hex(self) -> String {
        self.0.as_hex()
    }
}
pub const WINDOW_MS: i64 = 600_000;
pub const CLEANUP: &str = "DELETE FROM signin_starts WHERE rowid IN (SELECT rowid FROM signin_starts WHERE started_ms <= ?1 - 600000 ORDER BY started_ms LIMIT 256)";
pub const RESERVE: &str = "INSERT INTO signin_starts(budget_key,reservation_id,started_ms) SELECT ?1,?2,?3 WHERE
 (SELECT count(*) FROM signin_starts WHERE budget_key=?1 AND started_ms > ?3 - 600000) < 5
 OR EXISTS(SELECT 1 FROM signin_starts WHERE budget_key=?1 AND reservation_id=?2 AND started_ms=?3)
 ON CONFLICT(budget_key,reservation_id) DO UPDATE SET started_ms=excluded.started_ms WHERE signin_starts.started_ms=excluded.started_ms
 RETURNING started_ms";
#[derive(Deserialize)]
pub struct ReservationRow {
    pub started_ms: i64,
}
pub fn validate(_key: SigninBudgetKey, _reservation: RequestId, now_ms: i64) -> super::Result<()> {
    super::integer::signed(now_ms)?;
    Ok(())
}
