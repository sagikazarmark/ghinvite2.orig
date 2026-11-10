//! Operation-bound administrator reissue eligibility and current-generation epochs.
//!
//! Callers must resolve the original approval, current delivery and removal
//! barrier from retained owners. Projected SQL rows cannot authorize a reissue.
use crate::{
    InvitationState, RequestId, RequestState, admission::AdmissionOperationId,
    delivery::FinalResult, invitation_link::AccountAdmin,
};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Budget attribution is independent of provenance and link-control identity.
/// A reissue retains its original link while drawing on the unshared budget.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BudgetAttribution {
    #[default]
    SourceLink,
    AdministratorReissue,
}
impl BudgetAttribution {
    pub const fn is_source_link(&self) -> bool {
        matches!(self, Self::SourceLink)
    }
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReissueTarget {
    Repository { repo_id: u64 },
    Organization { org_id: u64 },
}
impl ReissueTarget {
    pub const fn id(self) -> u64 {
        match self {
            Self::Repository { repo_id } => repo_id,
            Self::Organization { org_id } => org_id,
        }
    }
    pub fn validate(self) -> Result<(), &'static str> {
        if self.id() == 0 || self.id() > crate::storage::integer::MAX_EXACT {
            Err("invalid reissue target")
        } else {
            Ok(())
        }
    }
    pub fn key(self) -> String {
        match self {
            Self::Repository { repo_id } => format!("repo:{repo_id}"),
            Self::Organization { org_id } => format!("org:{org_id}"),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReissueCommand {
    pub operation_id: AdmissionOperationId,
    pub request_id: RequestId,
    pub admin: AccountAdmin,
    pub target: ReissueTarget,
}
impl ReissueCommand {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.target.validate()?;
        if self.admin.account_id == 0
            || self.admin.user_id == 0
            || self.admin.account_id > crate::storage::integer::MAX_EXACT
            || self.admin.user_id > crate::storage::integer::MAX_EXACT
        {
            return Err("invalid reissue actor");
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Reissued {
    pub delivery_generation: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReissueRefusal {
    NotReissuable { final_result: Option<FinalResult> },
    BudgetClosed { until: DateTime<Utc> },
}
pub type ReissueOutcome = Result<Reissued, ReissueRefusal>;

/// A barrier returned by the old delivery owner after its cancellation work is
/// drained. Absence alone never establishes that an uncertain DELETE finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InvitationBarrier {
    Clear,
    Pending,
    MayHaveWritten,
}

/// Exact old immutable command, never a SQL-derived approximation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepositoryReissueBarrierQuery {
    pub operation_id: AdmissionOperationId,
    pub command: crate::delivery::CreateCommand,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OrganizationReissueBarrierQuery {
    pub operation_id: AdmissionOperationId,
    pub command: crate::org_delivery::OrganizationCreateCommand,
}

/// Verified owner evidence for a single current approved target. Organization
/// selection advances the organization and all attached team steps together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReissueEvidence {
    pub request_state: RequestState,
    pub current_generation: u64,
    pub settlement: Option<InvitationState>,
    pub final_result: Option<FinalResult>,
    pub invitation: InvitationBarrier,
    pub removal: crate::access_grant::RemovalBarrier,
    pub budget_closed_until: Option<DateTime<Utc>>,
}
impl ReissueEvidence {
    /// Identity and actor authorization are checked before this pure decision.
    /// This creates no receipt, budget reservation, use or delivery by itself.
    pub fn decide(self, now: DateTime<Utc>) -> ReissueOutcome {
        let refusal = ReissueRefusal::NotReissuable {
            final_result: self.final_result,
        };
        if !matches!(
            self.request_state,
            RequestState::Approved | RequestState::Lapsed
        ) || !matches!(
            self.settlement,
            Some(InvitationState::Expired | InvitationState::Declined | InvitationState::Cancelled)
        ) || self.invitation != InvitationBarrier::Clear
            || !matches!(
                self.removal,
                crate::access_grant::RemovalBarrier::NoRemoval
                    | crate::access_grant::RemovalBarrier::DefinitivelyFinished
            )
            || self.current_generation == 0
        {
            return Err(refusal);
        }
        let next = self
            .current_generation
            .checked_add(1)
            .and_then(|v| u32::try_from(v).ok())
            .ok_or(refusal)?;
        if let Some(until) = self.budget_closed_until.filter(|until| *until > now) {
            return Err(ReissueRefusal::BudgetClosed { until });
        }
        Ok(Reissued {
            delivery_generation: next,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access_grant::RemovalBarrier;
    fn evidence() -> ReissueEvidence {
        ReissueEvidence {
            request_state: RequestState::Approved,
            current_generation: 1,
            settlement: Some(InvitationState::Expired),
            final_result: Some(FinalResult::LapsedInvite),
            invitation: InvitationBarrier::Clear,
            removal: RemovalBarrier::NoRemoval,
            budget_closed_until: None,
        }
    }
    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000, 0).unwrap()
    }
    #[test]
    fn only_explicit_terminal_invitation_states_authorize_a_new_generation() {
        for state in [RequestState::Approved, RequestState::Lapsed] {
            for settlement in [
                InvitationState::Expired,
                InvitationState::Declined,
                InvitationState::Cancelled,
            ] {
                assert_eq!(
                    ReissueEvidence {
                        request_state: state,
                        settlement: Some(settlement),
                        ..evidence()
                    }
                    .decide(now()),
                    Ok(Reissued {
                        delivery_generation: 2
                    })
                );
            }
        }
        for state in [
            RequestState::Pending,
            RequestState::Declined,
            RequestState::Expired,
            RequestState::Cancelled,
        ] {
            assert!(
                ReissueEvidence {
                    request_state: state,
                    ..evidence()
                }
                .decide(now())
                .is_err()
            );
        }
        for settlement in [
            None,
            Some(InvitationState::Accepted),
            Some(InvitationState::Sent),
            Some(InvitationState::Sending),
            Some(InvitationState::Failed),
        ] {
            assert!(
                ReissueEvidence {
                    settlement,
                    ..evidence()
                }
                .decide(now())
                .is_err()
            );
        }
    }
    #[test]
    fn pending_and_uncertain_old_effects_never_advance_generation() {
        for invitation in [
            InvitationBarrier::Pending,
            InvitationBarrier::MayHaveWritten,
        ] {
            assert!(
                ReissueEvidence {
                    invitation,
                    ..evidence()
                }
                .decide(now())
                .is_err()
            );
        }
        for removal in [RemovalBarrier::Pending, RemovalBarrier::MayHaveWritten] {
            assert!(
                ReissueEvidence {
                    removal,
                    ..evidence()
                }
                .decide(now())
                .is_err()
            );
        }
        assert!(
            ReissueEvidence {
                removal: RemovalBarrier::DefinitivelyFinished,
                ..evidence()
            }
            .decide(now())
            .is_ok()
        );
    }
    #[test]
    fn pacing_deadline_and_generation_bounds_are_exact() {
        for current_generation in [0, u32::MAX as u64, u64::MAX] {
            assert!(
                ReissueEvidence {
                    current_generation,
                    ..evidence()
                }
                .decide(now())
                .is_err()
            );
        }
        assert_eq!(
            ReissueEvidence {
                current_generation: u32::MAX as u64 - 1,
                ..evidence()
            }
            .decide(now()),
            Ok(Reissued {
                delivery_generation: u32::MAX
            })
        );
        let until = now() + chrono::Duration::milliseconds(1);
        assert_eq!(
            ReissueEvidence {
                budget_closed_until: Some(until),
                ..evidence()
            }
            .decide(now()),
            Err(ReissueRefusal::BudgetClosed { until })
        );
        for until in [now(), now() - chrono::Duration::seconds(1)] {
            assert!(
                ReissueEvidence {
                    budget_closed_until: Some(until),
                    ..evidence()
                }
                .decide(now())
                .is_ok()
            );
        }
    }
    #[test]
    fn target_kind_separates_numeric_collisions_and_team_reissue_is_not_invented() {
        assert_ne!(
            ReissueTarget::Repository { repo_id: 7 }.key(),
            ReissueTarget::Organization { org_id: 7 }.key()
        );
        assert!(ReissueTarget::Repository { repo_id: 0 }.validate().is_err());
        assert!(
            serde_json::from_value::<ReissueTarget>(serde_json::json!({"kind":"team","team_id":7}))
                .is_err()
        );
    }
}
