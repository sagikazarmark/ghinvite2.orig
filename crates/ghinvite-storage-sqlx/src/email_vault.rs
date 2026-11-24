use crate::{SqlxStorage, decode};
use async_trait::async_trait;
use ghinvite_core::{
    email_delivery::EmailRef,
    storage::{
        Result,
        email_vault::{
            self as sql, EmailVaultBinding, EmailVaultRecord, EmailVaultStorage, NewEmailRecord,
        },
    },
};
#[async_trait]
impl EmailVaultStorage for SqlxStorage {
    async fn stage_email(&self, record: &NewEmailRecord) -> Result<bool> {
        record.validate()?;
        let b = &record.binding;
        let mut tx = self.pool.begin().await.map_err(|_| sql::unavailable())?;
        let changed = sqlx::query(sql::STAGE)
            .bind(b.email_ref.as_hex())
            .bind(b.batch.batch_id.to_string())
            .bind(i64::from(b.batch.row))
            .bind(b.account_id as i64)
            .bind(b.installation_id as i64)
            .bind(&record.ciphertext)
            .bind(record.created_at)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| sql::unavailable())?
            .is_some();
        tx.commit().await.map_err(|_| sql::unavailable())?;
        Ok(changed)
    }
    async fn email_record(&self, email_ref: EmailRef) -> Result<Option<EmailVaultRecord>> {
        let row = sqlx::query(sql::READ)
            .bind(email_ref.as_hex())
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| sql::unavailable())?;
        let record: Option<EmailVaultRecord> = row.as_ref().map(decode).transpose()?;
        if let Some(record) = &record {
            record.validate()?;
        }
        Ok(record)
    }
    async fn activate_email(&self, b: &EmailVaultBinding, now: i64) -> Result<bool> {
        self.email_vault_transition(sql::ACTIVATE, b, now).await
    }
    async fn retire_email(&self, b: &EmailVaultBinding, now: i64) -> Result<bool> {
        self.email_vault_transition(sql::RETIRE, b, now).await
    }
    async fn prune_emails(&self, now: i64) -> Result<u64> {
        sql::time(now)?;
        let mut tx = self.pool.begin().await.map_err(|_| sql::unavailable())?;
        let count = sqlx::query(sql::PRUNE)
            .bind(now)
            .fetch_all(&mut *tx)
            .await
            .map_err(|_| sql::unavailable())?
            .len() as u64;
        tx.commit().await.map_err(|_| sql::unavailable())?;
        Ok(count)
    }
}
impl SqlxStorage {
    async fn email_vault_transition(
        &self,
        statement: &'static str,
        b: &EmailVaultBinding,
        now: i64,
    ) -> Result<bool> {
        b.validate()?;
        sql::time(now)?;
        let mut tx = self.pool.begin().await.map_err(|_| sql::unavailable())?;
        let changed = sqlx::query(statement)
            .bind(b.email_ref.as_hex())
            .bind(b.batch.batch_id.to_string())
            .bind(i64::from(b.batch.row))
            .bind(b.account_id as i64)
            .bind(b.installation_id as i64)
            .bind(now)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| sql::unavailable())?
            .is_some();
        tx.commit().await.map_err(|_| sql::unavailable())?;
        Ok(changed)
    }
}

#[cfg(feature = "test-util")]
impl SqlxStorage {
    /// Block one row's activation without affecting earlier rows or retirement.
    pub async fn debug_set_email_activation_failure(&self, row: Option<u16>) -> Result<()> {
        sqlx::query("DROP TRIGGER IF EXISTS fail_email_activation")
            .execute(&self.pool)
            .await
            .map_err(|_| sql::unavailable())?;
        if let Some(row) = row {
            let statement = format!(
                "CREATE TEMP TRIGGER fail_email_activation BEFORE UPDATE OF activated_at ON email_vault WHEN OLD.row_number={row} BEGIN SELECT RAISE(ABORT,'fixture activation unavailable'); END"
            );
            // DDL takes no bound parameters; the only spliced value is a `u16`.
            sqlx::query(sqlx::AssertSqlSafe(statement))
                .execute(&self.pool)
                .await
                .map_err(|_| sql::unavailable())?;
        }
        Ok(())
    }
    /// Connection-local failure for actual retained-owner recovery tests.
    pub async fn debug_set_email_retirement_failure(&self, fail: bool) -> Result<()> {
        let statement = if fail {
            "CREATE TEMP TRIGGER fail_email_retirement BEFORE UPDATE OF retired_at ON email_vault BEGIN SELECT RAISE(ABORT,'fixture retirement unavailable'); END"
        } else {
            "DROP TRIGGER fail_email_retirement"
        };
        sqlx::query(statement)
            .execute(&self.pool)
            .await
            .map_err(|_| sql::unavailable())?;
        Ok(())
    }
}
