//! Actual request inbox and no-write delivery barriers, including delayed work.
use super::*;
use ghinvite_core::{
    bulk_cancel::{CancelRequest, CancelRequestReceipt},
    delivery::{AbortReason, DeliverySnapshot},
    request_lifecycle::{AdminRequestStatus, ApprovedDispatch, InitializeRequest},
    storage::ConsoleStorage,
};

pub(super) async fn rpc<T: serde::Serialize>(
    f: &BulkFixture,
    service: &str,
    key: &str,
    handler: &str,
    input: &T,
    status: u16,
) -> Value {
    let response =
        f.h.restate
            .invoke(
                &Call::object(service, key, handler),
                Some(&json!(input)),
                None,
            )
            .await;
    assert_eq!(response.status, status, "{response:?}");
    response.body
}
pub(super) async fn authorize(f: &BulkFixture) {
    let response =
        f.h.restate
            .invoke(
                &Call::object("AccountInstallation", "repo:100", "status"),
                None,
                None,
            )
            .await;
    assert_eq!(response.status, 200, "{response:?}");
    let setup = ghinvite_e2e::setup_command(response.body["account"].clone());
    let mut signed = ghinvite_e2e::signed_owner_command(&setup, 7);
    signed["updated_at"] = json!(Utc::now());
    rpc(f, "Installation", "repo:1", "onboard_webhook", &signed, 200).await;
    assert_eq!(
        rpc(
            f,
            "AccountInstallation",
            "repo:100",
            "decision_authority",
            &7,
            200
        )
        .await,
        json!(true)
    );
}
async fn prepared(f: &BulkFixture) -> InitializeRequest {
    let batch = BulkBatchId::new();
    let mut review = review(batch, Utc::now());
    review
        .edit(1, std::collections::BTreeSet::from([3]), Default::default())
        .unwrap();
    let row = RowRequest {
        source: BatchRef {
            batch_id: batch,
            row: 1,
        },
        request_id: RequestId::new(),
        requester_id: 8,
        requester_login: "alice".into(),
    };
    review
        .submit(
            2,
            Submission {
                submitted_by: 9,
                requests: vec![row.clone()],
                emails: vec![],
                personal_links: vec![],
                submitted_at: Utc::now(),
            },
        )
        .unwrap();
    f.storage.project_bulk_review(&review).await.unwrap();
    f.storage
        .upsert_user(&ghinvite_core::User {
            user_id: 8,
            login: "alice".into(),
            avatar_url: None,
            github_created_at: None,
            last_seen_at: Utc::now(),
        })
        .await
        .unwrap();
    ghinvite_core::bulk_commands::initialization(&review, &row).unwrap()
}
fn cancellation(input: InitializeRequest) -> CancelRequest {
    CancelRequest {
        operation_id: RequestId::new().to_string().try_into().unwrap(),
        admin: AccountAdmin {
            account_id: 100,
            user_id: 7,
        },
        input,
    }
}
async fn cancel(f: &BulkFixture, input: &CancelRequest, status: u16) -> Value {
    rpc(
        f,
        "InvitationRequest",
        &input.input.request.request_id.to_string(),
        "cancel_batch",
        input,
        status,
    )
    .await
}
async fn plan(f: &BulkFixture, input: &InitializeRequest) -> ApprovedDispatch {
    serde_json::from_value(
        rpc(
            f,
            "InvitationRequest",
            &input.request.request_id.to_string(),
            "admin_dispatch",
            &AdminRequestStatus {
                request_id: input.request.request_id,
                admin: AccountAdmin {
                    account_id: 100,
                    user_id: 7,
                },
            },
            200,
        )
        .await,
    )
    .unwrap()
}
async fn status(f: &BulkFixture, command: &CreateCommand) -> DeliverySnapshot {
    let response =
        f.h.restate
            .invoke(
                &Call::object("RepositoryDelivery", &command.delivery_key(), "status"),
                None,
                None,
            )
            .await;
    assert_eq!(response.status, 200, "{response:?}");
    serde_json::from_value(response.body).unwrap()
}
#[tokio::test]
async fn batch_cancel_before_initialize_materializes_aborted_and_fences_delayed_work() {
    let f = BulkFixture::new().await;
    authorize(&f).await;
    let input = prepared(&f).await;
    let cancel_input = cancellation(input.clone());
    let receipt = cancel(&f, &cancel_input, 200).await;
    assert_eq!(
        serde_json::from_value::<CancelRequestReceipt>(receipt.clone()).unwrap(),
        CancelRequestReceipt {
            request_id: input.request.request_id,
            stopped: true,
            aborted: 1,
            already_written: 0,
            already_completed: 0,
            uncertain: 0
        }
    );
    let plan = plan(&f, &input).await;
    assert_eq!(
        plan.input.request.decision.as_ref().unwrap().decided_by,
        Some(9),
        "cancelling installer must not replace original submitting administrator"
    );
    let command = &plan.commands[0];
    assert!(
        command.invitation_id.is_none(),
        "new batch commands use canonical identity"
    );
    let before = status(&f, command).await;
    assert_eq!(
        before.create.audit_version,
        ghinvite_core::delivery_audit::DeliveryAuditVersion::CanonicalV2
    );
    assert_eq!(
        before.create.outcome,
        CreateOutcome::Aborted {
            reason: AbortReason::BatchCancelled
        }
    );
    rpc(
        &f,
        "InvitationRequest",
        &input.request.request_id.to_string(),
        "initialize_batch",
        &input,
        200,
    )
    .await;
    for _ in 0..3 {
        rpc(
            &f,
            "RepositoryDelivery",
            &command.delivery_key(),
            "create",
            command,
            200,
        )
        .await;
        rpc(
            &f,
            "RepositoryDelivery",
            &command.delivery_key(),
            "recheck",
            &json!({"command":command,"generation":1}),
            200,
        )
        .await;
    }
    assert_eq!(status(&f, command).await, before);
    assert_eq!(cancel(&f, &cancel_input, 200).await, receipt);
    let mut changed = cancel_input.clone();
    changed.input.request.requester_id = 9;
    cancel(&f, &changed, 409).await;
    let mut unauthorized = cancel_input.clone();
    unauthorized.operation_id = RequestId::new().to_string().try_into().unwrap();
    unauthorized.admin.user_id = 9;
    cancel(&f, &unauthorized, 404).await;
    assert!(
        f.github
            .scenario
            .lock()
            .unwrap()
            .trace
            .iter()
            .all(|t| t.method != "PUT" && t.method != "DELETE"),
        "stopped batch rows must issue neither create nor removal writes"
    );
    let fences: i64 = sqlx::query_scalar("SELECT count(*) FROM delivery_attempts")
        .fetch_one(&f.github.sql)
        .await
        .unwrap();
    assert_eq!(fences, 0);
    f.finish().await;
}
#[tokio::test]
async fn batch_cancel_after_owned_write_preserves_receipt_charge_and_never_deletes() {
    let f = BulkFixture::new().await;
    authorize(&f).await;
    let input = prepared(&f).await;
    rpc(
        &f,
        "InvitationRequest",
        &input.request.request_id.to_string(),
        "initialize_batch",
        &input,
        200,
    )
    .await;
    let plan = plan(&f, &input).await;
    let command = &plan.commands[0];
    let before = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let response =
                f.h.restate
                    .invoke(
                        &Call::object("RepositoryDelivery", &command.delivery_key(), "status"),
                        None,
                        None,
                    )
                    .await;
            if let Ok(snapshot) = serde_json::from_value::<DeliverySnapshot>(response.body)
                && matches!(snapshot.create.outcome, CreateOutcome::Created { .. })
            {
                break snapshot;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let cancel_input = cancellation(input);
    let receipt: CancelRequestReceipt =
        serde_json::from_value(cancel(&f, &cancel_input, 200).await).unwrap();
    assert_eq!(
        (receipt.aborted, receipt.already_written, receipt.uncertain),
        (0, 1, 0)
    );
    rpc(
        &f,
        "RepositoryDelivery",
        &command.delivery_key(),
        "create",
        command,
        200,
    )
    .await;
    assert_eq!(status(&f, command).await, before);
    assert_eq!(cancel(&f, &cancel_input, 200).await, json!(receipt));
    {
        let scenario = f.github.scenario.lock().unwrap();
        assert_eq!(
            scenario.trace.iter().filter(|t| t.method == "PUT").count(),
            1
        );
        assert!(!scenario.trace.iter().any(|t| t.method == "DELETE"));
    }
    let fence: i64 = sqlx::query_scalar("SELECT count(*) FROM delivery_attempts")
        .fetch_one(&f.github.sql)
        .await
        .unwrap();
    assert_eq!(
        fence, 1,
        "successful write fence must survive batch cancellation"
    );
    f.finish().await;
}

#[tokio::test]
async fn batch_cancel_uncertain_write_keeps_charge_fence_and_never_retries_or_deletes() {
    let f = BulkFixture::new().await;
    authorize(&f).await;
    f.github.scenario.lock().unwrap().writes = VecDeque::from([WriteReply::Timeout]);
    let input = prepared(&f).await;
    rpc(
        &f,
        "InvitationRequest",
        &input.request.request_id.to_string(),
        "initialize_batch",
        &input,
        200,
    )
    .await;
    let plan = plan(&f, &input).await;
    let command = &plan.commands[0];
    let before = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let response =
                f.h.restate
                    .invoke(
                        &Call::object("RepositoryDelivery", &command.delivery_key(), "status"),
                        None,
                        None,
                    )
                    .await;
            if let Ok(snapshot) = serde_json::from_value::<DeliverySnapshot>(response.body)
                && snapshot.create.outcome == CreateOutcome::OutcomeUnknown
            {
                break snapshot;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let cancel_input = cancellation(input);
    let receipt: CancelRequestReceipt =
        serde_json::from_value(cancel(&f, &cancel_input, 200).await).unwrap();
    assert_eq!(
        (receipt.aborted, receipt.already_written, receipt.uncertain),
        (0, 0, 1)
    );
    for _ in 0..3 {
        rpc(
            &f,
            "RepositoryDelivery",
            &command.delivery_key(),
            "create",
            command,
            200,
        )
        .await;
    }
    assert_eq!(status(&f, command).await, before);
    assert_eq!(cancel(&f, &cancel_input, 200).await, json!(receipt));
    {
        let scenario = f.github.scenario.lock().unwrap();
        assert_eq!(
            scenario.trace.iter().filter(|t| t.method == "PUT").count(),
            1
        );
        assert!(!scenario.trace.iter().any(|t| t.method == "DELETE"));
    }
    let fence: i64 = sqlx::query_scalar("SELECT count(*) FROM delivery_attempts")
        .fetch_one(&f.github.sql)
        .await
        .unwrap();
    assert_eq!(fence, 1);
    let response =
        f.h.restate
            .invoke(
                &Call::object("InviteBudget", "repo:100", "snapshot"),
                None,
                None,
            )
            .await;
    assert_eq!(response.status, 200, "{response:?}");
    let budget: ghinvite_core::budget::BudgetSnapshot =
        serde_json::from_value(response.body).unwrap();
    assert_eq!(
        budget.content_used_minute, 1,
        "unknown write remains charged after cancellation"
    );
    assert!(budget.buckets.iter().any(|bucket| bucket.used_24h == 1));
    f.finish().await;
}

#[tokio::test]
async fn bulk_cancel_coordinator_retains_exact_aggregate_and_audit_without_deleting_written_rows() {
    coordinator_cancel(false).await;
}

#[tokio::test]
async fn bulk_cancel_coordinator_aborts_initialized_canonical_unsent_and_delayed_create() {
    coordinator_cancel(true).await;
}

async fn coordinator_cancel(unsent: bool) {
    use ghinvite_core::bulk_commands::{CancelBatch, CancelBatchReceipt, Observation};
    let f = BulkFixture::new().await;
    authorize(&f).await;
    let closed_until = Utc::now() + chrono::Duration::seconds(5);
    if unsent {
        rpc(
            &f,
            "InviteBudget",
            "repo:100",
            "record_limit",
            &json!({"bucket":{"kind":"repo","repo_id":10},"until":closed_until}),
            200,
        )
        .await;
    }
    let batch = BulkBatchId::new();
    let initial = review(batch, Utc::now());
    f.call(
        batch,
        "create",
        &CreateBatch {
            operation_id: operation(),
            review: initial,
        },
        200,
    )
    .await;
    let query = BatchQuery {
        batch_id: batch,
        admin: AccountAdmin {
            account_id: 100,
            user_id: 7,
        },
    };
    f.call(
        batch,
        "remove_rows",
        &EditBatch {
            query: query.clone(),
            operation_id: operation(),
            expected_revision: 1,
            remove: std::collections::BTreeSet::from([3]),
            personal_link_rows: Default::default(),
        },
        200,
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
    f.call(
        batch,
        "submit",
        &SubmitBatch {
            query: query.clone(),
            operation_id: operation(),
            expected_revision: 2,
            submission: Submission {
                submitted_by: 7,
                requests: vec![row],
                emails: vec![],
                personal_links: vec![],
                submitted_at: Utc::now(),
            },
        },
        200,
    )
    .await;
    let before = if unsent {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let progress: BatchProgress = serde_json::from_value(
                    f.call(batch, "progress", &query, 200).await,
                ).unwrap();
                if matches!(progress.requests.first().and_then(|row| row.repositories.first()).map(|r| &r.observation),
                    Some(Observation::Observed(snapshot)) if matches!(snapshot.create.outcome, CreateOutcome::Throttled { .. })) {
                    break progress;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }).await.expect("canonical owner must be initialized and throttled before cancellation")
    } else {
        f.wait_delivery(&query).await
    };
    let Observation::Observed(snapshot) = &before.requests[0].repositories[0].observation else {
        panic!("expected actual owner receipt");
    };
    let command = snapshot.create.command.clone();
    assert!(command.invitation_id.is_none());
    assert_eq!(
        snapshot.create.audit_version,
        ghinvite_core::delivery_audit::DeliveryAuditVersion::CanonicalV2
    );
    let cancellation = CancelBatch {
        query: query.clone(),
        operation_id: operation(),
    };
    let reply = f.call(batch, "cancel_remaining", &cancellation, 200).await;
    let receipt: CancelBatchReceipt = serde_json::from_value(reply.clone()).unwrap();
    assert!(receipt.stopped);
    assert_eq!(
        (
            receipt.aborted,
            receipt.already_written,
            receipt.already_completed,
            receipt.uncertain
        ),
        if unsent { (1, 0, 0, 0) } else { (0, 1, 0, 0) }
    );
    assert_eq!(
        f.call(batch, "cancel_remaining", &cancellation, 200).await,
        reply
    );
    let mut changed = cancellation.clone();
    changed.query.admin.user_id = 9;
    f.call(batch, "cancel_remaining", &changed, 409).await;
    let after: BatchProgress =
        serde_json::from_value(f.call(batch, "progress", &query, 200).await).unwrap();
    assert_eq!(before.review, after.review);
    if unsent {
        let stopped = status(&f, &command).await;
        assert_eq!(
            stopped.create.outcome,
            CreateOutcome::Aborted {
                reason: AbortReason::BatchCancelled
            }
        );
        assert_eq!(stopped.create.audit_version, snapshot.create.audit_version);
        if let Ok(wait) = (closed_until - Utc::now()).to_std() {
            tokio::time::sleep(wait + Duration::from_millis(10)).await;
        }
        rpc(
            &f,
            "RepositoryDelivery",
            &command.delivery_key(),
            "create",
            &command,
            200,
        )
        .await;
        rpc(
            &f,
            "RepositoryDelivery",
            &command.delivery_key(),
            "recheck",
            &json!({"command":command,"generation":1}),
            200,
        )
        .await;
        assert_eq!(status(&f, &command).await, stopped);
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if f.storage
                    .get_delivery(&command.delivery_key())
                    .await
                    .unwrap()
                    .as_ref()
                    == Some(&stopped)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("canonical cancellation projection must converge");
        let aborted_events: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit_events WHERE event_type='delivery.aborted' AND target_kind='repository_delivery' AND target_id=?",
        )
        .bind(command.delivery_key())
        .fetch_one(&f.github.sql)
        .await
        .unwrap();
        assert_eq!(aborted_events, 1);
        let fences: i64 = sqlx::query_scalar("SELECT count(*) FROM delivery_attempts")
            .fetch_one(&f.github.sql)
            .await
            .unwrap();
        assert_eq!(fences, 0);
    } else {
        assert_eq!(before.requests, after.requests);
    }
    assert_eq!(after.cancellation, Some(receipt));
    assert!(!after.cancellation_pending);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM audit_events WHERE event_type='bulk_batch.cancelled'",
            )
            .fetch_one(&f.github.sql)
            .await
            .unwrap();
            if count == 1 {
                break;
            }
            assert_eq!(count, 0, "exact stop replay must not duplicate audit");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    {
        let scenario = f.github.scenario.lock().unwrap();
        assert_eq!(
            scenario.trace.iter().filter(|t| t.method == "PUT").count(),
            usize::from(!unsent)
        );
        assert!(!scenario.trace.iter().any(|t| t.method == "DELETE"));
    }
    f.finish().await;
}

