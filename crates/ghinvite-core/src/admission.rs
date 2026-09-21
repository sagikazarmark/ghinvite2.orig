//! Browser admission commands. Identities are trusted caller assertions.
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
    pub link_id: InvitationLinkId,
    pub operation_id: AdmissionOperationId,
    pub requester_id: u64,
    pub justification: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("justification is too long")]
pub struct JustificationTooLong;

impl Admit {
    /// Trim the justification (blank means none) and bound it to
    /// [`MAX_JUSTIFICATION_BYTES`].
    pub fn normalize(&mut self) -> Result<(), JustificationTooLong> {
        self.justification = self
            .justification
            .take()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        if self
            .justification
            .as_ref()
            .is_some_and(|s| s.len() > MAX_JUSTIFICATION_BYTES)
        {
            return Err(JustificationTooLong);
        }
        Ok(())
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
impl From<crate::Inactive> for Rejection {
    fn from(inactive: crate::Inactive) -> Self {
        match inactive {
            crate::Inactive::Revoked => Self::Revoked,
            crate::Inactive::Expired => Self::Expired,
            crate::Inactive::Exhausted => Self::Exhausted,
        }
    }
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
