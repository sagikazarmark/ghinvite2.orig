use super::{D1Storage, int, text, wasm_send};
use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use ghinvite_core::storage::{
    Result,
    blocked_digest::{self, BlockedDigest, DailyDigestStorage},
    console_scope::ConsoleScope,
};
#[async_trait]
impl DailyDigestStorage for D1Storage {
    async fn record_blocked_digest(&self, now: DateTime<Utc>) -> Result<()> {
        wasm_send(async {
            let input = blocked_digest::record_parameters(now)?;
            let statements = vec![
                self.prepare(
                    blocked_digest::RECORD_HEADER,
                    &[text(&input.day), text(&input.observed_at)],
                )?,
                self.prepare(blocked_digest::RECORD_ITEMS, &[text(&input.day)])?,
                self.prepare(blocked_digest::RECORD_BOUND, &[text(&input.day)])?,
                self.prepare(blocked_digest::CLEAR_ASSERTIONS, &[text(&input.day)])?,
                self.prepare(blocked_digest::RECORD_COMPLETE, &[text(&input.day)])?,
                self.prepare(blocked_digest::CLEANUP, &[])?,
            ];
            self.db
                .batch(statements)
                .await
                .map_err(|error| ghinvite_core::storage::Error::Database(error.to_string()))?;
            Ok(())
        })
        .await
    }
}
impl D1Storage {
    pub(crate) async fn read_blocked_digest(
        &self,
        scope: &ConsoleScope,
        day: NaiveDate,
    ) -> Result<Option<BlockedDigest>> {
        wasm_send(async {
            blocked_digest::validate_day(day)?;
            let repos = scope
                .repo_ids_json()
                .map(|s| text(&s))
                .unwrap_or(super::JsValue::NULL);
            blocked_digest::decode(
                self.all::<blocked_digest::Row>(
                    blocked_digest::SCOPED,
                    &[text(&day.to_string()), int(scope.account_id())?, repos],
                )
                .await?,
            )
        })
        .await
    }
}