#[tokio::test]
async fn old_batch_cancel_replay_cannot_retarget_explicit_reissue_and_new_operation_captures_new_generation()
 {
    let f = BulkFixture::with_reissues(true).await;
    authorize(&f).await;
    f.github.scenario.lock().unwrap().writes =
        VecDeque::from([WriteReply::Status(201), WriteReply::Status(201)]);
    let input = prepared(&f).await;
    rpc(
        &f,
        "InvitationRequest",
        &input.request.request_id.to_string(),
        "initialize_batch",
        &input,
        200,
    )
    .await;
    let first_plan = plan(&f, &input).await;
    let first = &first_plan.commands[0];
    let old = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let response =
                f.h.restate
                    .invoke(
                        &Call::object("RepositoryDelivery", &first.delivery_key(), "status"),
                        None,
                        None,
                    )
                    .await;
            if let Ok(snapshot) = serde_json::from_value::<DeliverySnapshot>(response.body)
                && matches!(snapshot.create.outcome, CreateOutcome::Created { .. })
            {
                break snapshot;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let original = cancellation(input.clone());
    let old_receipt = cancel(&f, &original, 200).await;
    let expired = ghinvite_workflows::settlement_policy::Observation::new(
        &old,
        Utc::now(),
        ghinvite_workflows::settlement_policy::Evidence::Listed {
            upstream_id: 99,
            expired: true,
        },
    );
    rpc(
        &f,
        "RepositoryDelivery",
        &first.delivery_key(),
        "reconcile",
        &expired,
        200,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let reissue = ghinvite_core::reissue::ReissueCommand {
        operation_id: RequestId::new().to_string().try_into().unwrap(),
        request_id: input.request.request_id,
        admin: original.admin.clone(),
        target: ghinvite_core::reissue::ReissueTarget::Repository { repo_id: 10 },
    };
    let result = rpc(
        &f,
        "InvitationRequest",
        &input.request.request_id.to_string(),
        "reissue",
        &reissue,
        200,
    )
    .await;
    assert_eq!(result, json!({"Ok":{"delivery_generation":2}}));
    assert_eq!(
        cancel(&f, &original, 200).await,
        old_receipt,
        "old operation preserves its original generation cut"
    );
    let current = plan(&f, &input).await;
    let second = &current.commands[0];
    assert_eq!(second.delivery_generation, 2);
    assert_eq!(second.budget_link_id(), None);
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let response =
                f.h.restate
                    .invoke(
                        &Call::object("RepositoryDelivery", &second.delivery_key(), "status"),
                        None,
                        None,
                    )
                    .await;
            if let Ok(snapshot) = serde_json::from_value::<DeliverySnapshot>(response.body)
                && matches!(snapshot.create.outcome, CreateOutcome::Created { .. })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("old cancellation must not suppress explicitly reissued work");
    let fresh = cancellation(input);
    let result: CancelRequestReceipt =
        serde_json::from_value(cancel(&f, &fresh, 200).await).unwrap();
    assert_eq!(result.already_written, 1);
    let raw:Value=f.h.client.post(format!("{}/query",f.h.restate.admin_url())).header("accept","application/json").json(&json!({"query":format!("SELECT key,value_utf8 FROM state WHERE service_name='InvitationRequest' AND service_key='{}'",fresh.input.request.request_id)})).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    let head: Value = serde_json::from_str(
        raw["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["key"] == "batch_cancel/current")
            .unwrap()["value_utf8"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(head["plan"]["commands"][0]["delivery_generation"], 2);
    assert_eq!(cancel(&f, &original, 200).await, old_receipt);
    {
        let scenario = f.github.scenario.lock().unwrap();
        assert_eq!(
            scenario.trace.iter().filter(|t| t.method == "PUT").count(),
            2
        );
        assert_eq!(
            scenario
                .trace
                .iter()
                .filter(|t| t.method == "DELETE")
                .count(),
            1,
            "only explicit reissue barrier deletes the old pending invitation"
        );
    }
    f.finish().await;
}

#[tokio::test]
async fn batch_cancel_preserves_compatibility_identity_before_delayed_v1_create() {
    use ghinvite_core::{
        bulk_cancel::RepositoryCancelUnsent, delivery_audit::DeliveryAuditVersion,
    };
    let f = BulkFixture::new().await;
    let input = prepared(&f).await;
    let request = &input.request;
    let target = &input.targets.repos[0];
    let decision = request.decision.as_ref().unwrap();
    let command = CreateCommand {
        invitation_id: Some(GithubInvitationId::new()),
        delivery_generation: 1,
        link_id: None,
        batch: request.batch,
        budget_attribution: Default::default(),
        access_lifetime_seconds: input.access_lifetime_seconds,
        budget_share_pct: input.budget_share_pct,
        request_id: request.request_id,
        approval_id: decision.decision_id.clone(),
        account_id: request.account_id,
        installation_id: input.installation_id,
        requester_id: request.requester_id,
        repo_id: target.repo_id,
        repo_full_name: target.repo_full_name.clone(),
        permission: target.grant.into(),
        approved_at: decision.effective_at,
    };
    let stop = RepositoryCancelUnsent {
        operation_id: RequestId::new().to_string().try_into().unwrap(),
        admin: AccountAdmin {
            account_id: 100,
            user_id: 7,
        },
        command: command.clone(),
    };
    let reply = rpc(
        &f,
        "RepositoryDelivery",
        &command.delivery_key(),
        "cancel_unsent",
        &stop,
        200,
    )
    .await;
    assert_eq!(reply, json!("aborted"));
    let stopped = status(&f, &command).await;
    assert_eq!(stopped.create.command, command);
    assert_eq!(
        stopped.create.audit_version,
        DeliveryAuditVersion::CanonicalV1
    );
    rpc(
        &f,
        "RepositoryDelivery",
        &command.delivery_key(),
        "create",
        &command,
        200,
    )
    .await;
    assert_eq!(status(&f, &command).await, stopped);
    assert_eq!(
        rpc(
            &f,
            "RepositoryDelivery",
            &command.delivery_key(),
            "cancel_unsent",
            &stop,
            200
        )
        .await,
        reply
    );
    let mut changed = stop.clone();
    changed.command.invitation_id = None;
    rpc(
        &f,
        "RepositoryDelivery",
        &command.delivery_key(),
        "cancel_unsent",
        &changed,
        409,
    )
    .await;
    assert!(
        !f.github
            .scenario
            .lock()
            .unwrap()
            .trace
            .iter()
            .any(|t| t.method == "PUT" || t.method == "DELETE")
    );
    f.finish().await;
}
