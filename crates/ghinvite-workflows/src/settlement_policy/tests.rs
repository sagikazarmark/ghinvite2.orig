use super::*;
use ghinvite_core::{
    GithubInvitationId, InvitationLinkId, Permission, RequestId,
    delivery::{CreateCommand, CreateReceipt},
};

pub(crate) fn snapshot(outcome: CreateOutcome) -> DeliverySnapshot {
    CreateReceipt {
        audit_version: Default::default(),
        command: CreateCommand {
            batch: None,
            budget_attribution: Default::default(),
            access_lifetime_seconds: None,
            budget_share_pct: 50,
            invitation_id: Some(GithubInvitationId::new()),
            delivery_generation: 1,
            link_id: Some(InvitationLinkId::new()),
            request_id: RequestId::new(),
            approval_id: "approval".into(),
            account_id: 100,
            installation_id: 9,
            requester_id: 8,
            repo_id: 10,
            repo_full_name: "acme/api".into(),
            permission: Permission::Push,
            approved_at: "2026-05-04T12:00:00Z".parse().unwrap(),
        },
        outcome,
        revision: 3,
        confirmed_at: None,
    }
    .into()
}
fn decision(snapshot: &DeliverySnapshot, evidence: Evidence, direct: bool) -> ReconcileDecision {
    evaluate(
        snapshot,
        &Observation::new(snapshot, snapshot.create.command.approved_at, evidence),
        AcceptancePolicy {
            require_direct_collaborator: direct,
        },
    )
}
fn absent(role: Option<EffectiveRole>, direct: Option<bool>, available: bool) -> Evidence {
    Evidence::Absent {
        availability_verified: available,
        role_name: role,
        direct_collaborator: direct,
        membership: None,
    }
}
fn state(decision: ReconcileDecision) -> Option<InvitationState> {
    match decision {
        ReconcileDecision::Settle { state, .. } => Some(state),
        _ => None,
    }
}

#[test]
fn requested_role_and_direct_requirement_table() {
    let snapshot = snapshot(CreateOutcome::Created { upstream_id: 99 });
    for role in [
        EffectiveRole::None,
        EffectiveRole::BaseRole,
        EffectiveRole::Read,
        EffectiveRole::Triage,
        EffectiveRole::Write,
        EffectiveRole::Maintain,
        EffectiveRole::Admin,
        EffectiveRole::Custom(ghinvite_core::targets::CustomRoleName::new("reviewer").unwrap()),
    ] {
        for flag in [false, true] {
            for direct in [None, Some(false), Some(true)] {
                let expected = if flag && direct.is_none() {
                    None
                } else if role.satisfies(RepoRole::Write) && (!flag || direct == Some(true)) {
                    Some(InvitationState::Accepted)
                } else {
                    Some(InvitationState::Declined)
                };
                assert_eq!(
                    state(decision(
                        &snapshot,
                        absent(Some(role.clone()), direct, true),
                        flag
                    )),
                    expected,
                    "{role:?}, flag={flag}, direct={direct:?}"
                );
            }
        }
    }
    assert!(!AcceptancePolicy::default().require_direct_collaborator);
}

#[test]
fn base_read_does_not_satisfy_write_ac_acc_2() {
    let snapshot = snapshot(CreateOutcome::Created { upstream_id: 99 });
    assert_eq!(
        state(decision(
            &snapshot,
            absent(Some(EffectiveRole::Read), None, true),
            false
        )),
        Some(InvitationState::Declined)
    );
}

#[test]
fn incomplete_or_unavailable_observations_cannot_settle_ac_acc_4() {
    let snapshot = snapshot(CreateOutcome::Created { upstream_id: 99 });
    for evidence in [
        absent(Some(EffectiveRole::Admin), Some(true), false),
        absent(None, Some(true), true),
    ] {
        assert_eq!(
            decision(&snapshot, evidence, false),
            ReconcileDecision::NoChange
        );
    }
}

#[test]
fn uncertain_creation_requires_positive_invitee_presence() {
    let snapshot = snapshot(CreateOutcome::OutcomeUnknown);
    for evidence in [
        absent(Some(EffectiveRole::Admin), Some(true), true),
        Evidence::Listed {
            upstream_id: 99,
            expired: false,
        },
        Evidence::FailedInvitation {
            upstream_id: 99,
            reason: FailedReason::Expired,
        },
        Evidence::FoundByInvitee {
            upstream_id: 0,
            permission: EffectiveRole::Write,
        },
    ] {
        assert_eq!(
            decision(&snapshot, evidence, false),
            ReconcileDecision::NoChange
        );
    }
    assert_eq!(
        decision(
            &snapshot,
            Evidence::FoundByInvitee {
                upstream_id: 99,
                permission: EffectiveRole::Write
            },
            false
        ),
        ReconcileDecision::ResolveCreated { upstream_id: 99 }
    );
}

