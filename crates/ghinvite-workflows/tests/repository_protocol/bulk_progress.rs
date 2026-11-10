//! Real Bulk owner/journal paging; observation peers deliberately report pending
//! so the test measures fanout without manufacturing successful deliveries.
use super::*;
use ghinvite_core::{
    bulk_commands::{Observation, ProgressCursor, ProgressPageQuery},
    request_lifecycle::{AdminRequestStatus, ApprovedDispatch, InitializeRequest},
};
use restate_sdk::{
    context::{ContextReadState, ContextWriteState, ObjectContext, SharedObjectContext},
    errors::TerminalError,
    serde::Json,
};
use std::sync::atomic::{AtomicUsize, Ordering};
struct InvitationRequest {
    calls: Arc<AtomicUsize>,
}
#[restate_sdk::object]
impl InvitationRequest {
    #[handler]
    async fn initialize_batch(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<InitializeRequest>,
    ) -> Result<(), TerminalError> {
        let commands = input
            .targets
            .repos
            .iter()
            .map(|target| ghinvite_core::delivery::CreateCommand {
                invitation_id: Some(ghinvite_core::GithubInvitationId::new()),
                link_id: None,
                batch: input.request.batch,
                request_id: input.request.request_id,
                approval_id: input.request.decision.as_ref().unwrap().decision_id.clone(),
                account_id: input.request.account_id,
                installation_id: input.installation_id,
                requester_id: input.request.requester_id,
                repo_id: target.repo_id,
                repo_full_name: target.repo_full_name.clone(),
                permission: target.grant.into(),
                approved_at: input.request.admitted_at,
                delivery_generation: 2,
                budget_share_pct: 50,
                access_lifetime_seconds: None,
                budget_attribution: ghinvite_core::reissue::BudgetAttribution::AdministratorReissue,
            })
            .collect();
        ctx.set(
            "plan",
            Json(ApprovedDispatch {
                dispatch_id: format!("dispatch/{}", input.request.request_id),
                input,
                commands,
                organization_command: None,
            }),
        );
        Ok(())
    }
    #[handler]
    async fn admin_dispatch(
        &self,
        ctx: SharedObjectContext<'_>,
        Json(query): Json<AdminRequestStatus>,
    ) -> Result<Json<Option<ApprovedDispatch>>, TerminalError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(query.request_id.to_string(), ctx.key());
        Ok(Json(
            ctx.get::<Json<ApprovedDispatch>>("plan")
                .await?
                .map(|j| j.0),
        ))
    }
}
struct RepositoryDelivery {
    calls: Arc<AtomicUsize>,
}
#[restate_sdk::object]
impl RepositoryDelivery {
    #[handler]
    async fn status(
        &self,
        ctx: SharedObjectContext<'_>,
    ) -> Result<Json<Option<ghinvite_core::delivery::DeliverySnapshot>>, TerminalError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(ctx.key().ends_with(":2"), "read current owner generation");
        Ok(Json(None))
    }
}
async fn call<T: serde::Serialize>(
    h: &Harness,
    batch: BulkBatchId,
    method: &str,
    input: &T,
    status: u16,
) -> (Value, String) {
    let reply = h
        .restate
        .invoke(
            &Call::object("BulkInviteBatch", &batch.to_string(), method),
            Some(&json!(input)),
            None,
        )
        .await;
    assert_eq!(reply.status, status, "{reply:?}");
    let id = reply.invocation_id().to_owned();
    (reply.body, id)
}
#[tokio::test]
async fn bulk_progress_pages_bound_owner_reads_and_preserve_row_target_generation() {
    let h = Harness::start().await;
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let state =
        ghinvite_workflows::AppState::new(storage.clone()).with_bulk_invites(storage.clone());
    let calls = Arc::new(AtomicUsize::new(0));
    h.restate
        .deploy(
            ghinvite_workflows::bulk::bind(
                restate_sdk::endpoint::Endpoint::builder()
                    .bind(InvitationRequest {
                        calls: calls.clone(),
                    })
                    .bind(RepositoryDelivery {
                        calls: calls.clone(),
                    }),
                state,
                storage.clone(),
            )
            .build(),
        )
        .await;
    let batch = BulkBatchId::new();
    let at = Utc::now();
    let base = review(batch, at);
    let mut manifest = base.manifest;
    manifest.targets.repos = (10..110)
        .map(|id| ghinvite_core::targets::RepoTarget {
            repo_id: id,
            repo_full_name: format!("acme/repo-{id}"),
            grant: RepoRole::Write,
        })
        .collect();
    manifest.entries = vec![
        Entry::Handle {
            login: "alice".into(),
        },
        Entry::Handle {
            login: "bob".into(),
        },
    ];
    let evidence = (1..=2)
        .map(|row| RowEvidence {
            previews: vec![],
            row,
            resolution: Resolution::Found {
                user_id: 7 + u64::from(row),
                login: if row == 1 { "alice" } else { "bob" }.into(),
            },
            all_targets_satisfied: false,
            observed_at: at,
            next_budget_slot: None,
        })
        .collect();
    let review = Review::new(manifest, evidence, at).unwrap();
    let query = BatchQuery {
        batch_id: batch,
        admin: review.manifest.admin.clone(),
    };
    call(
        &h,
        batch,
        "create",
        &CreateBatch {
            operation_id: operation(),
            review: review.clone(),
        },
        200,
    )
    .await;
    let submission = Submission {
        submitted_by: 7,
        requests: (1..=2)
            .map(|row| RowRequest {
                source: BatchRef {
                    batch_id: batch,
                    row,
                },
                request_id: RequestId::new(),
                requester_id: 7 + u64::from(row),
                requester_login: if row == 1 { "alice" } else { "bob" }.into(),
            })
            .collect(),
        emails: vec![],
        personal_links: vec![],
        submitted_at: Utc::now(),
    };
    call(
        &h,
        batch,
        "submit",
        &SubmitBatch {
            query: query.clone(),
            operation_id: operation(),
            expected_revision: 1,
            submission: submission.clone(),
        },
        200,
    )
    .await;
    // Synchronize the one-way initialization before counting page observations.
    for row in &submission.requests {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let r = h
                    .restate
                    .invoke(
                        &Call::object(
                            "InvitationRequest",
                            &row.request_id.to_string(),
                            "admin_dispatch",
                        ),
                        Some(&json!(AdminRequestStatus {
                            request_id: row.request_id,
                            admin: query.admin.clone()
                        })),
                        None,
                    )
                    .await;
                if r.status == 200 && !r.body.is_null() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    let mut next = None;
    let mut seen = std::collections::BTreeSet::new();
    let mut pages = 0;
    loop {
        calls.store(0, Ordering::SeqCst);
        let (body, id) = call(
            &h,
            batch,
            "progress_page",
            &ProgressPageQuery {
                query: query.clone(),
                cursor: next,
            },
            200,
        )
        .await;
        let result: BatchProgress = serde_json::from_value(body).unwrap();
        let count = calls.load(Ordering::SeqCst);
        assert!(count <= 64, "{count}");
        let journal = h.restate.admin().journal(&id).await;
        assert_eq!(
            journal
                .iter()
                .filter(|e| e.entry_type == "Command: Call")
                .count(),
            count
        );
        for row in result.requests {
            assert!(matches!(row.initialized, Observation::Observed(())));
            for repo in row.repositories {
                assert_eq!(repo.delivery_generation, 2);
                assert!(matches!(repo.observation, Observation::Pending));
                assert!(
                    seen.insert((row.source.row, repo.repo_id)),
                    "no duplicated or skipped target offset"
                );
            }
            if let Some(offset) = row.next_target_offset {
                assert_eq!(result.next_cursor.unwrap().target_offset, offset);
            }
        }
        pages += 1;
        next = result.next_cursor;
        if next.is_none() {
            break;
        }
    }
    assert_eq!(seen.len(), 200);
    assert_eq!(pages, 4);
    calls.store(0, Ordering::SeqCst);
    let (first, _) = call(&h, batch, "progress", &query, 200).await;
    assert!(first["next_cursor"].is_object());
    assert_eq!(calls.load(Ordering::SeqCst), 64);
    for (cursor, status) in [
        (
            ProgressCursor {
                review_revision: 1,
                row: 1,
                target_offset: 0,
            },
            409,
        ),
        (
            ProgressCursor {
                review_revision: 2,
                row: 1,
                target_offset: 100,
            },
            400,
        ),
        (
            ProgressCursor {
                review_revision: 2,
                row: 3,
                target_offset: 0,
            },
            400,
        ),
    ] {
        calls.store(0, Ordering::SeqCst);
        call(
            &h,
            batch,
            "progress_page",
            &ProgressPageQuery {
                query: query.clone(),
                cursor: Some(cursor),
            },
            status,
        )
        .await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "invalid cursor must not query owners"
        );
    }
    h.finish().await;
}
