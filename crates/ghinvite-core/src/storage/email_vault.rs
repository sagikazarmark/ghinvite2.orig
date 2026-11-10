//! Protected email recovery port. Core owns SQL; raw addresses never enter it.
use super::{Error, Result};
use crate::{bulk::BatchRef, email_delivery::EmailRef};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
pub const RETRY_HORIZON_SECONDS: i64 = 86_400;
pub const CLEANUP_LIMIT: usize = 100;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmailVaultBinding {
    pub email_ref: EmailRef,
    pub batch: BatchRef,
    pub account_id: u64,
    pub installation_id: u64,
}
impl EmailVaultBinding {
    pub fn validate(&self) -> Result<()> {
        self.batch.validate().map_err(|_| invalid())?;
        for n in [self.account_id, self.installation_id] {
            if n == 0 {
                return Err(invalid());
            }
            super::integer::unsigned(n)?;
        }
        Ok(())
    }
}
#[derive(Clone, PartialEq, Eq)]
pub struct NewEmailRecord {
    pub binding: EmailVaultBinding,
    pub ciphertext: String,
    pub created_at: i64,
}
impl std::fmt::Debug for NewEmailRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewEmailRecord")
            .field("binding", &self.binding)
            .field("created_at", &self.created_at)
            .finish_non_exhaustive()
    }
}
impl NewEmailRecord {
    pub fn validate(&self) -> Result<()> {
        self.binding.validate()?;
        time(self.created_at)?;
        envelope(&self.ciphertext)
    }
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailVaultRecord {
    pub email_ref: EmailRef,
    pub batch_id: crate::bulk::BulkBatchId,
    pub row_number: u16,
    pub account_id: u64,
    pub installation_id: u64,
    pub ciphertext: Option<String>,
    pub created_at: i64,
    pub activated_at: Option<i64>,
    pub retired_at: Option<i64>,
    pub purge_after: Option<i64>,
    pub purged_at: Option<i64>,
}
impl std::fmt::Debug for EmailVaultRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmailVaultRecord")
            .field("binding", &self.binding())
            .field("purged_at", &self.purged_at)
            .finish_non_exhaustive()
    }
}
impl EmailVaultRecord {
    pub fn binding(&self) -> EmailVaultBinding {
        EmailVaultBinding {
            email_ref: self.email_ref,
            batch: BatchRef {
                batch_id: self.batch_id,
                row: self.row_number,
            },
            account_id: self.account_id,
            installation_id: self.installation_id,
        }
    }
    pub fn validate(&self) -> Result<()> {
        self.binding().validate()?;
        time(self.created_at)?;
        for n in [self.activated_at, self.retired_at].into_iter().flatten() {
            time(n)?;
            if n < self.created_at {
                return Err(invalid());
            }
        }
        if self
            .retired_at
            .zip(self.activated_at)
            .is_some_and(|(r, a)| r < a)
        {
            return Err(invalid());
        }
        let expected = self
            .retired_at
            .map(|r| r + RETRY_HORIZON_SECONDS)
            .or_else(|| {
                self.activated_at
                    .is_none()
                    .then_some(self.created_at + RETRY_HORIZON_SECONDS)
            });
        if self.purge_after != expected || self.ciphertext.is_none() != self.purged_at.is_some() {
            return Err(invalid());
        }
        if let Some(purged) = self.purged_at {
            super::integer::signed(purged)?;
            if self.purge_after.is_none_or(|p| purged < p) {
                return Err(invalid());
            }
        }
        if let Some(c) = &self.ciphertext {
            envelope(c)?;
        }
        Ok(())
    }
    pub fn readable_at(&self, now: i64) -> bool {
        self.ciphertext.is_some()
            && now >= self.created_at
            && self.purge_after.is_none_or(|p| p > now)
    }
}
pub fn time(value: i64) -> Result<()> {
    if value <= 0 || value > super::integer::MAX_EXACT as i64 - RETRY_HORIZON_SECONDS {
        return Err(invalid());
    }
    Ok(())
}
pub(crate) fn envelope(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 8192
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
    {
        return Err(invalid());
    }
    Ok(())
}
pub(crate) fn invalid() -> Error {
    Error::Corrupt("invalid email vault record".into())
}
pub fn unavailable() -> Error {
    Error::Database("email vault unavailable".into())
}
#[async_trait]
pub trait EmailVaultStorage: Send + Sync + 'static {
    /// False means the reference already exists, including a permanent tombstone.
    /// A caller must read/decrypt/compare the existing record, never replace it.
    async fn stage_email(&self, record: &NewEmailRecord) -> Result<bool>;
    async fn email_record(&self, email_ref: EmailRef) -> Result<Option<EmailVaultRecord>>;
    async fn activate_email(&self, binding: &EmailVaultBinding, now: i64) -> Result<bool>;
    async fn retire_email(&self, binding: &EmailVaultBinding, now: i64) -> Result<bool>;
    async fn prune_emails(&self, now: i64) -> Result<u64>;
}
pub const STAGE: &str = "INSERT INTO email_vault(email_ref,batch_id,row_number,account_id,installation_id,ciphertext,created_at,purge_after) VALUES(?1,?2,?3,?4,?5,?6,?7,?7+86400) ON CONFLICT(email_ref) DO NOTHING RETURNING 1 AS changed";
pub const READ: &str = "SELECT email_ref,batch_id,row_number,account_id,installation_id,ciphertext,created_at,activated_at,retired_at,purge_after,purged_at FROM email_vault WHERE email_ref=?1";
pub const ACTIVATE: &str = "UPDATE email_vault SET activated_at=COALESCE(activated_at,?6),purge_after=NULL WHERE email_ref=?1 AND batch_id=?2 AND row_number=?3 AND account_id=?4 AND installation_id=?5 AND ciphertext IS NOT NULL AND retired_at IS NULL AND ?6>=created_at AND (activated_at IS NOT NULL OR purge_after>?6) RETURNING 1 AS changed";
pub const RETIRE: &str = "UPDATE email_vault SET retired_at=COALESCE(retired_at,?6),purge_after=COALESCE(retired_at,?6)+86400 WHERE email_ref=?1 AND batch_id=?2 AND row_number=?3 AND account_id=?4 AND installation_id=?5 AND ciphertext IS NOT NULL AND ?6>=COALESCE(activated_at,created_at) RETURNING 1 AS changed";
pub const PRUNE: &str = "UPDATE email_vault SET ciphertext=NULL,purged_at=?1 WHERE email_ref IN (SELECT email_ref FROM email_vault WHERE ciphertext IS NOT NULL AND purge_after<=?1 ORDER BY purge_after,email_ref LIMIT 100) RETURNING email_ref";
