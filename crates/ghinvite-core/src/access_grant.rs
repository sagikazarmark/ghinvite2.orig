//! Ownership-qualified accepted access and separately fenced removal.
//!
//! A structurally valid command is not authority: the workflow resolves its
//! current delivery owner and validates the original Created + Accepted facts
//! before retaining it. Existing/foreign/upgraded access has no grant here.
use crate::{
    InvitationLinkId, RequestId,
    bulk::BatchRef,
    org_delivery::{OrganizationRole, TeamRole},
    storage::integer::MAX_EXACT,
    targets::{EffectiveRole, RepoRole},
};
use chrono::{DateTime, Duration, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GrantTarget {
    Repository {
        repo_id: u64,
        full_name: String,
        grant: RepoRole,
    },
    Organization {
        org_id: u64,
        login: String,
    },
    Team {
        org_id: u64,
        org_login: String,
        team_id: u64,
        slug: String,
    },
}
impl GrantTarget {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Repository { .. } => "repo",
            Self::Organization { .. } => "org",
            Self::Team { .. } => "team",
        }
    }
    pub fn id(&self) -> u64 {
        match self {
            Self::Repository { repo_id, .. } => *repo_id,
            Self::Organization { org_id, .. } => *org_id,
            Self::Team { team_id, .. } => *team_id,
        }
    }
    pub fn delivery_target(&self) -> u64 {
        match self {
            Self::Team { org_id, .. } => *org_id,
            _ => self.id(),
        }
    }
    pub fn app(&self) -> crate::account::App {
        match self {
            Self::Repository { .. } => crate::account::App::Repo,
            _ => crate::account::App::Org,
        }
    }
}

