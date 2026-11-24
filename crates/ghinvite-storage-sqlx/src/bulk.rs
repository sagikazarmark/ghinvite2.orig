use crate::{SqlxStorage, decode, to_db_err, u64_to_i64};
use async_trait::async_trait;
use ghinvite_core::{
    bulk::{BulkBatchId, Review},
    storage::{
        Result,
        bulk::{self as sql, BulkStorage},
    },
};

#[async_trait]
impl BulkStorage for SqlxStorage {
    async fn project_bulk_projection(&self, projection: &sql::Projection) -> Result<()> {
        let review = &projection.review;
        let mut tx = self.pool.begin().await.map_err(to_db_err)?;
        let row = sqlx::query(sql::GET)
            .bind(review.manifest.batch_id.to_string())
            .fetch_optional(&mut *tx)
            .await
            .map_err(to_db_err)?;
        let previous: Option<sql::Row> = row.as_ref().map(decode).transpose()?;
        if let Some(input) = sql::encode_projection(projection, previous.as_ref())? {
            for statement in sql::statements() {
                crate::shared_query(statement)
                    .bind(&input)
                    .execute(&mut *tx)
                    .await
                    .map_err(|error| sql::classify(error.to_string()))?;
            }
        }
        tx.commit().await.map_err(to_db_err)
    }
    async fn bulk_review(&self, account_id: u64, batch_id: BulkBatchId) -> Result<Option<Review>> {
        let row = sqlx::query(sql::SCOPED_GET)
            .bind(batch_id.to_string())
            .bind(u64_to_i64(account_id)?)
            .fetch_optional(&self.pool)
            .await
            .map_err(to_db_err)?;
        row.as_ref()
            .map(|row| decode::<sql::Row>(row)?.decode())
            .transpose()
    }
}
