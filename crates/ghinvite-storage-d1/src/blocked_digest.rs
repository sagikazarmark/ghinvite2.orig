use super::{D1Storage, int, text, wasm_send};
use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use ghinvite_core::storage::{
    Result,
    blocked_digest::{self, BlockedDigest, DailyDigestStorage},
    console_scope::ConsoleScope,
    sweep_digest,
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
    async fn record_sweep_health(
        &self,
        now: DateTime<Utc>,
        report: &ghinvite_core::sweep_health::WatchdogReport,
    ) -> Result<()> {
        wasm_send(async {
            let input = sweep_digest::record_parameters(now, report)?;
            let optional =
                |value: &Option<String>| value.as_deref().map(text).unwrap_or(super::JsValue::NULL);
            let statements = vec![
                self.prepare(
                    sweep_digest::RECORD,
                    &[
                        text(&input.day),
                        text(&input.observed_at),
                        int(input.schedules)?,
                        int(input.rearmed)?,
                        int(input.unavailable)?,
                        optional(&input.oldest_overdue_schedule),
                        optional(&input.oldest_overdue_due_at),
                        int(input.failed_schedules)?,
                        optional(&input.latest_failed_schedule),
                        optional(&input.latest_failed_at),
                    ],
                )?,
                self.prepare(sweep_digest::CLEANUP, &[])?,
            ];
            self.db
                .batch(statements)
                .await
                .map_err(|error| ghinvite_core::storage::Error::Database(error.to_string()))?;
            Ok(())
        })
        .await
    }
    async fn sweep_digest(&self, day: NaiveDate) -> Result<Option<sweep_digest::SweepDigest>> {
        wasm_send(async {
            blocked_digest::validate_day(day)?;
            sweep_digest::decode(
                self.first::<sweep_digest::Row>(sweep_digest::READ, &[text(&day.to_string())])
                    .await?,
            )
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
