use crate::{SqlxStorage, decode, to_db_err, u64_to_i64};
use async_trait::async_trait;
use ghinvite_core::{
    org_delivery::{OrganizationAttempt, OrganizationDeliverySnapshot},
    storage::{
        Error, Result,
        delivery_attempts::Generation,
        org_delivery::{self as sql, OrgDeliveryStorage},
    },
};
#[async_trait]
impl OrgDeliveryStorage for SqlxStorage {
    async fn project_organization_telemetry(
        &self,
        telemetry: &ghinvite_core::storage::organization_telemetry::OrganizationStepTelemetry,
    ) -> Result<()> {
        use ghinvite_core::storage::organization_telemetry as telemetry_sql;
        let input = telemetry_sql::encode(telemetry)?;
        self.transaction(
            &telemetry_sql::statements(),
            &input,
            ghinvite_core::storage::delivery_projection::classify,
        )
        .await
    }

    async fn claim_organization_attempt(
        &self,
        attempt: &OrganizationAttempt,
    ) -> Result<Option<u64>> {
        let encoded = sql::encode_attempt(attempt)?;
        let id = attempt.create.storage_id();
        let step = attempt.step();
        let generation = sqlx::query(sql::CLAIM)
            .bind(&id)
            .bind(&step)
            .bind(&encoded)
            .fetch_optional(&self.pool)
            .await
            .map_err(to_db_err)?
            .as_ref()
            .map(decode::<Generation>)
            .transpose()?;
        let row = sqlx::query(sql::ATTEMPT)
            .bind(&id)
            .bind(&step)
            .bind(&encoded)
            .fetch_optional(&self.pool)
            .await
            .map_err(to_db_err)?
            .as_ref()
            .map(decode::<sql::AttemptRow>)
            .transpose()?;
        sql::check_attempt(row, &encoded)?
            .ok_or_else(|| Error::Database("organization attempt missing after claim".into()))?;
        Ok(generation.map(|g| g.generation))
    }
    async fn organization_attempt_exists(&self, attempt: &OrganizationAttempt) -> Result<bool> {
        let encoded = sql::encode_attempt(attempt)?;
        let row = sqlx::query(sql::ATTEMPT)
            .bind(attempt.create.storage_id())
            .bind(attempt.step())
            .bind(&encoded)
            .fetch_optional(&self.pool)
            .await
            .map_err(to_db_err)?
            .as_ref()
            .map(decode::<sql::AttemptRow>)
            .transpose()?;
        Ok(sql::check_attempt(row, &encoded)?.unwrap_or(false))
    }
    async fn reject_organization_attempt(
        &self,
        attempt: &OrganizationAttempt,
        generation: u64,
    ) -> Result<()> {
        let encoded = sql::encode_attempt(attempt)?;
        sqlx::query(sql::REJECT)
            .bind(attempt.create.storage_id())
            .bind(attempt.step())
            .bind(encoded)
            .bind(u64_to_i64(generation)?)
            .execute(&self.pool)
            .await
            .map_err(to_db_err)?;
        Ok(())
    }
    async fn project_organization_delivery_with_events(
        &self,
        projection: &ghinvite_core::delivery_audit::OrganizationDeliveryProjection,
    ) -> Result<()> {
        let snapshot = &projection.snapshot;
        let row = sqlx::query(sql::GET)
            .bind(snapshot.create.command.storage_id())
            .fetch_optional(&self.pool)
            .await
            .map_err(to_db_err)?
            .as_ref()
            .map(decode::<sql::SnapshotRow>)
            .transpose()?;
        let input = sql::encode_projection_with_events(projection, row.as_ref())?;
        let mut tx = self.pool.begin().await.map_err(to_db_err)?;
        for statement in sql::statements() {
            sqlx::query(&statement)
                .bind(&input)
                .execute(&mut *tx)
                .await
                .map_err(|e| sql::classify(e.to_string()))?;
        }
        tx.commit().await.map_err(to_db_err)
    }
    async fn get_organization_delivery(
        &self,
        id: &str,
    ) -> Result<Option<OrganizationDeliverySnapshot>> {
        sqlx::query(sql::GET)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(to_db_err)?
            .as_ref()
            .map(|r| decode::<sql::SnapshotRow>(r)?.decode())
            .transpose()
    }
}

