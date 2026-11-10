//! Invitation-budget commands and UI projections. Restate owns reservations;
//! SQL snapshots never authorize a GitHub write.
use crate::{
    InvitationLinkId,
    account::{AccountInstallationRef, App},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BudgetKind {
    OrgInvite,
    RepoInvite { repo_id: u64 },
    OtherWrite,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Reserve {
    pub request_key: String,
    pub kind: BudgetKind,
    pub link_id: Option<InvitationLinkId>,
}
#[derive(
    Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(transparent)]
pub struct SlotId(pub u64);
/// Trusted delivery-owner command, issued only for a positively observed PUT204.
/// An unknown write or inferred absence is not sufficient evidence. Binding the
/// complete reserve input prevents refunding a different delivery's reservation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RefundInvitationAfterPut204 {
    pub slot_id: SlotId,
    pub reservation: Reserve,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InvitationRefund {
    Refunded,
    NotApplicable,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RetryReason {
    Bucket,
    LinkShare,
    Closed,
    Spacing,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reservation {
    Granted { slot_id: SlotId },
    RetryAt { at: DateTime<Utc>, why: RetryReason },
}
#[derive(
    Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BucketId {
    Org,
    Repo { repo_id: u64 },
    Content,
}
impl BucketId {
    pub fn key(&self) -> String {
        match self {
            Self::Org => "org".into(),
            Self::Repo { repo_id } => format!("repo:{repo_id}"),
            Self::Content => "content".into(),
        }
    }
    pub fn valid_for(&self, app: App) -> bool {
        match self {
            Self::Org => app == App::Org,
            Self::Repo { repo_id } => {
                app == App::Repo && *repo_id > 0 && *repo_id <= crate::storage::integer::MAX_EXACT
            }
            Self::Content => true,
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecordLimit {
    pub bucket: BucketId,
    pub until: Option<DateTime<Utc>>,
    /// Old closure-only commands remain readable, but supply no classified count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<LimitObservation>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LimitReason {
    RateLimited,
    Spammed,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LimitObservation {
    /// Assigned by the owner before retaining evidence and sending a budget RPC.
    /// Unbound HTTP-run results are not accepted by the budget handler.
    pub id: Option<String>,
    pub reason: LimitReason,
    pub observed_at: DateTime<Utc>,
}
impl RecordLimit {
    pub fn observed(
        bucket: BucketId,
        until: Option<DateTime<Utc>>,
        reason: LimitReason,
        observed_at: DateTime<Utc>,
    ) -> Self {
        Self {
            bucket,
            until,
            observation: Some(LimitObservation {
                id: None,
                reason,
                observed_at,
            }),
        }
    }
}
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LimitCounts {
    pub coverage_started_at: Option<DateTime<Utc>>,
    pub rate_limited: u32,
    pub spammed: u32,
    pub future_observations: u32,
    pub incomplete: bool,
}
impl LimitCounts {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}
/// Internal fixture/legacy policy update; not a web DTO. Authenticated cap
/// changes use SetOrgCap, and immutable link shares have their own command.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Configure {
    pub request_key: String,
    pub confirmed_established: bool,
    pub link_shares: Vec<LinkShare>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BucketSnapshot {
    pub bucket: BucketId,
    pub used_24h: u32,
    pub cap: u32,
    pub closed_until: Option<DateTime<Utc>>,
    pub next_slot_at: Option<DateTime<Utc>>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BudgetSnapshot {
    pub app: App,
    pub account_id: u64,
    pub revision: u64,
    pub observed_at: DateTime<Utc>,
    pub confirmed_established: bool,
    pub buckets: Vec<BucketSnapshot>,
    pub content_used_minute: u32,
    pub content_used_hour: u32,
    /// Explicit upstream closure, separate from window caps and write spacing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_closed_until: Option<DateTime<Utc>>,
    pub next_content_write_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "LimitCounts::is_empty")]
    pub limit_counts: LimitCounts,
}
impl BudgetSnapshot {
    pub fn owner(&self) -> AccountInstallationRef {
        AccountInstallationRef {
            app: self.app,
            account_id: self.account_id,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LinkShare {
    pub link_id: InvitationLinkId,
    pub percent: u8,
}

/// Authenticated account-owner command. It changes only the organization cap.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetOrgCap {
    pub operation_id: crate::admission::AdmissionOperationId,
    pub actor_user_id: u64,
    pub confirmed_established: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OrgCapPolicy {
    pub account_id: u64,
    pub revision: u64,
    pub command: SetOrgCap,
    pub changed_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OrgCapRefusal {
    NotOwner,
    OrgAppRequired,
    BudgetUnavailable,
}
pub type OrgCapOutcome = Result<OrgCapPolicy, OrgCapRefusal>;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct OrgCapView {
    pub enabled: bool,
    pub policy: Option<OrgCapPolicy>,
}
