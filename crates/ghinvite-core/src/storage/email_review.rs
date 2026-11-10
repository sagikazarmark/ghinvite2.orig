//! Short-lived protected review addresses; separate from activated delivery recovery.
use super::{
    Result,
    email_vault::{EmailVaultBinding, envelope, invalid, time},
};
use crate::email_delivery::EmailRef;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
pub const RETENTION_SECONDS: i64 = 3600;
pub const CLEANUP_LIMIT: usize = 100;
#[derive(Clone, PartialEq, Eq)]
pub struct NewReviewEmail {
    pub binding: EmailVaultBinding,
    pub ciphertext: String,
    pub created_at: i64,
}
impl NewReviewEmail {
    pub fn validate(&self) -> Result<()> {
        self.binding.validate()?;
        time(self.created_at)?;
        envelope(&self.ciphertext)
    }
}
impl std::fmt::Debug for NewReviewEmail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewReviewEmail")
            .field("binding", &self.binding)
            .field("created_at", &self.created_at)
            .finish_non_exhaustive()
    }
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewEmailRecord {
    pub email_ref: EmailRef,
    pub batch_id: crate::bulk::BulkBatchId,
    pub row_number: u16,
    pub account_id: u64,
    pub installation_id: u64,
    pub ciphertext: Option<String>,
    pub created_at: i64,
    pub expires_at: i64,
    pub cleared_at: Option<i64>,
}
impl ReviewEmailRecord {
    pub fn binding(&self) -> EmailVaultBinding {
        EmailVaultBinding {
            email_ref: self.email_ref,
            batch: crate::bulk::BatchRef {
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
        if self.expires_at != self.created_at + RETENTION_SECONDS
            || self.ciphertext.is_none() != self.cleared_at.is_some()
        {
            return Err(invalid());
        }
        if let Some(at) = self.cleared_at {
            time(at)?;
            if at < self.created_at {
                return Err(invalid());
            }
        }
        if let Some(ciphertext) = &self.ciphertext {
            envelope(ciphertext)?;
        }
        Ok(())
    }
}
impl std::fmt::Debug for ReviewEmailRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReviewEmailRecord")
            .field("binding", &self.binding())
            .field("expires_at", &self.expires_at)
            .field("cleared_at", &self.cleared_at)
            .finish_non_exhaustive()
    }
}
#[async_trait]
pub trait EmailReviewStorage: Send + Sync + 'static {
    async fn retain_review_email(&self, record: &NewReviewEmail) -> Result<bool>;
    async fn review_email(&self, email_ref: EmailRef) -> Result<Option<ReviewEmailRecord>>;
    /// Exact-repeat clearing succeeds; a wrong binding never clears a row.
    async fn clear_review_email(&self, binding: &EmailVaultBinding, now: i64) -> Result<bool>;
    async fn prune_review_emails(&self, now: i64) -> Result<u64>;
}
pub const RETAIN: &str = "INSERT INTO email_review(email_ref,batch_id,row_number,account_id,installation_id,ciphertext,created_at,expires_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?7+3600) ON CONFLICT DO NOTHING RETURNING 1 AS changed";
pub const READ: &str = "SELECT email_ref,batch_id,row_number,account_id,installation_id,ciphertext,created_at,expires_at,cleared_at FROM email_review WHERE email_ref=?1";
pub const CLEAR: &str = "UPDATE email_review SET ciphertext=NULL,cleared_at=COALESCE(cleared_at,?6) WHERE email_ref=?1 AND batch_id=?2 AND row_number=?3 AND account_id=?4 AND installation_id=?5 AND ?6>=created_at RETURNING 1 AS changed";
pub const PRUNE: &str = "UPDATE email_review SET ciphertext=NULL,cleared_at=?1 WHERE email_ref IN (SELECT email_ref FROM email_review WHERE ciphertext IS NOT NULL AND expires_at<=?1 ORDER BY expires_at,email_ref LIMIT 100) RETURNING email_ref";
