use crate::SqlxStorage;
use ghinvite_core::storage::Result;
use ghinvite_core::storage::projection::{ProjectionEnvelope, ProjectionStorage, sql};

#[async_trait::async_trait]
impl ProjectionStorage for SqlxStorage {
    async fn apply_reissue(
        &self,
        envelope: &ghinvite_core::reissue_projection::RequestReissueProjection,
    ) -> Result<()> {
        use ghinvite_core::storage::projection::reissue_sql;
        let input = reissue_sql::encode(envelope)?;
        let mut tx = self.pool.begin().await.map_err(crate::to_db_err)?;
        for statement in reissue_sql::statements() {
            sqlx::query(&statement)
                .bind(&input)
                .execute(&mut *tx)
                .await
                .map_err(|e| sql::classify(e.to_string()))?;
        }
        tx.commit().await.map_err(crate::to_db_err)
    }

    async fn apply_revocation(
        &self,
        envelope: &ghinvite_core::revocation::RevocationProjection,
    ) -> Result<()> {
        use ghinvite_core::storage::projection::revocation_sql;
        let input = revocation_sql::encode(envelope)?;
        let mut tx = self.pool.begin().await.map_err(crate::to_db_err)?;
        for statement in revocation_sql::STATEMENTS {
            sqlx::query(statement)
                .bind(&input)
                .execute(&mut *tx)
                .await
                .map_err(|e| sql::classify(e.to_string()))?;
        }
        tx.commit().await.map_err(crate::to_db_err)
    }

    async fn apply_request(
        &self,
        envelope: &ghinvite_core::storage::projection::RequestProjectionEnvelope,
    ) -> Result<()> {
        use ghinvite_core::storage::projection::request_sql;
        let input = request_sql::encode(envelope)?;
        let mut tx = self.pool.begin().await.map_err(crate::to_db_err)?;
        for statement in request_sql::statements(envelope) {
            sqlx::query(&statement)
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
