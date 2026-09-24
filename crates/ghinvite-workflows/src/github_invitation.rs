//! Private settlement commands handled by the repository-delivery owner.

use chrono::{DateTime, Utc};
use ghinvite_core::GithubInvitationId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct OnWebhookInput {
    pub invitation_id: GithubInvitationId,
    pub action: WebhookAction,
    pub at: DateTime<Utc>,
    #[serde(default)]
    pub verify: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WebhookAction {
    Accepted,
    Declined,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct CancelInvitationInput {
    pub invitation_id: GithubInvitationId,
    pub installation_id: u64,
    pub by_user: Option<u64>, // None when system-cancelled (cascade in v2)
    pub at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct TickExpireInput {
    pub invitation_id: GithubInvitationId,
    pub installation_id: u64,
    pub at: DateTime<Utc>,
}
