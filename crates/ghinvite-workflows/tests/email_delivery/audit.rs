use super::*;
use ghinvite_core::{audit::EventType, storage::ConsoleStorage};
use std::sync::atomic::Ordering;
async fn events(f: &Fixture, kind: EventType) -> Vec<ghinvite_core::audit::AuditEvent> {
    f.storage
        .list_audit_events(100, Some(kind), Default::default())
        .await
        .unwrap()
        .events
}
async fn wait_event(f: &Fixture, kind: EventType) -> Vec<ghinvite_core::audit::AuditEvent> {
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let rows = events(f, kind).await;
            if !rows.is_empty() {
                break rows;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn blocked_atomic_kill_replays_original_fact_before_created_without_private_metadata() {
    let faults = Arc::new(ghinvite_workflows::email_delivery::EmailAuditFaults::default());
    faults.pause.store(true, Ordering::SeqCst);
    let f = Fixture::with_audit_fault(201, Some(faults.clone())).await;
    let c = f.command(false).await;
    let pending =
        f.h.restate
            .invoke(
                &Call::object("EmailInviteDelivery", &c.delivery_key(), "create").send(),
                Some(&json!(c)),
                None,
            )
            .await;
    assert_eq!(pending.status, 202, "{pending:?}");
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while faults.reached.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let stored=f.h.restate.admin().sql_or_panic(&format!("SELECT value_utf8 FROM state WHERE service_name='EmailInviteDelivery' AND service_key='{}' AND key='email'",c.delivery_key())).await;
    assert_eq!(stored.len(), 1);
    let retained: Value = serde_json::from_str(stored[0]["value_utf8"].as_str().unwrap()).unwrap();
    let original = retained["audit"]["pending"][0].clone();
    assert_eq!(original["kind"]["kind"], "blocked");
    assert!(events(&f, EventType::DeliveryBlocked).await.is_empty());
    f.h.restate.admin().kill(pending.invocation_id()).await;
    f.h.restate
        .admin()
        .await_status(pending.invocation_id(), &["completed"])
        .await;
    faults.lose_projection_ack.store(true, Ordering::SeqCst);
    faults.pause.store(false, Ordering::SeqCst);
    let b = EmailVaultBinding {
        email_ref: c.email_ref,
        batch: c.batch,
        account_id: 100,
        installation_id: 2,
    };
    assert!(
        f.storage
            .activate_email(&b, Utc::now().timestamp())
            .await
            .unwrap()
    );
    let created = f.create(&c).await;
    assert_eq!(created["outcome"]["kind"], "created");
    assert_eq!(f.create(&c).await, created);
    let rows = wait_event(&f, EventType::DeliveryBlocked).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(json!(rows[0].occurred_at), original["at"]);
    assert_eq!(
        rows[0].metadata["transition_revision"],
        original["transition_revision"]
    );
    assert_eq!(wait_event(&f, EventType::OrgInvitationSent).await.len(), 1);
    assert_eq!(faults.lost_projection_acks.load(Ordering::SeqCst), 1);
    assert_eq!(f.posts(), 1);
    for row in rows {
        let encoded = serde_json::to_string(&row).unwrap();
        assert!(!encoded.contains(MAIL));
        assert!(!encoded.contains("email_ref"));
        assert!(!encoded.contains(&"08".repeat(32)));
    }
}
#[tokio::test]
async fn unknown_audit_replays_once_with_exact_first_unknown_clock_and_one_post() {
    let f = Fixture::new(503).await;
    let c = f.command(true).await;
    let result = f.create(&c).await;
    for _ in 0..3 {
        assert_eq!(f.create(&c).await, result);
    }
    let rows = wait_event(&f, EventType::DeliveryOutcomeUnknown).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(json!(rows[0].occurred_at), result["unknown_since"]);
    assert_eq!(f.posts(), 1);
    assert!(events(&f, EventType::OrgInvitationSent).await.is_empty());
}
