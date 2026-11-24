use super::*;
use ghinvite_core::{InvitationState, reissue::RepositoryReissueBarrierQuery};
async fn expired(
    runtime: &Runtime,
    reply: WriteReply,
) -> (CreateCommand, ghinvite_core::delivery::DeliverySnapshot) {
    let command = runtime
        .command(
            Scenario {
                deletes: VecDeque::from([reply]),
                ..Default::default()
            },
            Permission::Push,
        )
        .await;
    runtime.create(&command).await;
    let snapshot = control::status(runtime, &command).await.unwrap();
    let observation = ghinvite_workflows::settlement_policy::Observation::new(
        &snapshot,
        Utc::now(),
        ghinvite_workflows::settlement_policy::Evidence::Listed {
            upstream_id: 99,
            expired: true,
            created_at: None,
        },
    );
    let reply = runtime
        .harness
        .restate
        .invoke(
            &Call::object("RepositoryDelivery", &command.delivery_key(), "reconcile"),
            Some(&json!(observation)),
            None,
        )
        .await;
    assert_eq!(reply.status, 200, "{reply:?}");
    let old = control::status(runtime, &command).await.unwrap();
    assert_eq!(
        old.settlement.as_ref().unwrap().state,
        InvitationState::Expired
    );
    (command, old)
}
fn query(command: &CreateCommand) -> RepositoryReissueBarrierQuery {
    RepositoryReissueBarrierQuery {
        operation_id: RequestId::new().to_string().try_into().unwrap(),
        command: command.clone(),
    }
}
async fn invoke(runtime: &Runtime, input: &RepositoryReissueBarrierQuery) -> Value {
    let reply = runtime
        .harness
        .restate
        .invoke(
            &Call::object(
                "RepositoryDelivery",
                &input.command.delivery_key(),
                "prepare_reissue",
            ),
            Some(&json!(input)),
            None,
        )
        .await;
    assert_eq!(reply.status, 200, "{reply:?}");
    let journal = runtime
        .harness
        .restate
        .admin()
        .journal(reply.invocation_id())
        .await;
    assert!(!journal.is_empty());
    let names: Vec<_> = journal
        .iter()
        .filter(|e| e.is_run())
        .map(|e| e.name.as_deref().unwrap())
        .collect();
    // Source-derived branches; do not infer an expectation from the journal.
    const PATHS: &[&[&str]] = &[
        &[],
        &["reissue_invitation_read"],
        &["reissue_invitation_read", "reissue_cancel_fence"],
        &[
            "reissue_invitation_read",
            "reissue_cancel_fence",
            "reissue_cancel_time",
            "reissue_guarded_cancel",
        ],
        &["reissue_unused_fence"],
        &[
            "reissue_unused_fence",
            "reissue_invitation_read",
            "reissue_cancel_fence",
        ],
        &[
            "reissue_unused_fence",
            "reissue_invitation_read",
            "reissue_cancel_fence",
            "reissue_cancel_time",
            "reissue_guarded_cancel",
        ],
    ];
    assert!(
        PATHS.contains(&names.as_slice()),
        "unreviewed reissue run path: {names:?}"
    );
    reply.body
}
#[tokio::test]
async fn reissue_cancels_still_pending_old_id_once_without_rewriting_terminal_receipt() {
    let runtime = Runtime::start_all(true, true, false).await;
    let (command, old) = expired(&runtime, WriteReply::Status(204)).await;
    let input = query(&command);
    assert_eq!(ready(&runtime, &input).await, "clear");
    for _ in 0..2 {
        assert_eq!(invoke(&runtime, &input).await, "clear");
    }
    assert_eq!(control::status(&runtime, &command).await.unwrap(), old);
    assert_eq!(
        runtime
            .github
            .scenario
            .lock()
            .unwrap()
            .trace
            .iter()
            .filter(|t| t.method == "DELETE")
            .count(),
        1
    );
    let mut wrong = input.clone();
    wrong.command.repo_full_name = "acme/other".into();
    let reply = runtime
        .harness
        .restate
        .invoke(
            &Call::object(
                "RepositoryDelivery",
                &command.delivery_key(),
                "prepare_reissue",
            ),
            Some(&json!(wrong)),
            None,
        )
        .await;
    assert_eq!(reply.status, 409);
    let fence: String = sqlx::query_scalar(
        "SELECT command FROM delivery_attempts WHERE delivery_key=? AND step='cancel'",
    )
    .bind(command.delivery_key())
    .fetch_one(&runtime.github.sql)
    .await
    .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&fence).unwrap()["reason"],
        "admin_reissue"
    );
    runtime.finish().await;
}
#[tokio::test]
async fn reissue_uncertain_delete_cannot_become_clear_from_later_absence() {
    let runtime = Runtime::start_all(true, true, false).await;
    let (command, old) = expired(&runtime, WriteReply::Timeout).await;
    let input = query(&command);
    assert_eq!(ready(&runtime, &input).await, "may_have_written");
    runtime.github.scenario.lock().unwrap().pending = None;
    assert_eq!(invoke(&runtime, &input).await, "may_have_written");
    assert_eq!(invoke(&runtime, &query(&command)).await, "may_have_written");
    assert_eq!(control::status(&runtime, &command).await.unwrap(), old);
    assert_eq!(
        runtime
            .github
            .scenario
            .lock()
            .unwrap()
            .trace
            .iter()
            .filter(|t| t.method == "DELETE")
            .count(),
        1
    );
    runtime.finish().await;
}
#[tokio::test]
async fn reissue_positive_absence_needs_no_delete_or_content_slot() {
    let runtime = Runtime::start_all(true, true, false).await;
    let (command, old) = expired(&runtime, WriteReply::Status(204)).await;
    runtime.github.scenario.lock().unwrap().pending = None;
    assert_eq!(invoke(&runtime, &query(&command)).await, "clear");
    assert_eq!(control::status(&runtime, &command).await.unwrap(), old);
    assert!(
        !runtime
            .github
            .scenario
            .lock()
            .unwrap()
            .trace
            .iter()
            .any(|t| t.method == "DELETE")
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM delivery_attempts WHERE delivery_key=? AND step='cancel'",
    )
    .bind(command.delivery_key())
    .fetch_one(&runtime.github.sql)
    .await
    .unwrap();
    assert_eq!(count, 0);
    runtime.finish().await;
}

