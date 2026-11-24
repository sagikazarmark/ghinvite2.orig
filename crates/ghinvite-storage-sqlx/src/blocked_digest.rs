use crate::{SqlxStorage, decode, to_db_err, u64_to_i64};
use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use ghinvite_core::storage::{
    Result,
    blocked_digest::{self, BlockedDigest, DailyDigestStorage},
    console_scope::ConsoleScope,
    sweep_digest,
};
#[async_trait]
impl DailyDigestStorage for SqlxStorage {
    async fn record_blocked_digest(&self, now: DateTime<Utc>) -> Result<()> {
        let input = blocked_digest::record_parameters(now)?;
        let mut tx = self.pool.begin().await.map_err(to_db_err)?;
        sqlx::query(blocked_digest::RECORD_HEADER)
            .bind(&input.day)
            .bind(&input.observed_at)
            .execute(&mut *tx)
            .await
            .map_err(to_db_err)?;
        for statement in [
            blocked_digest::RECORD_ITEMS,
            blocked_digest::RECORD_BOUND,
            blocked_digest::CLEAR_ASSERTIONS,
            blocked_digest::RECORD_COMPLETE,
        ] {
            sqlx::query(statement)
                .bind(&input.day)
                .execute(&mut *tx)
                .await
                .map_err(to_db_err)?;
        }
        sqlx::query(blocked_digest::CLEANUP)
            .execute(&mut *tx)
            .await
            .map_err(to_db_err)?;
        tx.commit().await.map_err(to_db_err)
    }
    async fn record_sweep_health(
        &self,
        now: DateTime<Utc>,
        report: &ghinvite_core::sweep_health::WatchdogReport,
    ) -> Result<()> {
        let input = sweep_digest::record_parameters(now, report)?;
        let mut tx = self.pool.begin().await.map_err(to_db_err)?;
        sqlx::query(sweep_digest::RECORD)
            .bind(&input.day)
            .bind(&input.observed_at)
            .bind(u64_to_i64(input.schedules)?)
            .bind(u64_to_i64(input.rearmed)?)
            .bind(u64_to_i64(input.unavailable)?)
            .bind(&input.oldest_overdue_schedule)
            .bind(&input.oldest_overdue_due_at)
            .bind(u64_to_i64(input.failed_schedules)?)
            .bind(&input.latest_failed_schedule)
            .bind(&input.latest_failed_at)
            .execute(&mut *tx)
            .await
            .map_err(to_db_err)?;
        sqlx::query(sweep_digest::CLEANUP)
            .execute(&mut *tx)
            .await
            .map_err(to_db_err)?;
        tx.commit().await.map_err(to_db_err)
    }
    async fn sweep_digest(&self, day: NaiveDate) -> Result<Option<sweep_digest::SweepDigest>> {
        blocked_digest::validate_day(day)?;
        let row = sqlx::query(sweep_digest::READ)
            .bind(day.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(to_db_err)?;
        sweep_digest::decode(row.as_ref().map(decode::<sweep_digest::Row>).transpose()?)
    }
}
impl SqlxStorage {
    pub(crate) async fn read_blocked_digest(
        &self,
        scope: &ConsoleScope,
        day: NaiveDate,
    ) -> Result<Option<BlockedDigest>> {
        blocked_digest::validate_day(day)?;
        let rows = sqlx::query(blocked_digest::SCOPED)
            .bind(day.to_string())
            .bind(u64_to_i64(scope.account_id())?)
            .bind(scope.repo_ids_json())
            .fetch_all(&self.pool)
            .await
            .map_err(to_db_err)?;
        blocked_digest::decode(
            rows.iter()
                .map(decode::<blocked_digest::Row>)
                .collect::<Result<_>>()?,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghinvite_core::storage::test_suite;
    #[tokio::test]
    async fn transactional_failure_preserves_prior_window_and_all_columns() {
        let s = SqlxStorage::in_memory().await.unwrap();
        test_suite::scenario_blocked_digest(s.clone()).await;
        // Imported inconsistent rows still cannot inflate the daily signal.
        sqlx::query("UPDATE deliveries SET create_outcome='blocked',outcome_reason='mint_forbidden' WHERE tracking_stopped_reason IS NOT NULL OR settlement IS NOT NULL").execute(&s.pool).await.unwrap();
        let before: Vec<serde_json::Value> = sqlx::query("SELECT * FROM deliveries ORDER BY id")
            .fetch_all(&s.pool)
            .await
            .unwrap()
            .iter()
            .map(|r| decode(r).unwrap())
            .collect();
        let headers: Vec<String> =
            sqlx::query_scalar("SELECT day FROM blocked_digest_days ORDER BY day")
                .fetch_all(&s.pool)
                .await
                .unwrap();
        let items: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM blocked_digest_items")
            .fetch_one(&s.pool)
            .await
            .unwrap();
        sqlx::query("CREATE TRIGGER digest_failure BEFORE INSERT ON blocked_digest_items BEGIN SELECT RAISE(ABORT,'digest-test-failure'); END").execute(&s.pool).await.unwrap();
        let later = "2026-05-12T00:05:00Z".parse().unwrap();
        assert!(s.record_blocked_digest(later).await.is_err());
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT day FROM blocked_digest_days ORDER BY day")
                .fetch_all(&s.pool)
                .await
                .unwrap(),
            headers
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM blocked_digest_items")
                .fetch_one(&s.pool)
                .await
                .unwrap(),
            items
        );
        sqlx::query("DROP TRIGGER digest_failure")
            .execute(&s.pool)
            .await
            .unwrap();
        s.record_blocked_digest(later).await.unwrap();
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM blocked_digest_items WHERE day='2026-05-12' AND account_id=100").fetch_one(&s.pool).await.unwrap(),5);
        let after: Vec<serde_json::Value> = sqlx::query("SELECT * FROM deliveries ORDER BY id")
            .fetch_all(&s.pool)
            .await
            .unwrap()
            .iter()
            .map(|r| decode(r).unwrap())
            .collect();
        assert_eq!(before, after);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM blocked_digest_days")
                .fetch_one(&s.pool)
                .await
                .unwrap(),
            7
        );
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM blocked_digest_items i LEFT JOIN blocked_digest_days h ON h.day=i.day WHERE h.day IS NULL").fetch_one(&s.pool).await.unwrap(),0);
        // Same-day and backward retries do not inspect mutable delivery facts.
        sqlx::query("CREATE TRIGGER digest_no_retry_scan BEFORE INSERT ON blocked_digest_items BEGIN SELECT RAISE(ABORT,'must-not-reinsert'); END").execute(&s.pool).await.unwrap();
        s.record_blocked_digest(later).await.unwrap();
        s.record_blocked_digest("2026-05-04T00:00:00Z".parse().unwrap())
            .await
            .unwrap();
    }
}

#[cfg(test)]
mod operational_bounds {
    use super::*;
    use ghinvite_core::storage::OperationalReadStorage;

