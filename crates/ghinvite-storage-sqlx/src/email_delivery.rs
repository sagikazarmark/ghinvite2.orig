use crate::{SqlxStorage, decode};
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
impl EmailDeliveryStorage for SqlxStorage {
    async fn claim_email_attempt(&self, a: &EmailAttempt) -> Result<Option<u64>> {
        let command = sql::encode_attempt(a)?;
        let id = a.create.storage_id();
        let mut tx = self.pool.begin().await.map_err(|_| sql::unavailable())?;
        let row = sqlx::query(sql::CLAIM)
            .bind(&id)
            .bind(&command)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| sql::invalid())?;
        let generation: Option<Generation> = row.as_ref().map(decode).transpose()?;
        let row = sqlx::query(sql::ATTEMPT)
            .bind(&id)
            .bind(&command)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| sql::unavailable())?;
        sql::check_attempt(row.as_ref().map(decode).transpose()?, &command)?
            .ok_or_else(sql::unavailable)?;
        tx.commit().await.map_err(|_| sql::unavailable())?;
        Ok(generation.map(|g| g.generation))
    }
    async fn email_attempt_exists(&self, a: &EmailAttempt) -> Result<bool> {
        let command = sql::encode_attempt(a)?;
        let row = sqlx::query(sql::ATTEMPT)
            .bind(a.create.storage_id())
            .bind(&command)
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| sql::unavailable())?;
        Ok(sql::check_attempt(row.as_ref().map(decode).transpose()?, &command)?.unwrap_or(false))
    }
    async fn reject_email_attempt(&self, a: &EmailAttempt, generation: u64) -> Result<()> {
        let command = sql::encode_attempt(a)?;
        ghinvite_core::storage::integer::unsigned(generation)?;
        if generation == 0 {
            return Err(sql::invalid());
        }
        let mut tx = self.pool.begin().await.map_err(|_| sql::unavailable())?;
        sqlx::query(sql::REJECT)
            .bind(a.create.storage_id())
            .bind(command)
            .bind(generation as i64)
            .execute(&mut *tx)
            .await
            .map_err(|_| sql::unavailable())?;
        tx.commit().await.map_err(|_| sql::unavailable())?;
        Ok(())
    }
    async fn project_email_delivery_with_events(
        &self,
        projection: &sql::EmailDeliveryProjection,
    ) -> Result<()> {
        let s = &projection.snapshot;
        let mut tx = self.pool.begin().await.map_err(|_| sql::unavailable())?;
        let row = sqlx::query(sql::GET)
            .bind(s.command.storage_id())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| sql::unavailable())?;
        let previous: Option<sql::SnapshotRow> = row.as_ref().map(decode).transpose()?;
        if let Some(encoded) = sql::encode_projection_with_events(projection, previous.as_ref())? {
            for statement in sql::statements() {
                sqlx::query(&statement)
                    .bind(&encoded)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| sql::classify(e.to_string()))?;
            }
        }
        tx.commit().await.map_err(|_| sql::unavailable())?;
        Ok(())
    }
    async fn email_delivery(&self, id: &str) -> Result<Option<EmailDeliverySnapshot>> {
        let row = sqlx::query(sql::GET)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| sql::unavailable())?;
        row.as_ref()
            .map(|r| decode::<sql::SnapshotRow>(r)?.decode())
            .transpose()
    }
    async fn batch_email_deliveries(
        &self,
        account_id: u64,
        batch_id: BulkBatchId,
    ) -> Result<Vec<EmailDeliverySnapshot>> {
        ghinvite_core::storage::integer::unsigned(account_id)?;
        if account_id == 0 {
            return Err(sql::invalid());
        }
        let rows = sqlx::query(sql::LIST)
            .bind(account_id as i64)
            .bind(batch_id.to_string())
            .fetch_all(&self.pool)
            .await
            .map_err(|_| sql::unavailable())?;
        if rows.len() > 200 {
            return Err(sql::invalid());
        }
        rows.iter()
            .map(|r| {
                let s = decode::<sql::SnapshotRow>(r)?.decode()?;
                if s.command.account_id != account_id || s.command.batch.batch_id != batch_id {
                    return Err(sql::invalid());
                }
                Ok(s)
            })
            .collect()
    }
}

