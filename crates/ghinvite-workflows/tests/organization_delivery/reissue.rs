use super::*;
use ghinvite_core::{
    InvitationState,
    admission::AdmissionOperationId,
    reissue::{InvitationBarrier, OrganizationReissueBarrierQuery},
};

async fn ended(f: &Fixture, c: &OrganizationCreateCommand) -> OrganizationDeliverySnapshot {
    f.create(c).await;
    {
        let mut gh = f.github.0.lock().unwrap();
        gh.pending = false;
        gh.org = None;
    }
    standard_call(
        f,
        "OrganizationDelivery",
        &c.delivery_key(),
        "observe_listed",
        Some(observed(c, Utc::now(), false)),
    )
    .await;
    let ended = terminal(f, c).await;
    assert_eq!(
        ended.settlement.as_ref().unwrap().state,
        InvitationState::Declined
    );
    ended
}
async fn barrier(f: &Fixture, query: &OrganizationReissueBarrierQuery) -> InvitationBarrier {
    serde_json::from_value(
        standard_call(
            f,
            "OrganizationDelivery",
            &query.command.delivery_key(),
            "prepare_reissue",
            Some(json!(query)),
        )
        .await,
    )
    .unwrap()
}
async fn wait_for_content_write(f: &Fixture) {
    let budget: ghinvite_core::budget::BudgetSnapshot =
        serde_json::from_value(standard_call(f, "InviteBudget", "org:100", "snapshot", None).await)
            .unwrap();
    if let Some(deadline) = budget.next_content_write_at {
        // The initial POST and this DELETE share the real content-write bucket.
        // Exercise the cancellation result only after its existing spacing gate.
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while Utc::now() < deadline {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the original content-write spacing deadline must elapse");
    }
}
#[tokio::test]
async fn owned_pending_old_generation_cancels_once_without_rewriting_terminal_receipts() {
    let f = Fixture::cancellation(Default::default(), 604800).await;
    let c = f.command(&[5]).await;
    let original = ended(&f, &c).await;
    {
        let mut gh = f.github.0.lock().unwrap();
        gh.pending = true;
        gh.org = Some(("member", "pending"));
    }
    let query = OrganizationReissueBarrierQuery {
        operation_id: AdmissionOperationId::try_from(RequestId::new().to_string()).unwrap(),
        command: c.clone(),
    };
    wait_for_content_write(&f).await;
    for _ in 0..3 {
        assert_eq!(barrier(&f, &query).await, InvitationBarrier::Clear);
    }
    assert_eq!(f.writes("DELETE").len(), 1);
    let retained: OrganizationDeliverySnapshot = serde_json::from_value(
        standard_call(
            &f,
            "OrganizationDelivery",
            &c.delivery_key(),
            "status",
            None,
        )
        .await,
    )
    .unwrap();
    assert_eq!(retained, original);
    assert!(
        f.storage
            .organization_attempt_exists(&OrganizationAttempt {
                create: c.clone(),
                action: OrganizationWrite::Cancel {
                    upstream_id: 99,
                    reason: ghinvite_core::delivery::SettlementReason::AdminReissue
                }
            })
            .await
            .unwrap()
    );
    let mut conflict = query;
    conflict.command.requester_id = 9;
    assert_eq!(
        f.h.restate
            .invoke(
                &Call::object("OrganizationDelivery", &c.delivery_key(), "prepare_reissue"),
                Some(&json!(conflict)),
                None
            )
            .await
            .status,
        409
    );
}
#[tokio::test]
async fn uncertain_old_delete_never_retries_or_refunds_and_foreign_pending_is_not_deleted() {
    let f = Fixture::cancellation(
        Scenario {
            deletes: VecDeque::from([502]),
            ..Default::default()
        },
        604800,
    )
    .await;
    let c = f.command(&[]).await;
    let original = ended(&f, &c).await;
    {
        let mut gh = f.github.0.lock().unwrap();
        gh.pending = true;
        gh.org = Some(("member", "pending"));
    }
    let query = OrganizationReissueBarrierQuery {
        operation_id: AdmissionOperationId::try_from(RequestId::new().to_string()).unwrap(),
        command: c.clone(),
    };
    wait_for_content_write(&f).await;
    for _ in 0..3 {
        assert_eq!(barrier(&f, &query).await, InvitationBarrier::MayHaveWritten);
    }
    assert_eq!(f.writes("DELETE").len(), 1);
    let retained: OrganizationDeliverySnapshot = serde_json::from_value(
        standard_call(
            &f,
            "OrganizationDelivery",
            &c.delivery_key(),
            "status",
            None,
        )
        .await,
    )
    .unwrap();
    assert_eq!(retained, original);
    let budget = standard_call(&f, "InviteBudget", "org:100", "snapshot", None).await;
    assert_eq!(budget["content_used_hour"], 2);
    let foreign = Fixture::cancellation(
        Scenario {
            pending: true,
            org: Some(("member", "pending")),
            ..Default::default()
        },
        604800,
    )
    .await;
    let c = foreign.command(&[]).await;
    let original = ended(&foreign, &c).await;
    assert!(matches!(
        original.create.outcome,
        OrganizationCreateOutcome::AlreadyPending { .. }
    ));
    {
        let mut gh = foreign.github.0.lock().unwrap();
        gh.pending = true;
        gh.org = Some(("member", "pending"));
    }
    let query = OrganizationReissueBarrierQuery {
        operation_id: AdmissionOperationId::try_from(RequestId::new().to_string()).unwrap(),
        command: c,
    };
    assert_eq!(barrier(&foreign, &query).await, InvitationBarrier::Pending);
    assert!(foreign.writes("DELETE").is_empty());
    {
        let mut gh = foreign.github.0.lock().unwrap();
        gh.pending = false;
        gh.org = Some(("member", "active"));
    }
    assert_eq!(barrier(&foreign, &query).await, InvitationBarrier::Clear);
}
