use crate::{SqlxStorage, decode, to_db_err};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ghinvite_core::{
    org_webhook::OrganizationWebhookWake,
    storage::{
        Result,
        org_webhooks::{self as sql, OrgWebhookStorage, OrganizationWebhookEvent},
    },
};
#[async_trait]
impl OrgWebhookStorage for SqlxStorage {
    async fn bind_organization_webhook(
        &self,
        event: &OrganizationWebhookEvent,
        at: DateTime<Utc>,
        enabled: bool,
    ) -> Result<Option<OrganizationWebhookWake>> {
        let input = sql::encode(event, at, enabled)?;
        let mut tx = self.pool.begin().await.map_err(to_db_err)?;
        sqlx::query(sql::CLEANUP)
            .bind(&input)
            .execute(&mut *tx)
            .await
            .map_err(to_db_err)?;
        let row = sqlx::query(sql::BIND)
            .bind(&input)
            .fetch_one(&mut *tx)
            .await
            .map_err(to_db_err)?;
        let result = decode::<sql::BindingRow>(&row)?.decode(&event.payload_sha256)?;
        tx.commit().await.map_err(to_db_err)?;
        Ok(result)
    }
}
