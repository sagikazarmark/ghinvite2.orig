use super::{D1Storage, int, signed_int, text, wasm_send};
#[cfg(not(target_arch = "wasm32"))]
use crate::native as worker;
use async_trait::async_trait;
use ghinvite_core::{
    email_delivery::EmailRef,
    storage::{
        Result,
        email_review::{self as sql, EmailReviewStorage, NewReviewEmail, ReviewEmailRecord},
        email_vault::{EmailVaultBinding, time, unavailable},
    },
};
#[async_trait]
impl EmailReviewStorage for D1Storage {
    async fn retain_review_email(&self, record: &NewReviewEmail) -> Result<bool> {
        record.validate()?;
        let b = &record.binding;
        wasm_send(async {
            let row: Option<serde_json::Value> = self
                .first(
                    sql::RETAIN,
                    &[
                        text(&b.email_ref.as_hex()),
                        text(&b.batch.batch_id.to_string()),
                        int(u64::from(b.batch.row))?,
                        int(b.account_id)?,
                        int(b.installation_id)?,
                        text(&record.ciphertext),
                        signed_int(record.created_at)?,
                    ],
                )
                .await
                .map_err(|_| unavailable())?;
            Ok(row.is_some())
        })
        .await
    }
    async fn review_email(&self, email_ref: EmailRef) -> Result<Option<ReviewEmailRecord>> {
        wasm_send(async {
            let session = self
                .db
                .with_session_constraint(worker::D1SessionConstraint::FirstPrimary)
                .map_err(|_| unavailable())?;
            let row = session
                .prepare(sql::READ)
                .bind(&[text(&email_ref.as_hex())])
                .map_err(|_| unavailable())?
                .first::<serde_json::Value>(None)
                .await
                .map_err(|_| unavailable())?;
            let record: Option<ReviewEmailRecord> = row
                .map(serde_json::from_value)
                .transpose()
                .map_err(|_| unavailable())?;
            if let Some(record) = &record {
                record.validate()?;
            }
            Ok(record)
        })
        .await
    }
    async fn clear_review_email(&self, b: &EmailVaultBinding, now: i64) -> Result<bool> {
        self.email_review_transition(sql::CLEAR, b, now).await
    }
    async fn prune_review_emails(&self, now: i64) -> Result<u64> {
        time(now)?;
        wasm_send(async {
            let rows = self
                .prepare(sql::PRUNE, &[signed_int(now)?])?
                .all()
                .await
                .map_err(|_| unavailable())?;
            Ok(rows
                .results::<serde_json::Value>()
                .map_err(|_| unavailable())?
                .len() as u64)
        })
        .await
    }
}
impl D1Storage {
    async fn email_review_transition(
        &self,
        statement: &str,
        b: &EmailVaultBinding,
        now: i64,
    ) -> Result<bool> {
        b.validate()?;
        time(now)?;
        wasm_send(async {
            let row: Option<serde_json::Value> = self
                .first(
                    statement,
                    &[
                        text(&b.email_ref.as_hex()),
                        text(&b.batch.batch_id.to_string()),
                        int(u64::from(b.batch.row))?,
                        int(b.account_id)?,
                        int(b.installation_id)?,
                        signed_int(now)?,
                    ],
                )
                .await
                .map_err(|_| unavailable())?;
            Ok(row.is_some())
        })
        .await
    }
}
