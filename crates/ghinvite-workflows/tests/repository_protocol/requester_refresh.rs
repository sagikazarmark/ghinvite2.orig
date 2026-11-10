//! Real production read-only reconciliation and per-generation requester throttle.
use super::*;

async fn refresh(runtime: &Runtime, key: &str) -> ghinvite_e2e::Reply {
    runtime
        .harness
        .restate
        .invoke(
            &Call::object("RepositoryDelivery", key, "refresh"),
            None::<&Value>,
            None,
        )
        .await
}

async fn wait_for_reconciliation(runtime: &Runtime, count: usize) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let response: Value = runtime.harness.client
                .post(format!("{}/query", runtime.harness.restate.admin_url()))
                .header("accept", "application/json")
                .json(&json!({"query":"SELECT id,status,target_service_name,target_handler_name,completion_failure FROM sys_invocation WHERE target_service_name='Reconcile' OR (target_service_name='RepositoryDelivery' AND target_handler_name='reconcile')"}))
                .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
            let rows=response["rows"].as_array().expect("real reconciliation invocation rows");
            // target sends observe_next, which performs the verification reads
            // and sends reconcile. A completed parent alone is not a barrier
            // for those durable one-way children.
            let targets = rows.iter().filter(|r| r["target_service_name"] == "Reconcile" && r["target_handler_name"] == "target").count();
            let observations = rows.iter().filter(|r| r["target_service_name"] == "Reconcile" && r["target_handler_name"] == "observe_next").count();
            if targets==count && observations==count && rows.iter().all(|r|r["status"]=="completed") {
                assert!(rows.iter().all(|r|r["completion_failure"].is_null()), "{rows:?}");
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("the scheduled production read-only target finishes");
}

#[tokio::test]
async fn requester_refresh_is_read_only_durably_throttled_and_independent_per_generation() {
    let runtime = Runtime::start().await;
    let missing = format!("{}:10:1", RequestId::new());
    let response = refresh(&runtime, &missing).await;
    assert_eq!(response.status, 200, "{response:?}");
    assert_eq!(response.body["kind"], "not_tracked");

    let mut command = runtime.command(Scenario::default(), Permission::Push).await;
    command.invitation_id = None;
    let (receipt, _) = runtime.create(&command).await;
    assert!(matches!(receipt.outcome, CreateOutcome::Created { .. }));
    let before = runtime.trace().len();
    let response = refresh(&runtime, &command.delivery_key()).await;
    assert_eq!(response.status, 200, "{response:?}");
    assert_eq!(response.body["kind"], "requested");
    let journal = runtime
        .harness
        .restate
        .admin()
        .journal(response.invocation_id())
        .await;
    let names: Vec<_> = journal
        .iter()
        .filter(|e| e.is_run())
        .filter_map(|e| e.name.as_deref())
        .collect();
    assert_eq!(names, ["repository_refresh_time"]);
    assert!(journal.iter().all(|entry| !entry.raw_contains(MARKER)));
    wait_for_reconciliation(&runtime, 1).await;
    let after_first = runtime.trace();
    let reads = &after_first[before..];
    assert_eq!(
        operations(reads, "GET", "/repos/acme/api/invitations").len(),
        1
    );
    assert!(
        reads
            .iter()
            .all(|t| t.method == "GET" || (t.method == "POST" && t.path == MINT_PATH))
    );
    assert!(reads.iter().filter(|t| t.method == "GET").all(|t| {
        if t.path == "/app/installations/1" {
            // Installation verification uses the app JWT, not an IAT.
            t.credential.is_none()
        } else {
            t.credential.as_ref().is_some_and(|c| c.0 == "read")
        }
    }));

    let response = refresh(&runtime, &command.delivery_key()).await;
    assert_eq!(response.status, 200, "{response:?}");
    assert_eq!(response.body["kind"], "throttled");
    let retry_at: chrono::DateTime<Utc> =
        serde_json::from_value(response.body["retry_at"].clone()).unwrap();
    assert!(retry_at > Utc::now());
    assert!(retry_at <= Utc::now() + chrono::Duration::seconds(60));
    wait_for_reconciliation(&runtime, 1).await;
    assert_eq!(
        runtime.trace().len(),
        after_first.len(),
        "throttled retries dispatch no HTTP reads"
    );
    let status = runtime
        .harness
        .restate
        .invoke(
            &Call::object("RepositoryDelivery", &command.delivery_key(), "status"),
            None::<&Value>,
            None,
        )
        .await;
    assert_eq!(status.status, 200, "{status:?}");
    assert_eq!(status.body["create"], json!(receipt));

    command.delivery_generation = 2;
    let (current, _) = runtime.create(&command).await;
    assert!(matches!(
        current.outcome,
        CreateOutcome::AlreadyPending { .. }
    ));
    let response = refresh(&runtime, &command.delivery_key()).await;
    assert_eq!(response.status, 200, "{response:?}");
    assert_eq!(
        response.body["kind"], "requested",
        "a new generation has its own throttle"
    );
    wait_for_reconciliation(&runtime, 2).await;
    assert_eq!(operations(&runtime.trace(), "PUT", PUT_PATH).len(), 1);
    assert_eq!(operations(&runtime.trace(), "PATCH", PATCH_PATH).len(), 0);
    assert_eq!(operations(&runtime.trace(), "DELETE", PATCH_PATH).len(), 0);
    runtime.finish().await;
}

#[tokio::test]
async fn requester_refresh_never_dispatches_for_final_or_uncertain_outcomes() {
    let runtime = Runtime::start().await;
    for scenario in [
        Scenario {
            roles: vec!["write"],
            ..Scenario::default()
        },
        Scenario {
            writes: VecDeque::from([WriteReply::Timeout]),
            ..Scenario::default()
        },
    ] {
        let command = runtime.command(scenario, Permission::Push).await;
        let (receipt, _) = runtime.create(&command).await;
        assert!(matches!(
            receipt.outcome,
            CreateOutcome::AlreadySatisfied { .. } | CreateOutcome::OutcomeUnknown
        ));
        let before = runtime.trace().len();
        let response = refresh(&runtime, &command.delivery_key()).await;
        assert_eq!(response.status, 200, "{response:?}");
        assert_eq!(response.body["kind"], "not_tracked");
        let journal = runtime
            .harness
            .restate
            .admin()
            .journal(response.invocation_id())
            .await;
        assert!(
            journal.iter().all(|e| !e.is_run()),
            "untracked refresh has no clock or external run"
        );
        assert_eq!(runtime.trace().len(), before);
    }
    runtime.finish().await;
}
