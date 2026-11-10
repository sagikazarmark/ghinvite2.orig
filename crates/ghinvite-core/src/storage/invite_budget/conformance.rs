use super::*;
use crate::{
    account::App,
    budget::{BucketId, BucketSnapshot},
};
pub async fn run(s: &impl InviteBudgetStorage) {
    let at = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let mut original = BudgetSnapshot {
        app: App::Repo,
        account_id: 7,
        revision: 1,
        observed_at: at,
        confirmed_established: false,
        buckets: vec![BucketSnapshot {
            bucket: BucketId::Repo { repo_id: 10 },
            used_24h: 12,
            cap: 50,
            closed_until: Some(at + chrono::Duration::seconds(30)),
            next_slot_at: Some(at + chrono::Duration::seconds(30)),
        }],
        limit_counts: crate::budget::LimitCounts {
            coverage_started_at: Some(at),
            rate_limited: 3,
            spammed: 2,
            incomplete: true,
            ..Default::default()
        },
        content_used_minute: 12,
        content_used_hour: 12,
        content_closed_until: Some(at + chrono::Duration::seconds(60)),
        next_content_write_at: Some(at + chrono::Duration::seconds(60)),
    };
    assert!(
        s.get_invite_budget(original.owner())
            .await
            .unwrap()
            .is_none()
    );
    s.project_invite_budget(&original).await.unwrap();
    s.project_invite_budget(&original).await.unwrap();
    assert_eq!(
        s.get_invite_budget(original.owner()).await.unwrap(),
        Some(original.clone())
    );
    assert_eq!(
        s.list_invite_budget_buckets(original.owner())
            .await
            .unwrap(),
        original.buckets
    );
    let mut changed_counts = original.clone();
    changed_counts.limit_counts.spammed += 1;
    assert!(matches!(
        s.project_invite_budget(&changed_counts).await,
        Err(Error::ProjectionInvariant(_))
    ));
    assert_eq!(
        s.get_invite_budget(original.owner()).await.unwrap(),
        Some(original.clone())
    );
    let mut multi = original.clone();
    multi.account_id = 8;
    multi.buckets.clear();
    for repo_id in [10, 20, 100] {
        let mut row = original.buckets[0].clone();
        row.bucket = BucketId::Repo { repo_id };
        multi.buckets.push(row);
    }
    s.project_invite_budget(&multi).await.unwrap();
    assert_eq!(
        s.list_invite_budget_buckets(multi.owner()).await.unwrap(),
        multi.buckets
    );
    // A successful PUT204 changes only invitation usage: content counts and
    // spacing survive projection replay and an older pre-refund snapshot.
    let mut charged = multi.clone();
    charged.account_id = 9;
    s.project_invite_budget(&charged).await.unwrap();
    let mut refunded = charged.clone();
    refunded.revision = 2;
    refunded.buckets[0].used_24h -= 1;
    s.project_invite_budget(&refunded).await.unwrap();
    s.project_invite_budget(&refunded).await.unwrap();
    s.project_invite_budget(&charged).await.unwrap();
    assert_eq!(
        s.get_invite_budget(refunded.owner()).await.unwrap(),
        Some(refunded.clone())
    );
    assert_eq!(
        s.list_invite_budget_buckets(refunded.owner())
            .await
            .unwrap(),
        refunded.buckets
    );
    let mut conflict = original.clone();
    conflict.buckets[0].used_24h = 13;
    assert!(matches!(
        s.project_invite_budget(&conflict).await,
        Err(Error::ProjectionInvariant(_))
    ));
    assert_eq!(
        s.list_invite_budget_buckets(original.owner())
            .await
            .unwrap(),
        original.buckets
    );
    let mut newer = original.clone();
    newer.revision = 2;
    newer.buckets.clear();
    newer.content_closed_until = None;
    newer.observed_at = at + chrono::Duration::days(1);
    newer.limit_counts.spammed = 0;
    newer.limit_counts.rate_limited = 0;
    newer.limit_counts.incomplete = false;
    let legacy_json = encode(&newer).unwrap();
    assert!(!legacy_json.contains("content_closed_until"));
    assert_eq!(
        serde_json::from_str::<BudgetSnapshot>(&legacy_json).unwrap(),
        newer
    );
    let mut legacy = newer.clone();
    legacy.limit_counts = Default::default();
    let legacy_json = encode(&legacy).unwrap();
    assert!(!legacy_json.contains("limit_counts"));
    assert_eq!(
        serde_json::from_str::<BudgetSnapshot>(&legacy_json).unwrap(),
        legacy
    );
    s.project_invite_budget(&newer).await.unwrap();
    s.project_invite_budget(&original).await.unwrap();
    assert!(
        s.list_invite_budget_buckets(original.owner())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        s.get_invite_budget(original.owner()).await.unwrap(),
        Some(newer)
    );
    original.app = App::Org;
    original.buckets[0].bucket = BucketId::Org;
    s.project_invite_budget(&original).await.unwrap();
    assert_eq!(
        s.list_invite_budget_buckets(original.owner())
            .await
            .unwrap(),
        original.buckets
    );
    let mut raised = original.clone();
    raised.revision = 2;
    raised.buckets[0].cap = 500;
    raised.confirmed_established = true;
    s.project_invite_budget(&raised).await.unwrap();
    assert_eq!(
        s.get_invite_budget(raised.owner()).await.unwrap(),
        Some(raised.clone())
    );
    let mut invalid = raised.clone();
    invalid.revision = 3;
    invalid.account_id = integer::MAX_EXACT + 1;
    assert!(s.project_invite_budget(&invalid).await.is_err());
    invalid = raised.clone();
    invalid.revision = 3;
    invalid.buckets.push(invalid.buckets[0].clone());
    assert!(s.project_invite_budget(&invalid).await.is_err());
    assert_eq!(
        s.get_invite_budget(raised.owner()).await.unwrap(),
        Some(raised)
    );
}
