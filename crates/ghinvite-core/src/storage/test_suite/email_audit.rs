use super::*;
use crate::{
    bulk::BulkBatchId,
    email_delivery::EmailDeliverySnapshot,
    storage::email_delivery::{
        EmailAuditFact, EmailAuditKind, EmailDeliveryProjection, EmailDeliveryStorage,
    },
};

pub async fn scenario_email_transition_audit<S: Storage + EmailDeliveryStorage>(s: S) {
    let at = dt("2026-10-17T10:00:00Z");
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(2, 100, "acme")).clone(),
    ))
    .await
    .unwrap();
    let mut snapshot: EmailDeliverySnapshot = serde_json::from_value(serde_json::json!({
        "command":{"batch":{"batch_id":BulkBatchId::new(),"row":1},"account_id":100,"installation_id":2,
            "organization":{"org_id":100,"org_login":"acme","grant":"member"},"teams":[],
            "email_ref":"ab".repeat(32),"approved_by":7,"approved_at":at},
        "revision":2,"outcome":{"kind":"blocked","reason":"email_recovery_unavailable"}
    })).unwrap();
    let fact = EmailAuditFact {
        transition_revision: 2,
        at: at + chrono::Duration::seconds(1),
        kind: EmailAuditKind::Blocked {
            reason: crate::email_delivery::EmailBlockedReason::EmailRecoveryUnavailable,
        },
    };
    let delayed = EmailDeliveryProjection {
        snapshot: snapshot.clone(),
        audit_events: vec![fact.clone()],
    };
    // A later receipt arrives first; delayed original facts must still append.
    snapshot.revision = 3;
    snapshot.outcome = crate::email_delivery::EmailCreateOutcome::OutcomeUnknown;
    snapshot.unknown_since = Some(at + chrono::Duration::seconds(2));
    let current = EmailDeliveryProjection {
        snapshot: snapshot.clone(),
        audit_events: vec![EmailAuditFact {
            transition_revision: 3,
            at: snapshot.unknown_since.unwrap(),
            kind: EmailAuditKind::OutcomeUnknown,
        }],
    };
    s.project_email_delivery_with_events(&current)
        .await
        .unwrap();
    s.project_email_delivery_with_events(&delayed)
        .await
        .unwrap();
    s.project_email_delivery_with_events(&current)
        .await
        .unwrap();
    s.project_email_delivery_with_events(&delayed)
        .await
        .unwrap();
    assert_eq!(
        s.email_delivery(&snapshot.command.storage_id())
            .await
            .unwrap(),
        Some(snapshot.clone())
    );
    let blocked = s
        .list_audit_events(100, Some(EventType::DeliveryBlocked), Default::default())
        .await
        .unwrap();
    assert_eq!(blocked.events.len(), 1);
    assert_eq!(blocked.events[0].occurred_at, fact.at);
    let unknown = s
        .list_audit_events(
            100,
            Some(EventType::DeliveryOutcomeUnknown),
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(unknown.events.len(), 1);
    assert_eq!(
        unknown.events[0].occurred_at,
        snapshot.unknown_since.unwrap()
    );
    for event in blocked.events.iter().chain(unknown.events.iter()) {
        let encoded = serde_json::to_string(event).unwrap();
        assert!(!encoded.contains(&"ab".repeat(32)));
        assert!(!encoded.contains("email_ref"));
        assert_eq!(event.request_id, None);
        assert_eq!(event.target_id, snapshot.command.storage_id());
    }
    // Changed payload at the same event identity rolls back a newer head too.
    let mut conflict = delayed.clone();
    conflict.snapshot = snapshot.clone();
    conflict.snapshot.revision = 4;
    conflict.audit_events[0].at += chrono::Duration::nanoseconds(1);
    assert!(
        s.project_email_delivery_with_events(&conflict)
            .await
            .is_err()
    );
    assert_eq!(
        s.email_delivery(&snapshot.command.storage_id())
            .await
            .unwrap(),
        Some(snapshot.clone())
    );
    let mut malformed = current.clone();
    malformed.audit_events[0].transition_revision = 4;
    assert!(
        s.project_email_delivery_with_events(&malformed)
            .await
            .is_err()
    );
    malformed = current.clone();
    malformed.audit_events[0].at = at - chrono::Duration::seconds(1);
    assert!(
        s.project_email_delivery_with_events(&malformed)
            .await
            .is_err()
    );
    // Flat old snapshot payload remains accepted, with no invented history.
    let old: EmailDeliveryProjection =
        serde_json::from_value(serde_json::to_value(&snapshot).unwrap()).unwrap();
    assert!(old.audit_events.is_empty());
    s.project_email_delivery_with_events(&old).await.unwrap();
    assert_eq!(
        s.list_audit_events(100, Some(EventType::DeliveryBlocked), Default::default())
            .await
            .unwrap(),
        blocked
    );
}
