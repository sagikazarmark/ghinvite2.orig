use super::*;
use crate::bulk_review::{BulkReviewService, ReviewError};
use ghinvite_core::{
    bulk::{BulkBatchId, Entry, Manifest, Resolution, Review, ReviewStatus},
    bulk_preview::{ObservedAccess, PreviewAction},
    invitation_link::AccountAdmin,
};
fn manifest(entries: Vec<Entry>) -> Manifest {
    Manifest {
        batch_id: BulkBatchId::new(),
        admin: AccountAdmin {
            account_id: 100,
            user_id: 7,
        },
        installation_id: 1,
        organization_installation_id: None,
        targets: targets(&[1]),
        access_lifetime_seconds: None,
        entries,
    }
}
fn handle(login: &str) -> Entry {
    Entry::Handle {
        login: login.into(),
    }
}
fn user(login: &str, id: u64) -> Expectation {
    Expectation::ok_json(
        Method::Get,
        format!("https://api.github.com/users/{login}"),
        json!({"id":id,"login":login}),
    )
}
fn admin_permission() -> Expectation {
    Expectation::ok_json(
        Method::Get,
        "https://api.github.com/repos/acme/r1/collaborators/installer/permission",
        json!({"user":{"id":9,"login":"installer"},"role_name":"admin"}),
    )
}
async fn budget() -> (
    Arc<crate::RestateClient>,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let calls = Arc::new(AtomicUsize::new(0));
    let capture = calls.clone();
    let app=axum::Router::new().fallback(move |request:axum::extract::Request| {let calls=capture.clone();async move {
        assert_eq!(request.method(),axum::http::Method::POST);
        assert!(request.uri().path().starts_with("/InviteBudget/"));assert!(request.uri().path().ends_with("/snapshot"));
        calls.fetch_add(1,Ordering::SeqCst);let now=chrono::Utc::now();
        axum::Json(json!({"app":"repo","account_id":100,"revision":1,"observed_at":now,"confirmed_established":false,"buckets":[{"bucket":{"kind":"repo","repo_id":1},"used_24h":50,"cap":50,"closed_until":null,"next_slot_at":now+chrono::Duration::hours(1)}],"content_used_minute":0,"content_used_hour":0,"next_content_write_at":null}))
    }});
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (
        Arc::new(crate::RestateClient::new(format!("http://{address}")).unwrap()),
        calls,
        task,
    )
}
#[tokio::test]
async fn cap_and_current_actor_binding_reject_before_any_lookup_or_budget_rpc() {
    let (state, _, mock, limit) = fixture(vec![], false).await;
    let view = AuthorityView::open(&state, 9, "installer", &account())
        .await
        .unwrap()
        .unwrap();
    let (ingress, calls, task) = budget().await;
    let service = BulkReviewService::new(view, ingress);
    let admin = AccountAdmin {
        account_id: 100,
        user_id: 9,
    };
    assert_eq!(
        service
            .review(manifest(vec![handle("alice"); 201]), &admin)
            .await
            .unwrap_err(),
        ReviewError::Invalid
    );
    assert_eq!(
        service
            .review(
                manifest(vec![handle("alice")]),
                &AccountAdmin {
                    account_id: 100,
                    user_id: 8
                }
            )
            .await
            .unwrap_err(),
        ReviewError::Denied
    );
    let mut stale = manifest(vec![handle("alice")]);
    stale.installation_id = 999;
    assert_eq!(
        service.review(stale, &admin).await.unwrap_err(),
        ReviewError::Denied
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(limit.calls.load(Ordering::SeqCst), 0);
    mock.assert_exhausted();
    task.abort();
}
#[tokio::test]
async fn real_budget_read_numeric_dedup_and_distinct_failed_lookup_preserve_target_preview() {
    let mut missing = user("missing", 10);
    missing.response.status = 404;
    let mut unavailable = user("unavailable", 11);
    unavailable.response.status = 503;
    let (state, _, mock, _) = fixture(
        vec![
            page(&[1]),
            identity(1, 100),
            admin_permission(),
            user("alice", 8),
            permission(1, 8, "read"),
            user("renamed", 8),
            missing,
            unavailable,
        ],
        false,
    )
    .await;
    let view = AuthorityView::open(&state, 9, "installer", &account())
        .await
        .unwrap()
        .unwrap();
    let (ingress, calls, task) = budget().await;
    let service = BulkReviewService::new(view, ingress);
    let input = manifest(vec![
        handle("alice"),
        handle("ALICE"),
        handle("renamed"),
        handle("missing"),
        handle("unavailable"),
    ]);
    let rows = service
        .review(
            input.clone(),
            &AccountAdmin {
                account_id: 100,
                user_id: 9,
            },
        )
        .await
        .unwrap();
    assert_eq!(rows[0].previews.len(), 1);
    assert_eq!(rows[0].previews[0].action, PreviewAction::WillRaise);
    assert!(matches!(
        rows[0].previews[0].observed,
        ObservedAccess::Repository {
            role: ghinvite_core::targets::EffectiveRole::Read
        }
    ));
    assert_eq!(rows[1].previews, rows[0].previews);
    assert_eq!(rows[2].previews, rows[0].previews);
    assert_eq!(rows[3].resolution, Resolution::NotFound);
    assert_eq!(rows[4].resolution, Resolution::Unavailable);
    assert!(rows[0].next_budget_slot.unwrap() > chrono::Utc::now());
    assert!(!rows[0].all_targets_satisfied);
    let review = Review::new(input, rows, chrono::Utc::now()).unwrap();
    assert_eq!(review.rows[1].status, ReviewStatus::Duplicate { of_row: 1 });
    assert_eq!(review.rows[2].status, ReviewStatus::Duplicate { of_row: 1 });
    assert_eq!(review.rows[4].status, ReviewStatus::Unavailable);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    mock.assert_exhausted();
    task.abort();
}
#[tokio::test]
async fn mismatched_numeric_permission_user_is_unavailable_not_satisfied() {
    let (state, _, mock, _) = fixture(
        vec![
            page(&[1]),
            identity(1, 100),
            admin_permission(),
            user("alice", 8),
            permission(1, 99, "admin"),
        ],
        false,
    )
    .await;
    let view = AuthorityView::open(&state, 9, "installer", &account())
        .await
        .unwrap()
        .unwrap();
    let (ingress, _, task) = budget().await;
    let input = manifest(vec![handle("alice")]);
    let rows = BulkReviewService::new(view, ingress)
        .review(
            input.clone(),
            &AccountAdmin {
                account_id: 100,
                user_id: 9,
            },
        )
        .await
        .unwrap();
    assert_eq!(rows[0].previews[0].action, PreviewAction::Unavailable);
    assert!(!rows[0].all_targets_satisfied);
    assert_eq!(
        Review::new(input, rows, chrono::Utc::now()).unwrap().rows[0].status,
        ReviewStatus::Unavailable
    );
    mock.assert_exhausted();
    task.abort();
}
