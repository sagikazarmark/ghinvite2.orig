use super::*;

#[tokio::test]
async fn rejected_write_uses_only_fresh_positive_numeric_policy_facts() {
    for policy in [
        Some(PolicyAfterRejection::NoSeat),
        Some(PolicyAfterRejection::WrongOrganization),
        Some(PolicyAfterRejection::Unavailable),
        None,
    ] {
        let f = Fixture::new(Scenario {
            policy_after_rejection: policy,
            writes: [422].into(),
            ..Default::default()
        })
        .await;
        let command = f.command(&[]).await;
        let result = f.create(&command).await;
        if matches!(policy, Some(PolicyAfterRejection::NoSeat)) {
            assert!(matches!(
                result.create.outcome,
                OrganizationCreateOutcome::Blocked {
                    reason: OrganizationBlockedReason::NoSeat
                }
            ));
        } else {
            assert!(matches!(
                result.create.outcome,
                OrganizationCreateOutcome::Failed {
                    reason: ghinvite_core::delivery::FailureReason::Rejected { status: 422 }
                }
            ));
        }
        assert_eq!(f.writes("POST").len(), 1);
        assert_eq!(
            standard_call(&f, "InviteBudget", "org:100", "snapshot", None).await["content_used_hour"],
            0
        );
        assert!(
            !f.storage
                .organization_attempt_exists(&OrganizationAttempt {
                    create: command.clone(),
                    action: OrganizationWrite::Invite
                })
                .await
                .unwrap()
        );
        let trace = f.github.0.lock().unwrap().trace.len();
        assert_eq!(f.create(&command).await.create, result.create);
        assert_eq!(f.github.0.lock().unwrap().trace.len(), trace);
    }
}

#[tokio::test]
async fn rejected_team_write_requires_positive_managed_team_evidence() {
    for managed in [true, false] {
        let f = Fixture::new(Scenario {
            org: Some(("member", "active")),
            policy_after_rejection: managed.then_some(PolicyAfterRejection::ManagedTeam(5)),
            writes: [403].into(),
            ..Default::default()
        })
        .await;
        let command = f.command(&[5]).await;
        let result = f.create(&command).await;
        assert!(matches!(
            result.create.outcome,
            OrganizationCreateOutcome::AlreadySatisfied { .. }
        ));
        assert_eq!(
            result.teams[&5].create.outcome,
            TeamCreateOutcome::Blocked {
                reason: if managed {
                    OrganizationBlockedReason::IdpSyncedTeam
                } else {
                    OrganizationBlockedReason::InstallationUnavailable
                }
            }
        );
        assert_eq!(f.writes("PUT").len(), 1);
        assert_eq!(
            standard_call(&f, "InviteBudget", "org:100", "snapshot", None).await["content_used_hour"],
            0
        );
        assert!(
            !f.storage
                .organization_attempt_exists(&OrganizationAttempt {
                    create: command.clone(),
                    action: OrganizationWrite::Team { team_id: 5 },
                })
                .await
                .unwrap()
        );
        if managed {
            // Positive external management remains a prerequisite on every
            // retry. A generic rejected403 has no equivalent permanent fact.
            assert_eq!(f.create(&command).await.teams, result.teams);
            assert_eq!(f.writes("PUT").len(), 1);
        }
    }
}

#[tokio::test]
async fn uncertain_write_never_becomes_a_refundable_policy_refusal() {
    let f = Fixture::new(Scenario {
        policy_after_rejection: Some(PolicyAfterRejection::NoSeat),
        writes: [502].into(),
        ..Default::default()
    })
    .await;
    let command = f.command(&[]).await;
    let result = f.create(&command).await;
    assert_eq!(
        result.create.outcome,
        OrganizationCreateOutcome::OutcomeUnknown
    );
    assert_eq!(f.writes("POST").len(), 1);
    assert_eq!(
        f.github.0.lock().unwrap().org_reads,
        2,
        "uncertain response must not trigger policy reclassification"
    );
    assert_eq!(
        standard_call(&f, "InviteBudget", "org:100", "snapshot", None).await["content_used_hour"],
        1
    );
    assert!(
        f.storage
            .organization_attempt_exists(&OrganizationAttempt {
                create: command,
                action: OrganizationWrite::Invite,
            })
            .await
            .unwrap()
    );
}
