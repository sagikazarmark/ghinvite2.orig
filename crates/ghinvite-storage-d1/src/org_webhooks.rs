use super::{D1Storage, text, wasm_send};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ghinvite_core::{
    org_webhook::OrganizationWebhookWake,
    storage::{
        Error, Result,
        org_webhooks::{self as sql, OrgWebhookStorage, OrganizationWebhookEvent},
    },
};
#[async_trait]
impl OrgWebhookStorage for D1Storage {
    async fn bind_organization_webhook(
        &self,
        event: &OrganizationWebhookEvent,
        at: DateTime<Utc>,
        enabled: bool,
    ) -> Result<Option<OrganizationWebhookWake>> {
        wasm_send(async {
            let input = sql::encode(event, at, enabled)?;
            self.run(sql::CLEANUP, &[text(&input)]).await?;
            self.first::<sql::BindingRow>(sql::BIND, &[text(&input)])
                .await?
                .ok_or_else(|| Error::Database("organization webhook binding missing".into()))?
                .decode(&event.payload_sha256)
        })
        .await
    }
}
