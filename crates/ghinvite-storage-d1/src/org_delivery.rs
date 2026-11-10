use super::{D1Storage, int, text, wasm_send};
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
impl OrgDeliveryStorage for D1Storage {
    async fn project_organization_telemetry(
        &self,
        telemetry: &ghinvite_core::storage::organization_telemetry::OrganizationStepTelemetry,
    ) -> Result<()> {
        use ghinvite_core::storage::organization_telemetry as telemetry_sql;
        wasm_send(async {
            let input = telemetry_sql::encode(telemetry)?;
            self.batch(
                &telemetry_sql::statements(),
                &input,
                ghinvite_core::storage::delivery_projection::classify,
            )
            .await
        })
        .await
    }

    async fn claim_organization_attempt(
        &self,
        attempt: &OrganizationAttempt,
    ) -> Result<Option<u64>> {
        wasm_send(async {
            let encoded = sql::encode_attempt(attempt)?;
            let values = [
                text(&attempt.create.storage_id()),
                text(&attempt.step()),
                text(&encoded),
            ];
            let generation = self.first::<Generation>(sql::CLAIM, &values).await?;
            let row = self.first::<sql::AttemptRow>(sql::ATTEMPT, &values).await?;
            sql::check_attempt(row, &encoded)?.ok_or_else(|| {
                Error::Database("organization attempt missing after claim".into())
            })?;
            Ok(generation.map(|g| g.generation))
        })
        .await
    }
    async fn organization_attempt_exists(&self, attempt: &OrganizationAttempt) -> Result<bool> {
        wasm_send(async {
            let encoded = sql::encode_attempt(attempt)?;
            let row = self
                .first::<sql::AttemptRow>(
                    sql::ATTEMPT,
                    &[
                        text(&attempt.create.storage_id()),
                        text(&attempt.step()),
                        text(&encoded),
                    ],
                )
                .await?;
            Ok(sql::check_attempt(row, &encoded)?.unwrap_or(false))
        })
        .await
    }
    async fn reject_organization_attempt(
        &self,
        attempt: &OrganizationAttempt,
        generation: u64,
    ) -> Result<()> {
        wasm_send(async {
            let encoded = sql::encode_attempt(attempt)?;
            self.run(
                sql::REJECT,
                &[
                    text(&attempt.create.storage_id()),
                    text(&attempt.step()),
                    text(&encoded),
                    int(generation)?,
                ],
            )
            .await?;
            Ok(())
        })
        .await
    }
    async fn project_organization_delivery_with_events(
        &self,
        projection: &ghinvite_core::delivery_audit::OrganizationDeliveryProjection,
    ) -> Result<()> {
        let snapshot = &projection.snapshot;
        wasm_send(async {
            let row = self
                .first::<sql::SnapshotRow>(sql::GET, &[text(&snapshot.create.command.storage_id())])
                .await?;
            let input = sql::encode_projection_with_events(projection, row.as_ref())?;
            self.batch(&sql::statements(), &input, sql::classify).await
        })
        .await
    }
    async fn get_organization_delivery(
        &self,
        id: &str,
    ) -> Result<Option<OrganizationDeliverySnapshot>> {
        wasm_send(async {
            self.first::<sql::SnapshotRow>(sql::GET, &[text(id)])
                .await?
                .as_ref()
                .map(sql::SnapshotRow::decode)
                .transpose()
        })
        .await
    }
}

#[async_trait]
impl ghinvite_core::storage::org_delivery_reads::OrgDeliveryReadStorage for D1Storage {
    async fn list_organization_deliveries(
        &self,
        query: &ghinvite_core::storage::org_delivery_reads::OrgDeliveryQuery,
    ) -> Result<Vec<OrganizationDeliverySnapshot>> {
        use ghinvite_core::storage::org_delivery_reads as reads;
        wasm_send(async {
            self.all::<sql::SnapshotRow>(
                reads::LIST,
                &[
                    int(query.account_id)?,
                    text(&query.link_id.to_string()),
                    text(&query.request_id.to_string()),
                ],
            )
            .await?
            .into_iter()
            .map(|row| reads::decode(row, query))
            .collect()
        })
        .await
    }
}
