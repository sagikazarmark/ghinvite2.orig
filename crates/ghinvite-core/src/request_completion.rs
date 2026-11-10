//! Final target results and the approved-request truth table (P1 §5.2).
//! Callers supply retained owner snapshots and the exact approved generation set;
//! a missing result, delayed SQL row, or uncertain write is never completion.
use crate::{
    InvitationState,
    delivery::{DeliverySnapshot, FinalResult, SettlementReason},
    org_delivery::{
        OrganizationCreateOutcome, OrganizationDeliverySnapshot, TeamCreateOutcome, TeamCreation,
    },
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// A wake, not trusted settlement evidence. The request owner reads and binds
/// the named delivery to its retained approved manifest before aggregating.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeliveryNotice {
    pub kind: crate::delivery_control::DeliveryKind,
    pub target_id: u64,
    pub delivery_generation: u64,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    Repository,
    Organization,
    Team,
}

/// Kind and generation prevent equal numeric targets or an old reissue from
/// supplying another target's final result.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub struct Target {
    pub kind: TargetKind,
    pub id: u64,
    pub generation: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TargetResult {
    pub target: Target,
    pub result: FinalResult,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Completion {
    Complete,
    Partial,
    Closed,
    Lapsed,
}

/// Only the Lapsed result changes an approved request's lifecycle state.
/// A result set outside the approved manifest is an identity error, not evidence.
pub fn aggregate(
    expected: &BTreeSet<Target>,
    results: &BTreeMap<Target, FinalResult>,
) -> Result<Option<Completion>, &'static str> {
    if expected.is_empty()
        || expected.iter().any(|t| {
            t.id == 0
                || t.generation == 0
                || t.id > crate::storage::integer::MAX_EXACT
                || t.generation > crate::storage::integer::MAX_EXACT
        })
        || results.keys().any(|t| !expected.contains(t))
    {
        return Err("final results do not match approved targets");
    }
    if results.len() != expected.len() {
        return Ok(None);
    }
    let satisfied = results
        .values()
        .filter(|r| **r == FinalResult::Satisfied)
        .count();
    Ok(Some(if satisfied == expected.len() {
        Completion::Complete
    } else if satisfied > 0 {
        Completion::Partial
    } else if results.values().any(|r| *r == FinalResult::LapsedInvite) {
        Completion::Lapsed
    } else {
        Completion::Closed
    }))
}

fn settlement(
    state: InvitationState,
    reason: Option<SettlementReason>,
    foreign: bool,
) -> Option<FinalResult> {
    match state {
        InvitationState::Accepted => Some(FinalResult::Satisfied),
        InvitationState::Expired => Some(FinalResult::LapsedInvite),
        InvitationState::Declined if foreign => Some(FinalResult::LapsedInvite),
        InvitationState::Declined => Some(FinalResult::Declined),
        InvitationState::Cancelled => match reason {
            Some(SettlementReason::OwnExpiry) => Some(FinalResult::LapsedInvite),
            Some(SettlementReason::LinkRevoked | SettlementReason::AdminReissue) => {
                Some(FinalResult::Unfulfillable)
            }
            None => None,
        },
        InvitationState::Failed => Some(FinalResult::Unfulfillable),
        InvitationState::Sending | InvitationState::Sent => None,
    }
}
fn reason(event: &crate::audit::AuditEvent) -> Option<SettlementReason> {
    event
        .metadata
        .get("reason")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
}

pub fn repository(snapshot: &DeliverySnapshot) -> Option<TargetResult> {
    let outcome = &snapshot.create.outcome;
    let result = outcome.final_result().or_else(|| {
        if !outcome.needs_settlement() {
            return None;
        }
        if snapshot.tracking_stopped.is_some() {
            return Some(FinalResult::Unfulfillable);
        }
        let settled = snapshot.settlement.as_ref()?;
        settlement(
            settled.state,
            reason(&settled.event),
            outcome.tracked_foreign(),
        )
    })?;
    Some(TargetResult {
        target: Target {
            kind: TargetKind::Repository,
            id: snapshot.create.command.repo_id,
            generation: snapshot.create.command.delivery_generation,
        },
        result,
    })
}