#[cfg(test)]
mod telemetry_tests {
    use super::*;
    use serde_json::{Value, json};
    #[tokio::test]
    async fn raw_email_clock_guards_preserve_nanoseconds_and_legacy_absence() {
        let s = SqlxStorage::in_memory().await.unwrap();
        let batch = BulkBatchId::new().to_string();
        let id = format!("email:{batch}:1:42");
        let base = json!({"command":{"batch":{"batch_id":batch,"row":1},"account_id":42,
            "approved_at":"2026-01-01T00:00:00.000000001Z","organization":{"org_id":42}},
            "revision":1,"outcome":{"kind":"planned"}});
        let insert = "INSERT INTO email_deliveries(id,batch_id,row_number,account_id,receipt) VALUES(?1,?2,1,42,?3)";
        for bad in [
            json!(4),
            json!("invalid"),
            json!("2026-01-01T00:00:00Z"),
            json!("2026-01-01T00:00:00.000000000Z"),
        ] {
            let mut v = base.clone();
            v["unknown_since"] = bad;
            assert!(
                sqlx::query(insert)
                    .bind(&id)
                    .bind(&batch)
                    .bind(v.to_string())
                    .execute(&s.pool)
                    .await
                    .is_err()
            );
        }
        sqlx::query(insert)
            .bind(&id)
            .bind(&batch)
            .bind(base.to_string())
            .execute(&s.pool)
            .await
            .unwrap();
        let mut valid = base.clone();
        valid["revision"] = json!(2);
        valid["unknown_since"] = json!("2026-01-01T00:00:00.000000002Z");
        valid["last_observed_at"] = json!("2026-01-01T00:00:00.000000003Z");
        let update = "UPDATE email_deliveries SET receipt=?1 WHERE id=?2";
        sqlx::query(update)
            .bind(valid.to_string())
            .bind(&id)
            .execute(&s.pool)
            .await
            .unwrap();
        for (field, bad) in [
            ("unknown_since", Value::Null),
            ("unknown_since", json!("2026-01-01T00:00:00.000000003Z")),
            ("last_observed_at", Value::Null),
            ("last_observed_at", json!("2026-01-01T00:00:00.000000002Z")),
            ("confirmed_at", json!("2026-01-01T00:00:00.000000004Z")),
        ] {
            let mut v = valid.clone();
            v["revision"] = json!(3);
            v[field] = bad;
            assert!(
                sqlx::query(update)
                    .bind(v.to_string())
                    .bind(&id)
                    .execute(&s.pool)
                    .await
                    .is_err(),
                "{field}"
            );
        }
        let retained: String =
            sqlx::query_scalar("SELECT receipt FROM email_deliveries WHERE id=?1")
                .bind(&id)
                .fetch_one(&s.pool)
                .await
                .unwrap();
        assert_eq!(serde_json::from_str::<Value>(&retained).unwrap(), valid);
        valid["revision"] = json!(3);
        valid["last_observed_at"] = json!("2026-01-01T00:00:00.000000004Z");
        sqlx::query(update)
            .bind(valid.to_string())
            .bind(&id)
            .execute(&s.pool)
            .await
            .unwrap();
    }
    #[tokio::test]
    async fn delayed_email_history_retries_after_interleaved_projection_without_partial_audit() {
        use ghinvite_core::storage::{AppInstallationStorage, ConsoleStorage, Error};
        let s = SqlxStorage::in_memory().await.unwrap();
        let at = chrono::DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        s.put_app_installation(&ghinvite_core::account::Installation::repo(
            (ghinvite_core::Account {
                installation_id: 2,
                account_id: 100,
                account_login: "acme".into(),
                account_type: ghinvite_core::AccountType::Organization,
                installed_at: at,
                uninstalled_at: None,
                selected_repos: ghinvite_core::SelectedRepos::All,
            })
            .clone(),
        ))
        .await
        .unwrap();
        let mut snapshot: EmailDeliverySnapshot = serde_json::from_value(json!({"command":{
            "batch":{"batch_id":BulkBatchId::new(),"row":1},"account_id":100,"installation_id":2,
            "organization":{"org_id":100,"org_login":"acme","grant":"member"},"teams":[],
            "email_ref":"ab".repeat(32),"approved_by":7,"approved_at":at},
            "revision":2,"outcome":{"kind":"blocked","reason":"no_seat"}}))
        .unwrap();
        s.project_email_delivery(&snapshot).await.unwrap();
        let old: sql::SnapshotRow = decode(
            &sqlx::query(sql::GET)
                .bind(snapshot.command.storage_id())
                .fetch_one(&s.pool)
                .await
                .unwrap(),
        )
        .unwrap();
        let projection = sql::EmailDeliveryProjection {
            snapshot: snapshot.clone(),
            audit_events: vec![sql::EmailAuditFact {
                transition_revision: 2,
                at,
                kind: sql::EmailAuditKind::Blocked {
                    reason: ghinvite_core::email_delivery::EmailBlockedReason::NoSeat,
                },
            }],
        };
        let stale = sql::encode_projection_with_events(&projection, Some(&old))
            .unwrap()
            .unwrap();
        snapshot.revision = 3;
        snapshot.outcome = ghinvite_core::email_delivery::EmailCreateOutcome::OutcomeUnknown;
        snapshot.unknown_since = Some(at + chrono::Duration::seconds(1));
        s.project_email_delivery(&snapshot).await.unwrap();
        let mut tx = s.pool.begin().await.unwrap();
        let failure = sqlx::query(&sql::statements()[0])
            .bind(&stale)
            .execute(&mut *tx)
            .await
            .unwrap_err();
        assert!(matches!(
            sql::classify(failure.to_string()),
            Error::Database(_)
        ));
        tx.rollback().await.unwrap();
        assert!(
            s.list_audit_events(
                100,
                Some(ghinvite_core::audit::EventType::DeliveryBlocked),
                Default::default()
            )
            .await
            .unwrap()
            .events
            .is_empty()
        );
        s.project_email_delivery_with_events(&projection)
            .await
            .unwrap();
        s.project_email_delivery_with_events(&projection)
            .await
            .unwrap();
        assert_eq!(
            s.email_delivery(&snapshot.command.storage_id())
                .await
                .unwrap(),
            Some(snapshot)
        );
        assert_eq!(
            s.list_audit_events(
                100,
                Some(ghinvite_core::audit::EventType::DeliveryBlocked),
                Default::default()
            )
            .await
            .unwrap()
            .events
            .len(),
            1
        );
    }
}
