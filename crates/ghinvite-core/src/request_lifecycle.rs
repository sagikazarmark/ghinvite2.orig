//! Commands and retained records for the private request authority, and the
//! pure request rules it shares with the web tests' fake authority.
use crate::audit::EventType;
use crate::invitation_link::AccountAdmin;
use crate::{InvitationLinkId, RequestId, RequestState};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How long an admitted request awaits review: its decision deadline.
pub const PENDING_LIFETIME: chrono::Duration = chrono::Duration::days(7);

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
    Decline {
        reason: Option<String>,
        /// Explicitly shared with this requester; historical reason stays private.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        requester_message: Option<String>,
    },
}

/// Shared decision-input normalization; absent/blank is never a publication opt-in.
pub fn normalize_requester_message(value: Option<String>) -> Result<Option<String>, &'static str> {
    let Some(value) = value else { return Ok(None) };
    let value = value.replace("\r\n", "\n").trim().to_owned();
    if value.chars().count() > 500
        || value
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err("Requester message must be at most 500 characters without control characters.");
    }
    Ok((!value.is_empty()).then_some(value))
}

impl DecisionAction {
    /// Normalize a decline: a blank reason means none, and the requester
    /// message follows [`normalize_requester_message`].
    pub fn normalize(&mut self) -> Result<(), &'static str> {
        if let Self::Decline {
            reason,
            requester_message,
        } = self
        {
            *requester_message = normalize_requester_message(requester_message.take())?;
            *reason = reason
                .take()
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty());
            if reason.as_ref().is_some_and(|s| s.len() > 16_384) {
                return Err("invalid reason");
            }
        }
        Ok(())
    }
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
    /// Explicit requester-visible text from a new decision; never inferred from reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requester_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellation_reason: Option<crate::RequestCancellationReason>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecisionOutcome {
    Applied,
    AlreadyCompleted,
    Incompatible,
    NotPending,
    NotAuthorized,
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

/// Current web-verified account authority, independent of projected request rows.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AdminRequestStatus {
    pub request_id: RequestId,
    pub admin: AccountAdmin,
}

/// Authenticated requester lookup, independent of a browser's currently selected
/// link capability and of asynchronously projected SQL records.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequesterStatus {
    pub request_id: RequestId,
    pub requester_id: u64,
}

/// Current request-owned manifest progress. Submission never establishes access.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct RequesterView {
    pub request: RequestSnapshot,
    pub targets: crate::targets::Targets,
    pub repositories: Vec<crate::delivery::RepositoryProgress>,
    pub organization: Option<OrganizationProgress>,
}

impl RequesterView {
    /// Request-ID navigation grants no link capability, so beyond the
    /// requester's view of the request it also leaves out the domain evidence.
    pub fn new(
        request: RequestSnapshot,
        targets: crate::targets::Targets,
        repositories: Vec<crate::delivery::RepositoryProgress>,
        organization: Option<OrganizationProgress>,
    ) -> Self {
        let mut request = request.requester_view();
        request.domain_evidence = None;
        Self {
            request,
            targets,
            repositories,
            organization,
        }
    }
}

/// A request's revision would leave the portable integer range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("request revision exhausted")]
pub struct RevisionExhausted;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RequestSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain_evidence: Option<crate::email_domain::DomainEvidence>,
    pub request_id: RequestId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_id: Option<InvitationLinkId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch: Option<crate::bulk::BatchRef>,
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

impl RequestSnapshot {
    pub fn as_request(&self) -> crate::InvitationRequest {
        crate::InvitationRequest {
            id: self.request_id,
            invitation_link_id: self.link_id,
            batch: self.batch,
            requester_id: self.requester_id,
            justification: self.justification.clone(),
            domain_evidence: self.domain_evidence.clone(),
            state: self.state,
            created_at: self.admitted_at,
            decision_deadline: self.decision_deadline,
            decided_by: self.decision.as_ref().and_then(|d| d.decided_by),
            decided_at: self.decision.as_ref().map(|d| d.effective_at),
            cancellation_reason: self.decision.as_ref().and_then(|d| d.cancellation_reason),
            requester_message: self
                .decision
                .as_ref()
                .and_then(|d| d.requester_message.clone()),
            decline_reason: self
                .decision
                .as_ref()
                .and_then(|d| d.decline_reason.clone()),
        }
    }

    /// One deadline transition, shared by inspection, timers and decision
    /// arbitration: a pending request past its decision deadline expires as of
    /// that deadline. Answers whether the request changed.
    pub fn expire_due(&mut self, now: DateTime<Utc>) -> bool {
        let deadline = self.decision_deadline.filter(|d| now >= *d);
        let (RequestState::Pending, Some(deadline)) = (self.state, deadline) else {
            return false;
        };
        self.state = RequestState::Expired;
        self.revision += 1;
        self.decision = Some(TerminalDecision {
            decision_id: format!("request.expired/{}", self.request_id),
            decided_by: None,
            effective_at: deadline,
            evaluated_at: now,
            cancellation_reason: None,
            decline_reason: None,
            requester_message: None,
        });
        true
    }