/// Each team is a target even when it shares its organization's invitation.
/// A final org step never hides an unfinished independent team step.
pub fn organization(snapshot: &OrganizationDeliverySnapshot) -> Vec<TargetResult> {
    let mut results = Vec::new();
    let command = &snapshot.create.command;
    let org_result = match &snapshot.create.outcome {
        OrganizationCreateOutcome::AlreadySatisfied { .. } => Some(FinalResult::Satisfied),
        OrganizationCreateOutcome::Aborted { .. } | OrganizationCreateOutcome::Failed { .. } => {
            Some(FinalResult::Unfulfillable)
        }
        outcome @ (OrganizationCreateOutcome::Created { .. }
        | OrganizationCreateOutcome::AlreadyPending { .. }) => {
            if snapshot.tracking_stopped.is_some() {
                Some(FinalResult::Unfulfillable)
            } else {
                snapshot.settlement.as_ref().and_then(|s| {
                    settlement(
                        s.state,
                        reason(&s.event),
                        matches!(outcome, OrganizationCreateOutcome::AlreadyPending { .. }),
                    )
                })
            }
        }
        _ => None,
    };
    if let Some(result) = org_result {
        results.push(TargetResult {
            target: Target {
                kind: TargetKind::Organization,
                id: command.organization.org_id,
                generation: command.delivery_generation,
            },
            result,
        });
    }
    for (team_id, team) in &snapshot.teams {
        let result = match &team.create.outcome {
            // Existing active or pending membership is AlreadySatisfied (§7.3).
            // Only a newly created pending membership waits for settlement.
            TeamCreateOutcome::AlreadySatisfied { .. } | TeamCreateOutcome::Upgraded => {
                Some(FinalResult::Satisfied)
            }
            TeamCreateOutcome::Aborted { .. } | TeamCreateOutcome::Failed { .. } => {
                Some(FinalResult::Unfulfillable)
            }
            TeamCreateOutcome::Created {
                via: TeamCreation::OrganizationInvitation { upstream_id },
            } if matches!(snapshot.create.outcome, OrganizationCreateOutcome::Created { upstream_id: owned, .. } if owned == *upstream_id) => {
                org_result
            }
            TeamCreateOutcome::Created {
                via: TeamCreation::PendingMembership,
            } => {
                if team.tracking_stopped.is_some() {
                    Some(FinalResult::Unfulfillable)
                } else {
                    team.settlement
                        .as_ref()
                        .and_then(|s| settlement(s.state, None, false))
                }
            }
            _ => None,
        };
        if let Some(result) = result {
            results.push(TargetResult {
                target: Target {
                    kind: TargetKind::Team,
                    id: *team_id,
                    generation: command.delivery_generation,
                },
                result,
            });
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delivery::CreateOutcome;
    fn target(kind: TargetKind, id: u64) -> Target {
        Target {
            kind,
            id,
            generation: 1,
        }
    }
    #[test]
    fn complete_truth_table_and_missing_result_cannot_lapse() {
        use FinalResult::*;
        for a in [Satisfied, LapsedInvite, Declined, Unfulfillable] {
            for b in [Satisfied, LapsedInvite, Declined, Unfulfillable] {
                let x = target(TargetKind::Repository, 10);
                let y = target(TargetKind::Organization, 10);
                let expected = BTreeSet::from([x, y]);
                assert_eq!(aggregate(&expected, &BTreeMap::from([(x, a)])), Ok(None));
                let result = aggregate(&expected, &BTreeMap::from([(x, a), (y, b)]))
                    .unwrap()
                    .unwrap();
                match (a, b) {
                    (Satisfied, Satisfied) => assert_eq!(result, Completion::Complete),
                    (Satisfied, _) | (_, Satisfied) => assert_eq!(result, Completion::Partial),
                    (LapsedInvite, _) | (_, LapsedInvite) => assert_eq!(result, Completion::Lapsed),
                    _ => assert_eq!(result, Completion::Closed),
                }
            }
        }
    }
    #[test]
    fn old_generation_or_equal_numeric_wrong_kind_cannot_complete_manifest() {
        let expected = target(TargetKind::Repository, 10);
        for wrong in [
            target(TargetKind::Organization, 10),
            Target {
                generation: 2,
                ..expected
            },
        ] {
            assert!(
                aggregate(
                    &BTreeSet::from([expected]),
                    &BTreeMap::from([(wrong, FinalResult::Satisfied)])
                )
                .is_err()
            );
        }
        assert!(aggregate(&BTreeSet::new(), &BTreeMap::new()).is_err());
    }
    #[test]
    fn owned_decline_foreign_disappearance_and_own_expiry_have_distinct_results() {
        assert_eq!(
            settlement(InvitationState::Declined, None, false),
            Some(FinalResult::Declined)
        );
        assert_eq!(
            settlement(InvitationState::Declined, None, true),
            Some(FinalResult::LapsedInvite)
        );
        assert_eq!(
            settlement(
                InvitationState::Cancelled,
                Some(SettlementReason::OwnExpiry),
                false
            ),
            Some(FinalResult::LapsedInvite)
        );
        assert_eq!(
            settlement(
                InvitationState::Cancelled,
                Some(SettlementReason::LinkRevoked),
                false
            ),
            Some(FinalResult::Unfulfillable)
        );
        assert_eq!(settlement(InvitationState::Cancelled, None, false), None);
        assert_eq!(settlement(InvitationState::Sent, None, false), None);
        assert_eq!(
            settlement(InvitationState::Accepted, None, true),
            Some(FinalResult::Satisfied)
        );
    }
    #[test]
    fn uncertain_and_blocked_create_outcomes_have_no_final_result() {
        assert_eq!(CreateOutcome::OutcomeUnknown.final_result(), None);
        assert_eq!(
            CreateOutcome::Blocked {
                reason: crate::delivery::BlockedReason::TargetUnavailable
            }
            .final_result(),
            None
        );
    }
    fn organization_fixture() -> OrganizationDeliverySnapshot {
        use crate::{
            org_delivery::OrganizationCreateCommand,
            targets::{MemberGrant, OrganizationTarget, TeamTarget},
        };
        OrganizationDeliverySnapshot::planned(OrganizationCreateCommand {
            budget_attribution: Default::default(),
            batch: None,
            access_lifetime_seconds: None,
            invitation_id: Some(crate::GithubInvitationId::new()),
            link_id: Some(crate::InvitationLinkId::new()),
            request_id: crate::RequestId::new(),
            approval_id: "approval".into(),
            account_id: 10,
            installation_id: 2,
            requester_id: 8,
            organization: OrganizationTarget {
                org_id: 10,
                org_login: "acme".into(),
                grant: MemberGrant::Member,
            },
            teams: vec![TeamTarget {
                team_id: 5,
                team_slug: "engineering".into(),
                grant: MemberGrant::Member,
            }],
            delivery_generation: 1,
            approved_at: chrono::Utc::now(),
            budget_share_pct: 50,
        })
        .unwrap()
    }
    fn accepted(snapshot: &mut OrganizationDeliverySnapshot) {
        snapshot.settlement = Some(crate::org_delivery::OrganizationSettlement {
            state: InvitationState::Accepted,
            event: crate::audit::AuditEvent {
                id: crate::AuditEventId::new(),
                account_id: 10,
                occurred_at: chrono::Utc::now(),
                event_type: crate::audit::EventType::InvitationAccepted,
                actor_kind: crate::audit::ActorKind::Github,
                actor_id: None,
                target_kind: crate::audit::TargetKind::OrganizationDelivery,
                target_id: snapshot.create.command.storage_id(),
                metadata: serde_json::json!({}),
                request_id: None,
            },
        });
    }
    #[test]
    fn shared_owned_invitation_settles_teams_but_independent_pending_team_stays_nonfinal() {
        let mut snapshot = organization_fixture();
        snapshot.create.outcome = OrganizationCreateOutcome::Created {
            upstream_id: 99,
            created_at: chrono::Utc::now(),
        };
        snapshot.teams.get_mut(&5).unwrap().create.outcome = TeamCreateOutcome::Created {
            via: TeamCreation::OrganizationInvitation { upstream_id: 99 },
        };
        accepted(&mut snapshot);
        let results = organization(&snapshot);
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.result == FinalResult::Satisfied));
        snapshot.teams.get_mut(&5).unwrap().create.outcome = TeamCreateOutcome::Created {
            via: TeamCreation::PendingMembership,
        };
        assert_eq!(
            organization(&snapshot).len(),
            1,
            "org acceptance does not complete independent membership"
        );
        snapshot.teams.get_mut(&5).unwrap().settlement =
            Some(crate::org_delivery::TeamSettlement {
                reason: None,
                state: InvitationState::Accepted,
                observed_at: chrono::Utc::now(),
            });
        assert_eq!(organization(&snapshot).len(), 2);
    }
    #[test]
    fn existing_pending_membership_is_satisfied_and_foreign_invitation_cannot_own_a_team() {
        let mut snapshot = organization_fixture();
        snapshot.create.outcome = OrganizationCreateOutcome::AlreadySatisfied {
            role: crate::org_delivery::OrganizationRole::Member,
        };
        snapshot.teams.get_mut(&5).unwrap().create.outcome = TeamCreateOutcome::AlreadySatisfied {
            role: crate::org_delivery::TeamRole::Member,
            state: crate::org_delivery::MembershipState::Pending,
        };
        assert_eq!(
            organization(&snapshot).len(),
            2,
            "existing pending membership is a confirmed AlreadySatisfied receipt"
        );
        snapshot.create.outcome = OrganizationCreateOutcome::AlreadyPending {
            upstream_id: 99,
            role: crate::org_delivery::OrganizationInvitationRole::DirectMember,
        };
        accepted(&mut snapshot);
        snapshot.teams.get_mut(&5).unwrap().create.outcome = TeamCreateOutcome::Created {
            via: TeamCreation::OrganizationInvitation { upstream_id: 99 },
        };
        assert_eq!(
            organization(&snapshot).len(),
            1,
            "foreign invitation cannot supply a shared owned team result"
        );
    }
}
