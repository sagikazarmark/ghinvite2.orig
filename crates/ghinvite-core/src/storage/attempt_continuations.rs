//! Atomic opaque continuation storage, shared by SQLite and D1.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredContinuation {
    pub id: String,
    pub payload: String,
}

pub const INSERT: &str = "INSERT INTO attempt_continuations(scope,id,binding,payload,expires_at) VALUES (?1,?2,?3,?4,?5) ON CONFLICT DO NOTHING";
/// Opportunistic global cleanup: bounded work, including abandoned sessions.
/// These browser continuations are separate from retained Restate receipts.
pub const CLEANUP: &str = "DELETE FROM attempt_continuations WHERE rowid IN (SELECT rowid FROM attempt_continuations WHERE expires_at<=?1 ORDER BY expires_at LIMIT 100)";
// Exact identity conflicts precede logical-request recovery when both match.
pub const BY_BINDING: &str = "SELECT id,payload FROM attempt_continuations WHERE scope=?1 AND (binding=?2 OR id=?3) AND expires_at>?4 ORDER BY (id=?3) DESC LIMIT 1";
pub const GET: &str =
    "SELECT id,payload FROM attempt_continuations WHERE scope=?1 AND id=?2 AND expires_at>?3";
/// Oldest first: SQLite assigns each insert a rowid above every live row's.
pub const LIST: &str =
    "SELECT id,payload FROM attempt_continuations WHERE scope=?1 AND expires_at>?2 ORDER BY rowid";
/// Forget a continuation the authority definitively rejected, so its identity
/// can carry corrected input.
pub const RELEASE: &str = "DELETE FROM attempt_continuations WHERE scope=?1 AND id=?2";
