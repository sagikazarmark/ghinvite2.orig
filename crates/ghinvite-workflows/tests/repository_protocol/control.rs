use super::*;
use ghinvite_core::delivery_control::{
    ControlEnvelope, DeliveryKind, Disposition, HoldWindow, Identity, LinkControl,
};
use ghinvite_workflows::delivery_control::{
    ControlWake, DeliveryControlInput, DeliveryControlResult,
};

pub(super) fn input(
    command: &CreateCommand,
    revision: u64,
    hold_epoch: u64,
    control: LinkControl,
) -> DeliveryControlInput {
    DeliveryControlInput {
        envelope: ControlEnvelope {
            identity: Identity {
                batch: None,
                link_id: command.link_id,
                request_id: command.request_id,
                kind: DeliveryKind::Repository,
                target_id: command.repo_id,
                delivery_generation: command.delivery_generation,
            },
            operation_id: RequestId::new().to_string().try_into().unwrap(),
            revision,
            hold_epoch,
            at: Utc::now(),
            control,
        },
        admitted_at: command.approved_at - chrono::Duration::seconds(1),
        invitation_lifetime_seconds: 604800,
    }
}
fn hold(command: &CreateCommand, epoch: u64) -> HoldWindow {
    HoldWindow {
        epoch,
        window_start: command.approved_at - chrono::Duration::minutes(5),
        paused_at: Utc::now(),
    }
}
pub(super) async fn control(
    runtime: &Runtime,
    command: &CreateCommand,
    input: &DeliveryControlInput,
) -> DeliveryControlResult {
    let reply = runtime
        .harness
        .restate
        .invoke(
            &Call::object("RepositoryDelivery", &command.delivery_key(), "control"),
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
    assert!(journal.iter().all(|e| !e.raw_contains(MARKER)));
    serde_json::from_value(reply.body).unwrap()
}
pub(super) async fn status(
    runtime: &Runtime,
    command: &CreateCommand,
) -> Option<ghinvite_core::delivery::DeliverySnapshot> {
    let reply = runtime
        .harness
        .restate
        .invoke(
            &Call::object("RepositoryDelivery", &command.delivery_key(), "status"),
            None,
            None,
        )
        .await;
    assert_eq!(reply.status, 200, "{reply:?}");
    serde_json::from_value(reply.body).unwrap()
}
async fn wait_created(runtime: &Runtime, command: &CreateCommand) -> CreateReceipt {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(snapshot) = status(runtime, command).await
                && snapshot.create.outcome.confirmed()
            {
                return snapshot.create;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("released control wake must complete")
}
#[tokio::test]
async fn precreate_hold_release_receipts_and_wakes_preserve_one_write() {
    let runtime = Runtime::start().await;
    let command = runtime.command(Scenario::default(), Permission::Push).await;
    let hold = input(
        &command,
        1,
        1,
        LinkControl::AutoPause {
            window: hold(&command, 1),
        },
    );
    let original = control(&runtime, &command, &hold).await;
    assert_eq!(original.disposition, Disposition::Held { epoch: 1 });
    assert!(!original.initialized);
    assert!(status(&runtime, &command).await.is_none());
    let held = runtime.create(&command).await.0;
    assert!(matches!(held.outcome, CreateOutcome::Held { .. }));
    assert!(operations(&runtime.trace(), "PUT", PUT_PATH).is_empty());
    assert_eq!(
        control(&runtime, &command, &hold).await,
        original,
        "historical operation result remains before-initialize result"
    );
    let release = input(&command, 2, 1, LinkControl::Active);
    let result = control(&runtime, &command, &release).await;
    assert_eq!(result.disposition, Disposition::Ready);
    let confirmed = wait_created(&runtime, &command).await;
    assert_eq!(
        confirmed.outcome,
        CreateOutcome::Created { upstream_id: 99 }
    );
    assert_eq!(operations(&runtime.trace(), "PUT", PUT_PATH).len(), 1);
    assert_eq!(control(&runtime, &command, &release).await, result);
    assert_eq!(control(&runtime, &command, &hold).await, original);
    let mut changed = hold.clone();
    changed.envelope.control = LinkControl::Active;
    let refused = runtime
        .harness
        .restate
        .invoke(
            &Call::object("RepositoryDelivery", &command.delivery_key(), "control"),
            Some(&json!(changed)),
            None,
        )
        .await;
    assert_eq!(refused.status, 409);
    for generation in [1, 2, 2] {
        let wake = ControlWake {
            identity: release.envelope.identity.clone(),
            generation,
        };
        let reply = runtime
            .harness
            .restate
            .invoke(
                &Call::object(
                    "RepositoryDelivery",
                    &command.delivery_key(),
                    "control_wake",
                ),
                Some(&json!(wake)),
                None,
            )
            .await;
        assert_eq!(reply.status, 200);
    }
    assert_eq!(runtime.create(&command).await.0, confirmed);
    assert_eq!(operations(&runtime.trace(), "PUT", PUT_PATH).len(), 1);
    runtime.finish().await;
}
#[tokio::test]
async fn unseen_hold_revoke_aborts_before_create_and_old_wakes_cannot_dispatch() {
    let runtime = Runtime::start().await;
    let command = runtime.command(Scenario::default(), Permission::Push).await;
    let revoke = input(
        &command,
        2,
        1,
        LinkControl::Revoked {
            cancel_pending_invitations: false,
            decline_pending_requests: false,
            abort_hold: Some(hold(&command, 1)),
        },
    );
    assert_eq!(
        control(&runtime, &command, &revoke).await.disposition,
        Disposition::Aborted
    );
    let aborted = runtime.create(&command).await.0;
    assert_eq!(
        aborted.outcome,
        CreateOutcome::Aborted {
            reason: ghinvite_core::delivery::AbortReason::LinkRevoked
        }
    );
    assert_eq!(runtime.create(&command).await.0, aborted);
    assert!(
        runtime
            .trace()
            .iter()
            .all(|r| r.method != "PUT" && r.method != "DELETE")
    );
    let wake = ControlWake {
        identity: revoke.envelope.identity.clone(),
        generation: 1,
    };
    let result = runtime
        .harness
        .restate
        .invoke(
            &Call::object(
                "RepositoryDelivery",
                &command.delivery_key(),
                "control_wake",
            ),
            Some(&json!(wake)),
            None,
        )
        .await;
    assert_eq!(result.status, 200);
    assert_eq!(runtime.create(&command).await.0, aborted);
    runtime.finish().await;
}
#[tokio::test]
async fn uncertain_write_cannot_be_held_aborted_or_deleted_and_late_presence_keeps_cancel_intent() {
    let runtime = Runtime::start().await;
    let command = runtime
        .command(
            Scenario {
                writes: VecDeque::from([WriteReply::Timeout]),
                ..Default::default()
            },
            Permission::Push,
        )
        .await;
    assert_eq!(
        runtime.create(&command).await.0.outcome,
        CreateOutcome::OutcomeUnknown
    );
    let invocation = runtime.last_create.lock().unwrap().clone().unwrap();
    let journal = runtime.harness.restate.admin().journal(&invocation).await;
    let write = journal
        .iter()
        .find(|e| e.is_run() && e.name.as_deref() == Some("guarded_github_create"))
        .unwrap();
    let marker = journal
        .iter()
        .find(|e| e.entry_type.contains("SetState") && e.raw_contains("control_may_have_written"))
        .expect("uncertainty marker is retained before I/O");
    assert!(
        marker.index < write.index,
        "an interrupted run must leave durable uncertainty before the external write"
    );
    let auto = input(
        &command,
        1,
        1,
        LinkControl::AutoPause {
            window: hold(&command, 1),
        },
    );
    assert_eq!(
        control(&runtime, &command, &auto).await.disposition,
        Disposition::RecoverUnknown
    );
    let revoke = input(
        &command,
        2,
        1,
        LinkControl::Revoked {
            cancel_pending_invitations: true,
            decline_pending_requests: false,
            abort_hold: match &auto.envelope.control {
                LinkControl::AutoPause { window } => Some(window.clone()),
                _ => unreachable!(),
            },
        },
    );
    assert_eq!(
        control(&runtime, &command, &revoke).await.disposition,
        Disposition::RecoverUnknown
    );
    assert_eq!(
        runtime.create(&command).await.0.outcome,
        CreateOutcome::OutcomeUnknown
    );
    assert_eq!(operations(&runtime.trace(), "PUT", PUT_PATH).len(), 1);
    runtime.github.scenario.lock().unwrap().pending = Some("write");
    let confirmed = runtime.create(&command).await.0;
    assert_eq!(
        confirmed.outcome,
        CreateOutcome::Created { upstream_id: 99 }
    );
    assert_eq!(runtime.create(&command).await.0, confirmed);
    assert_eq!(operations(&runtime.trace(), "PUT", PUT_PATH).len(), 1);
    assert!(runtime.trace().iter().all(|r| r.method != "DELETE"));
    let state:Value=runtime.harness.client.post(format!("{}/query",runtime.harness.restate.admin_url())).header("accept","application/json").json(&json!({"query":format!("SELECT key,value_utf8 FROM state WHERE service_name='RepositoryDelivery' AND service_key='{}'",command.delivery_key())})).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    assert!(
        state["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["key"] == "control_cancellation_pending" && r["value_utf8"] == "true"),
        "{state}"
    );
    runtime.finish().await;
}
#[tokio::test]
async fn held_blocked_retry_is_invalidated_and_foreign_cascade_preserves_receipt() {
    let runtime = Runtime::start().await;
    let command = runtime
        .command(
            Scenario {
                writes: VecDeque::from([WriteReply::Policy(
                    403,
                    "You are not allowed to invite outside collaborators to this repository",
                )]),
                ..Default::default()
            },
            Permission::Push,
        )
        .await;
    sqlx::query("CREATE TRIGGER hold_release BEFORE UPDATE ON delivery_attempts WHEN NEW.retryable=1 BEGIN SELECT RAISE(ABORT, 'fixture release unavailable'); END").execute(&runtime.github.sql).await.unwrap();
    let first = runtime.create(&command).await.0;
    assert!(matches!(first.outcome, CreateOutcome::Blocked { .. }));
    assert_eq!(
        runtime.fence(&command).await,
        Some((1, 0)),
        "positive no-effect receipt survives unreleased fence"
    );
    let auto = input(
        &command,
        1,
        1,
        LinkControl::AutoPause {
            window: hold(&command, 1),
        },
    );
    assert!(matches!(
        control(&runtime, &command, &auto).await.disposition,
        Disposition::Held { .. }
    ));
    let wake = ghinvite_workflows::delivery::DeliveryRecheck {
        command: command.clone(),
        generation: 1,
    };
    let result = runtime
        .harness
        .restate
        .invoke(
            &Call::object("RepositoryDelivery", &command.delivery_key(), "recheck"),
            Some(&json!(wake)),
            None,
        )
        .await;
    assert_eq!(result.status, 200);
    assert_eq!(operations(&runtime.trace(), "PUT", PUT_PATH).len(), 1);
    runtime.finish().await;

    let runtime = Runtime::start().await;
    let command = runtime
        .command(
            Scenario {
                pending: Some("admin"),
                ..Default::default()
            },
            Permission::Push,
        )
        .await;
    let confirmed = runtime.create(&command).await.0;
    assert!(matches!(
        confirmed.outcome,
        CreateOutcome::AlreadyPending { .. }
    ));
    let revoke = input(
        &command,
        1,
        0,
        LinkControl::Revoked {
            cancel_pending_invitations: true,
            decline_pending_requests: false,
            abort_hold: None,
        },
    );
    assert_eq!(
        control(&runtime, &command, &revoke).await.disposition,
        Disposition::StopTrackingForeign
    );
    let snapshot = status(&runtime, &command).await.unwrap();
    assert_eq!(snapshot.create, confirmed);
    assert!(snapshot.settlement.is_none());
    assert_eq!(
        snapshot.tracking_stopped,
        Some(ghinvite_core::delivery::SettlementReason::LinkRevoked)
    );
    assert!(
        runtime
            .trace()
            .iter()
            .all(|r| r.method != "PUT" && r.method != "DELETE")
    );
    runtime.finish().await;
}

#[tokio::test]
async fn unavailable_fence_keeps_control_conservative_without_inventing_a_write_receipt() {
    let runtime = Runtime::start().await;
    let command = runtime.command(Scenario::default(), Permission::Push).await;
    control(
        &runtime,
        &command,
        &input(&command, 1, 0, LinkControl::Active),
    )
    .await;
    sqlx::query("ALTER TABLE delivery_attempts RENAME TO delivery_attempts_unavailable")
        .execute(&runtime.github.sql)
        .await
        .unwrap();
    let reply = runtime
        .harness
        .restate
        .invoke(
            &Call::object("RepositoryDelivery", &command.delivery_key(), "create"),
            Some(&json!(command)),
            None,
        )
        .await;
    assert_eq!(reply.status, 503);
    assert!(status(&runtime, &command).await.is_none());
    assert!(
        !runtime
            .github
            .scenario
            .lock()
            .unwrap()
            .trace
            .iter()
            .any(|t| t.method == "PUT")
    );
    sqlx::query("ALTER TABLE delivery_attempts_unavailable RENAME TO delivery_attempts")
        .execute(&runtime.github.sql)
        .await
        .unwrap();
    let created = runtime.create(&command).await.0;
    assert!(matches!(created.outcome, CreateOutcome::Created { .. }));
    assert_eq!(runtime.create(&command).await.0, created);
    assert_eq!(
        runtime
            .github
            .scenario
            .lock()
            .unwrap()
            .trace
            .iter()
            .filter(|t| t.method == "PUT")
            .count(),
        1
    );
}
