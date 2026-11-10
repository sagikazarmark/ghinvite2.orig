use crate::{SqlxStorage, decode, to_db_err, u64_to_i64};
use async_trait::async_trait;
use ghinvite_core::{
    access_grant::{AccessGrantSnapshot, RemovalCommand},
    storage::{
        Error, Result,
        access_grants::{self as sql, AccessGrantStorage},
        delivery_attempts::Generation,
    },
};
#[async_trait]
impl AccessGrantStorage for SqlxStorage {
    async fn claim_removal_attempt(&self, input: &RemovalCommand) -> Result<Option<u64>> {
        let encoded = sql::encode_attempt(input)?;
        let id = format!("access:{}", input.grant.storage_id());
        let step = input.grant.step();
        let generation = sqlx::query(sql::CLAIM)
            .bind(&id)
            .bind(&step)
            .bind(&encoded)
            .fetch_optional(&self.pool)
            .await
            .map_err(to_db_err)?
            .as_ref()
            .map(decode::<Generation>)
            .transpose()?;
        let row = sqlx::query(sql::ATTEMPT)
            .bind(&id)
            .bind(&step)
            .fetch_optional(&self.pool)
            .await
            .map_err(to_db_err)?
            .as_ref()
            .map(decode::<sql::AttemptRow>)
            .transpose()?;
        sql::check_attempt(row, &encoded)?
            .ok_or_else(|| Error::Database("removal attempt missing after claim".into()))?;
        Ok(generation.map(|g| g.generation))
    }
    async fn removal_attempt_exists(&self, input: &RemovalCommand) -> Result<bool> {
        let encoded = sql::encode_attempt(input)?;
        let row = sqlx::query(sql::ATTEMPT)
            .bind(format!("access:{}", input.grant.storage_id()))
            .bind(input.grant.step())
            .fetch_optional(&self.pool)
            .await
            .map_err(to_db_err)?
            .as_ref()
            .map(decode::<sql::AttemptRow>)
            .transpose()?;
        Ok(sql::check_attempt(row, &encoded)?.unwrap_or(false))
    }
    async fn reject_removal_attempt(&self, input: &RemovalCommand, generation: u64) -> Result<()> {
        let encoded = sql::encode_attempt(input)?;
        sqlx::query(sql::REJECT)
            .bind(format!("access:{}", input.grant.storage_id()))
            .bind(input.grant.step())
            .bind(encoded)
            .bind(u64_to_i64(generation)?)
            .execute(&self.pool)
            .await
            .map_err(to_db_err)?;
        Ok(())
    }
    async fn project_access_grant(&self, snapshot: &AccessGrantSnapshot) -> Result<()> {
        let row = sqlx::query(sql::GET)
            .bind(snapshot.command.storage_id())
            .fetch_optional(&self.pool)
            .await
            .map_err(to_db_err)?
            .as_ref()
            .map(decode::<sql::SnapshotRow>)
            .transpose()?;
        let source = sqlx::query(sql::SOURCE)
            .bind(sql::source_id(snapshot))
            .fetch_optional(&self.pool)
            .await
            .map_err(to_db_err)?
            .as_ref()
            .map(decode::<sql::SourceRow>)
            .transpose()?;
        let input = sql::encode_projection(snapshot, row.as_ref(), source.as_ref())?;
        self.transaction(&sql::statements(), &input, sql::classify)
            .await
    }
    async fn get_access_grant(&self, id: &str) -> Result<Option<AccessGrantSnapshot>> {
        sqlx::query(sql::GET)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(to_db_err)?
            .as_ref()
            .map(decode::<sql::SnapshotRow>)
            .transpose()?
            .map(|r| r.decode())
            .transpose()
    }
}
