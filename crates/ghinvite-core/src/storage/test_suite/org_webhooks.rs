use super::*;
use crate::{
    org_delivery::*,
    org_webhook::OrganizationWebhookSource,
    storage::org_webhooks::{OrganizationWebhookEvent, RETENTION_SECONDS},
};

pub async fn scenario_organization_webhook_bindings<S: Storage + ProjectionStorage>(s: S) {
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
    let request = sample_request(link.id, 701);
    seed(&s, &link, std::slice::from_ref(&request), 1).await;
    let now = dt("2026-05-04T13:00:00Z");
    let event = OrganizationWebhookEvent {
        app: crate::account::App::Org,
        account_id: 9001,
        installation_id: 1,
        requester_id: 701,
        team_id: None,
        source: OrganizationWebhookSource::OrganizationMemberAdded,
        payload_sha256: [1; 32],
    };
    assert!(
        s.bind_organization_webhook(&event, now, true)
            .await
            .unwrap()
            .is_none()
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
        approved_at: now,
        budget_share_pct: 50,
    };
    let mut projected = OrganizationDeliverySnapshot::planned(command).unwrap();
    projected.create.outcome = OrganizationCreateOutcome::Created {
        upstream_id: 990,
        created_at: now,
    };
    projected.create.confirmed_at = Some(now);
    let team = projected.teams.get_mut(&5).unwrap();
    team.create.outcome = TeamCreateOutcome::Created {
        via: TeamCreation::PendingMembership,
    };
    team.create.confirmed_at = Some(now);
    s.project_organization_delivery(&projected).await.unwrap();
    assert!(
        s.bind_organization_webhook(&event, now + chrono::Duration::seconds(1), true)
            .await
            .unwrap()
            .is_none(),
        "no-match cannot attach to a later projection"
    );
    let matched = OrganizationWebhookEvent {
        payload_sha256: [2; 32],
        ..event.clone()
    };
    let first = s
        .bind_organization_webhook(&matched, now, true)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.identity.delivery_generation, 1);
    assert_eq!(first.identity.request_id, request.id);
    assert_eq!(first.requester_id, 701);
    assert_eq!(first.received_at, now);
    let disabled = OrganizationWebhookEvent {
        payload_sha256: [3; 32],
        ..event.clone()
    };
    assert!(
        s.bind_organization_webhook(&disabled, now, false)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        s.bind_organization_webhook(&disabled, now, true)
            .await
            .unwrap()
            .is_none()
    );
    let mut generation2 = projected.clone();
    generation2.create.command.invitation_id = Some(GithubInvitationId::new());
    generation2.create.command.delivery_generation = 2;
    generation2.create.outcome = OrganizationCreateOutcome::AlreadyPending {
        upstream_id: 991,
        role: OrganizationInvitationRole::DirectMember,
    };
    s.project_organization_delivery(&generation2).await.unwrap();
    assert_eq!(
        s.bind_organization_webhook(&matched, now + chrono::Duration::seconds(5), true)
            .await
            .unwrap(),
        Some(first.clone()),
        "exact hash replay keeps generation and initial receipt time"
    );
    let fresh = OrganizationWebhookEvent {
        payload_sha256: [4; 32],
        ..event.clone()
    };
    assert_eq!(
        s.bind_organization_webhook(&fresh, now, true)
            .await
            .unwrap()
            .unwrap()
            .identity
            .delivery_generation,
        2
    );
    let team_event = OrganizationWebhookEvent {
        team_id: Some(5),
        source: OrganizationWebhookSource::TeamMembershipAdded,
        payload_sha256: [5; 32],
        ..event.clone()
    };
    let team = s
        .bind_organization_webhook(&team_event, now, true)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(team.identity.delivery_generation, 2);
    assert_eq!(team.team_id, Some(5));
    for (index, mut wrong) in [
        OrganizationWebhookEvent {
            account_id: 9002,
            ..event.clone()
        },
        OrganizationWebhookEvent {
            installation_id: 2,
            ..event.clone()
        },
        OrganizationWebhookEvent {
            requester_id: 990,
            ..event.clone()
        },
        OrganizationWebhookEvent {
            team_id: Some(6),
            ..team_event.clone()
        },
    ]
    .into_iter()
    .enumerate()
    {
        wrong.payload_sha256 = [10 + index as u8; 32];
        assert!(
            s.bind_organization_webhook(&wrong, now, true)
                .await
                .unwrap()
                .is_none()
        );
    }
    let mut wrong_app = event.clone();
    wrong_app.app = crate::account::App::Repo;
    assert!(
        s.bind_organization_webhook(&wrong_app, now, true)
            .await
            .is_err()
    );
    let mut wrong_number = event.clone();
    wrong_number.requester_id = super::super::integer::MAX_EXACT + 1;
    assert!(
        s.bind_organization_webhook(&wrong_number, now, true)
            .await
            .is_err()
    );
    let before = now + chrono::Duration::seconds(RETENTION_SECONDS - 1);
    assert!(
        s.bind_organization_webhook(&event, before, true)
            .await
            .unwrap()
            .is_none()
    );
    let expiry = now + chrono::Duration::seconds(RETENTION_SECONDS);
    let rebound = s
        .bind_organization_webhook(&event, expiry, true)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rebound.identity.delivery_generation, 2);
    assert_eq!(rebound.received_at, expiry);
    // A second request with the same pending numeric identity is ambiguous.
    let request2 = sample_request(link.id, 701);
    seed(&s, &link, &[request.clone(), request2.clone()], 2).await;
    let mut collision = projected.clone();
    collision.create.command.request_id = request2.id;
    collision.create.command.invitation_id = Some(GithubInvitationId::new());
    s.project_organization_delivery(&collision).await.unwrap();
    let ambiguous = OrganizationWebhookEvent {
        payload_sha256: [20; 32],
        ..event.clone()
    };
    assert!(
        s.bind_organization_webhook(&ambiguous, expiry, true)
            .await
            .unwrap()
            .is_none(),
        "multiple current owners require reconciliation rather than guessing"
    );
    // Terminal latest generation must hide an older still-pending generation.
    generation2.revision = 2;
    generation2.tracking_stopped = Some(crate::delivery::SettlementReason::LinkRevoked);
    let team = generation2.teams.get_mut(&5).unwrap();
    team.revision = 2;
    team.tracking_stopped = Some(crate::delivery::SettlementReason::LinkRevoked);
    s.project_organization_delivery(&generation2).await.unwrap();
    let only_other = OrganizationWebhookEvent {
        payload_sha256: [21; 32],
        ..event
    };
    let binding = s
        .bind_organization_webhook(&only_other, expiry, true)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(binding.identity.request_id, request2.id);
    assert!(
        s.bind_organization_webhook(&ambiguous, expiry, true)
            .await
            .unwrap()
            .is_none(),
        "ambiguous no-match stays frozen after ambiguity clears"
    );
}
