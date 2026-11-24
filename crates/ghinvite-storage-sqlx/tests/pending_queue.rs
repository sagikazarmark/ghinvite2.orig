use ghinvite_core::storage::pending_queue::PendingBoundary;
use ghinvite_core::storage::projection::fixture;
use ghinvite_core::storage::{AppInstallationStorage, ConsoleStorage, RecordStorage};
use ghinvite_core::{
    Account, AccountType, InvitationLink, InvitationLinkId, InvitationRequest, RequestId,
    RequestState, SelectedRepos, User,
};
use ghinvite_storage_sqlx::SqlxStorage;

static QUERY_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Clone)]
struct QueryCount(std::sync::Arc<std::sync::atomic::AtomicUsize>);
impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for QueryCount {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        if event.metadata().target() == "sqlx::query" {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

#[tokio::test]
async fn pages_are_bounded_and_seek_past_terminal_transitions_with_repeated_context() {
    let _lock = QUERY_TEST_LOCK.lock().await;
    use tracing_subscriber::prelude::*;
    let count = QueryCount(Default::default());
    tracing::subscriber::set_global_default(tracing_subscriber::registry().with(count.clone()))
        .unwrap();
    let storage = SqlxStorage::in_memory().await.unwrap();
    let now = "2026-01-01T00:00:00Z".parse().unwrap();
    storage
        .put_app_installation(&ghinvite_core::account::Installation::repo(
            (Account {
                installation_id: 1,
                account_id: 42,
                account_login: "acme".into(),
                account_type: AccountType::User,
                installed_at: now,
                uninstalled_at: None,
                selected_repos: SelectedRepos::All,
            })
            .clone(),
        ))
        .await
        .unwrap();
    let link = InvitationLink {
        batch_source: None,
        access_lifetime_seconds: None,
        email_domains: vec![],
        id: InvitationLinkId::new(),
        organization_installation_id: None,
        installation_id: 1,
        account_id: 42,
        created_by: 1,
        created_at: now,
        expires_at: None,
        max_uses: None,
        invitation_lifetime_seconds: 604800,
        abuse_threshold_per_hour: None,
        budget_share_pct: 50,
        uses_count: 0,

        approval_required: true,
        description: "Workshop".into(),
        internal_note: None,
        revoked_at: None,
        revoked_by: None,
        pause: None,
        targets: ghinvite_core::targets::Targets {
            repos: vec![ghinvite_core::targets::RepoTarget {
                repo_id: 10,
                repo_full_name: "acme/api".into(),
                grant: ghinvite_core::targets::RepoRole::Read,
            }],
            ..Default::default()
        },

        secret: ghinvite_core::LinkSecretState {
            secret_generation: 1,
            sha256: ghinvite_core::SecretHash::from_bytes([1; 32]),
        },
    };
    for user_id in 1..=53 {
        storage
            .upsert_user(&User {
                user_id,
                login: format!("user-{user_id}"),
                avatar_url: None,
                github_created_at: None,
                last_seen_at: now,
            })
            .await
            .unwrap();
    }
    let mut requests = Vec::new();
    for n in (1..=53).rev() {
        let id: RequestId = format!("01ARZ3NDEKTSV4RRFFQ69G5{:03}", n).parse().unwrap();
        let request = InvitationRequest {
            batch: None,
            domain_evidence: None,
            id,
            invitation_link_id: Some(link.id),
            requester_id: n,
            justification: None,
            state: RequestState::Pending,
            decided_by: None,
            decided_at: None,
            cancellation_reason: None,
            decline_reason: None,
            requester_message: None,
            created_at: now,
            decision_deadline: Some(now),
        };
        fixture::seed(&storage, &link, std::slice::from_ref(&request), 1)
            .await
            .unwrap();
        requests.push(request);
    }
    let mut ids: Vec<RequestId> = requests.iter().map(|r| r.id).collect();
    ids.sort_by_key(|id| id.to_string());
    count.0.store(0, std::sync::atomic::Ordering::Relaxed);
    let first = storage.pending_request_page(42, None).await.unwrap();
    assert_eq!(
        count.0.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "one SQL statement regardless of page size"
    );
    count.0.store(0, std::sync::atomic::Ordering::Relaxed);
    let history = storage.request_history(42, link.id, None).await.unwrap();
    assert_eq!(
        count.0.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "history includes requester profiles in one SQL statement"
    );
    assert_eq!(history.requests.len(), 25);
    assert_eq!(history.requester_logins.len(), 25);
    assert_eq!(
        history.requester_logins.get(&53).map(String::as_str),
        Some("user-53")
    );
    assert_eq!(
        first.rows.iter().map(|r| r.request_id).collect::<Vec<_>>(),
        ids[..25]
    );
    assert!(first.next.is_some());
    assert!(first.rows.iter().all(|r| {
        r.link_description.as_deref() == Some("Workshop")
            && r.targets
                .as_ref()
                .is_some_and(|t| t.repos.len() == 1 && t.repos[0].repo_full_name == "acme/api")
            && r.decision_deadline == Some(now)
    }));
    assert!(
        storage
            .pending_request_page(43, None)
            .await
            .unwrap()
            .rows
            .is_empty()
    );
    for id in &ids[..26] {
        let mut request = requests.iter().find(|r| r.id == *id).unwrap().clone();
        request.state = RequestState::Declined;
        request.decided_by = Some(1);
        request.decided_at = Some(now);
        fixture::seed(&storage, &link, &[request], 2).await.unwrap();
    }
    let second = storage.pending_request_page(42, first.next).await.unwrap();
    assert_eq!(
        second.rows.iter().map(|r| r.request_id).collect::<Vec<_>>(),
        ids[26..51]
    );
    let third = storage.pending_request_page(42, second.next).await.unwrap();
    assert_eq!(
        third.rows.iter().map(|r| r.request_id).collect::<Vec<_>>(),
        ids[51..]
    );
    assert_eq!(third.next, None);
    assert!(
        storage
            .pending_request_page(
                42,
                Some(PendingBoundary {
                    created_at: now,
                    request_id: ids[52]
                })
            )
            .await
            .unwrap()
            .rows
            .is_empty()
    );
}

#[tokio::test]
async fn queue_index_seeks_exact_times_and_missing_context_or_failed_reads_stay_truthful() {
    let _lock = QUERY_TEST_LOCK.lock().await;
    use ghinvite_core::storage::pending_queue;
    let path = std::env::temp_dir().join(format!("queue-{}.sqlite", RequestId::new()));
    let storage = SqlxStorage::at_path(&path).await.unwrap();
    let db = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!("sqlite://{}", path.display()))
        .await
        .unwrap();
    // External storage boundary: alternate time encodings, corruption, and
    // missing rows cannot be written through the domain's validated interface.
    sqlx::raw_sql("PRAGMA foreign_keys=OFF;
        INSERT INTO installations(installation_id,account_id,account_login,account_type,installed_at,uninstalled_at,selected_repos) VALUES(1,42,'acme','User','2026-01-01T00:00:00Z',NULL,'[]');
        INSERT INTO invitation_links(id,installation_id,account_id,created_by,created_at,approval_required,description,projection_revision,projection_content,secret_generation,secret_sha256)
        VALUES('01ARZ3NDEKTSV4RRFFQ69G5FAV',1,42,1,'2026-01-01T00:00:00Z',1,'Workshop',1,'{}',1,'0101010101010101010101010101010101010101010101010101010101010101');")
        .execute(&db).await.unwrap();
    for (id, time) in [
        ("001", "2026-01-01T00:00:00.000000001Z"),
        ("002", "2026-01-01T00:00:00+00:00"),
        ("003", "2026-01-01T00:00:00Z"),
    ] {
        sqlx::query("INSERT INTO invitation_requests(id,invitation_link_id,requester_id,state,created_at,projection_revision) VALUES(?,'01ARZ3NDEKTSV4RRFFQ69G5FAV',99,'pending',?,1)")
            .bind(format!("01ARZ3NDEKTSV4RRFFQ69G5{id}")).bind(time).execute(&db).await.unwrap();
    }
    let page = storage.pending_request_page(42, None).await.unwrap();
    assert_eq!(
        page.rows
            .iter()
            .map(|r| r.request_id.to_string())
            .collect::<Vec<_>>(),
        [
            "01ARZ3NDEKTSV4RRFFQ69G5002",
            "01ARZ3NDEKTSV4RRFFQ69G5003",
            "01ARZ3NDEKTSV4RRFFQ69G5001"
        ]
    );
    assert!(
        page.rows
            .iter()
            .all(|r| r.requester_login == "user-99" && r.targets.is_none())
    );
    let boundary = PendingBoundary {
        created_at: page.rows[1].created_at,
        request_id: page.rows[1].request_id,
    };
    assert_eq!(
        storage
            .pending_request_page(42, Some(boundary))
            .await
            .unwrap()
            .rows
            .len(),
        1
    );
    for after in [false, true] {
        let plan: Vec<(i64, i64, i64, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "EXPLAIN QUERY PLAN {}",
            pending_queue::query(after)
        )))
        .bind(42)
        .bind(after.then(|| boundary.seek_key()))
        .fetch_all(&db)
        .await
        .unwrap();
        let detail = plan
            .iter()
            .map(|r| r.3.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            detail.contains("SEARCH r USING INDEX idx_pending_queue_seek (queue_account_id=?"),
            "{detail}"
        );
        if after {
            assert!(detail.contains("<expr>>?"), "{detail}");
        }
        // The candidate request set is never sorted: the `page` CTE (its plan
        // runs up to `MATERIALIZE links`) reads the seek index in order. Only
        // the bounded final page may be: SQLite 3.46 sorts it, 3.54 streams
        // the already ordered materialized page.
        let page_plan = detail.split("MATERIALIZE links").next().unwrap();
        assert!(
            !page_plan.contains("USE TEMP B-TREE FOR ORDER BY"),
            "{detail}"
        );
        assert!(
            detail.matches("USE TEMP B-TREE FOR ORDER BY").count() <= 1,
            "{detail}"
        );
    }
    let plan: Vec<(i64, i64, i64, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "EXPLAIN QUERY PLAN {}",
        pending_queue::COUNT_QUERY
    )))
    .bind(42)
    .fetch_all(&db)
    .await
    .unwrap();
    let detail = plan
        .iter()
        .map(|r| r.3.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        detail.contains("SEARCH r USING INDEX idx_pending_queue_seek (queue_account_id=?)"),
        "the overview count seeks the pending index: {detail}"
    );
    sqlx::query("INSERT INTO invitation_link_targets VALUES('01ARZ3NDEKTSV4RRFFQ69G5FAV','repo',10,'acme/api','read')").execute(&db).await.unwrap();
    sqlx::raw_sql("PRAGMA ignore_check_constraints=ON; UPDATE invitation_link_targets SET grant='corrupt'; PRAGMA ignore_check_constraints=OFF")
        .execute(&db)
        .await
        .unwrap();
    assert!(matches!(
        storage.pending_request_page(42, None).await,
        Err(ghinvite_core::storage::Error::Corrupt(_))
    ));
    sqlx::raw_sql("UPDATE invitation_link_targets SET grant='read'; UPDATE invitation_links SET account_id=43")
        .execute(&db)
        .await
        .unwrap();
    assert!(
        storage
            .pending_request_page(42, None)
            .await
            .unwrap()
            .rows
            .is_empty()
    );
    assert_eq!(
        storage
            .pending_request_page(43, None)
            .await
            .unwrap()
            .rows
            .len(),
        3
    );
    let count = |account| storage.count_pending_requests_for_account(account);
    assert_eq!((count(42).await.unwrap(), count(43).await.unwrap()), (0, 3));
    sqlx::query("DELETE FROM invitation_links")
        .execute(&db)
        .await
        .unwrap();
    assert!(
        storage
            .pending_request_page(43, None)
            .await
            .unwrap()
            .rows
            .is_empty(),
        "orphan ownership cannot be verified"
    );
    assert_eq!(count(43).await.unwrap(), 0, "orphans are not counted");
    sqlx::query("UPDATE invitation_requests SET queue_account_id=NULL")
        .execute(&db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO invitation_links(id,installation_id,account_id,created_by,created_at,approval_required,description,projection_revision,projection_content,secret_generation,secret_sha256) VALUES('01ARZ3NDEKTSV4RRFFQ69G5FAV',1,43,1,'2026-01-01T00:00:00Z',1,'Restored',1,'{}',1,'0101010101010101010101010101010101010101010101010101010101010101')").execute(&db).await.unwrap();
    assert_eq!(
        storage
            .pending_request_page(43, None)
            .await
            .unwrap()
            .rows
            .len(),
        3
    );
    assert_eq!(
        count(43).await.unwrap(),
        3,
        "restored owner is counted again"
    );
    sqlx::query("DROP TABLE users").execute(&db).await.unwrap();
    assert!(matches!(
        storage.pending_request_page(42, None).await,
        Err(ghinvite_core::storage::Error::Database(_))
    ));
    db.close().await;
    drop(storage);
    std::fs::remove_file(path).unwrap();
}
