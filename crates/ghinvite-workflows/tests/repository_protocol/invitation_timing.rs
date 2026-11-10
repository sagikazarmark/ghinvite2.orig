use super::*;
use ghinvite_core::delivery_control::LinkControl;

#[tokio::test]
async fn expiry_uses_real_upstream_clock_and_retained_policy_without_changing_receipts() {
    let runtime = Runtime::start().await;
    let c = runtime.command(Scenario::default(), Permission::Push).await;
    let mut input = control::input(&c, 1, 0, LinkControl::Active);
    input.invitation_lifetime_seconds = 3600;
    control::control(&runtime, &c, &input).await;
    let (receipt, _) = runtime.create(&c).await;
    let snapshot = control::status(&runtime, &c).await.unwrap();
    let timing = snapshot
        .invitation_timing
        .as_ref()
        .expect("typed201 retains upstream creation time");
    assert_eq!(timing.upstream_id, 99);
    assert_eq!(timing.owned_lifetime_seconds, Some(3600));
    assert!(timing.created_at.timestamp() >= c.approved_at.timestamp());
    assert_eq!(
        snapshot.invitation_expires_at(),
        Some(timing.created_at + chrono::Duration::hours(1))
    );
    for _ in 0..2 {
        assert_eq!(runtime.create(&c).await.0, receipt);
    }
    assert_eq!(control::status(&runtime, &c).await.unwrap(), snapshot);
    assert_eq!(operations(&runtime.trace(), "PUT", PUT_PATH).len(), 1);
    assert!(operations(&runtime.trace(), "DELETE", PATCH_PATH).is_empty());

    // A pre-existing invitation is not shortened by this request's own lifetime.
    let c = runtime
        .command(
            Scenario {
                pending: Some("write"),
                ..Default::default()
            },
            Permission::Push,
        )
        .await;
    let mut input = control::input(&c, 1, 0, LinkControl::Active);
    input.invitation_lifetime_seconds = 3600;
    control::control(&runtime, &c, &input).await;
    let (receipt, _) = runtime.create(&c).await;
    assert!(matches!(
        receipt.outcome,
        CreateOutcome::AlreadyPending { .. }
    ));
    let snapshot = control::status(&runtime, &c).await.unwrap();
    let timing = snapshot.invitation_timing.as_ref().unwrap();
    assert!(timing.created_at < c.approved_at);
    assert_eq!(
        snapshot.invitation_expires_at(),
        Some(timing.created_at + chrono::Duration::days(7))
    );
    assert!(
        runtime
            .trace()
            .iter()
            .all(|t| t.method != "PUT" && t.method != "DELETE")
    );
}

#[tokio::test]
async fn malformed_advisory_write_clock_keeps_success_and_later_verified_list_fills_only_timing() {
    let runtime = Runtime::start().await;
    let c = runtime
        .command(
            Scenario {
                invitation_clock_override: Some(json!("invalid timestamp")),
                ..Default::default()
            },
            Permission::Push,
        )
        .await;
    let mut input = control::input(&c, 1, 0, LinkControl::Active);
    input.invitation_lifetime_seconds = 3600;
    control::control(&runtime, &c, &input).await;
    let (receipt, _) = runtime.create(&c).await;
    assert!(matches!(receipt.outcome, CreateOutcome::Created { .. }));
    let missing = control::status(&runtime, &c).await.unwrap();
    assert!(missing.invitation_timing.is_none());
    assert_eq!(missing.invitation_expires_at(), None);
    let response = runtime
        .harness
        .restate
        .invoke(
            &Call::object("RepositoryDelivery", &c.delivery_key(), "refresh"),
            None::<&Value>,
            None,
        )
        .await;
    assert_eq!(response.status, 200, "{response:?}");
    let learned = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let snapshot = control::status(&runtime, &c).await.unwrap();
            if snapshot.invitation_timing.is_some() {
                break snapshot;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("complete bound listing supplies missing advisory source clock");
    assert_eq!(learned.create, receipt);
    assert!(learned.revision > missing.revision);
    assert!(learned.invitation_expires_at().is_some());
    assert_eq!(runtime.create(&c).await.0, receipt);
    assert_eq!(operations(&runtime.trace(), "PUT", PUT_PATH).len(), 1);
    assert!(operations(&runtime.trace(), "DELETE", PATCH_PATH).is_empty());
}
