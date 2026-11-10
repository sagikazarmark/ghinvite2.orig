use super::*;

#[tokio::test]
async fn organization_expiry_binds_upstream_time_to_retained_short_lifetime_and_shared_teams() {
    let f = Fixture::start_options(Default::default(), false, false, 3600).await;
    let c = f.command(&[5, 6]).await;
    let snapshot = f.create(&c).await;
    let OrganizationCreateOutcome::Created {
        upstream_id,
        created_at,
    } = snapshot.create.outcome
    else {
        panic!("{snapshot:?}")
    };
    let timing = snapshot
        .invitation_timing
        .as_ref()
        .expect("owner retains source clock and inbox policy");
    assert_eq!(timing.upstream_id, upstream_id);
    assert_eq!(timing.created_at, created_at);
    assert_eq!(timing.owned_lifetime_seconds, Some(3600));
    assert_eq!(
        snapshot.invitation_expires_at(),
        Some(created_at + chrono::Duration::hours(1))
    );
    assert_eq!(f.create(&c).await, snapshot);
    assert_eq!(f.writes("POST").len(), 1);
    assert!(f.writes("DELETE").is_empty());
    let receipt = snapshot.create.clone();
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            if let Some(stored) = f
                .storage
                .get_organization_delivery(&c.storage_id())
                .await
                .unwrap()
                && stored.invitation_timing == snapshot.invitation_timing
            {
                assert_eq!(stored.create, receipt);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}
