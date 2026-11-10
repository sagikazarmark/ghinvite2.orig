use super::*;
use crate::{org_delivery::*, storage::org_delivery_reads::OrgDeliveryQuery};

pub async fn scenario_organization_delivery_reads<S: Storage + ProjectionStorage>(s: S) {
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
    link.targets.repos[0].repo_id = 9001;
    link.targets.teams = vec![crate::targets::TeamTarget {
        team_id: 5,
        team_slug: "devs".into(),
        grant: crate::targets::MemberGrant::Member,
    }];
    let request = sample_request(link.id, 701);
    seed(&s, &link, std::slice::from_ref(&request), 1).await;
    let scope = OrgDeliveryQuery {
        account_id: 9001,
        link_id: link.id,
        request_id: request.id,
    };
    assert!(
        s.list_organization_deliveries(&scope)
            .await
            .unwrap()
            .is_empty(),
        "missing projection is not a successful delivery"
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
        approved_at: dt("2026-05-04T13:00:00Z"),
        budget_share_pct: 50,
    };
    let mut blocked = OrganizationDeliverySnapshot::planned(command.clone()).unwrap();
    blocked.create.outcome = OrganizationCreateOutcome::Blocked {
        reason: OrganizationBlockedReason::InstallationUnavailable,
    };
    blocked.teams.get_mut(&5).unwrap().create.outcome = TeamCreateOutcome::Blocked {
        reason: OrganizationBlockedReason::InstallationUnavailable,
    };
    s.project_organization_delivery(&blocked).await.unwrap();
    let mut unknown = blocked.clone();
    unknown.revision = 2;
    unknown.create.revision = 2;
    unknown.create.outcome = OrganizationCreateOutcome::OutcomeUnknown;
    let team = unknown.teams.get_mut(&5).unwrap();
    team.revision = 2;
    team.create.revision = 2;
    team.create.outcome = TeamCreateOutcome::OutcomeUnknown;
    assert_eq!(
        s.list_organization_deliveries(&scope).await.unwrap(),
        vec![blocked.clone()],
        "an unprojected update cannot replace the stored observation"
    );
    s.project_organization_delivery(&unknown).await.unwrap();
    s.project_organization_delivery(&blocked).await.unwrap();
    assert_eq!(
        s.list_organization_deliveries(&scope).await.unwrap(),
        vec![unknown.clone()],
        "delayed older projection cannot erase uncertainty"
    );
    let mut foreign_command = command.clone();
    foreign_command.invitation_id = Some(GithubInvitationId::new());
    foreign_command.delivery_generation = 3;
    let mut foreign = OrganizationDeliverySnapshot::planned(foreign_command).unwrap();
    foreign.create.outcome = OrganizationCreateOutcome::AlreadyPending {
        upstream_id: 991,
        role: OrganizationInvitationRole::DirectMember,
    };
    foreign.create.confirmed_at = Some(command.approved_at);
    foreign.tracking_stopped = Some(crate::delivery::SettlementReason::LinkRevoked);
    s.project_organization_delivery(&foreign).await.unwrap();
    let mut member_command = command.clone();
    member_command.invitation_id = Some(GithubInvitationId::new());
    member_command.delivery_generation = 2;
    let mut member = OrganizationDeliverySnapshot::planned(member_command).unwrap();
    member.create.outcome = OrganizationCreateOutcome::AlreadySatisfied {
        role: OrganizationRole::Member,
    };
    member.create.confirmed_at = Some(command.approved_at);
    let team = member.teams.get_mut(&5).unwrap();
    team.create.outcome = TeamCreateOutcome::Created {
        via: TeamCreation::PendingMembership,
    };
    team.create.confirmed_at = Some(command.approved_at);
    s.project_organization_delivery(&member).await.unwrap();
    let expected = vec![unknown, member, foreign];
    assert_eq!(
        s.list_organization_deliveries(&scope).await.unwrap(),
        expected,
        "numeric generation order is independent of projection arrival and team pending has no fabricated upstream ID"
    );
    let repo:crate::delivery::CreateCommand=serde_json::from_value(serde_json::json!({"invitation_id":GithubInvitationId::new(),"link_id":link.id,"request_id":request.id,"approval_id":"approval","account_id":9001,"installation_id":1,"requester_id":701,"repo_id":9001,"repo_full_name":"acme/api","permission":"pull","approved_at":command.approved_at})).unwrap();
    let snapshot: crate::delivery::DeliverySnapshot = crate::delivery::CreateReceipt {
        audit_version: crate::delivery_audit::DeliveryAuditVersion::Legacy,
        command: repo,
        outcome: crate::delivery::CreateOutcome::Created { upstream_id: 990 },
        revision: 2,
        confirmed_at: Some(command.approved_at),
    }
    .into();
    s.project_delivery(&snapshot).await.unwrap();
    assert_eq!(
        s.list_organization_deliveries(&scope).await.unwrap(),
        expected,
        "same numeric repo/org target must not cross kinds"
    );
    for wrong in [
        OrgDeliveryQuery {
            account_id: 9002,
            ..scope
        },
        OrgDeliveryQuery {
            link_id: InvitationLinkId::new(),
            ..scope
        },
        OrgDeliveryQuery {
            request_id: RequestId::new(),
            ..scope
        },
        OrgDeliveryQuery {
            account_id: 0,
            ..scope
        },
    ] {
        assert!(
            s.list_organization_deliveries(&wrong)
                .await
                .unwrap()
                .is_empty()
        );
    }
    let other_link = sample_link(9001, 1, 701);
    let other_request = sample_request(other_link.id, 701);
    seed(&s, &other_link, std::slice::from_ref(&other_request), 1).await;
    assert!(
        s.list_organization_deliveries(&OrgDeliveryQuery {
            link_id: other_link.id,
            ..scope
        })
        .await
        .unwrap()
        .is_empty(),
        "existing authorized link cannot read another link's request"
    );
    assert!(
        s.list_organization_deliveries(&OrgDeliveryQuery {
            request_id: other_request.id,
            ..scope
        })
        .await
        .unwrap()
        .is_empty()
    );
    assert!(
        s.list_organization_deliveries(&OrgDeliveryQuery {
            account_id: super::super::integer::MAX_EXACT + 1,
            ..scope
        })
        .await
        .is_err()
    );
}
