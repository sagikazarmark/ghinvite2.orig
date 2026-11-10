//! Advisory, per-target bulk review evidence; never write authorization.
use crate::{
    bulk::Refusal,
    targets::{EffectiveRole, Targets},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PreviewTarget {
    Repository { repo_id: u64 },
    Organization { org_id: u64 },
    Team { team_id: u64 },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MembershipState {
    Active,
    Pending,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MembershipRole {
    Member,
    Admin,
    Maintainer,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObservedAccess {
    None,
    Repository {
        role: EffectiveRole,
    },
    Membership {
        state: MembershipState,
        role: MembershipRole,
    },
    Unavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PreviewAction {
    Satisfied,
    WillInvite,
    WillRaise,
    CustomRole,
    Pending,
    Unavailable,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TargetPreview {
    pub target: PreviewTarget,
    pub observed: ObservedAccess,
    pub action: PreviewAction,
}
impl TargetPreview {
    pub fn complete(previews: &[Self], targets: &Targets) -> bool {
        let expected: BTreeSet<_> = targets
            .repos
            .iter()
            .map(|r| PreviewTarget::Repository { repo_id: r.repo_id })
            .chain(
                targets
                    .organization
                    .iter()
                    .map(|o| PreviewTarget::Organization { org_id: o.org_id }),
            )
            .chain(
                targets
                    .teams
                    .iter()
                    .map(|t| PreviewTarget::Team { team_id: t.team_id }),
            )
            .collect();
        let actual: BTreeSet<_> = previews.iter().map(|p| p.target.clone()).collect();
        actual == expected
            && actual.len() == previews.len()
            && previews.iter().all(|p| {
                p.action != PreviewAction::Unavailable && p.observed != ObservedAccess::Unavailable
            })
    }
    pub fn validate(&self, targets: &Targets) -> Result<(), Refusal> {
        let expected = match (&self.target, &self.observed) {
            (PreviewTarget::Repository { repo_id }, ObservedAccess::Repository { role }) => {
                let target = targets
                    .repos
                    .iter()
                    .find(|r| r.repo_id == *repo_id)
                    .ok_or(Refusal::InvalidReview)?;
                if matches!(role, EffectiveRole::Custom(_)) {
                    PreviewAction::CustomRole
                } else if role.satisfies(target.grant) {
                    PreviewAction::Satisfied
                } else if *role == EffectiveRole::None {
                    PreviewAction::WillInvite
                } else {
                    PreviewAction::WillRaise
                }
            }
            (
                PreviewTarget::Organization { org_id },
                ObservedAccess::Membership { state, role },
            ) if targets
                .organization
                .as_ref()
                .is_some_and(|o| o.org_id == *org_id)
                && *role != MembershipRole::Maintainer =>
            {
                if *state == MembershipState::Active {
                    PreviewAction::Satisfied
                } else {
                    PreviewAction::Pending
                }
            }
            (PreviewTarget::Team { team_id }, ObservedAccess::Membership { state, role })
                if targets.teams.iter().any(|t| t.team_id == *team_id)
                    && *role != MembershipRole::Admin =>
            {
                if *state == MembershipState::Active {
                    PreviewAction::Satisfied
                } else {
                    PreviewAction::Pending
                }
            }
            (_, ObservedAccess::None) => PreviewAction::WillInvite,
            (_, ObservedAccess::Unavailable) => PreviewAction::Unavailable,
            _ => return Err(Refusal::InvalidReview),
        };
        if expected == self.action {
            Ok(())
        } else {
            Err(Refusal::InvalidReview)
        }
    }
}
