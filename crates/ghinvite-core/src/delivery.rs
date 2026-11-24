//! Immutable repository create identities and retained delivery facts.
use crate::{GithubInvitationId, InvitationLinkId, Permission, RequestId};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BlockedReason {
    InstallationUnavailable,
    InstallationPermissions,
    RequesterIdentity,
    OutsideCollaboratorsRestricted,
    IpAllowList,
    TargetUnavailable,
    MintForbidden,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HoldReason {
    AbuseSuspected,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AbortReason {
    LinkRevoked,
    BatchCancelled,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FailureReason {
    Rejected { status: u16 },
    InvalidTarget,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SettlementReason {
    OwnExpiry,
    LinkRevoked,
    AdminReissue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateCommand {
    #[serde(
        default,
        skip_serializing_if = "crate::reissue::BudgetAttribution::is_source_link"
    )]
    pub budget_attribution: crate::reissue::BudgetAttribution,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_lifetime_seconds: Option<u32>,
    #[serde(
        default = "crate::invitation_link::default_budget_share_pct",
        skip_serializing_if = "crate::invitation_link::is_default_budget_share_pct"
    )]
    pub budget_share_pct: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invitation_id: Option<GithubInvitationId>,
    #[serde(default = "initial_generation")]
    pub delivery_generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_id: Option<InvitationLinkId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch: Option<crate::bulk::BatchRef>,
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
    pub fn budget_link_id(&self) -> Option<crate::InvitationLinkId> {
        match self.budget_attribution {
            crate::reissue::BudgetAttribution::SourceLink => self.link_id,
            crate::reissue::BudgetAttribution::AdministratorReissue => None,
        }
    }
    pub fn delivery_key(&self) -> String {
        delivery_key(self.request_id, self.repo_id, self.delivery_generation)
    }
}

pub const fn initial_generation() -> u64 {
    1
}

pub fn delivery_key(request_id: RequestId, repo_id: u64, delivery_generation: u64) -> String {
    format!("{request_id}:{repo_id}:{delivery_generation}")
}

/// GitHub's fixed lifetime for a pending repository or organization invitation
/// (GitHub documents 7 days; octoemu models it as `invitation-expiry`).
pub const GITHUB_INVITATION_LIFETIME_SECONDS: u32 = 604_800;

/// Whether a GitHub observation taken at `observed_at` still saw an invitation
/// created at `created_at` more than GitHub's 7-day lifetime later. Only the
/// presence that observation itself listed may be judged this way: a retained
/// creation time alone never settles anything (#86).
pub fn past_github_lifetime(created_at: DateTime<Utc>, observed_at: DateTime<Utc>) -> bool {
    created_at
        .checked_add_signed(chrono::Duration::seconds(i64::from(
            GITHUB_INVITATION_LIFETIME_SECONDS,
        )))
        .is_some_and(|expiry| observed_at > expiry)
}

/// Advisory upstream timing, independent of immutable create/settlement receipts.
/// It never authorizes a lifecycle transition or an external effect.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InvitationTiming {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owned_lifetime_seconds: Option<u32>,
    pub upstream_id: u64,
    pub created_at: DateTime<Utc>,
    pub observed_at: DateTime<Utc>,
}
impl InvitationTiming {
    pub fn observed(
        upstream_id: u64,
        created_at: DateTime<Utc>,
        observed_at: DateTime<Utc>,
        approved_at: DateTime<Utc>,
        owned: bool,
    ) -> Option<Self> {
        (upstream_id > 0
            && upstream_id <= crate::storage::integer::MAX_EXACT
            && observed_at >= approved_at
            && created_at <= observed_at
            && (!owned || created_at.timestamp() >= approved_at.timestamp()))
        .then_some(Self {
            upstream_id,
            created_at,
            observed_at,
            owned_lifetime_seconds: None,
        })
    }
    pub fn expires_at(
        &self,
        upstream_id: u64,
        approved_at: DateTime<Utc>,
        owned: bool,
    ) -> Option<DateTime<Utc>> {
        Self::observed(
            self.upstream_id,
            self.created_at,
            self.observed_at,
            approved_at,
            owned,
        )?;
        if self.upstream_id != upstream_id {
            return None;
        }
        let seconds = if owned {
            self.owned_lifetime_seconds?
        } else {
            GITHUB_INVITATION_LIFETIME_SECONDS
        };
        if !(3600..=604800).contains(&seconds) {
            return None;
        }
        self.created_at
            .checked_add_signed(chrono::Duration::seconds(i64::from(seconds)))
    }
}

/// The delivery owner retains create history independently of later settlement.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DeliverySnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invitation_timing: Option<InvitationTiming>,
    pub create: CreateReceipt,
    pub revision: u64,
    pub settlement: Option<Settlement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracking_stopped: Option<SettlementReason>,
}

