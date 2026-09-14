//! Versioned, bounded admission after-images. Parents are verified identities,
//! not profile snapshots: their owners must restore missing installations/users.
use crate::audit::EventType;
use crate::{InvitationLinkId, InvitationLinkRepo, Permission, RequestId, RequestState};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AccountAdmin {
    pub account_id: u64,
    pub user_id: u64,
}

/// Allocate the link ID before first submission; reuse it on retry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateLink {
    pub version: u32,
    pub link_id: InvitationLinkId,
    pub admin: AccountAdmin,
    pub account_id: u64,
    pub installation_id: u64,
    pub description: String,
    pub internal_note: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub max_uses: Option<u32>,
    pub permission: Permission,
    pub approval_required: bool,
    pub repos: Vec<InvitationLinkRepo>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LinkSnapshot {
    pub link_id: InvitationLinkId,
    pub creation: CreateLink,
    pub invitation_code: String,
    pub created_at: DateTime<Utc>,
    pub uses: u64,
    pub revision: u64,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_by: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RequestSnapshot {
    pub request_id: RequestId,
    pub link_id: InvitationLinkId,
    pub account_id: u64,
    pub requester_id: u64,
    pub justification: Option<String>,
    pub state: RequestState,
    pub admitted_at: DateTime<Utc>,
    pub decision_deadline: Option<DateTime<Utc>>,
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<crate::request_lifecycle::TerminalDecision>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectionEnvelope {
    pub version: u32,
    pub transition_id: String,
    pub link: LinkSnapshot,
    pub requests: Vec<RequestSnapshot>,
    pub events: Vec<AuditIntent>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuditIntent {
    pub event_id: String,
    #[schemars(with = "String")]
    pub kind: EventType,
    pub actor_id: Option<u64>,
    pub target_id: String,
    pub effective_at: DateTime<Utc>,
    pub evaluated_at: DateTime<Utc>,
}

/// Atomic application, independent revisions, content-checked immutable events.
/// Errors leave the entire envelope unapplied. Retry dependency/database errors;
/// invariant errors need repair and redrive with the original envelope.
#[async_trait::async_trait]
pub trait ProjectionStorage: Send + Sync + 'static {
    async fn apply_transition(&self, envelope: &ProjectionEnvelope) -> super::Result<()>;

    /// Eventually consistent versioned request read, including its immutable
    /// admission deadline. None means absent or legacy, never admission rejection.
    async fn get_projected_request(&self, id: RequestId) -> super::Result<Option<RequestSnapshot>>;
}

pub mod sql;
