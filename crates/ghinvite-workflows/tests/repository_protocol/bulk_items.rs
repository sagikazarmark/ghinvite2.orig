//! Owner-driven progress and fixed inventory against the actual standard endpoint.
use super::*;
use ghinvite_core::{
    bulk_commands::*,
    delivery_control::DeliveryKind,
    request_completion::{DeliveryNotice, TargetResult},
};
use ghinvite_e2e::RunPath;
const SERVICES: &[&str] = &["BulkInviteBatch", "BulkProjection"];
const PATHS: &[RunPath] = &[
    RunPath::new("BulkInviteBatch", "create", &["bulk_create_time"]),
    RunPath::new("BulkInviteBatch", "create", &[]),
    RunPath::new("BulkInviteBatch", "remove_rows", &["bulk_edit_time"]),
    RunPath::new("BulkInviteBatch", "remove_rows", &[]),
    RunPath::new(
        "BulkInviteBatch",
        "submit",
        &["bulk_submit_time", "batch_request_profile"],
    ),
    RunPath::new("BulkInviteBatch", "submit", &[]),
    RunPath::new("BulkInviteBatch", "cancel_remaining", &["bulk_cancel_time"]),
    RunPath::new("BulkInviteBatch", "cancel_remaining", &[]),
    RunPath::new("BulkInviteBatch", "on_item_progress", &[]),
    RunPath::new("BulkInviteBatch", "review", &[]),
    RunPath::new("BulkInviteBatch", "progress", &[]),
    RunPath::new("BulkInviteBatch", "progress_page", &[]),
    RunPath::new("BulkProjection", "apply", &["apply_bulk_projection"]),
];
async fn audited(
    a: &mut JournalAudit,
    f: &BulkFixture,
    service: &str,
    key: &str,
    handler: &str,
    input: Value,
) -> Value {
    let r = a.invoke(&f.h, service, key, handler, &input).await;
    assert_eq!(r.status, 200, "{service}/{handler}: {r:?}");
    r.body
}
async fn submit(f: &BulkFixture, a: &mut JournalAudit) -> (BatchQuery, RowRequest, Review) {
    let batch = BulkBatchId::new();
    let initial = review(batch, Utc::now());
    let create = CreateBatch {
        operation_id: operation(),
        review: initial,
    };
    audited(
        a,
        f,
        "BulkInviteBatch",
        &batch.to_string(),
        "create",
        json!(create),
    )
    .await;
    audited(
        a,
        f,
        "BulkInviteBatch",
        &batch.to_string(),
        "create",
        json!(create),
    )
    .await;
    let query = BatchQuery {
        batch_id: batch,
        admin: AccountAdmin {
            account_id: 100,
            user_id: 7,
        },
    };
    let edit = EditBatch {
        query: query.clone(),
        operation_id: operation(),
        expected_revision: 1,
        remove: std::collections::BTreeSet::from([3]),
        personal_link_rows: Default::default(),
    };
    audited(
        a,
        f,
        "BulkInviteBatch",
        &batch.to_string(),
        "remove_rows",
        json!(edit),
    )
    .await;
    audited(
        a,
        f,
        "BulkInviteBatch",
        &batch.to_string(),
        "remove_rows",
        json!(edit),
    )
    .await;
    let row = RowRequest {
        source: BatchRef {
            batch_id: batch,
            row: 1,
        },
        request_id: RequestId::new(),
        requester_id: 8,
        requester_login: "alice".into(),
    };
    let command = SubmitBatch {
        query: query.clone(),
        operation_id: operation(),
        expected_revision: 2,
        submission: Submission {
            submitted_by: 7,
            requests: vec![row.clone()],
            emails: vec![],
            personal_links: vec![],
            submitted_at: Utc::now(),
        },
    };
    let review: Review = serde_json::from_value(
        audited(
            a,
            f,
            "BulkInviteBatch",
            &batch.to_string(),
            "submit",
            json!(command),
        )
        .await,
    )
    .unwrap();
    audited(
        a,
        f,
        "BulkInviteBatch",
        &batch.to_string(),
        "submit",
        json!(command),
    )
    .await;
    (query, row, review)
}
async fn finals(f: &BulkFixture, q: &BatchQuery, count: usize) -> Vec<TargetResult> {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let p: BatchProgress =
                serde_json::from_value(f.call(q.batch_id, "progress", q, 200).await).unwrap();
            if let Some(row) = p.requests.first()
                && row.final_results.len() == count
            {
                return row.final_results.clone();
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("owner notification did not converge")
}
#[tokio::test]
async fn bulk_fixed_journal_all_handlers_and_real_owner_progress_replays() {
    let f = BulkFixture::new().await;
    cancel::authorize(&f).await;
    f.github.scenario.lock().unwrap().roles = vec!["write"];
    let mut a = JournalAudit::default();
    let (q, row, review) = submit(&f, &mut a).await;
    let results = finals(&f, &q, 1).await;
    assert_eq!(
        results[0].result,
        ghinvite_core::delivery::FinalResult::Satisfied
    );
    let notice = ItemProgressNotice::Request {
        source: row.source,
        request_id: row.request_id,
        delivery: Some(DeliveryNotice {
            kind: DeliveryKind::Repository,
            target_id: 10,
            delivery_generation: 1,
        }),
    };
    for _ in 0..3 {
        audited(
            &mut a,
            &f,
            "BulkInviteBatch",
            &q.batch_id.to_string(),
            "on_item_progress",
            json!(notice),
        )
        .await;
    }
    assert_eq!(finals(&f, &q, 1).await, results);
    let wrong = ItemProgressNotice::Request {
        source: row.source,
        request_id: RequestId::new(),
        delivery: None,
    };
    f.call(q.batch_id, "on_item_progress", &wrong, 409).await;
    for handler in ["review", "progress"] {
        audited(
            &mut a,
            &f,
            "BulkInviteBatch",
            &q.batch_id.to_string(),
            handler,
            json!(q),
        )
        .await;
    }
    audited(
        &mut a,
        &f,
        "BulkInviteBatch",
        &q.batch_id.to_string(),
        "progress_page",
        json!(ProgressPageQuery {
            query: q.clone(),
            cursor: None
        }),
    )
    .await;
    let stop = CancelBatch {
        query: q.clone(),
        operation_id: operation(),
    };
    audited(
        &mut a,
        &f,
        "BulkInviteBatch",
        &q.batch_id.to_string(),
        "cancel_remaining",
        json!(stop),
    )
    .await;
    audited(
        &mut a,
        &f,
        "BulkInviteBatch",
        &q.batch_id.to_string(),
        "cancel_remaining",
        json!(stop),
    )
    .await;
    audited(
        &mut a,
        &f,
        "BulkProjection",
        &q.batch_id.to_string(),
        "apply",
        json!(ghinvite_core::storage::bulk::Projection {
            review,
            events: vec![]
        }),
    )
    .await;
    let walked = a.check(&f.h, SERVICES, PATHS).await;
    assert_eq!(walked.handlers, 9);
    assert_eq!(walked.paths, PATHS.len());
    eprintln!("Bulk fixed journal table: {walked}");
    assert!(
        f.github
            .scenario
            .lock()
            .unwrap()
            .trace
            .iter()
            .all(|t| t.method != "PUT")
    );
    f.finish().await;
}
#[tokio::test]
async fn bulk_callback_reissue_uses_current_manifest_and_discards_old_generation_results() {
    let f = BulkFixture::with_reissues(true).await;
    cancel::authorize(&f).await;
    f.github.scenario.lock().unwrap().writes =
        VecDeque::from([WriteReply::Status(201), WriteReply::Status(201)]);
    let mut audit = JournalAudit::default();
    let (q, row, _) = submit(&f, &mut audit).await;
    let p = f.wait_delivery(&q).await;
    let Observation::Observed(before) = &p.requests[0].repositories[0].observation else {
        panic!()
    };
    let c = &before.create.command;
    let observation = ghinvite_workflows::settlement_policy::Observation::new(
        before,
        Utc::now(),
        ghinvite_workflows::settlement_policy::Evidence::Listed {
            upstream_id: 99,
            expired: true,
            created_at: None,
        },
    );
    cancel::rpc(
        &f,
        "RepositoryDelivery",
        &c.delivery_key(),
        "reconcile",
        &observation,
        200,
    )
    .await;
    let old = finals(&f, &q, 1).await;
    assert_eq!(
        old[0].result,
        ghinvite_core::delivery::FinalResult::LapsedInvite
    );
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let reissue = ghinvite_core::reissue::ReissueCommand {
        operation_id: RequestId::new().to_string().try_into().unwrap(),
        request_id: row.request_id,
        admin: q.admin.clone(),
        target: ghinvite_core::reissue::ReissueTarget::Repository { repo_id: 10 },
    };
    assert_eq!(
        cancel::rpc(
            &f,
            "InvitationRequest",
            &row.request_id.to_string(),
            "reissue",
            &reissue,
            200
        )
        .await,
        json!({"Ok":{"delivery_generation":2}})
    );
    let old_notice = ItemProgressNotice::Request {
        source: row.source,
        request_id: row.request_id,
        delivery: Some(DeliveryNotice {
            kind: DeliveryKind::Repository,
            target_id: 10,
            delivery_generation: 1,
        }),
    };
    for _ in 0..3 {
        f.call(q.batch_id, "on_item_progress", &old_notice, 200)
            .await;
    }
    assert!(finals(&f, &q, 0).await.is_empty());
    let p: BatchProgress = serde_json::from_value(
        f.call(
            q.batch_id,
            "progress_page",
            &ProgressPageQuery {
                query: q.clone(),
                cursor: None,
            },
            200,
        )
        .await,
    )
    .unwrap();
    assert_eq!(p.requests[0].repositories[0].delivery_generation, 2);
    let reset = ItemProgressNotice::Request {
        source: row.source,
        request_id: row.request_id,
        delivery: None,
    };
    f.call(q.batch_id, "on_item_progress", &reset, 200).await;
    assert!(finals(&f, &q, 0).await.is_empty());
    // New confirmed evidence belongs to generation2; historical callback replay
    // cannot overwrite it, even though its result differs from generation1.
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let p: BatchProgress =
                serde_json::from_value(f.call(q.batch_id, "progress", &q, 200).await).unwrap();
            if let Observation::Observed(s) = &p.requests[0].repositories[0].observation
                && matches!(s.create.outcome, CreateOutcome::Created { .. })
            {
                let evidence = ghinvite_workflows::settlement_policy::Observation::new(
                    s,
                    Utc::now(),
                    ghinvite_workflows::settlement_policy::Evidence::Absent {
                        availability_verified: true,
                        role_name: Some(ghinvite_core::targets::EffectiveRole::Write),
                        direct_collaborator: Some(true),
                        membership: None,
                    },
                );
                cancel::rpc(
                    &f,
                    "RepositoryDelivery",
                    &s.create.command.delivery_key(),
                    "reconcile",
                    &evidence,
                    200,
                )
                .await;
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let current = finals(&f, &q, 1).await;
    assert_eq!(current[0].target.generation, 2);
    assert_eq!(
        current[0].result,
        ghinvite_core::delivery::FinalResult::Satisfied
    );
    f.call(q.batch_id, "on_item_progress", &old_notice, 200)
        .await;
    assert_eq!(finals(&f, &q, 1).await, current);
    f.finish().await;
}

#[tokio::test]
async fn killed_terminal_request_replay_drains_batch_notification_without_changing_final_result() {
    let faults = Arc::new(ghinvite_workflows::request_owner::Faults::default());
    *faults.stage.lock().unwrap() = Some("after-batch-final-results".into());
    let f = BulkFixture::options(false, Some(faults.clone())).await;
    f.github.scenario.lock().unwrap().roles = vec!["write"];
    let mut a = JournalAudit::default();
    let (q, row, _) = submit(&f, &mut a).await;
    let id=tokio::time::timeout(Duration::from_secs(15),async {
        loop {
            if faults.reached.load(std::sync::atomic::Ordering::SeqCst)>0 {
                let invocations=f.h.restate.admin().sql_or_panic(&format!("SELECT id FROM sys_invocation WHERE target_service_name='InvitationRequest' AND target_service_key='{}' AND target_handler_name='on_delivery_settled' AND status!='completed'",row.request_id)).await;
                if let Some(id)=invocations.first().and_then(|v|v["id"].as_str()){break id.to_owned();}
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }).await.unwrap();
    assert!(
        finals(&f, &q, 0).await.is_empty(),
        "notification has not been durably sent"
    );
    f.h.restate.admin().kill(&id).await;
    f.h.restate.admin().await_status(&id, &["completed"]).await;
    *faults.stage.lock().unwrap() = None;
    let notice = DeliveryNotice {
        kind: DeliveryKind::Repository,
        target_id: 10,
        delivery_generation: 1,
    };
    for _ in 0..3 {
        cancel::rpc(
            &f,
            "InvitationRequest",
            &row.request_id.to_string(),
            "on_delivery_settled",
            &notice,
            200,
        )
        .await;
    }
    let results = finals(&f, &q, 1).await;
    assert_eq!(
        results[0].result,
        ghinvite_core::delivery::FinalResult::Satisfied
    );
    faults
        .unavailable
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let retry = ItemProgressNotice::Request {
        source: row.source,
        request_id: row.request_id,
        delivery: Some(notice),
    };
    let response =
        f.h.restate
            .invoke(
                &Call::object(
                    "BulkInviteBatch",
                    &q.batch_id.to_string(),
                    "on_item_progress",
                ),
                Some(&json!(retry)),
                None,
            )
            .await;
    assert_eq!(
        response.status, 200,
        "5xx schedules a fresh invocation, not a permanent failure: {response:?}"
    );
    let journal = f.h.restate.admin().journal(response.invocation_id()).await;
    assert!(
        journal.iter().any(|e| e.raw_contains("on_item_progress")),
        "durable delayed notice must be journaled"
    );
    faults
        .unavailable
        .store(false, std::sync::atomic::Ordering::SeqCst);
    f.call(q.batch_id, "on_item_progress", &retry, 200).await;
    assert_eq!(finals(&f, &q, 1).await, results);
    assert!(
        f.github
            .scenario
            .lock()
            .unwrap()
            .trace
            .iter()
            .all(|t| t.method != "PUT")
    );
    f.finish().await;
}

#[tokio::test]
async fn prewrite_batch_cancellation_notifies_unfulfillable_without_rewriting_approved_plan() {
    let faults = Arc::new(ghinvite_workflows::request_owner::Faults::default());
    *faults.stage.lock().unwrap() = Some("initialization-recorded".into());
    let f = BulkFixture::options(false, Some(faults.clone())).await;
    cancel::authorize(&f).await;
    let mut a = JournalAudit::default();
    let (q, row, _) = submit(&f, &mut a).await;
    let id=tokio::time::timeout(Duration::from_secs(15),async{
        loop{
            if faults.reached.load(std::sync::atomic::Ordering::SeqCst)>0{
                let rows=f.h.restate.admin().sql_or_panic(&format!("SELECT id FROM sys_invocation WHERE target_service_name='InvitationRequest' AND target_service_key='{}' AND target_handler_name='initialize_batch' AND status!='completed'",row.request_id)).await;
                if let Some(id)=rows.first().and_then(|r|r["id"].as_str()){break id.to_owned()}
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.unwrap();
    f.h.restate.admin().kill(&id).await;
    f.h.restate.admin().await_status(&id, &["completed"]).await;
    *faults.stage.lock().unwrap() = None;
    let stop = CancelBatch {
        query: q.clone(),
        operation_id: operation(),
    };
    let result = f.call(q.batch_id, "cancel_remaining", &stop, 200).await;
    assert_eq!(result["aborted"], 1);
    let results = finals(&f, &q, 1).await;
    assert_eq!(
        results[0].result,
        ghinvite_core::delivery::FinalResult::Unfulfillable
    );
    assert_eq!(
        f.call(q.batch_id, "cancel_remaining", &stop, 200).await,
        result
    );
    assert!(
        f.github
            .scenario
            .lock()
            .unwrap()
            .trace
            .iter()
            .all(|t| t.method != "PUT" && t.method != "DELETE")
    );
    f.finish().await;
}