    /// An admin's decision, by `decided_by`. An overdue pending request
    /// expires first; otherwise the decision applies to a pending request, and
    /// a request that already left pending is unchanged. The outcome compares
    /// the result with `action`, so the decision that settled a request
    /// replays as already completed and any other one is incompatible.
    pub fn decide(
        &mut self,
        decided_by: u64,
        action: &DecisionAction,
        now: DateTime<Utc>,
    ) -> DecisionOutcome {
        let was_pending = self.state == RequestState::Pending;
        if was_pending && !self.expire_due(now) {
            let (state, kind, decline_reason, requester_message) = match action {
                DecisionAction::Approve => (
                    RequestState::Approved,
                    EventType::RequestApproved,
                    None,
                    None,
                ),
                DecisionAction::Decline {
                    reason,
                    requester_message,
                } => (
                    RequestState::Declined,
                    EventType::RequestDeclined,
                    reason.clone(),
                    requester_message.clone(),
                ),
            };
            self.state = state;
            self.revision += 1;
            self.decision = Some(TerminalDecision {
                decision_id: format!("{kind}/{}", self.request_id),
                decided_by: Some(decided_by),
                effective_at: now,
                evaluated_at: now,
                cancellation_reason: None,
                decline_reason,
                requester_message,
            });
        }
        let matching = match action {
            DecisionAction::Approve => self.state == RequestState::Approved,
            DecisionAction::Decline {
                reason,
                requester_message,
            } => {
                self.state == RequestState::Declined
                    && self.decision.as_ref().is_some_and(|d| {
                        &d.decline_reason == reason && &d.requester_message == requester_message
                    })
            }
        };
        if self.state == RequestState::Cancelled {
            DecisionOutcome::NotPending
        } else if !matching {
            DecisionOutcome::Incompatible
        } else if was_pending {
            DecisionOutcome::Applied
        } else {
            DecisionOutcome::AlreadyCompleted
        }
    }

    /// A revoked link's cascade: a local cancellation of a pending request,
    /// never an admin's decline. An overdue one expires instead, and a request
    /// that already left pending is unchanged. Answers whether it changed.
    pub fn cancel_for_revoked_link(
        &mut self,
        now: DateTime<Utc>,
    ) -> Result<bool, RevisionExhausted> {
        if self.state != RequestState::Pending {
            return Ok(false);
        }
        if self.expire_due(now) {
            return Ok(true);
        }
        self.revision = self
            .revision
            .checked_add(1)
            .filter(|n| *n <= crate::storage::integer::MAX_EXACT)
            .ok_or(RevisionExhausted)?;
        self.state = RequestState::Cancelled;
        self.decision = Some(TerminalDecision {
            decision_id: format!("request.cancelled/{}", self.request_id),
            decided_by: None,
            effective_at: now,
            evaluated_at: now,
            decline_reason: None,
            requester_message: None,
            cancellation_reason: Some(crate::RequestCancellationReason::LinkRevoked),
        });
        Ok(true)
    }

    /// What the requester sees of their request: never the decline reason.
    pub fn requester_view(mut self) -> Self {
        if let Some(decision) = &mut self.decision {
            decision.decline_reason = None;
        }
        self
    }
}

/// Immutable admission-time policy and repository scope, never a mutable snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InitializeRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_lifetime_seconds: Option<u32>,
    #[serde(
        default = "crate::invitation_link::default_budget_share_pct",
        skip_serializing_if = "crate::invitation_link::is_default_budget_share_pct"
    )]
    pub budget_share_pct: u8,
    #[serde(default = "crate::invitation_link::default_invitation_lifetime_seconds")]
    pub invitation_lifetime_seconds: u32,
    pub request: RequestSnapshot,
    pub installation_id: u64,
    /// The additional org-app installation for mixed repo/org targets. For
    /// org-only targets the primary installation is already the org app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_installation_id: Option<u64>,
    pub targets: crate::targets::Targets,
    pub approval_required: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApprovedDispatch {
    pub dispatch_id: String,
    pub input: InitializeRequest,
    pub commands: Vec<crate::delivery::CreateCommand>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_command: Option<crate::org_delivery::OrganizationCreateCommand>,
}

impl InitializeRequest {
    pub fn org_installation_id(&self) -> Option<u64> {
        crate::invitation_link::target_org_installation_id(
            &self.targets,
            self.installation_id,
            self.organization_installation_id,
        )
    }

    pub fn validate_installations(&self) -> Result<(), crate::invitation_link::InvalidLinkValues> {
        crate::invitation_link::validate_target_installations(
            &self.targets,
            self.request.account_id,
            self.installation_id,
            self.organization_installation_id,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SubmittedCommand {
    pub command: crate::delivery::CreateCommand,
    pub invocation_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SubmittedOrganizationCommand {
    pub command: crate::org_delivery::OrganizationCreateCommand,
    pub invocation_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct DeliveryStatus {
    pub plan: ApprovedDispatch,
    pub submitted: Vec<SubmittedCommand>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submitted_organization: Option<SubmittedOrganizationCommand>,
}

/// Dispatch progress for the organization command and its immutable team set.
/// Submission is not proof that GitHub has granted any access.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OrganizationProgress {
    pub org_id: u64,
    pub delivery_generation: u64,
    pub team_ids: Vec<u64>,
    pub stage: crate::delivery::DispatchStage,
}

#[cfg(test)]
#[path = "request_rules_tests.rs"]
mod request_rules_tests;
#[cfg(test)]
#[path = "requester_message_tests.rs"]
mod requester_message_tests;
