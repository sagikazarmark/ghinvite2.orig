//! Operation-bound link revocation and frozen cascade identities.
use crate::{
    InvitationLinkId, RequestId, delivery_control::Identity, invitation_link::AccountAdmin,
    link_lifecycle::LifecycleOperationId,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RevokeCascade {
    pub decline_pending_requests: bool,
    pub cancel_pending_invitations: bool,
    pub remove_granted_access: bool,
}
impl RevokeCascade {
    pub fn any(self) -> bool {
        self.decline_pending_requests
            || self.cancel_pending_invitations
            || self.remove_granted_access
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RevokeCommand {
    pub link_id: InvitationLinkId,
    pub admin: AccountAdmin,
    pub operation_id: LifecycleOperationId,
    pub cascade: RevokeCascade,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RevokeAccepted {
    #[serde(default, skip_serializing_if = "zero")]
    pub addressed_access_grants: u32,
    pub addressed_requests: u32,
    pub addressed_deliveries: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RevokeRefusal {
    Revoked,
    UnsupportedCascade,
}
pub type RevokeOutcome = Result<RevokeAccepted, RevokeRefusal>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeliveryCascadeQuery {
    pub identity: Identity,
    pub required_control_revision: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CascadeEntryState {
    Pending,
    WaitingOnGithub,
    Done,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeliveryCascadeProgress {
    pub observed_control_revision: u64,
    pub state: CascadeEntryState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestCascadeQuery {
    pub link_id: InvitationLinkId,
    pub request_id: RequestId,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestCascadeManifest {
    pub request_id: RequestId,
    pub state: crate::RequestState,
    pub observed_control_revision: u64,
    pub deliveries: Vec<Identity>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CascadeProgress {
    #[serde(default, skip_serializing_if = "zero")]
    pub addressed_access_grants: u32,
    #[serde(default, skip_serializing_if = "zero")]
    pub done_access_grants: u32,
    pub addressed_requests: u32,
    pub addressed_deliveries: u32,
    pub done_requests: u32,
    pub done_deliveries: u32,
    pub waiting_on_github: u32,
    /// Counts identities whose authority could not be read. Never counts as done.
    pub unavailable: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RevocationProjection {
    pub command: RevokeCommand,
    pub accepted: RevokeAccepted,
    pub revision: u64,
    pub at: chrono::DateTime<chrono::Utc>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RevokePreview {
    pub pending_requests: u64,
    pub pending_invitations: u64,
}

fn zero(value: &u32) -> bool {
    *value == 0
}

/// An authenticated operator's immutable capture request. Confirmation must use
/// this same operation identity; no caller-supplied grant list is accepted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PrepareAccessRemoval {
    pub link_id: InvitationLinkId,
    pub admin: AccountAdmin,
    pub operation_id: LifecycleOperationId,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AccessRemovalPreview {
    pub captured_at: chrono::DateTime<chrono::Utc>,
    pub grants: Vec<crate::access_grant::AccessGrantCommand>,
}
