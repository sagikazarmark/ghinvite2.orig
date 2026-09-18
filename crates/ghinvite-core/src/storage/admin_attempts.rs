//! Atomic opaque continuation storage, shared by SQLite and D1.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredAttempt {
    pub id: String,
    pub payload: String,
}

pub const INSERT: &str = "INSERT INTO admin_attempts(scope,id,binding,payload,expires_at) VALUES (?1,?2,?3,?4,?5) ON CONFLICT DO NOTHING";
// Exact identity conflicts precede logical-request recovery when both match.
pub const BY_BINDING: &str = "SELECT id,payload FROM admin_attempts WHERE scope=?1 AND (binding=?2 OR id=?3) AND expires_at>?4 ORDER BY (id=?3) DESC LIMIT 1";
pub const GET: &str =
    "SELECT id,payload FROM admin_attempts WHERE scope=?1 AND id=?2 AND expires_at>?3";
pub const LIST: &str =
    "SELECT id,payload FROM admin_attempts WHERE scope=?1 AND expires_at>?2 ORDER BY id";
