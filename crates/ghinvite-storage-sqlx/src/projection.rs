use crate::SqlxStorage;
use ghinvite_core::storage::Result;
use ghinvite_core::storage::projection::{ProjectionEnvelope, ProjectionStorage, sql};

#[async_trait::async_trait]
impl ProjectionStorage for SqlxStorage {
    async fn get_projected_request(
        &self,
        id: ghinvite_core::RequestId,
    ) -> Result<Option<ghinvite_core::storage::projection::RequestSnapshot>> {
        let content: Option<String> = sqlx::query_scalar(
            "SELECT projection_content FROM invitation_requests WHERE id = ?1 AND projection_revision IS NOT NULL"
        ).bind(id.to_string()).fetch_optional(&self.pool).await.map_err(crate::to_db_err)?;
        content
            .map(|value| {
                serde_json::from_str(&value)
                    .map_err(|_| ghinvite_core::storage::Error::Corrupt("projected request".into()))
            })
            .transpose()
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
