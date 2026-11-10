//! Private batch ingress: the web caller verifies current authority over all
//! retained targets. Commands bind that actor without changing the creator.
use crate::{
    bulk::{BatchRef, BulkBatchId, Review, RowRequest, Submission},
    invitation_link::AccountAdmin,
    request_lifecycle::{
        InitializeRequest, LifecycleOperationId, RequestSnapshot, TerminalDecision,
    },
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BatchQuery {
    pub batch_id: BulkBatchId,
    pub admin: AccountAdmin,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateBatch {
    pub operation_id: LifecycleOperationId,
    pub review: Review,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditBatch {
    pub query: BatchQuery,
    pub operation_id: LifecycleOperationId,
    pub expected_revision: u64,
    pub remove: BTreeSet<u16>,
    pub personal_link_rows: BTreeSet<u16>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubmitBatch {
    pub query: BatchQuery,
    pub operation_id: LifecycleOperationId,
    pub expected_revision: u64,
    pub submission: Submission,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CancelBatch {
    pub query: BatchQuery,
    pub operation_id: LifecycleOperationId,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CancelBatchReceipt {
    pub batch_id: BulkBatchId,
    /// Every addressed owner acknowledged its stop barrier. Uncertainty is
    /// counted separately and does not claim that an upstream effect was undone.
    pub stopped: bool,
    pub aborted: u32,
    pub already_written: u32,
    pub already_completed: u32,
    pub uncertain: u32,
}

/// Allocate IDs in the web continuation once; this derivation uses only the
/// retained approved row. It never borrows link admission or consumes a use.
pub fn initialization(
    review: &Review,
    row: &RowRequest,
) -> Result<InitializeRequest, &'static str> {
    review.validate().map_err(|_| "invalid batch review")?;
    let submission = review.submitted.as_ref().ok_or("batch not submitted")?;
    if !submission.requests.contains(row) {
        return Err("request is outside the batch submission");
    }
    let at = submission.submitted_at;
    Ok(InitializeRequest {
        access_lifetime_seconds: review.manifest.access_lifetime_seconds,
        budget_share_pct: crate::invitation_link::default_budget_share_pct(),
        invitation_lifetime_seconds: crate::invitation_link::default_invitation_lifetime_seconds(),
        request: RequestSnapshot {
            domain_evidence: None,
            request_id: row.request_id,
            link_id: None,
            batch: Some(row.source),
            account_id: review.manifest.admin.account_id,
            requester_id: row.requester_id,
            justification: None,
            state: crate::RequestState::Approved,
            admitted_at: at,
            decision_deadline: None,
            revision: 1,
            decision: Some(TerminalDecision {
                decision_id: format!("request.approved/{}", row.request_id),
                decided_by: Some(submission.submitted_by),
                effective_at: at,
                evaluated_at: at,
                decline_reason: None,
                requester_message: None,
                cancellation_reason: None,
            }),
        },
        installation_id: review.manifest.installation_id,
        organization_installation_id: review.manifest.organization_installation_id,
        targets: review.manifest.targets.clone(),
        approval_required: false,
    })
}

/// Missing observations are explicit and never interpreted as a failed or
/// unwritten delivery. A later refresh reads the owner again.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", content = "snapshot", rename_all = "snake_case")]
pub enum Observation<T> {
    Pending,
    Unavailable,
    Observed(T),
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RepositoryProgress {
    pub repo_id: u64,
    pub delivery_generation: u64,
    pub observation: Observation<crate::delivery::DeliverySnapshot>,
}
/// Position in the immutable submitted row/target order. Generation is always
/// read afresh from the request owner, never inferred from this cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProgressCursor {
    pub review_revision: u64,
    pub row: u16,
    pub target_offset: u16,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProgressPageQuery {
    pub query: BatchQuery,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ProgressCursor>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RequestProgress {
    pub source: BatchRef,
    pub request_id: crate::RequestId,
    pub initialized: Observation<()>,
    /// A partial row continues at this target index on the next page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_target_offset: Option<u16>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub final_results: Vec<crate::request_completion::TargetResult>,
    pub repositories: Vec<RepositoryProgress>,
    pub organization: Option<Observation<crate::org_delivery::OrganizationDeliverySnapshot>>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EmailProgress {
    pub source: BatchRef,
    pub observation: Observation<crate::email_delivery::EmailDeliverySnapshot>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BatchProgress {
    pub review: Review,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<ProgressCursor>,
    /// Durable handoff is separate from observing the downstream effect.
    pub handoff_pending: bool,
    pub requests: Vec<RequestProgress>,
    pub emails: Vec<EmailProgress>,
    pub cancellation_pending: bool,
    pub cancellation: Option<CancelBatchReceipt>,
}

/// Identity-only wake: the batch reads the current approved generation and its
/// delivery owner. Payloads cannot assert a result or resurrect an old epoch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ItemProgressNotice {
    Request {
        source: BatchRef,
        request_id: crate::RequestId,
        delivery: Option<crate::request_completion::DeliveryNotice>,
    },
    Email {
        source: BatchRef,
    },
}
impl ItemProgressNotice {
    pub fn source(&self) -> BatchRef {
        match self {
            Self::Request { source, .. } | Self::Email { source } => *source,
        }
    }
}
