use super::*;
use crate::org_delivery::*;
use crate::storage::{Error, delivery_attempts::CancelCommand};

pub async fn scenario_organization_delivery<S: Storage + ProjectionStorage>(s: S) {
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(1, 9001, "acme")).clone(),
    ))
    .await
    .unwrap();
    s.upsert_user(&sample_user(701, "requester")).await.unwrap();
    s.put_app_installation(&sample_org_installation(1, 9001, "acme"))
        .await
        .unwrap();
    let mut link = sample_link(9001, 1, 701);
    link.organization_installation_id = Some(1);
    link.targets.organization = Some(crate::targets::OrganizationTarget {
        org_id: 9001,
        org_login: "acme".into(),
        grant: crate::targets::MemberGrant::Member,
    });
    link.targets.teams = vec![5, 6]
        .into_iter()
        .map(|team_id| crate::targets::TeamTarget {
            team_id,
            team_slug: format!("team-{team_id}"),
            grant: crate::targets::MemberGrant::Member,
        })
        .collect();
    link.targets.repos[0].repo_id = 9001;
    let request = sample_request(link.id, 701);
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
        approved_at: dt("2026-05-04T13:00:00Z"),
        budget_share_pct: 17,
    };
    let planned = OrganizationDeliverySnapshot::planned(command.clone()).unwrap();
    assert!(matches!(
        s.project_organization_delivery(&planned).await,
        Err(Error::ProjectionDependency)
    ));
    assert!(
        s.get_organization_delivery(&command.storage_id())
            .await
            .unwrap()
            .is_none()
    );
    seed(&s, &link, std::slice::from_ref(&request), 1).await;
    s.project_organization_delivery(&planned).await.unwrap();
    assert_eq!(
        s.get_organization_delivery(&command.storage_id())
            .await
            .unwrap(),
        Some(planned.clone())
    );
    let repo:crate::delivery::CreateCommand=serde_json::from_value(serde_json::json!({"invitation_id":GithubInvitationId::new(),"link_id":link.id,"request_id":request.id,"approval_id":"approval","account_id":9001,"installation_id":1,"requester_id":701,"repo_id":9001,"repo_full_name":"acme/api","permission":"pull","approved_at":command.approved_at})).unwrap();
    assert_eq!(repo.delivery_key(), command.delivery_key());
    assert_ne!(repo.delivery_key(), command.storage_id());
    let repo_receipt: crate::delivery::DeliverySnapshot = crate::delivery::CreateReceipt {
        audit_version: crate::delivery_audit::DeliveryAuditVersion::Legacy,
        command: repo.clone(),
        outcome: crate::delivery::CreateOutcome::Created { upstream_id: 990 },
        revision: 2,
        confirmed_at: Some(command.approved_at),
    }
    .into();
    s.project_delivery(&repo_receipt).await.unwrap();
    assert_eq!(
        s.list_delivery_for_request(request.id).await.unwrap(),
        vec![repo_receipt]
    );
    assert!(
        s.get_delivery(&command.storage_id())
            .await
            .unwrap()
            .is_none()
    );

    // Each HTTP action has its own input-bound fence; repo/org cancel cannot collide.
    assert_eq!(s.claim_delivery_attempt(&repo).await.unwrap(), Some(1));
    let cancel = CancelCommand {
        create: repo,
        upstream_id: 990,
        reason: crate::delivery::SettlementReason::LinkRevoked,
    };
    assert_eq!(
        s.claim_cancellation_attempt(&cancel).await.unwrap(),
        Some(1)
    );
    let invite = OrganizationAttempt {
        create: command.clone(),
        action: OrganizationWrite::Invite,
    };
    let team = OrganizationAttempt {
        create: command.clone(),
        action: OrganizationWrite::Team { team_id: 5 },
    };
    let org_cancel = OrganizationAttempt {
        create: command.clone(),
        action: OrganizationWrite::Cancel {
            upstream_id: 990,
            reason: crate::delivery::SettlementReason::LinkRevoked,
        },
    };
    for attempt in [&invite, &team, &org_cancel] {
        assert!(!s.organization_attempt_exists(attempt).await.unwrap());
        assert_eq!(
            s.claim_organization_attempt(attempt).await.unwrap(),
            Some(1)
        );
        assert_eq!(s.claim_organization_attempt(attempt).await.unwrap(), None);
        s.reject_organization_attempt(attempt, 1).await.unwrap();
        assert_eq!(
            s.claim_organization_attempt(attempt).await.unwrap(),
            Some(2)
        );
        s.reject_organization_attempt(attempt, 1).await.unwrap();
        assert!(s.organization_attempt_exists(attempt).await.unwrap());
        let mut changed = attempt.clone();
        changed.create.budget_share_pct = 18;
        assert!(matches!(
            s.claim_organization_attempt(&changed).await,
            Err(Error::ProjectionInvariant(_))
        ));
        s.reject_organization_attempt(&changed, 2).await.unwrap();
        assert_eq!(s.claim_organization_attempt(attempt).await.unwrap(), None);
    }
    assert!(s.cancellation_attempt_exists(&cancel).await.unwrap());
    let mut wrong = team.clone();
    wrong.action = OrganizationWrite::Team { team_id: 7 };
    assert!(matches!(
        s.claim_organization_attempt(&wrong).await,
        Err(Error::ProjectionInvariant(_))
    ));
    wrong = invite.clone();
    wrong.create.delivery_generation = super::super::integer::MAX_EXACT + 1;
    assert!(s.claim_organization_attempt(&wrong).await.is_err());

    let mut created = planned.clone();
    created.revision = 2;
    created.create.revision = 2;
    created.create.confirmed_at = Some(command.approved_at);
    created.create.outcome = OrganizationCreateOutcome::Created {
        upstream_id: 990,
        created_at: command.approved_at,
    };
    for (id, team) in &mut created.teams {
        team.revision = 2;
        team.create.revision = 2;
        team.create.confirmed_at = Some(command.approved_at);
        team.create.outcome = TeamCreateOutcome::Created {
            via: if *id == 5 {
                TeamCreation::OrganizationInvitation { upstream_id: 990 }
            } else {
                TeamCreation::PendingMembership
            },
        };
    }
    for _ in 0..3 {
        s.project_organization_delivery(&created).await.unwrap();
    }
    s.project_organization_delivery(&planned).await.unwrap();
    assert_eq!(
        s.get_organization_delivery(&command.storage_id())
            .await
            .unwrap(),
        Some(created.clone())
    );
    let sent = s
        .list_audit_events(
            9001,
            Some(EventType::InvitationSent),
            super::super::AuditPosition::Latest,
        )
        .await
        .unwrap()
        .events;
    assert_eq!(
        sent.len(),
        4,
        "repo+org+two distinct team receipts exactly once"
    );
    assert_eq!(
        sent.iter()
            .filter(|e| e.target_kind == TargetKind::OrganizationDelivery)
            .count(),
        1
    );
    assert_eq!(
        sent.iter()
            .filter(|e| e.target_kind == TargetKind::TeamDelivery)
            .count(),
        2
    );
    let mut invalid = created.clone();
    invalid.revision = 3;
    invalid.teams.get_mut(&5).unwrap().create.outcome = TeamCreateOutcome::Upgraded;
    assert!(matches!(
        s.project_organization_delivery(&invalid).await,
        Err(Error::ProjectionInvariant(_))
    ));
    let mut settled = created.clone();
    settled.revision = 3;
    settled.settlement = Some(OrganizationSettlement {
        state: InvitationState::Accepted,
        event: AuditEvent {
            id: AuditEventId::new(),
            account_id: 9001,
            occurred_at: command.approved_at,
            event_type: EventType::InvitationAccepted,
            actor_kind: ActorKind::Github,
            actor_id: None,
            target_kind: TargetKind::OrganizationDelivery,
            target_id: command.storage_id(),
            metadata: serde_json::json!({}),
            request_id: Some(request.id.to_string()),
        },
    });
    for team in settled.teams.values_mut() {
        team.revision = 3;
        team.settlement = Some(TeamSettlement {
            reason: None,
            state: InvitationState::Accepted,
            observed_at: command.approved_at,
        });
    }
    let mut audit_conflict = settled.clone();
    audit_conflict.settlement.as_mut().unwrap().event.id = sent
        .iter()
        .find(|e| e.target_kind == TargetKind::OrganizationDelivery)
        .unwrap()
        .id;
    assert!(matches!(
        s.project_organization_delivery(&audit_conflict).await,
        Err(Error::ProjectionInvariant(_))
    ));
    assert_eq!(
        s.get_organization_delivery(&command.storage_id())
            .await
            .unwrap(),
        Some(created.clone()),
        "audit conflict must roll back header, receipt and team updates"
    );
    s.project_organization_delivery(&settled).await.unwrap();
    s.project_organization_delivery(&created).await.unwrap();
    assert_eq!(
        s.get_organization_delivery(&command.storage_id())
            .await
            .unwrap(),
        Some(settled.clone())
    );
    let mut reopen = settled.clone();
    reopen.revision = 4;
    reopen.settlement = None;
    assert!(matches!(
        s.project_organization_delivery(&reopen).await,
        Err(Error::ProjectionInvariant(_))
    ));

    // Foreign pending identity cannot acquire ownership or be cancelled; only local tracking stops.
    let mut foreign_command = command.clone();
    foreign_command.invitation_id = Some(GithubInvitationId::new());
    foreign_command.delivery_generation = 2;
    let mut foreign = OrganizationDeliverySnapshot::planned(foreign_command).unwrap();
    foreign.revision = 2;
    foreign.create.revision = 2;
    foreign.create.confirmed_at = Some(command.approved_at);
    foreign.create.outcome = OrganizationCreateOutcome::AlreadyPending {
        upstream_id: 991,
        role: OrganizationInvitationRole::DirectMember,
    };
    s.project_organization_delivery(&foreign).await.unwrap();
    foreign.revision = 3;
    foreign.tracking_stopped = Some(crate::delivery::SettlementReason::LinkRevoked);
    s.project_organization_delivery(&foreign).await.unwrap();
    assert_eq!(
        s.get_organization_delivery(&foreign.create.command.storage_id())
            .await
            .unwrap(),
        Some(foreign.clone())
    );
    let mut rebound = foreign.clone();
    rebound.revision = 4;
    rebound.create.command.requester_id = 702;
    assert!(matches!(
        s.project_organization_delivery(&rebound).await,
        Err(Error::ProjectionInvariant(_))
    ));
    assert_eq!(
        s.list_delivery_for_request(request.id).await.unwrap().len(),
        1
    );
}