#[test]
fn known_identity_expiry_and_foreign_invitations_share_settlement_rules() {
    for outcome in [
        CreateOutcome::Created { upstream_id: 99 },
        CreateOutcome::AlreadyPending {
            upstream_id: 99,
            permission: RepoRole::Read,
        },
    ] {
        let snapshot = snapshot(outcome);
        for id in [98, 99] {
            for expired in [false, true] {
                assert_eq!(
                    state(decision(
                        &snapshot,
                        Evidence::Listed {
                            upstream_id: id,
                            expired
                        },
                        false
                    )),
                    if id == 99 && expired {
                        Some(InvitationState::Expired)
                    } else {
                        None
                    }
                );
            }
            assert_eq!(
                state(decision(
                    &snapshot,
                    Evidence::FailedInvitation {
                        upstream_id: id,
                        reason: FailedReason::Expired
                    },
                    false
                )),
                if id == 99 {
                    Some(InvitationState::Expired)
                } else {
                    None
                }
            );
        }
        assert_eq!(
            decision(
                &snapshot,
                Evidence::FailedInvitation {
                    upstream_id: 99,
                    reason: FailedReason::Other
                },
                false
            ),
            ReconcileDecision::NoChange
        );
        assert_eq!(
            decision(
                &snapshot,
                Evidence::FoundByInvitee {
                    upstream_id: 100,
                    permission: EffectiveRole::Admin
                },
                false
            ),
            ReconcileDecision::NoChange
        );
        assert_eq!(
            state(decision(
                &snapshot,
                absent(Some(EffectiveRole::Write), None, true),
                false
            )),
            Some(InvitationState::Accepted)
        );
    }
}

#[test]
fn foreign_pending_never_authorizes_delete() {
    assert!(can_withdraw(&snapshot(CreateOutcome::Created {
        upstream_id: 99
    })));
    for outcome in [
        CreateOutcome::AlreadyPending {
            upstream_id: 99,
            permission: RepoRole::Write,
        },
        CreateOutcome::OutcomeUnknown,
        CreateOutcome::Created { upstream_id: 0 },
        CreateOutcome::Planned,
    ] {
        assert!(!can_withdraw(&snapshot(outcome)));
    }
}

#[test]
fn stale_revision_generation_and_time_cannot_win() {
    let snapshot = snapshot(CreateOutcome::Created { upstream_id: 99 });
    let observation = Observation::new(
        &snapshot,
        snapshot.create.command.approved_at,
        absent(Some(EffectiveRole::Admin), None, true),
    );
    let mut later_revision = snapshot.clone();
    later_revision.revision += 1;
    let mut reissued = snapshot.clone();
    reissued.create.command.delivery_generation += 1;
    for changed in [later_revision, reissued] {
        assert_eq!(
            evaluate(&changed, &observation, AcceptancePolicy::default()),
            ReconcileDecision::NoChange
        );
    }
    let mut old = observation.clone();
    old.at -= chrono::Duration::seconds(1);
    assert_eq!(
        evaluate(&snapshot, &old, AcceptancePolicy::default()),
        ReconcileDecision::NoChange
    );
    let encoded = serde_json::to_vec(&observation).unwrap();
    assert_eq!(
        serde_json::from_slice::<Observation>(&encoded).unwrap(),
        observation
    );
}

#[test]
fn accepted_records_whether_direct_evidence_was_required() {
    let snapshot = snapshot(CreateOutcome::Created { upstream_id: 99 });
    for direct in [false, true] {
        let result = decision(
            &snapshot,
            absent(Some(EffectiveRole::Write), Some(true), true),
            direct,
        );
        let expected = if direct {
            Provenance::DirectCollaborator {
                role_name: EffectiveRole::Write,
            }
        } else {
            Provenance::EffectiveRole {
                role_name: EffectiveRole::Write,
            }
        };
        assert_eq!(
            result,
            ReconcileDecision::Settle {
                state: InvitationState::Accepted,
                provenance: expected
            }
        );
    }
}

#[test]
fn stopping_foreign_tracking_does_not_create_an_upstream_settlement() {
    let mut snapshot = snapshot(CreateOutcome::AlreadyPending {
        upstream_id: 99,
        permission: RepoRole::Write,
    });
    snapshot.tracking_stopped = Some(ghinvite_core::delivery::SettlementReason::LinkRevoked);
    assert_eq!(
        decision(
            &snapshot,
            absent(Some(EffectiveRole::Write), None, true),
            false
        ),
        ReconcileDecision::NoChange
    );
    assert!(!can_withdraw(&snapshot));
    assert!(snapshot.settlement.is_none());
    assert!(snapshot.create.outcome.tracked_foreign());
}