impl DeliverySnapshot {
    pub fn timing_binding(&self) -> Option<(u64, bool)> {
        match self.create.outcome {
            CreateOutcome::Created { upstream_id } => Some((upstream_id, true)),
            CreateOutcome::AlreadyPending { upstream_id, .. } => Some((upstream_id, false)),
            _ => None,
        }
    }
    pub fn retain_invitation_timing(&mut self, timing: InvitationTiming) -> bool {
        let Some((id, owned)) = self.timing_binding() else {
            return false;
        };
        if timing
            .owned_lifetime_seconds
            .is_some_and(|s| !(3600..=604800).contains(&s))
        {
            return false;
        }
        if self.invitation_timing.is_some()
            || timing.upstream_id != id
            || InvitationTiming::observed(
                id,
                timing.created_at,
                timing.observed_at,
                self.create.command.approved_at,
                owned,
            )
            .is_none()
        {
            return false;
        }
        let Some(revision) = self
            .revision
            .checked_add(1)
            .filter(|v| *v <= crate::storage::integer::MAX_EXACT)
        else {
            return false;
        };
        self.invitation_timing = Some(timing);
        self.revision = revision;
        true
    }
    pub fn invitation_expires_at(&self) -> Option<DateTime<Utc>> {
        let (id, owned) = self.timing_binding()?;
        self.invitation_timing
            .as_ref()?
            .expires_at(id, self.create.command.approved_at, owned)
    }
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
            invitation_timing: None,
            revision: create.revision,
            create,
            settlement: None,
            tracking_stopped: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CreateOutcome {
    Planned,
    Held {
        reason: HoldReason,
    },
    Submitted,
    Blocked {
        reason: BlockedReason,
    },
    Throttled {
        until: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<crate::budget::RetryReason>,
    },
    OutcomeUnknown,
    Created {
        upstream_id: u64,
    },
    AlreadySatisfied {
        effective_role: crate::targets::EffectiveRole,
    },
    AlreadyPending {
        upstream_id: u64,
        permission: crate::targets::RepoRole,
    },
    Upgraded {
        from: crate::targets::EffectiveRole,
        to: crate::targets::RepoRole,
    },
    SkippedCustomRole {
        role_name: crate::targets::CustomRoleName,
    },
    Aborted {
        reason: AbortReason,
    },
    Failed {
        reason: FailureReason,
    },
}
impl CreateOutcome {
    /// A confirmed create result is immutable; pending invitations continue to settlement.
    pub fn confirmed(&self) -> bool {
        matches!(
            self,
            Self::Created { .. }
                | Self::AlreadySatisfied { .. }
                | Self::AlreadyPending { .. }
                | Self::Upgraded { .. }
                | Self::SkippedCustomRole { .. }
                | Self::Aborted { .. }
                | Self::Failed { .. }
        )
    }
    pub fn tracked_foreign(&self) -> bool {
        matches!(self, Self::AlreadyPending { .. })
    }
    pub fn needs_settlement(&self) -> bool {
        matches!(self, Self::Created { .. } | Self::AlreadyPending { .. })
    }
    pub fn final_result(&self) -> Option<FinalResult> {
        match self {
            Self::AlreadySatisfied { .. } | Self::Upgraded { .. } => Some(FinalResult::Satisfied),
            Self::SkippedCustomRole { .. } | Self::Aborted { .. } | Self::Failed { .. } => {
                Some(FinalResult::Unfulfillable)
            }
            _ => None,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FinalResult {
    Satisfied,
    LapsedInvite,
    Declined,
    Unfulfillable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CreateReceipt {
    #[serde(
        default,
        skip_serializing_if = "crate::delivery_audit::DeliveryAuditVersion::is_legacy"
    )]
    pub audit_version: crate::delivery_audit::DeliveryAuditVersion,
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
    #[serde(
        default = "initial_generation",
        skip_serializing_if = "is_initial_generation"
    )]
    pub delivery_generation: u64,
    pub repo_id: u64,
    pub stage: DispatchStage,
}

/// An observation scheduling receipt, never an invitation/access outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RepositoryRefresh {
    Requested,
    Throttled { retry_at: DateTime<Utc> },
    NotTracked,
}

fn is_initial_generation(value: &u64) -> bool {
    *value == initial_generation()
}

#[cfg(test)]
mod timing_tests {
    use super::*;
    #[test]
    fn advisory_timing_requires_bound_source_and_owned_policy_without_rewriting_receipts() {
        let at = DateTime::parse_from_rfc3339("2026-10-17T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut t = InvitationTiming::observed(
            99,
            at,
            at + chrono::Duration::seconds(1),
            at + chrono::Duration::milliseconds(900),
            true,
        )
        .unwrap();
        assert_eq!(t.expires_at(99, at, true), None);
        t.owned_lifetime_seconds = Some(3600);
        assert_eq!(
            t.expires_at(99, at, true),
            Some(at + chrono::Duration::hours(1))
        );
        assert_eq!(t.expires_at(100, at, true), None);
        assert_eq!(
            t.expires_at(99, at, false),
            Some(at + chrono::Duration::days(7))
        );
        assert!(
            InvitationTiming::observed(99, at + chrono::Duration::nanoseconds(1), at, at, true)
                .is_none()
        );
        assert!(
            InvitationTiming::observed(99, at - chrono::Duration::seconds(1), at, at, true)
                .is_none()
        );
        t.owned_lifetime_seconds = Some(0);
        assert_eq!(t.expires_at(99, at, true), None);
    }
}