    #[tokio::test]
    async fn report_and_daily_snapshot_refuse_overflow_without_publishing_partial_health() {
        let s = SqlxStorage::in_memory().await.unwrap();
        ghinvite_core::storage::test_suite::scenario_repository_telemetry(s.clone()).await;
        let now = "2026-05-04T16:00:00Z".parse().unwrap();
        s.record_blocked_digest(now).await.unwrap();
        let original: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM blocked_digest_items")
            .fetch_one(&s.pool)
            .await
            .unwrap();
        sqlx::query("WITH RECURSIVE n(i) AS (SELECT 100 UNION ALL SELECT i+1 FROM n WHERE i<10100) INSERT INTO deliveries(id,request_id,kind,target_id,delivery_generation,create_outcome,outcome_reason,created_at,updated_at) SELECT r.id||':'||n.i||':1',r.id,'repo',n.i,1,'blocked','mint_forbidden',r.created_at,r.created_at FROM n JOIN (SELECT * FROM invitation_requests ORDER BY id LIMIT 1) r").execute(&s.pool).await.unwrap();
        assert!(matches!(
            s.delivery_signals(now).await,
            Err(ghinvite_core::storage::Error::Database(_))
        ));
        assert!(
            s.record_blocked_digest("2026-05-05T00:05:00Z".parse().unwrap())
                .await
                .is_err()
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM blocked_digest_days")
                .fetch_one(&s.pool)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM blocked_digest_items")
                .fetch_one(&s.pool)
                .await
                .unwrap(),
            original
        );
        // A completed day's immutable snapshot is still replayable at capacity.
        s.record_blocked_digest(now).await.unwrap();
    }
}

#[cfg(test)]
mod migration_tests {
    #[tokio::test]
    async fn existing_daily_snapshot_keeps_repository_identity_and_exact_observation() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        for migration in [
            include_str!("../../../migrations/0001_initial.sql"),
            include_str!("../../../migrations/0002_invite_budget.sql"),
            include_str!("../../../migrations/0003_pending_signin.sql"),
            include_str!("../../../migrations/0004_blocked_digest.sql"),
            include_str!("../../../migrations/0005_link_pause.sql"),
            include_str!("../../../migrations/0006_request_cancellation.sql"),
            include_str!("../../../migrations/0007_link_budget_share.sql"),
            include_str!("../../../migrations/0008_organization_delivery_steps.sql"),
            include_str!("../../../migrations/0009_organization_webhook_receipts.sql"),
            include_str!("../../../migrations/0010_request_lapsed.sql"),
            include_str!("../../../migrations/0011_email_domains.sql"),
            include_str!("../../../migrations/0012_delivery_audit_version.sql"),
        ] {
            sqlx::raw_sql(migration).execute(&pool).await.unwrap();
        }
        sqlx::query("INSERT INTO blocked_digest_days(day,observed_at,complete) VALUES('2026-05-04','2026-05-04T00:05:00.123456789Z',1)").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO blocked_digest_items(day,delivery_key,link_id,account_id,reason) VALUES('2026-05-04','old-delivery','old-link',100,'installation_permissions')").execute(&pool).await.unwrap();
        sqlx::raw_sql(include_str!(
            "../../../migrations/0013_organization_telemetry.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();
        let actual: (String, String, String, String, String, i64, String) = sqlx::query_as(
            "SELECT day,delivery_key,step,kind,link_id,account_id,reason FROM blocked_digest_items",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            actual,
            (
                "2026-05-04".into(),
                "old-delivery".into(),
                "repo".into(),
                "repo".into(),
                "old-link".into(),
                100,
                "installation_permissions".into()
            )
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT observed_at FROM blocked_digest_days WHERE complete=1"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            "2026-05-04T00:05:00.123456789Z"
        );
    }
}
