use super::*;
use crate::{
    delivery::{CreateOutcome, CreateReceipt, DeliverySnapshot},
    delivery_audit::*,
    org_delivery::{
        OrganizationCreateCommand, OrganizationCreateOutcome, OrganizationDeliverySnapshot,
    },
    storage::{AuditPosition, Error},
};

pub async fn scenario_delivery_transition_audit<S: Storage + ProjectionStorage>(s: S) {
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
        team_slug: "team-5".into(),
        grant: crate::targets::MemberGrant::Member,
    }];
    link.targets.repos[0].repo_id = 9001;
    let request = sample_request(link.id, 701);
    seed(&s, &link, std::slice::from_ref(&request), 1).await;
    let approved_at = dt("2026-05-04T13:00:00Z");
    let command=serde_json::from_value(serde_json::json!({"invitation_id":GithubInvitationId::new(),"link_id":link.id,"request_id":request.id,"approval_id":"approval","account_id":9001,"installation_id":1,"requester_id":701,"repo_id":9001,"repo_full_name":"acme/api","permission":"pull","approved_at":approved_at})).unwrap();
    let latest: DeliverySnapshot = CreateReceipt {
        audit_version: crate::delivery_audit::DeliveryAuditVersion::Legacy,
        command,
        outcome: CreateOutcome::OutcomeUnknown,
        revision: 10,
        confirmed_at: None,
    }
    .into();
    s.project_delivery(&latest).await.unwrap();
    let fact = |step, revision, kind| DeliveryAuditFact {
        step,
        transition_revision: revision,
        at: approved_at + chrono::Duration::seconds(revision as i64),
        kind,
    };
    let mut old = latest.clone();
    old.revision = 5;
    old.create.revision = 5;
    let mut delayed = RepositoryDeliveryProjection {
        snapshot: old,
        audit_events: vec![
            fact(
                DeliveryAuditStep::Repository,
                2,
                DeliveryAuditKind::Blocked {
                    reason: DeliveryBlockedReason::Repository(
                        crate::delivery::BlockedReason::InstallationUnavailable,
                    ),
                },
            ),
            fact(
                DeliveryAuditStep::Repository,
                3,
                DeliveryAuditKind::Held { hold_epoch: 7 },
            ),
            fact(
                DeliveryAuditStep::Repository,
                4,
                DeliveryAuditKind::Released { hold_epoch: 7 },
            ),
            fact(
                DeliveryAuditStep::Repository,
                5,
                DeliveryAuditKind::OutcomeUnknown,
            ),
        ],
    };
    for _ in 0..2 {
        s.project_delivery_with_events(&delayed).await.unwrap();
    }
    assert_eq!(
        s.list_delivery_for_request(request.id).await.unwrap(),
        vec![latest.clone()]
    );
    for kind in [
        EventType::DeliveryBlocked,
        EventType::DeliveryHeld,
        EventType::DeliveryReleased,
        EventType::DeliveryOutcomeUnknown,
    ] {
        let events = s
            .list_audit_events(9001, Some(kind), AuditPosition::Latest)
            .await
            .unwrap()
            .events;
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].target_id,
            latest
                .create
                .command
                .invitation_id
                .expect("legacy fixture identity")
                .to_string()
        );
        assert_eq!(events[0].request_id, Some(request.id.to_string()));
    }
    let original = delayed.events().unwrap();
    delayed.audit_events[0].at += chrono::Duration::seconds(1);
    assert!(matches!(
        s.project_delivery_with_events(&delayed).await,
        Err(Error::ProjectionInvariant(_))
    ));
    delayed.audit_events[0].at -= chrono::Duration::seconds(1);
    // A fresh event preceding a conflicting historical fact and newer snapshot
    // must all roll back, including the newly inserted event.
    let mut advance = delayed.clone();
    advance.snapshot.revision = 11;
    advance.snapshot.create.revision = 11;
    advance.audit_events = vec![
        fact(
            DeliveryAuditStep::Repository,
            11,
            DeliveryAuditKind::Blocked {
                reason: DeliveryBlockedReason::Repository(
                    crate::delivery::BlockedReason::MintForbidden,
                ),
            },
        ),
        delayed.audit_events[0].clone(),
    ];
    advance.audit_events[1].kind = DeliveryAuditKind::Blocked {
        reason: DeliveryBlockedReason::Repository(
            crate::delivery::BlockedReason::TargetUnavailable,
        ),
    };
    assert!(matches!(
        s.project_delivery_with_events(&advance).await,
        Err(Error::ProjectionInvariant(_))
    ));
    assert_eq!(
        s.list_delivery_for_request(request.id).await.unwrap(),
        vec![latest.clone()]
    );
    assert_eq!(
        s.list_audit_events(
            9001,
            Some(EventType::DeliveryBlocked),
            AuditPosition::Latest
        )
        .await
        .unwrap()
        .events,
        vec![original[0].clone()]
    );
    let mut invalid = delayed.clone();
    invalid.audit_events[0].step = DeliveryAuditStep::Organization;
    assert!(matches!(
        s.project_delivery_with_events(&invalid).await,
        Err(Error::ProjectionInvariant(_))
    ));
    invalid = delayed.clone();
    invalid.snapshot.create.command.account_id = 9002;
    assert!(matches!(
        s.project_delivery_with_events(&invalid).await,
        Err(Error::ProjectionInvariant(_))
    ));

    let org_command = OrganizationCreateCommand {
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
        approved_at,
        budget_share_pct: 50,
    };
    assert_eq!(
        latest.create.command.delivery_key(),
        org_command.delivery_key()
    );
    let mut org_latest = OrganizationDeliverySnapshot::planned(org_command).unwrap();
    org_latest.revision = 10;
    org_latest.create.revision = 10;
    org_latest.create.outcome = OrganizationCreateOutcome::OutcomeUnknown;
    s.project_organization_delivery(&org_latest).await.unwrap();
    let mut org_old = org_latest.clone();
    org_old.revision = 5;
    org_old.create.revision = 5;
    let org_delayed = OrganizationDeliveryProjection {
        snapshot: org_old,
        audit_events: vec![
            fact(
                DeliveryAuditStep::Organization,
                5,
                DeliveryAuditKind::OutcomeUnknown,
            ),
            fact(
                DeliveryAuditStep::Team { team_id: 5 },
                3,
                DeliveryAuditKind::Held { hold_epoch: 7 },
            ),
            fact(
                DeliveryAuditStep::Team { team_id: 5 },
                4,
                DeliveryAuditKind::Released { hold_epoch: 7 },
            ),
            fact(
                DeliveryAuditStep::Team { team_id: 5 },
                5,
                DeliveryAuditKind::Blocked {
                    reason: DeliveryBlockedReason::Organization(
                        crate::org_delivery::OrganizationBlockedReason::IdpSyncedTeam,
                    ),
                },
            ),
        ],
    };
    for _ in 0..2 {
        s.project_organization_delivery_with_events(&org_delayed)
            .await
            .unwrap();
    }
    assert_eq!(
        s.get_organization_delivery(&org_latest.create.command.storage_id())
            .await
            .unwrap(),
        Some(org_latest.clone())
    );
    let unknown = s
        .list_audit_events(
            9001,
            Some(EventType::DeliveryOutcomeUnknown),
            AuditPosition::Latest,
        )
        .await
        .unwrap()
        .events;
    assert_eq!(
        unknown.len(),
        2,
        "repo/org same numeric target must not collide"
    );
    assert_ne!(unknown[0].id, unknown[1].id);
    for kind in [
        EventType::DeliveryHeld,
        EventType::DeliveryReleased,
        EventType::DeliveryBlocked,
    ] {
        let events = s
            .list_audit_events(9001, Some(kind), AuditPosition::Latest)
            .await
            .unwrap()
            .events;
        assert_eq!(events.len(), 2);
        assert!(
            events
                .iter()
                .any(|event| event.target_kind == TargetKind::TeamDelivery
                    && event.target_id
                        == format!("{}/team:5", org_latest.create.command.storage_id()))
        );
    }
    let mut org_advance = org_delayed.clone();
    org_advance.snapshot = org_latest.clone();
    org_advance.snapshot.revision = 11;
    org_advance.snapshot.create.revision = 11;
    org_advance.audit_events = vec![
        fact(
            DeliveryAuditStep::Organization,
            11,
            DeliveryAuditKind::Held { hold_epoch: 8 },
        ),
        org_delayed.audit_events[0].clone(),
    ];
    org_advance.audit_events[1].at += chrono::Duration::nanoseconds(1);
    assert!(matches!(
        s.project_organization_delivery_with_events(&org_advance)
            .await,
        Err(Error::ProjectionInvariant(_))
    ));
    assert_eq!(
        s.get_organization_delivery(&org_latest.create.command.storage_id())
            .await
            .unwrap(),
        Some(org_latest.clone())
    );
    assert_eq!(
        s.list_audit_events(9001, Some(EventType::DeliveryHeld), AuditPosition::Latest)
            .await
            .unwrap()
            .events
            .len(),
        2
    );
    for fact in [
        fact(
            DeliveryAuditStep::Repository,
            5,
            DeliveryAuditKind::OutcomeUnknown,
        ),
        fact(
            DeliveryAuditStep::Team { team_id: 6 },
            5,
            DeliveryAuditKind::OutcomeUnknown,
        ),
        fact(
            DeliveryAuditStep::Organization,
            5,
            DeliveryAuditKind::Blocked {
                reason: DeliveryBlockedReason::Repository(
                    crate::delivery::BlockedReason::MintForbidden,
                ),
            },
        ),
        fact(
            DeliveryAuditStep::Organization,
            11,
            DeliveryAuditKind::OutcomeUnknown,
        ),
        DeliveryAuditFact {
            at: approved_at - chrono::Duration::nanoseconds(1),
            ..org_delayed.audit_events[0].clone()
        },
    ] {
        let invalid = OrganizationDeliveryProjection {
            snapshot: org_latest.clone(),
            audit_events: vec![fact],
        };
        assert!(matches!(
            s.project_organization_delivery_with_events(&invalid).await,
            Err(Error::ProjectionInvariant(_))
        ));
    }
    // Equal snapshots can also receive missing historical facts; exact replay
    // after an acknowledged commit remains successful.
    let equal = OrganizationDeliveryProjection {
        snapshot: org_latest.clone(),
        audit_events: vec![fact(
            DeliveryAuditStep::Organization,
            6,
            DeliveryAuditKind::Blocked {
                reason: DeliveryBlockedReason::Organization(
                    crate::org_delivery::OrganizationBlockedReason::NoSeat,
                ),
            },
        )],
    };
    s.project_organization_delivery_with_events(&equal)
        .await
        .unwrap();
    s.project_organization_delivery_with_events(&equal)
        .await
        .unwrap();
    assert_eq!(
        s.list_audit_events(
            9001,
            Some(EventType::DeliveryBlocked),
            AuditPosition::Latest
        )
        .await
        .unwrap()
        .events
        .len(),
        3
    );
    assert!(
        s.list_audit_events(
            9002,
            Some(EventType::DeliveryBlocked),
            AuditPosition::Latest
        )
        .await
        .unwrap()
        .events
        .is_empty()
    );
}
