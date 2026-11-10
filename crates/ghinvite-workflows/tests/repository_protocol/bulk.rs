//! Real batch coordinator, ordinary request/delivery owners and shared SQLite.
use super::*;
use ghinvite_core::{
    bulk::{
        BatchRef, BulkBatchId, Entry, Manifest, Resolution, Review, RowEvidence, RowRequest,
        Submission,
    },
    bulk_commands::{BatchProgress, BatchQuery, CreateBatch, EditBatch, SubmitBatch},
    invitation_link::AccountAdmin,
    storage::bulk::BulkStorage,
    targets::{RepoTarget, Targets},
};

#[path = "bulk_console.rs"]
mod console;

pub(super) fn review(batch_id: BulkBatchId, at: chrono::DateTime<Utc>) -> Review {
    Review::new(
        Manifest {
            batch_id,
            admin: AccountAdmin {
                account_id: 100,
                user_id: 7,
            },
            installation_id: 1,
            organization_installation_id: None,
            targets: Targets {
                repos: vec![RepoTarget {
                    repo_id: 10,
                    repo_full_name: "acme/api".into(),
                    grant: RepoRole::Write,
                }],
                ..Targets::default()
            },
            access_lifetime_seconds: None,
            entries: vec![
                Entry::Handle {
                    login: "alice".into(),
                },
                Entry::Handle {
                    login: "alias".into(),
                },
                Entry::Handle {
                    login: "missing".into(),
                },
            ],
        },
        vec![
            RowEvidence {
                previews: Vec::new(),
                row: 1,
                resolution: Resolution::Found {
                    user_id: 8,
                    login: "alice".into(),
                },
                all_targets_satisfied: false,
                observed_at: at,
                next_budget_slot: None,
            },
            RowEvidence {
                previews: Vec::new(),
                row: 2,
                resolution: Resolution::Found {
                    user_id: 8,
                    login: "alice".into(),
                },
                all_targets_satisfied: false,
                observed_at: at,
                next_budget_slot: None,
            },
            RowEvidence {
                previews: Vec::new(),
                row: 3,
                resolution: Resolution::NotFound,
                all_targets_satisfied: false,
                observed_at: at,
                next_budget_slot: None,
            },
        ],
        at,
    )
    .unwrap()
}
struct BulkFixture {
    h: Harness,
    storage: Arc<ghinvite_storage_sqlx::SqlxStorage>,
    github: Arc<Github>,
    database: std::path::PathBuf,
}
impl BulkFixture {
    async fn new() -> Self {
        Self::with_reissues(false).await
    }
    async fn with_reissues(reissues: bool) -> Self {
        Self::options(reissues, None).await
    }
    async fn options(
        reissues: bool,
        request_faults: Option<Arc<ghinvite_workflows::request_owner::Faults>>,
    ) -> Self {
        let database =
            std::env::temp_dir().join(format!("bulk-protocol-{}.sqlite", RequestId::new()));
        let storage = Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::at_path(&database)
                .await
                .unwrap(),
        );
        let sql = sqlx::SqlitePool::connect_with(
            sqlx::sqlite::SqliteConnectOptions::new().filename(&database),
        )
        .await
        .unwrap();
        let github_state = Arc::new(Github {
            scenario: Mutex::new(Scenario::default()),
            sql,
        });
        let h = Harness::start_with_github_router(
            axum::Router::new()
                .fallback(github)
                .with_state(github_state.clone()),
        )
        .await;
        for (user_id, login) in [(7, "creator"), (9, "submitting-admin")] {
            storage
                .upsert_user(&ghinvite_core::User {
                    user_id,
                    login: login.into(),
                    avatar_url: None,
                    github_created_at: None,
                    last_seen_at: Utc::now(),
                })
                .await
                .unwrap();
        }
        let github_transport = ghinvite_e2e::github_transport(h.client.clone());
        let state = ghinvite_workflows::AppState::new(storage.clone())
            .with_github_apps(Arc::new(
                ghinvite_workflows::RepositoryGithub::new(
                    github_transport,
                    &h.github_url,
                    ghinvite_e2e::repository_apps(&h.github_url),
                )
                .unwrap(),
            ))
            .with_bulk_invites(storage.clone());
        let state = if reissues {
            state.with_reissues(storage.clone())
        } else {
            state
        };
        let mut state = state;
        state.request_faults = request_faults;
        h.restate
            .deploy(ghinvite_workflows::build_endpoint(state, storage.clone(), None).unwrap())
            .await;
        JournalAudit::retain(
            &h,
            &[
                "BulkInviteBatch",
                "BulkProjection",
                "InvitationRequest",
                "RequestProjection",
                "RepositoryDelivery",
                "DeliveryProjection",
                "InviteBudget",
            ],
        )
        .await;
        let setup = ghinvite_e2e::setup_command(
            json!({"installation_id":1,"account_id":100,"account_login":"acme","account_type":"Organization","selected_repos":"all","installed_at":Utc::now()}),
        );
        let r = h
            .restate
            .invoke(
                &Call::object("Installation", "repo:1", "onboard"),
                Some(&setup),
                None,
            )
            .await;
        assert_eq!(r.status, 200, "{r:?}");
        Self {
            h,
            storage,
            github: github_state,
            database,
        }
    }
    async fn call<T: serde::Serialize>(
        &self,
        batch: BulkBatchId,
        handler: &str,
        input: &T,
        status: u16,
    ) -> Value {
        let r = self
            .h
            .restate
            .invoke(
                &Call::object("BulkInviteBatch", &batch.to_string(), handler),
                Some(&serde_json::to_value(input).unwrap()),
                None,
            )
            .await;
        assert_eq!(r.status, status, "{r:?}");
        if status == 200 {
            let journal = self.h.restate.admin().journal(r.invocation_id()).await;
            assert!(!journal.is_empty());
            assert!(
                journal.iter().all(|entry| !entry.raw_contains(MARKER)),
                "token marker reached batch journal"
            );
        }
        r.body
    }
    async fn wait_delivery(&self, query: &BatchQuery) -> BatchProgress {
        tokio::time::timeout(Duration::from_secs(20),async {
            loop {
                let p:BatchProgress=serde_json::from_value(self.call(query.batch_id,"progress",query,200).await).unwrap();
                if let Some(row)=p.requests.first()
                    && matches!(row.repositories.first().map(|p|&p.observation),Some(ghinvite_core::bulk_commands::Observation::Observed(s)) if matches!(s.create.outcome,CreateOutcome::Created{..})) {break p;}

                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }).await.expect("ordinary batch delivery did not complete")
    }
    async fn finish(self) {
        self.h.finish().await;
        self.github.sql.close().await;
        drop(self.storage);
        std::fs::remove_file(&self.database).unwrap();
    }
}
fn operation() -> ghinvite_core::request_lifecycle::LifecycleOperationId {
    RequestId::new().to_string().try_into().unwrap()
}