#[async_trait]
impl ghinvite_core::storage::org_delivery_reads::OrgDeliveryReadStorage for SqlxStorage {
    async fn list_organization_deliveries(
        &self,
        query: &ghinvite_core::storage::org_delivery_reads::OrgDeliveryQuery,
    ) -> Result<Vec<OrganizationDeliverySnapshot>> {
        use ghinvite_core::storage::org_delivery_reads as reads;
        sqlx::query(reads::LIST)
            .bind(u64_to_i64(query.account_id)?)
            .bind(query.link_id.to_string())
            .bind(query.request_id.to_string())
            .fetch_all(&self.pool)
            .await
            .map_err(to_db_err)?
            .iter()
            .map(|row| reads::decode(decode::<sql::SnapshotRow>(row)?, query))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn scoped_reads_reject_receipt_identity_corruption() {
        use ghinvite_core::storage::org_delivery_reads::{
            OrgDeliveryQuery, OrgDeliveryReadStorage,
        };
        let storage = SqlxStorage::in_memory().await.unwrap();
        let clone = storage.clone();
        ghinvite_core::storage::test_suite::scenario_organization_delivery_reads(clone).await;
        let row = sqlx::query("SELECT d.id,o.receipt FROM deliveries d JOIN delivery_outcomes o ON o.delivery_id=d.id WHERE d.kind='org' ORDER BY d.delivery_generation LIMIT 1")
            .fetch_one(&storage.pool).await.unwrap();
        let row = decode::<sql::SnapshotRow>(&row).unwrap();
        let snapshot = row.decode().unwrap();
        let command = &snapshot.create.command;
        let scope = OrgDeliveryQuery {
            account_id: command.account_id,
            link_id: command.link_id.expect("linked fixture"),
            request_id: command.request_id,
        };
        let mut corrupted = snapshot.clone();
        corrupted.create.command.link_id = Some(ghinvite_core::InvitationLinkId::new());
        let encoded = serde_json::to_string(&corrupted).unwrap();
        // The database rejects immutable identity replacement before it can
        // reach a reader; the decoder independently rejects corrupt old data.
        let rejected = sqlx::query("UPDATE delivery_outcomes SET receipt=? WHERE delivery_id=?")
            .bind(&encoded)
            .bind(&row.id)
            .execute(&storage.pool)
            .await;
        assert!(rejected.is_err());
        assert!(matches!(
            ghinvite_core::storage::org_delivery_reads::decode(
                sql::SnapshotRow {
                    id: row.id,
                    receipt: encoded
                },
                &scope
            ),
            Err(Error::Corrupt(_))
        ));
        assert_eq!(
            storage.list_organization_deliveries(&scope).await.unwrap()[0],
            snapshot
        );
        let outside = OrgDeliveryQuery {
            account_id: scope.account_id + 1,
            ..scope
        };
        assert!(
            storage
                .list_organization_deliveries(&outside)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn concurrent_projection_predecessor_change_is_retryable_and_atomic() {
        let storage = SqlxStorage::in_memory().await.unwrap();
        let clone = storage.clone();
        ghinvite_core::storage::test_suite::scenario_organization_delivery(clone).await;
        let id: String = sqlx::query_scalar(
            "SELECT id FROM deliveries WHERE kind='org' ORDER BY delivery_generation LIMIT 1",
        )
        .fetch_one(&storage.pool)
        .await
        .unwrap();
        let row = sqlx::query(sql::GET)
            .bind(&id)
            .fetch_one(&storage.pool)
            .await
            .unwrap();
        let before = decode::<sql::SnapshotRow>(&row).unwrap();
        let mut next = before.decode().unwrap();
        next.revision += 1;
        let delayed = sql::encode_projection(&next, Some(&before))
            .unwrap()
            .unwrap();
        storage.project_organization_delivery(&next).await.unwrap();
        let mut tx = storage.pool.begin().await.unwrap();
        let mut conflict = None;
        for statement in sql::statements() {
            if let Err(error) = sqlx::query(&statement)
                .bind(&delayed)
                .execute(&mut *tx)
                .await
            {
                conflict = Some(sql::classify(error.to_string()));
                break;
            }
        }
        assert!(
            matches!(conflict, Some(Error::Database(_))),
            "a changed predecessor is not a domain conflict"
        );
        tx.rollback().await.unwrap();
        assert_eq!(
            storage.get_organization_delivery(&id).await.unwrap(),
            Some(next)
        );
        let assertions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM projection_assertions")
            .fetch_one(&storage.pool)
            .await
            .unwrap();
        assert_eq!(assertions, 0);

        // A delayed audit-only batch must revalidate after a predecessor race.
        use ghinvite_core::delivery_audit::{
            DeliveryAuditFact, DeliveryAuditKind, DeliveryAuditStep, OrganizationDeliveryProjection,
        };
        let before = decode::<sql::SnapshotRow>(
            &sqlx::query(sql::GET)
                .bind(&id)
                .fetch_one(&storage.pool)
                .await
                .unwrap(),
        )
        .unwrap();
        let projection = OrganizationDeliveryProjection {
            snapshot: before.decode().unwrap(),
            audit_events: vec![DeliveryAuditFact {
                step: DeliveryAuditStep::Organization,
                transition_revision: 1,
                at: before.decode().unwrap().create.command.approved_at,
                kind: DeliveryAuditKind::OutcomeUnknown,
            }],
        };
        let event_id = projection.events().unwrap()[0].id.to_string();
        let encoded = sql::encode_projection_with_events(&projection, Some(&before)).unwrap();
        let mut newer = projection.snapshot.clone();
        newer.revision += 1;
        storage.project_organization_delivery(&newer).await.unwrap();
        let mut tx = storage.pool.begin().await.unwrap();
        let mut rejected = false;
        for statement in sql::statements() {
            if let Err(error) = sqlx::query(&statement)
                .bind(&encoded)
                .execute(&mut *tx)
                .await
            {
                assert!(matches!(
                    sql::classify(error.to_string()),
                    Error::Database(_)
                ));
                rejected = true;
                break;
            }
        }
        assert!(rejected);
        tx.rollback().await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_events WHERE id=?")
            .bind(&event_id)
            .fetch_one(&storage.pool)
            .await
            .unwrap();
        assert_eq!(count, 0, "racing audit-only batch must not commit its fact");
        storage
            .project_organization_delivery_with_events(&projection)
            .await
            .unwrap();
        storage
            .project_organization_delivery_with_events(&projection)
            .await
            .unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_events WHERE id=?")
            .bind(&event_id)
            .fetch_one(&storage.pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(
            storage.get_organization_delivery(&id).await.unwrap(),
            Some(newer)
        );
    }
}

#[cfg(test)]
mod audit_version_tests;
