use super::*;

pub async fn scenario_requester_profiles<S: Storage + ProjectionStorage>(s: S) {
    use crate::storage::console_scope::ConsoleScope;
    let old = sample_user(701, "requester");
    s.upsert_user(&old).await.unwrap();
    assert_eq!(
        s.get_user(701).await.unwrap().unwrap().github_created_at,
        None
    );
    let serialized = serde_json::to_value(&old).unwrap();
    assert!(serialized.get("github_created_at").is_none());
    let mut enriched = old.clone();
    enriched.avatar_url = Some("https://avatars.githubusercontent.com/u/701?v=4".into());
    enriched.github_created_at = Some(dt("2010-01-02T03:04:05Z"));
    s.upsert_user(&enriched).await.unwrap();
    assert_eq!(s.get_user(701).await.unwrap(), Some(enriched.clone()));
    // A partial older producer cannot erase already observed display metadata.
    s.upsert_user(&old).await.unwrap();
    assert_eq!(s.get_user(701).await.unwrap(), Some(enriched.clone()));
    s.put_app_installation(&crate::account::Installation::repo(sample_account(
        1, 9001, "acme",
    )))
    .await
    .unwrap();
    let link = sample_link(9001, 1, 701);
    let request = sample_request(link.id, 701);
    seed(&s, &link, &[request], 1).await;
    let queue = s
        .scoped_list_pending_requests_page(&ConsoleScope::account(9001).unwrap(), None)
        .await
        .unwrap();
    assert_eq!(queue.rows.len(), 1);
    assert_eq!(queue.rows[0].requester_avatar_url, enriched.avatar_url);
    assert_eq!(
        queue.rows[0].requester_github_created_at,
        enriched.github_created_at
    );
    assert!(
        s.scoped_list_pending_requests_page(&ConsoleScope::account(9002).unwrap(), None)
            .await
            .unwrap()
            .rows
            .is_empty()
    );
    let history = s.request_history(9001, link.id, None).await.unwrap();
    assert_eq!(
        history.requester_avatars.get(&701),
        enriched.avatar_url.as_ref()
    );
    assert_eq!(
        history.requester_created_at.get(&701),
        enriched.github_created_at.as_ref()
    );
    let foreign = s.request_history(9002, link.id, None).await.unwrap();
    assert!(
        foreign.requests.is_empty()
            && foreign.requester_avatars.is_empty()
            && foreign.requester_created_at.is_empty()
    );
}
