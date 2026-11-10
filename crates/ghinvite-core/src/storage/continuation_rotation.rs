//! Bounded private continuation rotation. All rows, including expired records,
//! participate. Rotation never changes identity, context, retention or plaintext.
use serde::{Deserialize, Serialize};

pub const PAGE_SIZE: usize = 4;
pub const MAX_CIPHERTEXT_BYTES: usize = 4 * 1024 * 1024;
// The extra row is a sentinel. Large/corrupt values are classified without
// allocating unbounded ciphertext or metadata in a Worker.
pub const PAGE: &str = r#"
SELECT e.revision, c.rowid AS row_id,
 substr(c.scope,1,256) AS scope, substr(c.id,1,256) AS id,
 substr(c.binding,1,256) AS binding,
 CASE WHEN length(CAST(c.payload AS BLOB))<=4194304 THEN c.payload ELSE NULL END AS payload,
 c.expires_at,
 CASE WHEN length(CAST(c.scope AS BLOB))>255 OR length(CAST(c.id AS BLOB))>255
 OR length(CAST(c.binding AS BLOB))>255 OR length(CAST(c.payload AS BLOB))>4194304 THEN 1 ELSE 0 END AS oversized
FROM continuation_rotation_epoch e LEFT JOIN
 (SELECT rowid,* FROM attempt_continuations WHERE (?1 IS NULL OR rowid>?1) ORDER BY rowid LIMIT 5) c ON 1=1
WHERE e.singleton=1 ORDER BY c.rowid
"#;
pub const EPOCH: &str = "SELECT revision FROM continuation_rotation_epoch WHERE singleton=1";
pub const CAS: &str = r#"UPDATE attempt_continuations SET payload=?1
WHERE rowid=?2 AND scope=?3 AND id=?4 AND binding=?5 AND payload=?6 AND expires_at=?7
AND (SELECT revision FROM continuation_rotation_epoch WHERE singleton=1)=?8
RETURNING rowid AS row_id"#;

#[derive(Clone, Deserialize)]
pub struct ScanRow {
    pub revision: i64,
    pub row_id: Option<i64>,
    pub scope: Option<String>,
    pub id: Option<String>,
    pub binding: Option<String>,
    pub payload: Option<String>,
    pub expires_at: Option<i64>,
    pub oversized: i64,
}
impl ScanRow {
    pub fn validate(&self) -> super::Result<()> {
        super::integer::signed(self.revision)?;
        if self.revision < 0 || !matches!(self.oversized, 0 | 1) {
            return Err(super::Error::Corrupt("continuation rotation row".into()));
        }
        if let Some(id) = self.row_id {
            super::integer::signed(id)?;
            super::integer::signed(
                self.expires_at
                    .ok_or_else(|| super::Error::Corrupt("continuation rotation expiry".into()))?,
            )?;
            if self.scope.is_none()
                || self.id.is_none()
                || self.binding.is_none()
                || (self.payload.is_none() && self.oversized == 0)
            {
                return Err(super::Error::Corrupt("continuation rotation row".into()));
            }
        }
        Ok(())
    }
}
#[derive(Deserialize)]
pub struct Epoch {
    pub revision: i64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Rewrap,
    Verify,
}

/// D1 meta.changes includes AFTER-trigger changes. Only this direct-row
/// RETURNING receipt establishes a successful CAS on the intended row.
#[derive(Deserialize)]
pub struct CasReceipt {
    pub row_id: i64,
}
impl CasReceipt {
    pub fn applied(rows: &[Self], expected: i64) -> super::Result<bool> {
        super::integer::signed(expected)?;
        match rows {
            [] => Ok(false),
            [row] if row.row_id == expected => Ok(true),
            _ => Err(super::Error::Corrupt("continuation CAS receipt".into())),
        }
    }
}
