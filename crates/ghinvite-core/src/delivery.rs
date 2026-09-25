//! Immutable repository create identities and retained delivery facts.
use crate::{GithubInvitationId, InvitationLinkId, Permission, RequestId};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateCommand {
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

impl CreateCommand {
    pub fn delivery_key(&self) -> String {
        delivery_key(self.request_id, self.repo_id)
    }
}

pub fn delivery_key(request_id: RequestId, repo_id: u64) -> String {
    format!("{request_id}:{repo_id}")
}

/// The delivery owner retains create history independently of later settlement.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DeliverySnapshot {
    pub create: CreateReceipt,
    pub revision: u64,
    pub settlement: Option<Settlement>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Settlement {
    pub state: crate::InvitationState,
    #[schemars(with = "serde_json::Value")]
    pub event: crate::audit::AuditEvent,
}

impl From<CreateReceipt> for DeliverySnapshot {
    fn from(create: CreateReceipt) -> Self {
        Self {
            revision: create.revision,
            create,
            settlement: None,
        }
    }
}

impl DeliverySnapshot {
    pub fn invitation(&self) -> crate::GithubInvitation {
        use crate::InvitationState;
        let (state, upstream_id, error) = match &self.create.outcome {
            CreateOutcome::Created { upstream_id } => {
                (InvitationState::Sent, Some(*upstream_id), None)
            }
            CreateOutcome::AlreadyCollaborator => (InvitationState::Accepted, None, None),
            CreateOutcome::Failed { status } => (
                InvitationState::Failed,
                None,
                Some(format!("GitHub returned {status}")),
            ),
            _ => (InvitationState::Sending, None, None),
        };
        crate::GithubInvitation {
            id: self.create.command.invitation_id,
            invitation_request_id: self.create.command.request_id,
            repo_id: self.create.command.repo_id,
            github_invitation_id: upstream_id,
            state: self.settlement.as_ref().map_or(state, |s| s.state),
            error_message: error,
            created_at: self.create.command.approved_at,
            updated_at: self
                .settlement
                .as_ref()
                .map(|s| s.event.occurred_at)
                .or(self.create.confirmed_at)
                .unwrap_or(self.create.command.approved_at),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CreateOutcome {
    Blocked {
        reason: String,
    },
    /// GitHub positively refused an operation due to its rate limit.
    Throttled,
    OutcomeUnknown,
    Created {
        upstream_id: u64,
    },
    AlreadyCollaborator,
    Failed {
        status: u16,
    },
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
