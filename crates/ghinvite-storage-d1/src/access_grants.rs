use super::{D1Storage, int, text, wasm_send};
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
impl AccessGrantStorage for D1Storage {
    async fn claim_removal_attempt(&self, input: &RemovalCommand) -> Result<Option<u64>> {
        wasm_send(async {
            let encoded = sql::encode_attempt(input)?;
            let values = [
                text(&format!("access:{}", input.grant.storage_id())),
                text(&input.grant.step()),
                text(&encoded),
            ];
            let generation = self.first::<Generation>(sql::CLAIM, &values).await?;
            let row = self
                .first::<sql::AttemptRow>(sql::ATTEMPT, &values[..2])
                .await?;
            sql::check_attempt(row, &encoded)?
                .ok_or_else(|| Error::Database("removal attempt missing after claim".into()))?;
            Ok(generation.map(|g| g.generation))
        })
        .await
    }
    async fn removal_attempt_exists(&self, input: &RemovalCommand) -> Result<bool> {
        wasm_send(async {
            let encoded = sql::encode_attempt(input)?;
            let row = self
                .first::<sql::AttemptRow>(
                    sql::ATTEMPT,
                    &[
                        text(&format!("access:{}", input.grant.storage_id())),
                        text(&input.grant.step()),
                    ],
                )
                .await?;
            Ok(sql::check_attempt(row, &encoded)?.unwrap_or(false))
        })
        .await
    }
    async fn reject_removal_attempt(&self, input: &RemovalCommand, generation: u64) -> Result<()> {
        wasm_send(async {
            let encoded = sql::encode_attempt(input)?;
            self.run(
                sql::REJECT,
                &[
                    text(&format!("access:{}", input.grant.storage_id())),
                    text(&input.grant.step()),
                    text(&encoded),
                    int(generation)?,
                ],
            )
            .await?;
            Ok(())
        })
        .await
    }
    async fn project_access_grant(&self, snapshot: &AccessGrantSnapshot) -> Result<()> {
        wasm_send(async {
            let row = self
                .first::<sql::SnapshotRow>(sql::GET, &[text(&snapshot.command.storage_id())])
                .await?;
            let source = self
                .first::<sql::SourceRow>(sql::SOURCE, &[text(&sql::source_id(snapshot))])
                .await?;
            let input = sql::encode_projection(snapshot, row.as_ref(), source.as_ref())?;
            self.batch(&sql::statements(), &input, sql::classify).await
        })
        .await
    }
    async fn get_access_grant(&self, id: &str) -> Result<Option<AccessGrantSnapshot>> {
        wasm_send(async {
            self.first::<sql::SnapshotRow>(sql::GET, &[text(id)])
                .await?
                .map(|r| r.decode())
                .transpose()
        })
        .await
    }
}
