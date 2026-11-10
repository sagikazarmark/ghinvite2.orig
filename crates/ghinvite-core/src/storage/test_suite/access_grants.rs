use super::*;
use crate::{
    access_grant::*,
    delivery::{CreateCommand, CreateOutcome, CreateReceipt, DeliverySnapshot, Settlement},
    storage::{Error, access_grants::AccessGrantStorage},
};
pub async fn scenario_access_grants<S: Storage + ProjectionStorage + AccessGrantStorage>(s: S) {
    let now = dt("2026-05-04T13:00:00Z");
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(1, 100, "acme")).clone(),
    ))
    .await
    .unwrap();
    s.upsert_user(&sample_user(7, "owner")).await.unwrap();
    s.upsert_user(&sample_user(8, "requester")).await.unwrap();
    let mut link = sample_link(100, 1, 7);
    link.access_lifetime_seconds = Some(3600);
    let request = sample_request(link.id, 8);
    seed(&s, &link, std::slice::from_ref(&request), 1).await;
    assert_eq!(
        s.get_invitation_link_by_id(link.id)
            .await
            .unwrap()
            .unwrap()
            .access_lifetime_seconds,
        Some(3600)
    );
    let create = CreateReceipt {
        audit_version: crate::delivery_audit::DeliveryAuditVersion::Legacy,
        command: CreateCommand {
            budget_attribution: Default::default(),
            batch: None,
            access_lifetime_seconds: Some(3600),
            budget_share_pct: 50,
            invitation_id: Some(GithubInvitationId::new()),
            delivery_generation: 1,
            link_id: Some(link.id),
            request_id: request.id,
            approval_id: "access-test".into(),
            account_id: 100,
            installation_id: 1,
            requester_id: 8,
            repo_id: 10,
            repo_full_name: "acme/api".into(),
            permission: Permission::Pull,
            approved_at: request.created_at,
        },
        outcome: CreateOutcome::Created { upstream_id: 90 },
        revision: 2,
        confirmed_at: Some(now),
    };
    let mut delivery = DeliverySnapshot::from(create);
    delivery.revision = 3;
    delivery.settlement = Some(Settlement {
        state: InvitationState::Accepted,
        event: AuditEvent {
            id: AuditEventId::new(),
            account_id: 100,
            occurred_at: now,
            event_type: EventType::InvitationAccepted,
            actor_kind: ActorKind::Github,
            actor_id: None,
            target_kind: TargetKind::GithubInvitation,
            target_id: delivery
                .create
                .command
                .invitation_id
                .expect("legacy fixture identity")
                .to_string(),
            metadata: serde_json::Value::Null,
            request_id: Some(request.id.to_string()),
        },
    });
    let grant = accepted_repository(&delivery).unwrap().unwrap();
    let original = AccessGrantSnapshot {
        command: grant.clone(),
        revision: 1,
        outcome: RemovalOutcome::Scheduled,
    };
    assert!(matches!(
        s.project_access_grant(&original).await,
        Err(Error::ProjectionDependency)
    ));
    s.project_delivery(&delivery).await.unwrap();
    for change in [0, 1, 2] {
        let mut forged = original.clone();
        match change {
            0 => forged.command.access_lifetime_seconds = Some(7200),
            1 => forged.command.installation_id = 99,
            _ => forged.command.accepted_at += chrono::Duration::seconds(1),
        }
        assert!(matches!(
            s.project_access_grant(&forged).await,
            Err(Error::ProjectionInvariant(_))
        ));
        assert!(
            s.get_access_grant(&grant.storage_id())
                .await
                .unwrap()
                .is_none()
        );
    }
    s.project_access_grant(&original).await.unwrap();
    s.project_access_grant(&original).await.unwrap();
    assert_eq!(
        s.get_access_grant(&grant.storage_id()).await.unwrap(),
        Some(original.clone())
    );
    let command = RemovalCommand {
        grant: grant.clone(),
        reason: RemovalReason::AccessExpiry,
    };
    assert!(!s.removal_attempt_exists(&command).await.unwrap());
    assert_eq!(s.claim_removal_attempt(&command).await.unwrap(), Some(1));
    assert!(s.removal_attempt_exists(&command).await.unwrap());
    assert_eq!(s.claim_removal_attempt(&command).await.unwrap(), None);
    let mut wrong = command.clone();
    wrong.reason = RemovalReason::LinkRevoked;
    assert!(matches!(
        s.claim_removal_attempt(&wrong).await,
        Err(Error::ProjectionInvariant(_))
    ));
    s.reject_removal_attempt(&wrong, 1).await.unwrap();
    assert!(s.removal_attempt_exists(&command).await.unwrap());
    s.reject_removal_attempt(&command, 1).await.unwrap();
    assert!(!s.removal_attempt_exists(&command).await.unwrap());
    assert_eq!(s.claim_removal_attempt(&command).await.unwrap(), Some(2));
    s.reject_removal_attempt(&command, 1).await.unwrap();
    assert!(s.removal_attempt_exists(&command).await.unwrap());
    let mut removed = original.clone();
    removed.revision = 2;
    removed.outcome = RemovalOutcome::Removed {
        reason: RemovalReason::AccessExpiry,
        at: now + chrono::Duration::hours(1),
    };
    s.project_access_grant(&removed).await.unwrap();
    s.project_access_grant(&original).await.unwrap();
    assert_eq!(
        s.get_access_grant(&grant.storage_id()).await.unwrap(),
        Some(removed.clone())
    );
    s.project_access_grant(&removed).await.unwrap();
    let events = s
        .list_audit_events(
            100,
            Some(EventType::AccessRemoved),
            crate::storage::audit_read::AuditPosition::Latest,
        )
        .await
        .unwrap();
    assert_eq!(events.events.len(), 1);
    assert_eq!(events.events[0].target_kind, TargetKind::AccessGrant);
    assert_eq!(events.events[0].target_id, grant.storage_id());
    assert_eq!(events.events[0].request_id, Some(request.id.to_string()));
    let mut changed = removed.clone();
    changed.revision = 3;
    changed.outcome = RemovalOutcome::Scheduled;
    assert!(matches!(
        s.project_access_grant(&changed).await,
        Err(Error::ProjectionInvariant(_))
    ));
    changed = removed.clone();
    changed.command.account_id = 101;
    assert!(matches!(
        s.project_access_grant(&changed).await,
        Err(Error::ProjectionInvariant(_))
    ));
    assert_eq!(
        s.get_access_grant(&grant.storage_id()).await.unwrap(),
        Some(removed)
    );
    assert_eq!(
        s.get_delivery(&delivery.create.command.delivery_key())
            .await
            .unwrap(),
        Some(delivery)
    );
}
