use super::{D1Storage, int, text, wasm_send};
#[cfg(not(target_arch = "wasm32"))]
use crate::native as worker;
use async_trait::async_trait;
use ghinvite_core::{
    bulk::BulkBatchId,
    email_delivery::{EmailAttempt, EmailDeliverySnapshot},
    storage::{
        Result,
        delivery_attempts::Generation,
        email_delivery::{self as sql, EmailDeliveryStorage},
    },
};
#[async_trait]
impl EmailDeliveryStorage for D1Storage {
    async fn claim_email_attempt(&self, a: &EmailAttempt) -> Result<Option<u64>> {
        let command = sql::encode_attempt(a)?;
        let id = a.create.storage_id();
        wasm_send(async {
            let values = [text(&id), text(&command)];
            let generation = self
                .first::<Generation>(sql::CLAIM, &values)
                .await
                .map_err(|_| sql::unavailable())?;
            let row = self
                .first::<sql::AttemptRow>(sql::ATTEMPT, &values)
                .await
                .map_err(|_| sql::unavailable())?;
            sql::check_attempt(row, &command)?.ok_or_else(sql::unavailable)?;
            Ok(generation.map(|g| g.generation))
        })
        .await
    }
    async fn email_attempt_exists(&self, a: &EmailAttempt) -> Result<bool> {
        let command = sql::encode_attempt(a)?;
        wasm_send(async {
            // Must not authorize a POST from a stale read replica.
            let session = self
                .db
                .with_session_constraint(worker::D1SessionConstraint::FirstPrimary)
                .map_err(|_| sql::unavailable())?;
            let row = session
                .prepare(sql::ATTEMPT)
                .bind(&[text(&a.create.storage_id()), text(&command)])
                .map_err(|_| sql::unavailable())?
                .first::<sql::AttemptRow>(None)
                .await
                .map_err(|_| sql::unavailable())?;
            Ok(sql::check_attempt(row, &command)?.unwrap_or(false))
        })
        .await
    }
    async fn reject_email_attempt(&self, a: &EmailAttempt, generation: u64) -> Result<()> {
        let command = sql::encode_attempt(a)?;
        if generation == 0 {
            return Err(sql::invalid());
        }
        wasm_send(async {
            self.run(
                sql::REJECT,
                &[
                    text(&a.create.storage_id()),
                    text(&command),
                    int(generation)?,
                ],
            )
            .await
            .map_err(|_| sql::unavailable())?;
            Ok(())
        })
        .await
    }
    async fn project_email_delivery_with_events(
        &self,
        projection: &sql::EmailDeliveryProjection,
    ) -> Result<()> {
        let s = &projection.snapshot;
        wasm_send(async {
            let previous = self
                .first::<sql::SnapshotRow>(sql::GET, &[text(&s.command.storage_id())])
                .await
                .map_err(|_| sql::unavailable())?;
            if let Some(encoded) =
                sql::encode_projection_with_events(projection, previous.as_ref())?
            {
                self.batch(&sql::statements(), &encoded, sql::classify)
                    .await?;
            }
            Ok(())
        })
        .await
    }
    async fn email_delivery(&self, id: &str) -> Result<Option<EmailDeliverySnapshot>> {
        wasm_send(async {
            self.first::<sql::SnapshotRow>(sql::GET, &[text(id)])
                .await
                .map_err(|_| sql::unavailable())?
                .as_ref()
                .map(sql::SnapshotRow::decode)
                .transpose()
        })
        .await
    }
    async fn batch_email_deliveries(
        &self,
        account_id: u64,
        batch_id: BulkBatchId,
    ) -> Result<Vec<EmailDeliverySnapshot>> {
        if account_id == 0 {
            return Err(sql::invalid());
        }
        wasm_send(async {
            let rows = self
                .prepare(sql::LIST, &[int(account_id)?, text(&batch_id.to_string())])?
                .all()
                .await
                .map_err(|_| sql::unavailable())?
                .results::<sql::SnapshotRow>()
                .map_err(|_| sql::invalid())?;
            if rows.len() > 200 {
                return Err(sql::invalid());
            }
            rows.iter()
                .map(|r| {
                    let s = r.decode()?;
                    if s.command.account_id != account_id || s.command.batch.batch_id != batch_id {
                        return Err(sql::invalid());
                    }
                    Ok(s)
                })
                .collect()
        })
        .await
    }
}
