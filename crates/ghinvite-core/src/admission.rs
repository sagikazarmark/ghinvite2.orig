//! Versioned browser admission commands. Identities are trusted caller assertions.
use crate::{InvitationLinkId, InvitationLinkRepo, Permission, RequestId, RequestState};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const MAX_JUSTIFICATION_BYTES: usize = 16_384;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub struct AdmissionOperationId(RequestId);
impl TryFrom<String> for AdmissionOperationId {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        crate::request_lifecycle::parse_operation_id(&value).map(Self)
    }
}
impl From<AdmissionOperationId> for String {
    fn from(id: AdmissionOperationId) -> Self {
        id.0.to_string()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Admit {
    pub version: u32,
    pub link_id: InvitationLinkId,
    pub operation_id: AdmissionOperationId,
    pub requester_id: u64,
    pub justification: Option<String>,
}
impl Admit {
    pub fn normalize(&mut self) {
        self.justification = self
            .justification
            .take()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AdmissionReceipt {
    pub decided_at: DateTime<Utc>,
    pub result: AdmissionResult,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AdmissionResult {
    Accepted {
        request_id: RequestId,
        state: RequestState,
        decision_deadline: Option<DateTime<Utc>>,
    },
    Rejected {
        reason: Rejection,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Rejection {
    Revoked,
    Expired,
    Exhausted,
    ExistingRequest,
    InstallationUnavailable,
    RepositoryUnavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AdminLinkCommand {
    pub link_id: InvitationLinkId,
    pub admin: crate::storage::projection::AccountAdmin,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AttemptQuery {
    pub link_id: InvitationLinkId,
    pub requester_id: u64,
    pub operation_id: Option<AdmissionOperationId>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Attempt {
    pub input: Admit,
    pub receipt: Option<AdmissionReceipt>,
}
/// Deliberately excludes all admin metadata and other requesters' facts.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct RequesterPage {
    pub link_id: InvitationLinkId,
    pub invitation_code: String,
    pub repos: Vec<InvitationLinkRepo>,
    pub permission: Permission,
    pub approval_required: bool,
    /// Advisory only: current link guardrails and requester suppression permit
    /// a fresh attempt.
    pub can_start_fresh: bool,
    pub attempt: Option<Attempt>,
    pub request: Option<crate::storage::projection::RequestSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateMetadata {
    pub link_id: InvitationLinkId,
    pub admin: crate::storage::projection::AccountAdmin,
    pub description: String,
    pub internal_note: Option<String>,
}
