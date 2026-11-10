//! Organization delivery identities and per-team receipts. Numeric target kinds
//! have separate SQL identities; confirmed create facts never become retry input.
use crate::{
    GithubInvitationId, InvitationLinkId, InvitationState, RequestId,
    delivery::{AbortReason, FailureReason, HoldReason, SettlementReason},
    targets::{OrganizationTarget, TeamTarget},
};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OrganizationCreateCommand {
    #[serde(
        default,
        skip_serializing_if = "crate::reissue::BudgetAttribution::is_source_link"
    )]
    pub budget_attribution: crate::reissue::BudgetAttribution,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_lifetime_seconds: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invitation_id: Option<GithubInvitationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_id: Option<InvitationLinkId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch: Option<crate::bulk::BatchRef>,
    pub request_id: RequestId,
    pub approval_id: String,
    pub account_id: u64,
    pub installation_id: u64,
    pub requester_id: u64,
    pub organization: OrganizationTarget,
    pub teams: Vec<TeamTarget>,
    #[serde(default = "crate::delivery::initial_generation")]
    pub delivery_generation: u64,
    pub approved_at: DateTime<Utc>,
    #[serde(
        default = "crate::invitation_link::default_budget_share_pct",
        skip_serializing_if = "crate::invitation_link::is_default_budget_share_pct"
    )]
    pub budget_share_pct: u8,
}
impl OrganizationCreateCommand {
    pub fn budget_link_id(&self) -> Option<crate::InvitationLinkId> {
        match self.budget_attribution {
            crate::reissue::BudgetAttribution::SourceLink => self.link_id,
            crate::reissue::BudgetAttribution::AdministratorReissue => None,
        }
    }
    pub fn delivery_key(&self) -> String {
        crate::delivery::delivery_key(
            self.request_id,
            self.organization.org_id,
            self.delivery_generation,
        )
    }
    /// SQL identity is namespaced; the Restate object key remains normative.
    pub fn storage_id(&self) -> String {
        format!("org:{}", self.delivery_key())
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        crate::bulk::validate_origin(self.link_id, self.batch)?;
        let positive = |n| n > 0 && n <= crate::storage::integer::MAX_EXACT;
        if ![
            self.account_id,
            self.installation_id,
            self.requester_id,
            self.delivery_generation,
        ]
        .into_iter()
        .all(positive)
            || self.account_id != self.organization.org_id
            || self.approval_id.is_empty()
            || self.approval_id.len() > 256
            || self.approval_id.chars().any(char::is_control)
            || !(1..=100).contains(&self.budget_share_pct)
        {
            return Err("invalid organization delivery identity");
        }
        crate::targets::Targets {
            organization: Some(self.organization.clone()),
            teams: self.teams.clone(),
            repos: vec![],
        }
        .validate()
        .map_err(|_| "invalid organization delivery targets")?;
        if self
            .teams
            .windows(2)
            .any(|pair| pair[0].team_id >= pair[1].team_id)
        {
            return Err("organization teams are not canonical");
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OrganizationWrite {
    Invite,
    Team {
        team_id: u64,
    },
    Cancel {
        upstream_id: u64,
        reason: SettlementReason,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OrganizationAttempt {
    pub create: OrganizationCreateCommand,
    pub action: OrganizationWrite,
}
impl OrganizationAttempt {
    pub fn step(&self) -> String {
        match self.action {
            OrganizationWrite::Invite => "org".into(),
            OrganizationWrite::Team { team_id } => format!("team:{team_id}"),
            OrganizationWrite::Cancel { .. } => "cancel".into(),
        }
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        self.create.validate()?;
        match self.action {
            OrganizationWrite::Team { team_id }
                if !self.create.teams.iter().any(|t| t.team_id == team_id) =>
            {
                Err("team is outside immutable delivery")
            }
            OrganizationWrite::Cancel { upstream_id, .. }
                if upstream_id == 0 || upstream_id > crate::storage::integer::MAX_EXACT =>
            {
                Err("invalid owned invitation identity")
            }
            _ => Ok(()),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OrganizationRole {
    Member,
    Admin,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OrganizationInvitationRole {
    DirectMember,
    Admin,
    BillingManager,
    HiringManager,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TeamRole {
    Member,
    Maintainer,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MembershipState {
    Active,
    Pending,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OrganizationBlockedReason {
    InstallationUnavailable,
    InstallationPermissions,
    RequesterIdentity,
    TargetUnavailable,
    MintForbidden,
    IpAllowList,
    NoSeat,
    IdpSyncedTeam,
    ManagedEnterprise,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OrganizationCreateOutcome {
    Planned,
    Held {
        reason: HoldReason,
    },
    Submitted,
    Blocked {
        reason: OrganizationBlockedReason,
    },
    Throttled {
        until: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<crate::budget::RetryReason>,
    },
    OutcomeUnknown,
    Created {
        upstream_id: u64,
        created_at: DateTime<Utc>,
    },
    AlreadySatisfied {
        role: OrganizationRole,
    },
    AlreadyPending {
        upstream_id: u64,
        role: OrganizationInvitationRole,
    },
    Aborted {
        reason: AbortReason,
    },
    Failed {
        reason: FailureReason,
    },
}
impl OrganizationCreateOutcome {
    pub fn confirmed(&self) -> bool {
        matches!(
            self,
            Self::Created { .. }
                | Self::AlreadySatisfied { .. }
                | Self::AlreadyPending { .. }
                | Self::Aborted { .. }
                | Self::Failed { .. }
        )
    }
    pub fn upstream_id(&self) -> Option<u64> {
        match self {
            Self::Created { upstream_id, .. } | Self::AlreadyPending { upstream_id, .. } => {
                Some(*upstream_id)
            }
            _ => None,
        }
    }
}
/// A team membership response has no invitation ID. The shared org POST is the
/// only way a team step can explicitly reference an owned org invitation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TeamCreation {
    OrganizationInvitation { upstream_id: u64 },
    PendingMembership,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TeamCreateOutcome {
    Planned,
    Held {
        reason: HoldReason,
    },
    Submitted,
    Blocked {
        reason: OrganizationBlockedReason,
    },
    Throttled {
        until: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<crate::budget::RetryReason>,
    },
    OutcomeUnknown,
    AlreadySatisfied {
        role: TeamRole,
        state: MembershipState,
    },
    Created {
        via: TeamCreation,
    },
    Upgraded,
    Aborted {
        reason: AbortReason,
    },
    Failed {
        reason: FailureReason,
    },
}
impl TeamCreateOutcome {
    pub fn confirmed(&self) -> bool {
        matches!(
            self,
            Self::Created { .. }
                | Self::AlreadySatisfied { .. }
                | Self::Upgraded
                | Self::Aborted { .. }
                | Self::Failed { .. }
        )
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OrganizationCreateReceipt {
    #[serde(
        default,
        skip_serializing_if = "crate::delivery_audit::DeliveryAuditVersion::is_legacy"
    )]
    pub audit_version: crate::delivery_audit::DeliveryAuditVersion,
    pub command: OrganizationCreateCommand,
    pub outcome: OrganizationCreateOutcome,
    pub revision: u64,
    pub confirmed_at: Option<DateTime<Utc>>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TeamCreateReceipt {
    pub team_id: u64,
    pub outcome: TeamCreateOutcome,
    pub revision: u64,
    pub confirmed_at: Option<DateTime<Utc>>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OrganizationSettlement {
    pub state: InvitationState,
    #[schemars(with = "serde_json::Value")]
    pub event: crate::audit::AuditEvent,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TeamSettlement {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<SettlementReason>,
    pub state: InvitationState,
    pub observed_at: DateTime<Utc>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TeamDeliverySnapshot {
    pub create: TeamCreateReceipt,
    pub settlement: Option<TeamSettlement>,
    pub tracking_stopped: Option<SettlementReason>,
    pub revision: u64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OrganizationDeliverySnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invitation_timing: Option<crate::delivery::InvitationTiming>,
    pub create: OrganizationCreateReceipt,
    pub teams: BTreeMap<u64, TeamDeliverySnapshot>,
    pub revision: u64,
    pub settlement: Option<OrganizationSettlement>,
    pub tracking_stopped: Option<SettlementReason>,
}
impl OrganizationDeliverySnapshot {
    pub fn retain_invitation_timing(&mut self, timing: crate::delivery::InvitationTiming) -> bool {
        let Some(id) = self.create.outcome.upstream_id() else {
            return false;
        };
        let owned = matches!(
            self.create.outcome,
            OrganizationCreateOutcome::Created { .. }
        );
        if timing
            .owned_lifetime_seconds
            .is_some_and(|s| !(3600..=604800).contains(&s))
        {
            return false;
        }
        if self.invitation_timing.is_some()
            || timing.upstream_id != id
            || crate::delivery::InvitationTiming::observed(
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
        let id = self.create.outcome.upstream_id()?;
        if matches!(self.create.outcome, OrganizationCreateOutcome::Created { created_at, .. } if Some(created_at) != self.invitation_timing.as_ref().map(|t| t.created_at))
        {
            return None;
        }
        self.invitation_timing.as_ref()?.expires_at(
            id,
            self.create.command.approved_at,
            matches!(
                self.create.outcome,
                OrganizationCreateOutcome::Created { .. }
            ),
        )
    }
    /// Tracking follows unfinished effects and pending receipts independently;
    /// a completed organization step cannot hide an unresolved team step.
    pub fn needs_tracking(&self) -> bool {
        if matches!(
            self.create.outcome,
            OrganizationCreateOutcome::Blocked {
                reason: OrganizationBlockedReason::ManagedEnterprise
            }
        ) {
            return false;
        }

        let org = self.settlement.is_none()
            && self.tracking_stopped.is_none()
            && (!self.create.outcome.confirmed()
                || matches!(
                    self.create.outcome,
                    OrganizationCreateOutcome::Created { .. }
                        | OrganizationCreateOutcome::AlreadyPending { .. }
                ));
        org || self.teams.values().any(|team| {
            team.settlement.is_none()
                && team.tracking_stopped.is_none()
                && !matches!(
                    team.create.outcome,
                    TeamCreateOutcome::Blocked {
                        reason: OrganizationBlockedReason::ManagedEnterprise
                    }
                )
                && (!team.create.outcome.confirmed()
                    || matches!(team.create.outcome, TeamCreateOutcome::Created { .. }))
        })
    }
    pub fn planned(command: OrganizationCreateCommand) -> Result<Self, &'static str> {
        command.validate()?;
        let teams = command
            .teams
            .iter()
            .map(|t| {
                (
                    t.team_id,
                    TeamDeliverySnapshot {
                        create: TeamCreateReceipt {
                            team_id: t.team_id,
                            outcome: TeamCreateOutcome::Planned,
                            revision: 1,
                            confirmed_at: None,
                        },
                        settlement: None,
                        tracking_stopped: None,
                        revision: 1,
                    },
                )
            })
            .collect();
        Ok(Self {
            create: OrganizationCreateReceipt {
                audit_version: if command.invitation_id.is_some() {
                    crate::delivery_audit::DeliveryAuditVersion::Legacy
                } else {
                    crate::delivery_audit::DeliveryAuditVersion::CanonicalV2
                },
                command,
                outcome: OrganizationCreateOutcome::Planned,
                revision: 1,
                confirmed_at: None,
            },
            teams,
            invitation_timing: None,
            revision: 1,
            settlement: None,
            tracking_stopped: None,
        })
    }
}

fn terminal(state: InvitationState) -> bool {
    matches!(
        state,
        InvitationState::Accepted
            | InvitationState::Declined
            | InvitationState::Expired
            | InvitationState::Cancelled
    )
}
fn valid_revision(value: u64) -> bool {
    value > 0 && value <= crate::storage::integer::MAX_EXACT
}
fn confirmation(confirmed: bool, at: Option<DateTime<Utc>>, approved: DateTime<Utc>) -> bool {
    confirmed == at.is_some() && at.is_none_or(|at| at >= approved)
}
impl OrganizationDeliverySnapshot {
    pub fn validate(&self) -> Result<(), &'static str> {
        if let Some(timing) = &self.invitation_timing {
            let owned = matches!(
                self.create.outcome,
                OrganizationCreateOutcome::Created { .. }
            );
            if timing
                .owned_lifetime_seconds
                .is_some_and(|s| !(3600..=604800).contains(&s))
                || self.create.outcome.upstream_id() != Some(timing.upstream_id)
                || crate::delivery::InvitationTiming::observed(
                    timing.upstream_id,
                    timing.created_at,
                    timing.observed_at,
                    self.create.command.approved_at,
                    owned,
                )
                .is_none()
                || matches!(self.create.outcome, OrganizationCreateOutcome::Created { created_at, .. } if created_at != timing.created_at)
            {
                return Err("invalid invitation timing");
            }
        }

        crate::delivery_audit::validate_identity(
            self.create.audit_version,
            self.create.command.invitation_id,
        )?;
        let command = &self.create.command;
        command.validate()?;
        if !valid_revision(self.revision)
            || !valid_revision(self.create.revision)
            || self.create.revision > self.revision
            || !confirmation(
                self.create.outcome.confirmed(),
                self.create.confirmed_at,
                command.approved_at,
            )
            || self.teams.len() != command.teams.len()
        {
            return Err("invalid organization receipt revision");
        }
        if self
            .create
            .outcome
            .upstream_id()
            .is_some_and(|id| id == 0 || id > crate::storage::integer::MAX_EXACT)
        {
            return Err("invalid organization invitation id");
        }
        if let OrganizationCreateOutcome::Created { created_at, .. } = &self.create.outcome
            && (created_at.timestamp() < command.approved_at.timestamp()
                || self.create.confirmed_at.is_none_or(|at| *created_at > at))
        {
            return Err("invalid owned organization invitation time");
        }
        if self.tracking_stopped.is_some()
            && (!matches!(
                self.create.outcome,
                OrganizationCreateOutcome::AlreadyPending { .. }
            ) || self.settlement.is_some())
        {
            return Err("invalid foreign organization tracking stop");
        }
        if let Some(settlement) = &self.settlement {
            let expected_event = crate::delivery_audit::organization_settlement_kind(
                self.create.audit_version,
                settlement.state,
            )?;
            if self.create.audit_version != crate::delivery_audit::DeliveryAuditVersion::Legacy {
                if settlement.event.id
                    != crate::delivery_audit::organization_settlement_id(
                        self.create.audit_version,
                        command,
                    )
                {
                    return Err("organization settlement audit identity conflict");
                }
                if settlement.state == InvitationState::Cancelled {
                    serde_json::from_value::<SettlementReason>(
                        settlement
                            .event
                            .metadata
                            .get("reason")
                            .cloned()
                            .unwrap_or_default(),
                    )
                    .map_err(|_| "organization cancellation reason missing")?;
                }
            }
            if self.create.outcome.upstream_id().is_none()
                || (settlement.state == InvitationState::Cancelled
                    && !matches!(
                        self.create.outcome,
                        OrganizationCreateOutcome::Created { .. }
                    ))
                || settlement.event.account_id != command.account_id
                || settlement.event.target_kind != crate::audit::TargetKind::OrganizationDelivery
                || settlement.event.target_id != command.storage_id()
                || settlement.event.event_type != expected_event
                || settlement.event.request_id.as_deref()
                    != Some(command.request_id.to_string().as_str())
                || self
                    .create
                    .confirmed_at
                    .is_none_or(|at| settlement.event.occurred_at < at)
            {
                return Err("organization settlement identity conflict");
            }
        }
        for target in &command.teams {
            let team = self
                .teams
                .get(&target.team_id)
                .ok_or("organization team missing")?;
            if team.create.team_id != target.team_id
                || !valid_revision(team.revision)
                || !valid_revision(team.create.revision)
                || team.create.revision > team.revision
                || team.revision > self.revision
                || !confirmation(
                    team.create.outcome.confirmed(),
                    team.create.confirmed_at,
                    command.approved_at,
                )
            {
                return Err("invalid team receipt revision");
            }
            if let TeamCreateOutcome::Created {
                via: TeamCreation::OrganizationInvitation { upstream_id },
            } = team.create.outcome
                && !matches!(self.create.outcome,OrganizationCreateOutcome::Created{upstream_id:owned,..} if upstream_id==owned)
            {
                return Err("team invitation ownership conflict");
            }
            if team.tracking_stopped.is_some()
                && (!matches!(
                    team.create.outcome,
                    TeamCreateOutcome::Created {
                        via: TeamCreation::PendingMembership
                    }
                ) || team.settlement.is_some())
            {
                return Err("invalid team tracking stop");
            }
            if self.create.audit_version != crate::delivery_audit::DeliveryAuditVersion::Legacy
                && let Some(settlement) = &team.settlement
            {
                if settlement.state == InvitationState::Cancelled && settlement.reason.is_none()
                    || settlement.state != InvitationState::Cancelled && settlement.reason.is_some()
                {
                    return Err("invalid team cancellation reason");
                }
                if matches!(
                    team.create.outcome,
                    TeamCreateOutcome::Created {
                        via: TeamCreation::OrganizationInvitation { .. }
                    }
                ) {
                    let org = self
                        .settlement
                        .as_ref()
                        .ok_or("team organization settlement missing")?;
                    let reason = if org.state == InvitationState::Cancelled {
                        Some(
                            serde_json::from_value::<SettlementReason>(
                                org.event
                                    .metadata
                                    .get("reason")
                                    .cloned()
                                    .unwrap_or_default(),
                            )
                            .map_err(|_| "organization cancellation reason missing")?,
                        )
                    } else {
                        None
                    };
                    if settlement.state != org.state
                        || settlement.observed_at != org.event.occurred_at
                        || settlement.reason != reason
                    {
                        return Err("team organization settlement mismatch");
                    }
                }
            }
            if let Some(settlement) = &team.settlement
                && (!terminal(settlement.state)
                    || !matches!(team.create.outcome, TeamCreateOutcome::Created { .. })
                    || team
                        .create
                        .confirmed_at
                        .is_none_or(|at| settlement.observed_at < at)
                    || (settlement.state == InvitationState::Cancelled
                        && !matches!(
                            team.create.outcome,
                            TeamCreateOutcome::Created {
                                via: TeamCreation::OrganizationInvitation { .. }
                            }
                        )))
            {
                return Err("invalid team settlement");
            }
        }
        Ok(())
    }
    /// `false` means a valid stale/idempotent projection. Identity conflicts
    /// still fail, and newer snapshots cannot rewrite any confirmed receipt.
    pub fn validate_successor(&self, previous: &Self) -> Result<bool, &'static str> {
        self.validate()?;
        previous.validate()?;
        if self.create.command != previous.create.command
            || self.create.audit_version != previous.create.audit_version
        {
            return Err("organization delivery identity conflict");
        }
        if self.revision < previous.revision {
            return Ok(false);
        }
        if self.revision == previous.revision {
            return if self == previous {
                Ok(false)
            } else {
                Err("organization equal revision conflict")
            };
        }
        if previous
            .invitation_timing
            .as_ref()
            .is_some_and(|old| self.invitation_timing.as_ref() != Some(old))
        {
            return Err("invitation timing conflict");
        }
        if self.create.revision < previous.create.revision
            || ((self.create.revision == previous.create.revision
                || previous.create.outcome.confirmed())
                && self.create != previous.create)
            || previous
                .settlement
                .as_ref()
                .is_some_and(|old| Some(old) != self.settlement.as_ref())
            || previous
                .tracking_stopped
                .is_some_and(|old| Some(old) != self.tracking_stopped)
        {
            return Err("organization confirmed receipt conflict");
        }
        for (id, old) in &previous.teams {
            let new = self.teams.get(id).ok_or("organization team missing")?;
            if new.revision < old.revision
                || (new.revision == old.revision && new != old)
                || new.create.revision < old.create.revision
                || ((new.create.revision == old.create.revision || old.create.outcome.confirmed())
                    && new.create != old.create)
                || old
                    .settlement
                    .as_ref()
                    .is_some_and(|settlement| Some(settlement) != new.settlement.as_ref())
                || old
                    .tracking_stopped
                    .is_some_and(|reason| Some(reason) != new.tracking_stopped)
            {
                return Err("team confirmed receipt conflict");
            }
        }
        Ok(true)
    }
}

/// On-demand read scheduling result; never a delivery or acceptance receipt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OrganizationRefresh {
    Requested,
    Throttled { retry_at: DateTime<Utc> },
    NotTracked,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn command() -> OrganizationCreateCommand {
        OrganizationCreateCommand {
            budget_attribution: Default::default(),
            batch: None,
            access_lifetime_seconds: None,
            invitation_id: Some(GithubInvitationId::new()),
            link_id: Some(InvitationLinkId::new()),
            request_id: RequestId::new(),
            approval_id: "fixture-decision".into(),
            account_id: 10,
            installation_id: 2,
            requester_id: 8,
            organization: OrganizationTarget {
                org_id: 10,
                org_login: "acme".into(),
                grant: crate::targets::MemberGrant::Member,
            },
            teams: vec![TeamTarget {
                team_id: 5,
                team_slug: "engineering".into(),
                grant: crate::targets::MemberGrant::Member,
            }],
            delivery_generation: 1,
            approved_at: "2026-10-16T12:00:00Z".parse().unwrap(),
            budget_share_pct: 50,
        }
    }
    #[test]
    fn kind_partition_and_exact_step_binding() {
        let command = command();
        assert_eq!(
            command.storage_id(),
            format!("org:{}", command.delivery_key())
        );
        assert_ne!(
            command.storage_id(),
            crate::delivery::delivery_key(command.request_id, 10, 1)
        );
        let mut attempt = OrganizationAttempt {
            create: command,
            action: OrganizationWrite::Team { team_id: 5 },
        };
        assert!(attempt.validate().is_ok());
        assert_eq!(attempt.step(), "team:5");
        attempt.action = OrganizationWrite::Team { team_id: 6 };
        assert!(attempt.validate().is_err());
        attempt.action = OrganizationWrite::Cancel {
            upstream_id: 0,
            reason: SettlementReason::LinkRevoked,
        };
        assert!(attempt.validate().is_err());
    }
    #[test]
    fn confirmed_org_and_team_receipts_survive_later_step_progress() {
        let mut previous = OrganizationDeliverySnapshot::planned(command()).unwrap();
        let planned = previous.clone();
        previous.revision = 2;
        previous.create.revision = 2;
        previous.create.outcome = OrganizationCreateOutcome::AlreadySatisfied {
            role: OrganizationRole::Admin,
        };
        previous.create.confirmed_at = Some(previous.create.command.approved_at);
        assert_eq!(previous.validate_successor(&planned), Ok(true));
        let mut next = previous.clone();
        next.revision = 3;
        let team = next.teams.get_mut(&5).unwrap();
        team.revision = 3;
        team.create.revision = 3;
        team.create.outcome = TeamCreateOutcome::Upgraded;
        team.create.confirmed_at = Some(next.create.command.approved_at);
        assert_eq!(next.validate_successor(&previous), Ok(true));
        assert_eq!(planned.validate_successor(&next), Ok(false));
        let mut conflict = next.clone();
        conflict.revision = 4;
        conflict.create.revision = 4;
        conflict.create.outcome = OrganizationCreateOutcome::AlreadySatisfied {
            role: OrganizationRole::Member,
        };
        assert!(conflict.validate_successor(&next).is_err());
        let mut conflict = next.clone();
        conflict.teams.get_mut(&5).unwrap().create.outcome = TeamCreateOutcome::AlreadySatisfied {
            role: TeamRole::Maintainer,
            state: MembershipState::Active,
        };
        assert!(conflict.validate_successor(&next).is_err());
    }
    #[test]
    fn team_pending_membership_does_not_invent_owned_org_invitation() {
        let mut snapshot = OrganizationDeliverySnapshot::planned(command()).unwrap();
        snapshot.revision = 2;
        snapshot.create.revision = 2;
        snapshot.create.outcome = OrganizationCreateOutcome::AlreadyPending {
            upstream_id: 99,
            role: OrganizationInvitationRole::Admin,
        };
        snapshot.create.confirmed_at = Some(snapshot.create.command.approved_at);
        let team = snapshot.teams.get_mut(&5).unwrap();
        team.revision = 2;
        team.create.revision = 2;
        team.create.outcome = TeamCreateOutcome::Created {
            via: TeamCreation::PendingMembership,
        };
        team.create.confirmed_at = Some(snapshot.create.command.approved_at);
        assert!(snapshot.validate().is_ok());
        snapshot.teams.get_mut(&5).unwrap().create.outcome = TeamCreateOutcome::Created {
            via: TeamCreation::OrganizationInvitation { upstream_id: 99 },
        };
        assert!(snapshot.validate().is_err());
    }
}
