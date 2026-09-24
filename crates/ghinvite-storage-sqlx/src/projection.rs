use crate::SqlxStorage;
use ghinvite_core::storage::Result;
use ghinvite_core::storage::projection::{ProjectionEnvelope, ProjectionStorage, sql};

#[async_trait::async_trait]
impl ProjectionStorage for SqlxStorage {
    async fn apply_request(
        &self,
        envelope: &ghinvite_core::storage::projection::RequestProjectionEnvelope,
    ) -> Result<()> {
        use ghinvite_core::storage::projection::request_sql;
        let input = request_sql::encode(envelope)?;
        let mut tx = self.pool.begin().await.map_err(crate::to_db_err)?;
        for statement in request_sql::STATEMENTS {
            sqlx::query(statement)
                .bind(&input)
                .execute(&mut *tx)
                .await
                .map_err(|error| sql::classify(error.to_string()))?;
        }
        tx.commit().await.map_err(crate::to_db_err)
    }
    async fn apply_transition(&self, envelope: &ProjectionEnvelope) -> Result<()> {
        let input = sql::encode(envelope)?;
        let mut tx = self.pool.begin().await.map_err(crate::to_db_err)?;
        for statement in sql::STATEMENTS {
            sqlx::query(statement)
                .bind(&input)
                .execute(&mut *tx)
                .await
                .map_err(|error| sql::classify(error.to_string()))?;
        }
        tx.commit().await.map_err(crate::to_db_err)
    }
}