/// Positive creation provenance, never an effective role or a foreign pending
/// invitation. Team attachment must reference the same owned org invitation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GrantCreation {
    RepositoryInvitation { upstream_id: u64 },
    OrganizationInvitation { upstream_id: u64 },
    AttachedTeam { upstream_id: u64 },
    TeamMembership,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AccessGrantCommand {
    pub request_id: RequestId,
    pub link_id: Option<InvitationLinkId>,
    pub batch: Option<BatchRef>,
    pub account_id: u64,
    pub installation_id: u64,
    pub requester_id: u64,
    pub delivery_generation: u64,
    pub target: GrantTarget,
    pub creation: GrantCreation,
    pub accepted_at: DateTime<Utc>,
    /// Positive seconds. None records revoke eligibility without a timer.
    pub access_lifetime_seconds: Option<u32>,
}
impl AccessGrantCommand {
    pub fn key(&self) -> String {
        format!(
            "{}:{}:{}",
            self.request_id,
            self.target.kind(),
            self.target.id()
        )
    }
    pub fn storage_id(&self) -> String {
        format!("{}:{}", self.key(), self.delivery_generation)
    }
    pub fn step(&self) -> String {
        format!("remove:{}:{}", self.target.kind(), self.target.id())
    }
    pub fn remove_at(&self) -> Result<Option<DateTime<Utc>>, &'static str> {
        self.access_lifetime_seconds
            .map(|seconds| {
                if seconds == 0 {
                    return Err("zero access lifetime");
                }
                self.accepted_at
                    .checked_add_signed(Duration::seconds(i64::from(seconds)))
                    .ok_or("access expiry overflow")
            })
            .transpose()
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        let positive = |id| id > 0 && id <= MAX_EXACT;
        if [
            self.account_id,
            self.installation_id,
            self.requester_id,
            self.delivery_generation,
            self.target.id(),
            self.target.delivery_target(),
        ]
        .into_iter()
        .any(|id| !positive(id))
        {
            return Err("invalid access grant identity");
        }
        match (self.link_id, &self.batch) {
            (Some(_), None) => {}
            (None, Some(batch)) => batch.validate()?,
            _ => return Err("access grant origin must be exact"),
        }
        let name = |s: &str| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control);
        match &self.target {
            GrantTarget::Repository { full_name, .. } => {
                crate::RepositoryIdentity::parse(full_name.clone())
                    .map_err(|_| "invalid access repository")?;
            }
            GrantTarget::Organization { org_id, login }
                if *org_id == self.account_id && name(login) => {}
            GrantTarget::Team {
                org_id,
                org_login,
                slug,
                ..
            } if *org_id == self.account_id && name(org_login) && name(slug) => {}
            _ => return Err("invalid access target"),
        }
        match (&self.target, &self.creation) {
            (
                GrantTarget::Repository { .. },
                GrantCreation::RepositoryInvitation { upstream_id },
            )
            | (
                GrantTarget::Organization { .. },
                GrantCreation::OrganizationInvitation { upstream_id },
            )
            | (GrantTarget::Team { .. }, GrantCreation::AttachedTeam { upstream_id })
                if positive(*upstream_id) => {}
            (GrantTarget::Team { .. }, GrantCreation::TeamMembership) => {}
            _ => return Err("access creation kind mismatch"),
        }
        self.remove_at()?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CurrentRole {
    Repository { role: EffectiveRole },
    Organization { role: OrganizationRole },
    Team { role: TeamRole },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RemovalSkip {
    RoleChanged { now: CurrentRole },
    AlreadyAbsent,
    SupersededGeneration,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RemovalReason {
    AccessExpiry,
    LinkRevoked,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RemovalBlocked {
    InstallationUnavailable,
    InstallationPermissions,
    TargetUnavailable,
    RequesterIdentity,
    ReadUnavailable,
    RateLimited,
    PolicyRefused,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RemovalOutcome {
    Scheduled,
    Blocked {
        reason: RemovalBlocked,
    },
    OutcomeUnknown,
    Removed {
        reason: RemovalReason,
        at: DateTime<Utc>,
    },
    RemovalSkipped {
        reason: RemovalSkip,
        at: DateTime<Utc>,
    },
}
impl RemovalOutcome {
    pub fn terminal(&self) -> bool {
        matches!(self, Self::Removed { .. } | Self::RemovalSkipped { .. })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemovalCommand {
    pub grant: AccessGrantCommand,
    pub reason: RemovalReason,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AccessGrantSnapshot {
    pub command: AccessGrantCommand,
    pub revision: u64,
    pub outcome: RemovalOutcome,
}
impl AccessGrantSnapshot {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.command.validate()?;
        if self.revision == 0 || self.revision > MAX_EXACT {
            return Err("invalid grant revision");
        }
        match &self.outcome {
            RemovalOutcome::Removed { at, .. } | RemovalOutcome::RemovalSkipped { at, .. }
                if *at < self.command.accepted_at =>
            {
                Err("removal before acceptance")
            }
            RemovalOutcome::RemovalSkipped {
                reason: RemovalSkip::RoleChanged { now },
                ..
            } if removal_role(&self.command.target, now)? != RoleDecision::Skip => {
                Err("unsubstantiated role-change skip")
            }
            _ => Ok(()),
        }
    }
    pub fn validate_successor(&self, next: &Self) -> Result<(), &'static str> {
        self.validate()?;
        next.validate()?;
        if self.command != next.command || next.revision < self.revision {
            return Err("access grant identity or revision conflict");
        }
        if (next.revision == self.revision || self.outcome.terminal()) && self != next {
            return Err("access grant receipt conflict");
        }
        // No absence, access-role change or retry may turn an uncertain write
        // into permission to repeat DELETE. Only positive completion resolves it.
        if matches!(self.outcome, RemovalOutcome::OutcomeUnknown)
            && !matches!(
                next.outcome,
                RemovalOutcome::OutcomeUnknown | RemovalOutcome::Removed { .. }
            )
        {
            return Err("uncertain removal cannot become unused");
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoleDecision {
    Remove,
    Skip,
}
/// Numeric identities and active/direct membership evidence are prerequisites
/// supplied by the protocol. Unranked repository roles are never safe to remove.
pub fn removal_role(
    target: &GrantTarget,
    current: &CurrentRole,
) -> Result<RoleDecision, &'static str> {
    let skip = match (target, current) {
        (GrantTarget::Repository { grant, .. }, CurrentRole::Repository { role }) => {
            role.rank().is_none_or(|rank| rank > grant.rank())
        }
        (GrantTarget::Organization { .. }, CurrentRole::Organization { role }) => {
            *role == OrganizationRole::Admin
        }
        (GrantTarget::Team { .. }, CurrentRole::Team { role }) => *role == TeamRole::Maintainer,
        _ => return Err("removal role kind mismatch"),
    };
    Ok(if skip {
        RoleDecision::Skip
    } else {
        RoleDecision::Remove
    })
}

/// Reissue must first drain or stop this generation. A scheduled removal is not
/// safe to ignore: its delayed timer could otherwise delete the replacement.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RemovalBarrier {
    NoRemoval,
    DefinitivelyFinished,
    Pending,
    MayHaveWritten,
}
impl AccessGrantSnapshot {
    pub fn barrier(&self) -> RemovalBarrier {
        match self.outcome {
            RemovalOutcome::OutcomeUnknown => RemovalBarrier::MayHaveWritten,
            RemovalOutcome::Removed { .. } | RemovalOutcome::RemovalSkipped { .. } => {
                RemovalBarrier::DefinitivelyFinished
            }
            _ => RemovalBarrier::Pending,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemovalBarrierQuery {
    pub request_id: RequestId,
    pub target: GrantTarget,
    pub delivery_generation: u64,
}

/// Derive eligibility from original positive ownership and later acceptance.
/// Effective Accepted display for AlreadySatisfied/Upgraded is insufficient.
pub fn accepted_repository(
    snapshot: &crate::delivery::DeliverySnapshot,
) -> Result<Option<AccessGrantCommand>, &'static str> {
    let crate::delivery::CreateOutcome::Created { upstream_id } = snapshot.create.outcome else {
        return Ok(None);
    };
    let Some(settlement) = &snapshot.settlement else {
        return Ok(None);
    };
    if settlement.state != crate::InvitationState::Accepted || snapshot.tracking_stopped.is_some() {
        return Ok(None);
    }
    let c = &snapshot.create.command;
    if settlement.event.occurred_at < c.approved_at {
        return Err("acceptance predates approval");
    }
    let command = AccessGrantCommand {
        request_id: c.request_id,
        link_id: c.link_id,
        batch: c.batch,
        account_id: c.account_id,
        installation_id: c.installation_id,
        requester_id: c.requester_id,
        delivery_generation: c.delivery_generation,
        target: GrantTarget::Repository {
            repo_id: c.repo_id,
            full_name: c.repo_full_name.clone(),
            grant: c.permission.into(),
        },
        creation: GrantCreation::RepositoryInvitation { upstream_id },
        accepted_at: settlement.event.occurred_at,
        access_lifetime_seconds: c.access_lifetime_seconds,
    };
    command.validate()?;
    Ok(Some(command))
}
pub fn accepted_organization(
    snapshot: &crate::org_delivery::OrganizationDeliverySnapshot,
) -> Result<Vec<AccessGrantCommand>, &'static str> {
    use crate::org_delivery::{OrganizationCreateOutcome, TeamCreateOutcome, TeamCreation};
    snapshot.validate()?;
    let c = &snapshot.create.command;
    let make = |target, creation, accepted_at| AccessGrantCommand {
        request_id: c.request_id,
        link_id: c.link_id,
        batch: c.batch,
        account_id: c.account_id,
        installation_id: c.installation_id,
        requester_id: c.requester_id,
        delivery_generation: c.delivery_generation,
        target,
        creation,
        accepted_at,
        access_lifetime_seconds: c.access_lifetime_seconds,
    };
    let mut result = Vec::new();
    if let (OrganizationCreateOutcome::Created { upstream_id, .. }, Some(settlement)) =
        (&snapshot.create.outcome, &snapshot.settlement)
        && settlement.state == crate::InvitationState::Accepted
        && snapshot.tracking_stopped.is_none()
    {
        result.push(make(
            GrantTarget::Organization {
                org_id: c.organization.org_id,
                login: c.organization.org_login.clone(),
            },
            GrantCreation::OrganizationInvitation {
                upstream_id: *upstream_id,
            },
            settlement.event.occurred_at,
        ));
    }
    for target in &c.teams {
        let Some(team) = snapshot.teams.get(&target.team_id) else {
            return Err("missing team receipt");
        };
        let Some(settlement) = &team.settlement else {
            continue;
        };
        if settlement.state != crate::InvitationState::Accepted || team.tracking_stopped.is_some() {
            continue;
        }
        let creation = match &team.create.outcome {
            TeamCreateOutcome::Created {
                via: TeamCreation::PendingMembership,
            } => GrantCreation::TeamMembership,
            TeamCreateOutcome::Created {
                via: TeamCreation::OrganizationInvitation { upstream_id },
            } if matches!(snapshot.create.outcome,OrganizationCreateOutcome::Created {upstream_id:owned,..} if owned==*upstream_id) => {
                GrantCreation::AttachedTeam {
                    upstream_id: *upstream_id,
                }
            }
            _ => continue,
        };
        result.push(make(
            GrantTarget::Team {
                org_id: c.organization.org_id,
                org_login: c.organization.org_login.clone(),
                team_id: target.team_id,
                slug: target.team_slug.clone(),
            },
            creation,
            settlement.observed_at,
        ));
    }
    for command in &result {
        command.validate()?;
        if command.accepted_at < c.approved_at {
            return Err("acceptance predates approval");
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn grant(target: GrantTarget) -> AccessGrantCommand {
        let creation = match target {
            GrantTarget::Repository { .. } => {
                GrantCreation::RepositoryInvitation { upstream_id: 8 }
            }
            GrantTarget::Organization { .. } => {
                GrantCreation::OrganizationInvitation { upstream_id: 8 }
            }
            GrantTarget::Team { .. } => GrantCreation::AttachedTeam { upstream_id: 8 },
        };
        AccessGrantCommand {
            request_id: RequestId::new(),
            link_id: Some(InvitationLinkId::new()),
            batch: None,
            account_id: 1,
            installation_id: 2,
            requester_id: 3,
            delivery_generation: 1,
            target,
            creation,
            accepted_at: DateTime::from_timestamp(1_000, 0).unwrap(),
            access_lifetime_seconds: Some(3600),
        }
    }
    fn repo() -> GrantTarget {
        GrantTarget::Repository {
            repo_id: 1,
            full_name: "acme/repo".into(),
            grant: RepoRole::Write,
        }
    }
    #[test]
    fn expiry_starts_at_acceptance_and_none_is_registered_without_timer() {
        let mut command = grant(repo());
        assert_eq!(command.remove_at().unwrap().unwrap().timestamp(), 4600);
        command.access_lifetime_seconds = None;
        assert_eq!(command.remove_at(), Ok(None));
        command.access_lifetime_seconds = Some(0);
        assert!(command.validate().is_err());
    }
    #[test]
    fn role_promotion_and_custom_roles_protect_later_access() {
        for role in [
            EffectiveRole::Maintain,
            EffectiveRole::Admin,
            EffectiveRole::Custom(crate::targets::CustomRoleName::new("custom").unwrap()),
            EffectiveRole::BaseRole,
        ] {
            assert_eq!(
                removal_role(&repo(), &CurrentRole::Repository { role }),
                Ok(RoleDecision::Skip)
            );
        }
        for role in [
            EffectiveRole::Read,
            EffectiveRole::Triage,
            EffectiveRole::Write,
        ] {
            assert_eq!(
                removal_role(&repo(), &CurrentRole::Repository { role }),
                Ok(RoleDecision::Remove)
            );
        }
        let org = GrantTarget::Organization {
            org_id: 1,
            login: "acme".into(),
        };
        assert_eq!(
            removal_role(
                &org,
                &CurrentRole::Organization {
                    role: OrganizationRole::Admin
                }
            ),
            Ok(RoleDecision::Skip)
        );
        let team = GrantTarget::Team {
            org_id: 1,
            org_login: "acme".into(),
            team_id: 1,
            slug: "team".into(),
        };
        assert_eq!(
            removal_role(
                &team,
                &CurrentRole::Team {
                    role: TeamRole::Maintainer
                }
            ),
            Ok(RoleDecision::Skip)
        );
        assert!(
            removal_role(
                &team,
                &CurrentRole::Organization {
                    role: OrganizationRole::Member
                }
            )
            .is_err()
        );
    }
    #[test]
    fn kind_generation_origin_and_creation_are_bound() {
        let command = grant(repo());
        command.validate().unwrap();
        let mut org = command.clone();
        org.target = GrantTarget::Organization {
            org_id: 1,
            login: "acme".into(),
        };
        assert_ne!(command.key(), org.key());
        assert!(org.validate().is_err());
        org.creation = GrantCreation::OrganizationInvitation { upstream_id: 8 };
        org.validate().unwrap();
        let mut next = command.clone();
        next.delivery_generation = 2;
        assert_eq!(command.key(), next.key());
        assert_ne!(command.storage_id(), next.storage_id());
        next.link_id = None;
        assert!(next.validate().is_err());
        next.batch = Some(BatchRef {
            batch_id: crate::ids::BulkBatchId::new(),
            row: 1,
        });
        next.validate().unwrap();
        next.link_id = command.link_id;
        assert!(next.validate().is_err());
    }
    #[test]
    fn uncertainty_cannot_be_erased_by_absence_or_promotion() {
        let snapshot = AccessGrantSnapshot {
            command: grant(repo()),
            revision: 1,
            outcome: RemovalOutcome::OutcomeUnknown,
        };
        assert_eq!(snapshot.barrier(), RemovalBarrier::MayHaveWritten);
        for outcome in [
            RemovalOutcome::Scheduled,
            RemovalOutcome::Blocked {
                reason: RemovalBlocked::ReadUnavailable,
            },
            RemovalOutcome::RemovalSkipped {
                reason: RemovalSkip::AlreadyAbsent,
                at: DateTime::from_timestamp(1001, 0).unwrap(),
            },
        ] {
            let mut next = snapshot.clone();
            next.revision += 1;
            next.outcome = outcome;
            assert!(snapshot.validate_successor(&next).is_err());
        }
        let mut completed = snapshot.clone();
        completed.revision += 1;
        completed.outcome = RemovalOutcome::Removed {
            reason: RemovalReason::AccessExpiry,
            at: DateTime::from_timestamp(1001, 0).unwrap(),
        };
        snapshot.validate_successor(&completed).unwrap();
        assert_eq!(completed.barrier(), RemovalBarrier::DefinitivelyFinished);
        let mut changed = completed.clone();
        changed.revision += 1;
        assert!(completed.validate_successor(&changed).is_err());
    }
    #[test]
    fn false_skip_and_changed_equal_revision_are_rejected() {
        let snapshot = AccessGrantSnapshot {
            command: grant(repo()),
            revision: 1,
            outcome: RemovalOutcome::Scheduled,
        };
        let mut next = snapshot.clone();
        next.outcome = RemovalOutcome::Blocked {
            reason: RemovalBlocked::ReadUnavailable,
        };
        assert!(snapshot.validate_successor(&next).is_err());
        next.revision += 1;
        next.outcome = RemovalOutcome::RemovalSkipped {
            reason: RemovalSkip::RoleChanged {
                now: CurrentRole::Repository {
                    role: EffectiveRole::Read,
                },
            },
            at: DateTime::from_timestamp(1001, 0).unwrap(),
        };
        assert!(next.validate().is_err());
    }
}
