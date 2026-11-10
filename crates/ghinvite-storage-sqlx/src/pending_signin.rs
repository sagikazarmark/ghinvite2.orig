use crate::{SqlxStorage, decode};
use async_trait::async_trait;
use ghinvite_core::storage::{
    Result,
    pending_signin::{self as sql, PendingSignInKey, PendingSignInStorage, StoredPendingSignIn},
};

#[async_trait]
impl PendingSignInStorage for SqlxStorage {
    async fn replace_pending_signin(
        &self,
        key: PendingSignInKey,
        record: &StoredPendingSignIn,
        now: i64,
    ) -> Result<()> {
        record.validate()?;
        ghinvite_core::storage::integer::signed(now)?;
        let mut tx = self.pool.begin().await.map_err(|_| sql::unavailable())?;
        sqlx::query(sql::CLEANUP)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(|_| sql::unavailable())?;
        sqlx::query(sql::REPLACE)
            .bind(key.as_hex())
            .bind(&record.ciphertext)
            .bind(record.expires_at)
            .execute(&mut *tx)
            .await
            .map_err(|_| sql::unavailable())?;
        tx.commit().await.map_err(|_| sql::unavailable())
    }
    async fn read_pending_signin(
        &self,
        key: PendingSignInKey,
    ) -> Result<Option<StoredPendingSignIn>> {
        let row = sqlx::query(sql::READ)
            .bind(key.as_hex())
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| sql::unavailable())?;
        let record: Option<StoredPendingSignIn> = row.as_ref().map(decode).transpose()?;
        if let Some(record) = &record {
            record.validate()?;
        }
        Ok(record)
    }
    async fn consume_pending_signin(
        &self,
        key: PendingSignInKey,
        expected: &StoredPendingSignIn,
    ) -> Result<bool> {
        let mut tx = self.pool.begin().await.map_err(|_| sql::unavailable())?;
        let consumed = sqlx::query(sql::CONSUME)
            .bind(key.as_hex())
            .bind(&expected.ciphertext)
            .bind(expected.expires_at)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| sql::unavailable())?
            .is_some();
        tx.commit().await.map_err(|_| sql::unavailable())?;
        Ok(consumed)
    }
}
