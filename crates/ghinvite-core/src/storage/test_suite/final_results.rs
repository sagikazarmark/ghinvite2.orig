use super::*;
use crate::{delivery::*, org_delivery::*, request_completion};

pub async fn scenario_final_result_projection<S: Storage + ProjectionStorage>(s: S) {
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(1, 9001, "acme")).clone(),
    ))
    .await
    .unwrap();
    s.put_app_installation(&sample_org_installation(1, 9001, "acme"))
        .await
        .unwrap();
    s.upsert_user(&sample_user(701, "requester")).await.unwrap();
    let mut link = sample_link(9001, 1, 701);
    link.organization_installation_id = Some(1);
    link.targets.organization = Some(crate::targets::OrganizationTarget {
        org_id: 9001,
        org_login: "acme".into(),
        grant: crate::targets::MemberGrant::Member,
    });
    link.targets.teams = vec![crate::targets::TeamTarget {
        team_id: 5,
        team_slug: "devs".into(),
        grant: crate::targets::MemberGrant::Member,
    }];
    seed(&s, &link, &[], 1).await;
    for (name, foreign, state, reason, expected) in [
        (
            "owned_declined",
            false,
            Some(InvitationState::Declined),
            None,
            Some(FinalResult::Declined),
        ),
        (
            "foreign_disappeared",
            true,
            Some(InvitationState::Declined),
            None,
            Some(FinalResult::LapsedInvite),
        ),
        (
            "own_expiry",
            false,
            Some(InvitationState::Cancelled),
            Some(SettlementReason::OwnExpiry),
            Some(FinalResult::LapsedInvite),
        ),
        (
            "expired",
            false,
            Some(InvitationState::Expired),
            None,
            Some(FinalResult::LapsedInvite),
        ),
        (
            "link_revoked",
            false,
            Some(InvitationState::Cancelled),
            Some(SettlementReason::LinkRevoked),
            Some(FinalResult::Unfulfillable),
        ),
        (
            "accepted",
            false,
            Some(InvitationState::Accepted),
            None,
            Some(FinalResult::Satisfied),
        ),
        (
            "foreign_stopped",
            true,
            None,
            Some(SettlementReason::LinkRevoked),
            Some(FinalResult::Unfulfillable),
        ),
        (
            "failed",
            false,
            None,
            None,
            Some(FinalResult::Unfulfillable),
        ),
        ("unknown", false, None, None, None),
    ] {
        let mut request = sample_request(link.id, 701);
        request.justification = Some(name.into());
        seed(&s, &link, std::slice::from_ref(&request), 1).await;
        let at = dt("2026-05-04T13:00:00Z");
        let repo_command = CreateCommand {
            budget_attribution: Default::default(),
            batch: None,
            access_lifetime_seconds: None,
            budget_share_pct: 50,
            delivery_generation: 1,
            invitation_id: Some(GithubInvitationId::new()),
            link_id: Some(link.id),
            request_id: request.id,
            approval_id: "approval".into(),
            account_id: 9001,
            installation_id: 1,
            requester_id: 701,
            repo_id: 10,
            repo_full_name: "acme/api".into(),
            permission: Permission::Pull,
            approved_at: at,
        };
        let repo_outcome = if name == "unknown" {
            CreateOutcome::OutcomeUnknown
        } else if name == "failed" {
            CreateOutcome::Failed {
                reason: FailureReason::InvalidTarget,
            }
        } else if foreign {
            CreateOutcome::AlreadyPending {
                upstream_id: 991,
                permission: crate::targets::RepoRole::Read,
            }
        } else {
            CreateOutcome::Created { upstream_id: 991 }
        };
        let mut repo: DeliverySnapshot = CreateReceipt {
            audit_version: crate::delivery_audit::DeliveryAuditVersion::Legacy,
            command: repo_command,
            outcome: repo_outcome,
            revision: 1,
            confirmed_at: (name != "unknown").then_some(at),
        }
        .into();
        let repo_initial = repo.clone();
        repo.revision = 2;
        if let Some(state) = state {
            repo.settlement = Some(Settlement {
                state,
                event: event(
                    state,
                    reason,
                    TargetKind::GithubInvitation,
                    repo.create
                        .command
                        .invitation_id
                        .expect("legacy fixture identity")
                        .to_string(),
                    request.id,
                    at,
                ),
            });
        } else if name == "foreign_stopped" {
            repo.tracking_stopped = reason;
        }
        assert_eq!(
            request_completion::repository(&repo).map(|r| r.result),
            expected,
            "{name}"
        );
        let encoded: serde_json::Value =
            serde_json::from_str(&super::super::delivery_projection::encode(&repo).unwrap())
                .unwrap();
        assert_eq!(
            encoded["final_result"],
            serde_json::json!(expected),
            "{name}"
        );
        s.project_delivery(&repo_initial).await.unwrap();
        s.project_delivery(&repo).await.unwrap();
        s.project_delivery(&repo_initial).await.unwrap();
        let read = s.list_delivery_for_request(request.id).await.unwrap();
        assert_eq!(read, vec![repo]);
        assert_eq!(
            request_completion::repository(&read[0]).map(|r| r.result),
            expected
        );

        let command = OrganizationCreateCommand {
            budget_attribution: Default::default(),
            batch: None,
            access_lifetime_seconds: None,
            invitation_id: Some(GithubInvitationId::new()),
            link_id: Some(link.id),
            request_id: request.id,
            approval_id: "approval".into(),
            account_id: 9001,
            installation_id: 1,
            requester_id: 701,
            organization: link.targets.organization.clone().unwrap(),
            teams: link.targets.teams.clone(),
            delivery_generation: 1,
            approved_at: at,
            budget_share_pct: 50,
        };
        let mut org = OrganizationDeliverySnapshot::planned(command).unwrap();
        org.create.outcome = if name == "unknown" {
            OrganizationCreateOutcome::OutcomeUnknown
        } else if name == "failed" {
            OrganizationCreateOutcome::Failed {
                reason: FailureReason::InvalidTarget,
            }
        } else if foreign {
            OrganizationCreateOutcome::AlreadyPending {
                upstream_id: 992,
                role: OrganizationInvitationRole::DirectMember,
            }
        } else {
            OrganizationCreateOutcome::Created {
                upstream_id: 992,
                created_at: at,
            }
        };
        org.create.confirmed_at = (name != "unknown").then_some(at);
        let team = org.teams.get_mut(&5).unwrap();
        team.create.outcome = if name == "unknown" {
            TeamCreateOutcome::OutcomeUnknown
        } else if name == "failed" {
            TeamCreateOutcome::Failed {
                reason: FailureReason::InvalidTarget,
            }
        } else {
            TeamCreateOutcome::Created {
                via: if foreign {
                    TeamCreation::PendingMembership
                } else {
                    TeamCreation::OrganizationInvitation { upstream_id: 992 }
                },
            }
        };
        team.create.confirmed_at = (name != "unknown").then_some(at);
        let org_initial = org.clone();
        org.revision = 2;
        if let Some(state) = state {
            org.settlement = Some(OrganizationSettlement {
                state,
                event: event(
                    state,
                    reason,
                    TargetKind::OrganizationDelivery,
                    org.create.command.storage_id(),
                    request.id,
                    at,
                ),
            });
            if !foreign {
                let team = org.teams.get_mut(&5).unwrap();
                team.revision = 2;
                team.settlement = Some(TeamSettlement {
                    reason: None,
                    state,
                    observed_at: at,
                });
            }
        } else if name == "foreign_stopped" {
            org.tracking_stopped = reason;
            let team = org.teams.get_mut(&5).unwrap();
            team.revision = 2;
            team.tracking_stopped = reason;
        }
        let results = request_completion::organization(&org);
        assert_eq!(
            results
                .iter()
                .find(|r| r.target.kind == request_completion::TargetKind::Organization)
                .map(|r| r.result),
            expected,
            "{name}"
        );
        let team_result = results
            .iter()
            .find(|r| r.target.kind == request_completion::TargetKind::Team)
            .map(|r| r.result);
        assert_eq!(
            team_result,
            if foreign && state.is_some() {
                None
            } else {
                expected
            },
            "independent foreign team does not inherit org settlement"
        );
        let encoded: serde_json::Value = serde_json::from_str(
            &super::super::org_delivery::encode_projection(&org, None)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            encoded["final_result"],
            serde_json::json!(expected),
            "{name}"
        );
        s.project_organization_delivery(&org_initial).await.unwrap();
        s.project_organization_delivery(&org).await.unwrap();
        s.project_organization_delivery(&org_initial).await.unwrap();
        let read = s
            .get_organization_delivery(&org.create.command.storage_id())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read, org);
        assert_eq!(request_completion::organization(&read), results);
    }
}
fn event(
    state: InvitationState,
    reason: Option<SettlementReason>,
    kind: TargetKind,
    target: String,
    request: RequestId,
    at: DateTime<Utc>,
) -> AuditEvent {
    AuditEvent {
        id: AuditEventId::new(),
        account_id: 9001,
        occurred_at: at,
        event_type: match state {
            InvitationState::Accepted => EventType::InvitationAccepted,
            InvitationState::Declined => EventType::InvitationDeclined,
            InvitationState::Expired => EventType::InvitationExpired,
            InvitationState::Cancelled => EventType::InvitationCancelled,
            _ => unreachable!(),
        },
        actor_kind: ActorKind::Github,
        actor_id: None,
        target_kind: kind,
        target_id: target,
        metadata: reason
            .map(|reason| serde_json::json!({"reason":reason}))
            .unwrap_or(serde_json::json!({})),
        request_id: Some(request.to_string()),
    }
}
