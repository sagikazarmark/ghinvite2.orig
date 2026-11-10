use super::{D1Storage, signed_int, text, wasm_send};
#[cfg(not(target_arch = "wasm32"))]
use crate::native as worker;
use async_trait::async_trait;
use ghinvite_core::storage::{
    Result,
    pending_signin::{self as sql, PendingSignInKey, PendingSignInStorage, StoredPendingSignIn},
};

#[async_trait]
impl PendingSignInStorage for D1Storage {
    async fn replace_pending_signin(
        &self,
        key: PendingSignInKey,
        record: &StoredPendingSignIn,
        now: i64,
    ) -> Result<()> {
        record.validate()?;
        wasm_send(async {
            let statements = vec![
                self.prepare(sql::CLEANUP, &[signed_int(now)?])?,
                self.prepare(
                    sql::REPLACE,
                    &[
                        text(&key.as_hex()),
                        text(&record.ciphertext),
                        signed_int(record.expires_at)?,
                    ],
                )?,
            ];
            self.db
                .batch(statements)
                .await
                .map_err(|_| sql::unavailable())?;
            Ok(())
        })
        .await
    }
    async fn read_pending_signin(
        &self,
        key: PendingSignInKey,
    ) -> Result<Option<StoredPendingSignIn>> {
        wasm_send(async {
            let session = self
                .db
                .with_session_constraint(worker::D1SessionConstraint::FirstPrimary)
                .map_err(|_| sql::unavailable())?;
            let row = session
                .prepare(sql::READ)
                .bind(&[text(&key.as_hex())])
                .map_err(|_| sql::unavailable())?
                .first::<serde_json::Value>(None)
                .await
                .map_err(|_| sql::unavailable())?;
            let record: Option<StoredPendingSignIn> = row
                .map(serde_json::from_value)
                .transpose()
                .map_err(|_| sql::unavailable())?;
            if let Some(record) = &record {
                record.validate()?;
            }
            Ok(record)
        })
        .await
    }
    async fn consume_pending_signin(
        &self,
        key: PendingSignInKey,
        expected: &StoredPendingSignIn,
    ) -> Result<bool> {
        wasm_send(async {
            let row: Option<serde_json::Value> = self
                .first(
                    sql::CONSUME,
                    &[
                        text(&key.as_hex()),
                        text(&expected.ciphertext),
                        signed_int(expected.expires_at)?,
                    ],
                )
                .await
                .map_err(|_| sql::unavailable())?;
            Ok(row.is_some())
        })
        .await
    }
}