#[tokio::test]
async fn bulk_review_submit_uses_exact_linkless_request_and_original_operation_receipts() {
    let f = BulkFixture::new().await;
    let batch = BulkBatchId::new();
    let initial = review(batch, Utc::now());
    let create = CreateBatch {
        operation_id: operation(),
        review: initial.clone(),
    };
    f.call(batch, "create", &create, 200).await;
    f.call(batch, "create", &create, 200).await;
    assert!(
        !f.github
            .scenario
            .lock()
            .unwrap()
            .trace
            .iter()
            .any(|t| t.path == PUT_PATH),
        "review must not issue a collaborator write"
    );
    let query = BatchQuery {
        batch_id: batch,
        admin: AccountAdmin {
            account_id: 100,
            user_id: 9,
        },
    };
    let request_id = RequestId::new();
    let submit = SubmitBatch {
        query: query.clone(),
        operation_id: operation(),
        expected_revision: 1,
        submission: Submission {
            submitted_by: 9,
            requests: vec![RowRequest {
                source: BatchRef {
                    batch_id: batch,
                    row: 1,
                },
                request_id,
                requester_id: 8,
                requester_login: "alice".into(),
            }],
            emails: vec![],
            personal_links: vec![],
            submitted_at: Utc::now(),
        },
    };
    // A not-found row must be removed in the mandatory review first.
    f.call(batch, "submit", &submit, 409).await;
    assert!(
        !f.github
            .scenario
            .lock()
            .unwrap()
            .trace
            .iter()
            .any(|t| t.path == PUT_PATH)
    );
    let edit = EditBatch {
        query: query.clone(),
        operation_id: operation(),
        expected_revision: 1,
        remove: std::collections::BTreeSet::from([3]),
        personal_link_rows: Default::default(),
    };
    let edited = f.call(batch, "remove_rows", &edit, 200).await;
    let submit = SubmitBatch {
        operation_id: operation(),
        expected_revision: 2,
        ..submit
    };
    let accepted = f.call(batch, "submit", &submit, 200).await;
    assert_eq!(f.call(batch, "submit", &submit, 200).await, accepted);
    let p = f.wait_delivery(&query).await;
    assert_eq!(
        p.requests.len(),
        1,
        "duplicate and removed rows create no requests"
    );
    assert!(!p.handoff_pending);
    assert_eq!(p.review.manifest.admin.user_id, 7);
    assert_eq!(p.review.submitted.as_ref().unwrap().submitted_by, 9);
    assert_eq!(
        f.call(batch, "remove_rows", &edit, 200).await,
        edited,
        "an old exact operation returns its original receipt after Submit"
    );
    assert_eq!(
        f.call(batch, "create", &create, 200).await,
        serde_json::to_value(&initial).unwrap()
    );
    let mut changed = submit.clone();
    changed.submission.requests[0].request_id = RequestId::new();
    f.call(batch, "submit", &changed, 409).await;
    assert_eq!(
        f.github
            .scenario
            .lock()
            .unwrap()
            .trace
            .iter()
            .filter(|t| t.method == "PUT" && t.path == PUT_PATH)
            .count(),
        1
    );
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(r) = f.storage.get_invitation_request(request_id).await.unwrap() {
                assert_eq!(r.invitation_link_id, None);
                assert_eq!(
                    r.batch,
                    Some(BatchRef {
                        batch_id: batch,
                        row: 1
                    })
                );
                assert_eq!(r.decided_by, Some(9));
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let links: i64 = sqlx::query_scalar("SELECT count(*) FROM invitation_links")
        .fetch_one(&f.github.sql)
        .await
        .unwrap();
    assert_eq!(
        links, 0,
        "batch issuance consumes no link use and creates no synthetic link"
    );
    let linked_fences:i64=sqlx::query_scalar("SELECT count(*) FROM delivery_attempts WHERE step='repo' AND json_extract(command,'$.link_id') IS NOT NULL").fetch_one(&f.github.sql).await.unwrap();
    assert_eq!(
        linked_fences, 0,
        "batch create fence has no link attribution"
    );
    let fenced: i64 =
        sqlx::query_scalar("SELECT count(*) FROM delivery_attempts WHERE step='repo'")
            .fetch_one(&f.github.sql)
            .await
            .unwrap();
    assert_eq!(fenced, 1);
    if let ghinvite_core::bulk_commands::Observation::Observed(snapshot) =
        &p.requests[0].repositories[0].observation
    {
        assert_eq!(
            snapshot.create.command.budget_link_id(),
            None,
            "batch delivery uses the unshared account remainder"
        );
    } else {
        panic!("delivery owner must have a confirmed snapshot");
    }
    let retained = f.storage.bulk_review(100, batch).await.unwrap().unwrap();
    assert_eq!(retained, p.review);
    f.finish().await;
}

#[tokio::test]
async fn bulk_review_refuses_changed_creation_actor_and_stale_edits_before_writes() {
    let f = BulkFixture::new().await;
    let batch = BulkBatchId::new();
    let create = CreateBatch {
        operation_id: operation(),
        review: review(batch, Utc::now()),
    };
    f.call(batch, "create", &create, 200).await;
    let query = BatchQuery {
        batch_id: batch,
        admin: AccountAdmin {
            account_id: 100,
            user_id: 9,
        },
    };
    let mut wrong = query.clone();
    wrong.admin.account_id = 101;
    f.call(batch, "progress", &wrong, 404).await;
    let mut changed = create.clone();
    changed.review.manifest.admin.user_id = 9;
    f.call(batch, "create", &changed, 409).await;
    let edit = EditBatch {
        query: query.clone(),
        operation_id: operation(),
        expected_revision: 1,
        remove: std::collections::BTreeSet::from([3]),
        personal_link_rows: Default::default(),
    };
    f.call(batch, "remove_rows", &edit, 200).await;
    let stale = EditBatch {
        operation_id: operation(),
        ..edit.clone()
    };
    f.call(batch, "remove_rows", &stale, 409).await;
    let mut corrected_under_refused_identity = stale.clone();
    corrected_under_refused_identity.expected_revision = 2;
    corrected_under_refused_identity.remove.insert(1);
    f.call(batch, "remove_rows", &corrected_under_refused_identity, 409)
        .await;
    f.call(batch, "remove_rows", &stale, 409).await;
    let mut conflict = edit;
    conflict.remove.insert(1);
    f.call(batch, "remove_rows", &conflict, 409).await;
    assert!(
        !f.github
            .scenario
            .lock()
            .unwrap()
            .trace
            .iter()
            .any(|t| t.path == PUT_PATH)
    );
    let requests: i64 = sqlx::query_scalar("SELECT count(*) FROM invitation_requests")
        .fetch_one(&f.github.sql)
        .await
        .unwrap();
    assert_eq!(requests, 0);
    f.finish().await;
}

#[path = "bulk_cancel.rs"]
mod cancel;

#[path = "bulk_progress.rs"]
mod progress;

#[path = "bulk_items.rs"]
mod items;
