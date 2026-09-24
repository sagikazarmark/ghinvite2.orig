//! Commands and retained records for the private request authority.
use crate::storage::projection::AccountAdmin;
use crate::{InvitationLinkId, InvitationLinkRepo, Permission, RequestId, RequestState};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Request-scoped identity, allocated before submission and retained on uncertainty.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub struct LifecycleOperationId(RequestId);

/// Shared canonical parser for command identities; wrappers keep namespaces typed.
pub fn parse_operation_id(value: &str) -> Result<RequestId, &'static str> {
    let id: RequestId = value.parse().map_err(|_| "invalid operation ID")?;
    if value.len() != 26 || value.to_ascii_uppercase() != id.to_string() {
        return Err("invalid operation ID");
    }
    Ok(id)
}

impl TryFrom<String> for LifecycleOperationId {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        parse_operation_id(&value).map(Self)
    }
}
impl From<LifecycleOperationId> for String {
    fn from(id: LifecycleOperationId) -> Self {
        id.0.to_string()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DecisionAction {
    Approve,
    Decline { reason: Option<String> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecideRequest {
    pub link_id: InvitationLinkId,
    pub request_id: RequestId,
    pub operation_id: LifecycleOperationId,
    pub admin: AccountAdmin,
    pub action: DecisionAction,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalDecision {
    pub decision_id: String,
    pub decided_by: Option<u64>,
    pub effective_at: DateTime<Utc>,
    pub evaluated_at: DateTime<Utc>,
    /// Admin-only; requester status removes this field's value.
    pub decline_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecisionOutcome {
    Applied,
    AlreadyCompleted,
    Incompatible,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DecisionReceipt {
    pub outcome: DecisionOutcome,
    pub request: RequestSnapshot,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestStatus {
    pub link_id: InvitationLinkId,
    pub request_id: RequestId,
    pub requester_id: u64,
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
    pub decision: Option<TerminalDecision>,
}

/// Immutable admission-time policy and repository scope, never a mutable snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InitializeRequest {
    pub request: RequestSnapshot,
    pub installation_id: u64,
    pub repos: Vec<InvitationLinkRepo>,
    pub permission: Permission,
    pub approval_required: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApprovedDispatch {
    pub dispatch_id: String,
    pub input: InitializeRequest,
    pub commands: Vec<crate::delivery::CreateCommand>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SubmittedCommand {
    pub command: crate::delivery::CreateCommand,
    pub invocation_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct DeliveryStatus {
    pub plan: ApprovedDispatch,
    pub submitted: Vec<SubmittedCommand>,
}
