use super::*;

#[tokio::test]
async fn organization_spam_observation_is_retained_once_and_never_reopens_the_bucket() {
    let f = Fixture::new(Scenario {
        spammed_writes: true,
        writes: VecDeque::from([422]),
        ..Default::default()
    })
    .await;
    let command = f.command(&[5]).await;
    let result = f.create(&command).await;
    assert!(matches!(
        result.create.outcome,
        OrganizationCreateOutcome::Throttled { .. }
    ));
    let first = standard_call(&f, "InviteBudget", "org:100", "snapshot", None).await;
    assert_eq!(first["limit_counts"]["spammed"], 1);
    assert_eq!(first["limit_counts"]["rate_limited"], 0);
    for _ in 0..2 {
        f.create(&command).await;
    }
    let repeated = standard_call(&f, "InviteBudget", "org:100", "snapshot", None).await;
    assert_eq!(repeated["limit_counts"], first["limit_counts"]);
    assert_eq!(f.writes("POST").len(), 1);
    assert!(f.writes("PUT").is_empty());
    let reserve = standard_call(&f, "InviteBudget", "org:100", "reserve", Some(json!({"request_key":"independent-org-invite-after-spam","kind":{"kind":"org_invite"},"link_id":null}))).await;
    assert_eq!(reserve["kind"], "retry_at");
    assert_eq!(reserve["why"], "closed");
}
