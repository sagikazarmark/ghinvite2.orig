//! Atomic current-generation routing and bounded authenticated body-hash receipts.
use super::{Error, Result};
use crate::{
    account::App,
    org_webhook::{OrganizationWebhookSource, OrganizationWebhookWake},
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const RETENTION_SECONDS: i64 = 7 * 24 * 60 * 60;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrganizationWebhookEvent {
    pub app: App,
    pub account_id: u64,
    pub installation_id: u64,
    pub requester_id: u64,
    pub team_id: Option<u64>,
    pub source: OrganizationWebhookSource,
    pub payload_sha256: [u8; 32],
}
#[async_trait]
pub trait OrgWebhookStorage: Send + Sync + 'static {
    /// Atomically freeze the unique current projected target or no-match. An
    /// identical body keeps that result for seven days even after projections
    /// change. Disabled composition deliberately retains a no-match receipt.
    async fn bind_organization_webhook(
        &self,
        event: &OrganizationWebhookEvent,
        received_at: DateTime<Utc>,
        enabled: bool,
    ) -> Result<Option<OrganizationWebhookWake>>;
}
pub fn encode(
    event: &OrganizationWebhookEvent,
    at: DateTime<Utc>,
    enabled: bool,
) -> Result<String> {
    super::integer::validate(event)?;
    let valid = |n| n > 0 && n <= super::integer::MAX_EXACT;
    if event.app != App::Org
        || !valid(event.account_id)
        || !valid(event.installation_id)
        || !valid(event.requester_id)
        || event.team_id.is_some_and(|id| !valid(id))
        || (event.source == OrganizationWebhookSource::TeamMembershipAdded)
            != event.team_id.is_some()
    {
        return Err(Error::Corrupt("invalid organization webhook scope".into()));
    }
    let now = at.timestamp();
    let expires = now
        .checked_add(RETENTION_SECONDS)
        .ok_or_else(|| Error::Database("webhook receipt time exceeds range".into()))?;
    super::integer::signed(now)?;
    super::integer::signed(expires)?;
    Ok(serde_json::json!({"event":event,"received_at":at,"now":now,"expires":expires,"enabled":enabled,
        "hash":event.payload_sha256.iter().map(|b| format!("{b:02x}")).collect::<String>()}).to_string())
}
pub const CLEANUP: &str = "DELETE FROM organization_webhook_receipts WHERE payload_sha256 IN (SELECT payload_sha256 FROM organization_webhook_receipts WHERE expires_at<=json_extract(?1,'$.now') AND payload_sha256!=json_extract(?1,'$.hash') ORDER BY expires_at,payload_sha256 LIMIT 128)";
pub const BIND: &str = r#"WITH candidates AS (
 SELECT d.id,d.request_id,d.target_id,d.delivery_generation,r.invitation_link_id AS link_id
 FROM deliveries d
 JOIN delivery_outcomes o ON o.delivery_id=d.id AND o.request_id=d.request_id
 JOIN invitation_requests r ON r.id=d.request_id
 JOIN invitation_links l ON l.id=r.invitation_link_id
 JOIN installations i ON i.app='org' AND i.installation_id=json_extract(?1,'$.event.installation_id') AND i.account_id=l.account_id AND i.uninstalled_at IS NULL AND i.suspended_at IS NULL
 WHERE json_extract(?1,'$.enabled')=1 AND d.kind='org'
 AND l.account_id=json_extract(?1,'$.event.account_id') AND d.target_id=l.account_id
 AND r.requester_id=json_extract(?1,'$.event.requester_id')
 AND json_extract(o.receipt,'$.create.command.installation_id')=i.installation_id
 AND d.delivery_generation=(SELECT MAX(latest.delivery_generation) FROM deliveries latest WHERE latest.kind='org' AND latest.request_id=d.request_id AND latest.target_id=d.target_id)
 AND (
   (json_extract(?1,'$.event.team_id') IS NULL AND d.create_outcome IN ('created','already_pending') AND d.upstream_invitation_id IS NOT NULL AND d.settlement IS NULL AND d.tracking_stopped_reason IS NULL)
   OR (json_extract(?1,'$.event.team_id') IS NOT NULL AND EXISTS(
      SELECT 1 FROM delivery_steps s WHERE s.delivery_id=d.id AND s.step='team:'||CAST(json_extract(?1,'$.event.team_id') AS INTEGER)
      AND s.outcome='created' AND json_extract(s.receipt,'$.settlement') IS NULL AND json_extract(s.receipt,'$.tracking_stopped') IS NULL))
 ) LIMIT 2
), binding AS (
 SELECT CASE WHEN (SELECT count(*) FROM candidates)=1 THEN (
 SELECT json_object('identity',json_object('link_id',link_id,'request_id',request_id,'kind','organization','target_id',target_id,'delivery_generation',delivery_generation),
 'account_id',json_extract(?1,'$.event.account_id'),'installation_id',json_extract(?1,'$.event.installation_id'),
 'requester_id',json_extract(?1,'$.event.requester_id'),'team_id',json_extract(?1,'$.event.team_id'),
 'source',json_extract(?1,'$.event.source'),'payload_sha256',json_extract(?1,'$.event.payload_sha256'),'received_at',json_extract(?1,'$.received_at')) FROM candidates
 ) ELSE NULL END AS receipt
)
INSERT INTO organization_webhook_receipts(app,payload_sha256,received_at,expires_at,receipt)
SELECT 'org',json_extract(?1,'$.hash'),json_extract(?1,'$.now'),json_extract(?1,'$.expires'),receipt FROM binding WHERE true
ON CONFLICT(app,payload_sha256) DO UPDATE SET
 receipt=CASE WHEN expires_at<=json_extract(?1,'$.now') THEN excluded.receipt ELSE receipt END,
 received_at=CASE WHEN expires_at<=json_extract(?1,'$.now') THEN excluded.received_at ELSE received_at END,
 expires_at=CASE WHEN expires_at<=json_extract(?1,'$.now') THEN excluded.expires_at ELSE expires_at END
RETURNING receipt"#;
#[derive(Deserialize)]
pub struct BindingRow {
    pub receipt: Option<String>,
}
impl BindingRow {
    pub fn decode(self, hash: &[u8; 32]) -> Result<Option<OrganizationWebhookWake>> {
        self.receipt
            .map(|value| {
                let wake: OrganizationWebhookWake = serde_json::from_str(&value)
                    .map_err(|_| Error::Corrupt("invalid organization webhook binding".into()))?;
                wake.validate().map_err(|e| Error::Corrupt(e.into()))?;
                if &wake.payload_sha256 != hash {
                    return Err(Error::Corrupt("organization webhook hash mismatch".into()));
                }
                Ok(wake)
            })
            .transpose()
    }
}
