//! Immutable repository create identities and retained delivery facts.
use crate::{GithubInvitationId, InvitationLinkId, Permission, RequestId};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateCommand {
    pub version: u32,
    pub invitation_id: GithubInvitationId,
    pub link_id: InvitationLinkId,
    pub request_id: RequestId,
    pub approval_id: String,
    pub account_id: u64,
    pub installation_id: u64,
    pub requester_id: u64,
    pub repo_id: u64,
    pub repo_full_name: String,
    pub permission: Permission,
    pub approved_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CreateOutcome {
    Blocked { reason: String },
    OutcomeUnknown,
    Created { upstream_id: u64 },
    AlreadyCollaborator,
    Failed { status: u16 },
}

impl CreateOutcome {
    pub fn confirmed(&self) -> bool {
        matches!(
            self,
            Self::Created { .. } | Self::AlreadyCollaborator | Self::Failed { .. }
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CreateReceipt {
    pub command: CreateCommand,
    pub outcome: CreateOutcome,
    pub revision: u64,
    /// Time the outcome was confirmed, retained independently of projection and
    /// workflow retention. Present exactly when the outcome is confirmed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmed_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DispatchStage {
    Approved,
    Planned,
    Submitted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RepositoryProgress {
    pub repo_id: u64,
    pub stage: DispatchStage,
}
