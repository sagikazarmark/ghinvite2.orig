use chrono::{DateTime, Duration};
use ghinvite_core::{
    bulk::{BatchRef, BulkBatchId},
    email_delivery::{
        EmailAttempt, EmailCreateCommand, EmailCreateOutcome, EmailDeliverySnapshot, EmailRef,
        EmailSettlement,
    },
    storage::{
        email_delivery::EmailDeliveryStorage,
        email_vault::{EmailVaultBinding, EmailVaultStorage, NewEmailRecord},
    },
    targets::{MemberGrant, OrganizationTarget},
};
use ghinvite_storage_sqlx::SqlxStorage;
use std::sync::Arc;
fn command() -> EmailCreateCommand {
    EmailCreateCommand {
        batch: BatchRef {
            batch_id: BulkBatchId::new(),
            row: 1,
        },
        account_id: 42,
        installation_id: 9,
        organization: OrganizationTarget {
            org_id: 42,
            org_login: "acme".into(),
            grant: MemberGrant::Member,
        },
        teams: vec![],
        email_ref: EmailRef::from_bytes([4; 32]),
        approved_by: 7,
        approved_at: DateTime::from_timestamp(1800000000, 0).unwrap(),
    }
}
async fn active(s: &SqlxStorage, c: &EmailCreateCommand) {
    let b = EmailVaultBinding {
        email_ref: c.email_ref,
        batch: c.batch,
        account_id: c.account_id,
        installation_id: c.installation_id,
    };
    s.stage_email(&NewEmailRecord {
        binding: b.clone(),
        ciphertext: "b3BhcXVl".into(),
        created_at: c.approved_at.timestamp(),
    })
    .await
    .unwrap();
    assert!(
        s.activate_email(&b, c.approved_at.timestamp())
            .await
            .unwrap()
    );
}
#[tokio::test]
async fn full_binding_fence_concurrent_claim_stale_release_and_vault_gate() {
    let s = Arc::new(SqlxStorage::in_memory().await.unwrap());
    let a = EmailAttempt { create: command() };
    assert!(s.claim_email_attempt(&a).await.is_err());
    assert!(!s.email_attempt_exists(&a).await.unwrap());
    active(&s, &a.create).await;
    let mut tasks = vec![];
    for _ in 0..8 {
        let s = s.clone();
        let a = a.clone();
        tasks.push(tokio::spawn(async move {
            s.claim_email_attempt(&a).await.unwrap()
        }));
    }
    let mut winners = vec![];
    for t in tasks {
        if let Some(g) = t.await.unwrap() {
            winners.push(g);
        }
    }
    assert_eq!(winners, vec![1]);
    assert!(s.email_attempt_exists(&a).await.unwrap());
    assert_eq!(s.claim_email_attempt(&a).await.unwrap(), None);
    for n in 0..5 {
        let mut other = a.clone();
        match n {
            0 => other.create.email_ref = EmailRef::from_bytes([5; 32]),
            1 => other.create.installation_id = 10,
            2 => other.create.approved_by = 8,
            3 => other.create.approved_at += Duration::seconds(1),
            _ => other.create.organization.org_login = "renamed".into(),
        };
        assert!(s.email_attempt_exists(&other).await.is_err());
        assert!(s.claim_email_attempt(&other).await.is_err());
        s.reject_email_attempt(&other, 1).await.unwrap();
        assert!(s.email_attempt_exists(&a).await.unwrap());
    }
    s.reject_email_attempt(&a, 1).await.unwrap();
    assert!(!s.email_attempt_exists(&a).await.unwrap());
    assert_eq!(s.claim_email_attempt(&a).await.unwrap(), Some(2));
    s.reject_email_attempt(&a, 1).await.unwrap();
    assert!(s.email_attempt_exists(&a).await.unwrap());
    assert_eq!(s.claim_email_attempt(&a).await.unwrap(), None);
    assert!(s.reject_email_attempt(&a, 9007199254740992).await.is_err());
}
#[tokio::test]
async fn receipts_scoped_monotonic_and_terminal_create_immutable() {
    let s = SqlxStorage::in_memory().await.unwrap();
    let c = command();
    let now = c.approved_at;
    let mut snapshot = EmailDeliverySnapshot {
        command: c.clone(),
        revision: 1,
        outcome: EmailCreateOutcome::Planned,
        completed_at: None,
        confirmed_at: None,
        settlement: None,
        settled_at: None,
        unknown_since: None,
        last_observed_at: None,
    };
    s.project_email_delivery(&snapshot).await.unwrap();
    s.project_email_delivery(&snapshot).await.unwrap();
    let planned = snapshot.clone();
    snapshot.revision = 2;
    snapshot.outcome = EmailCreateOutcome::OutcomeUnknown;
    s.project_email_delivery(&snapshot).await.unwrap();
    let mut bad = snapshot.clone();
    bad.revision = 3;
    bad.outcome = EmailCreateOutcome::Planned;
    assert!(s.project_email_delivery(&bad).await.is_err());
    snapshot.revision = 3;
    snapshot.outcome = EmailCreateOutcome::Created {
        upstream_id: 77,
        created_at: now,
    };
    snapshot.confirmed_at = Some(now);
    // An audit insertion outage must roll back the receipt in the same transaction.
    let before = s.email_delivery(&c.storage_id()).await.unwrap();
    s.debug_set_audit_failure(true).await.unwrap();
    assert!(s.project_email_delivery(&snapshot).await.is_err());
    assert_eq!(s.email_delivery(&c.storage_id()).await.unwrap(), before);
    s.debug_set_audit_failure(false).await.unwrap();
    s.project_email_delivery(&snapshot).await.unwrap();
    bad = snapshot.clone();
    bad.revision = 4;
    bad.outcome = EmailCreateOutcome::Created {
        upstream_id: 78,
        created_at: now,
    };
    assert!(s.project_email_delivery(&bad).await.is_err());
    snapshot.revision = 4;
    snapshot.settlement = Some(EmailSettlement::NoLongerPending);
    snapshot.settled_at = Some(now + Duration::seconds(1));
    s.project_email_delivery(&snapshot).await.unwrap();
    s.project_email_delivery(&planned).await.unwrap();
    s.project_email_delivery(&snapshot).await.unwrap();
    let audit = s.debug_list_audit(42).await.unwrap();
    assert_eq!(audit.len(), 2);
    assert_eq!(
        audit[0].event_type,
        ghinvite_core::audit::EventType::OrgInvitationSent
    );
    assert_eq!(
        audit[1].event_type,
        ghinvite_core::audit::EventType::EmailInvitationNoLongerPending
    );
    assert_eq!(audit[0].metadata["source"]["row"], c.batch.row);
    let wire = serde_json::to_string(&audit).unwrap();
    assert!(!wire.contains(&c.email_ref.as_hex()));
    assert!(!wire.contains("email_ref"));
    assert!(!wire.contains("invitation.accepted"));

    assert_eq!(
        s.email_delivery(&c.storage_id()).await.unwrap(),
        Some(snapshot.clone())
    );
    assert_eq!(
        s.batch_email_deliveries(42, c.batch.batch_id)
            .await
            .unwrap(),
        vec![snapshot]
    );
    assert!(
        s.batch_email_deliveries(43, c.batch.batch_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        s.batch_email_deliveries(42, BulkBatchId::new())
            .await
            .unwrap()
            .is_empty()
    );
    let mut other = planned;
    other.command.organization.org_id = 43;
    other.command.account_id = 43;
    assert!(s.project_email_delivery(&other).await.is_err());
}
