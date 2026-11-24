use crate::{SqlxStorage, decode};
use async_trait::async_trait;
use ghinvite_core::{
    email_delivery::EmailRef,
    storage::{
        Result,
        email_review::{self as sql, EmailReviewStorage, NewReviewEmail, ReviewEmailRecord},
        email_vault::{EmailVaultBinding, time, unavailable},
    },
};
#[async_trait]
impl EmailReviewStorage for SqlxStorage {
    async fn retain_review_email(&self, record: &NewReviewEmail) -> Result<bool> {
        record.validate()?;
        let b = &record.binding;
        let mut tx = self.pool.begin().await.map_err(|_| unavailable())?;
        let changed = sqlx::query(sql::RETAIN)
            .bind(b.email_ref.as_hex())
            .bind(b.batch.batch_id.to_string())
            .bind(i64::from(b.batch.row))
            .bind(b.account_id as i64)
            .bind(b.installation_id as i64)
            .bind(&record.ciphertext)
            .bind(record.created_at)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| unavailable())?
            .is_some();
        tx.commit().await.map_err(|_| unavailable())?;
        Ok(changed)
    }
    async fn review_email(&self, email_ref: EmailRef) -> Result<Option<ReviewEmailRecord>> {
        let row = sqlx::query(sql::READ)
            .bind(email_ref.as_hex())
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| unavailable())?;
        let record: Option<ReviewEmailRecord> = row.as_ref().map(decode).transpose()?;
        if let Some(record) = &record {
            record.validate()?;
        }
        Ok(record)
    }
    async fn clear_review_email(&self, b: &EmailVaultBinding, now: i64) -> Result<bool> {
        self.email_review_transition(sql::CLEAR, b, now).await
    }
    async fn prune_review_emails(&self, now: i64) -> Result<u64> {
        time(now)?;
        let mut tx = self.pool.begin().await.map_err(|_| unavailable())?;
        let count = sqlx::query(sql::PRUNE)
            .bind(now)
            .fetch_all(&mut *tx)
            .await
            .map_err(|_| unavailable())?
            .len() as u64;
        tx.commit().await.map_err(|_| unavailable())?;
        Ok(count)
    }
}
impl SqlxStorage {
    async fn email_review_transition(
        &self,
        statement: &'static str,
        b: &EmailVaultBinding,
        now: i64,
    ) -> Result<bool> {
        b.validate()?;
        time(now)?;
        let mut tx = self.pool.begin().await.map_err(|_| unavailable())?;
        let changed = sqlx::query(statement)
            .bind(b.email_ref.as_hex())
            .bind(b.batch.batch_id.to_string())
            .bind(i64::from(b.batch.row))
            .bind(b.account_id as i64)
            .bind(b.installation_id as i64)
            .bind(now)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| unavailable())?
            .is_some();
        tx.commit().await.map_err(|_| unavailable())?;
        Ok(changed)
    }
}