async fn ready(runtime: &Runtime, input: &RepositoryReissueBarrierQuery) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let value = invoke(runtime, input).await;
            if value != "pending" {
                break value;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn killed_reissue_before_fence_recovers_only_after_positive_absence_of_write_fence() {
    use std::sync::atomic::Ordering;
    let runtime = Runtime::start_all(true, true, false).await;
    let (command, old) = expired(&runtime, WriteReply::Status(204)).await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    *runtime.faults.stage.lock().unwrap() = Some("reissue-before-fence".into());
    let input = query(&command);
    let sent = runtime
        .harness
        .restate
        .invoke(
            &Call::object(
                "RepositoryDelivery",
                &command.delivery_key(),
                "prepare_reissue",
            )
            .send(),
            Some(&json!(input)),
            None,
        )
        .await;
    assert_eq!(sent.status, 202);
    tokio::time::timeout(Duration::from_secs(10), async {
        while runtime.faults.interruptions.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let raw:Value=runtime.harness.client.post(format!("{}/query",runtime.harness.restate.admin_url())).header("accept","application/json").json(&json!({"query":format!("SELECT key,value_utf8 FROM state WHERE service_name='RepositoryDelivery' AND service_key='{}'",command.delivery_key())})).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
            if raw["rows"].as_array().unwrap().iter().any(|row|row["key"]=="reissue_cancel_uncertain" && row["value_utf8"]=="true") { break; }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }).await.expect("pre-fence marker must be durably visible before killing its invocation");
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM delivery_attempts WHERE delivery_key=? AND step='cancel'",
    )
    .bind(command.delivery_key())
    .fetch_one(&runtime.github.sql)
    .await
    .unwrap();
    assert_eq!(count, 0);
    assert!(
        !runtime
            .github
            .scenario
            .lock()
            .unwrap()
            .trace
            .iter()
            .any(|t| t.method == "DELETE")
    );
    runtime
        .harness
        .restate
        .admin()
        .kill(sent.invocation_id())
        .await;
    runtime
        .harness
        .restate
        .admin()
        .await_status(sent.invocation_id(), &["completed"])
        .await;
    *runtime.faults.stage.lock().unwrap() = None;
    assert_eq!(ready(&runtime, &input).await, "clear");
    assert_eq!(control::status(&runtime, &command).await.unwrap(), old);
    assert_eq!(
        runtime
            .github
            .scenario
            .lock()
            .unwrap()
            .trace
            .iter()
            .filter(|t| t.method == "DELETE")
            .count(),
        1
    );
    runtime.finish().await;
}
