//! Authenticated organization webhook routing hints. Only fresh owner reads can
//! establish acceptance; these fields never constitute settlement evidence.
use crate::delivery_control::{DeliveryKind, Identity};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OrganizationWebhookSource {
    OrganizationMemberAdded,
    TeamMembershipAdded,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OrganizationWebhookWake {
    pub identity: Identity,
    pub account_id: u64,
    pub installation_id: u64,
    pub requester_id: u64,
    pub team_id: Option<u64>,
    pub source: OrganizationWebhookSource,
    pub payload_sha256: [u8; 32],
    /// First authenticated ingress time, retained unchanged on replay. GitHub
    /// does not supply a universal occurrence timestamp for these events.
    pub received_at: DateTime<Utc>,
}
impl OrganizationWebhookWake {
    pub fn validate(&self) -> Result<(), &'static str> {
        let portable = |n| n > 0 && n <= crate::storage::integer::MAX_EXACT;
        if self.identity.kind != DeliveryKind::Organization
            || self.identity.target_id != self.account_id
            || !portable(self.identity.delivery_generation)
            || !portable(self.account_id)
            || !portable(self.installation_id)
            || !portable(self.requester_id)
            || self.team_id.is_some_and(|id| !portable(id))
            || (self.source == OrganizationWebhookSource::TeamMembershipAdded)
                != self.team_id.is_some()
        {
            return Err("invalid organization webhook identity");
        }
        Ok(())
    }
}
