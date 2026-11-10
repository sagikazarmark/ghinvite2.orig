use super::{D1Storage, int, text, wasm_send};
use async_trait::async_trait;
use ghinvite_core::{
    bulk::{BulkBatchId, Review},
    storage::{
        Result,
        bulk::{self as sql, BulkStorage},
    },
};

#[async_trait]
impl BulkStorage for D1Storage {
    async fn project_bulk_projection(&self, projection: &sql::Projection) -> Result<()> {
        let review = &projection.review;
        wasm_send(async {
            let row = self
                .first::<sql::Row>(sql::GET, &[text(&review.manifest.batch_id.to_string())])
                .await?;
            if let Some(input) = sql::encode_projection(projection, row.as_ref())? {
                let statements = sql::statements();
                let statements: Vec<&str> = statements.iter().map(String::as_str).collect();
                self.batch(&statements, &input, sql::classify).await?;
            }
            Ok(())
        })
        .await
    }
    async fn bulk_review(&self, account_id: u64, batch_id: BulkBatchId) -> Result<Option<Review>> {
        wasm_send(async {
            self.first::<sql::Row>(
                sql::SCOPED_GET,
                &[text(&batch_id.to_string()), int(account_id)?],
            )
            .await?
            .map(|row| row.decode())
            .transpose()
        })
        .await
    }
}
