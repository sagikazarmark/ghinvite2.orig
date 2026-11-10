use super::*;
use ghinvite_core::delivery_control::{HoldWindow, LinkControl};

async fn facts(runtime: &Runtime, command: &CreateCommand) -> Vec<Value> {
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT id,event_type,occurred_at,metadata FROM audit_events WHERE target_id=? AND event_type IN ('delivery.blocked','delivery.outcome_unknown','delivery.held','delivery.released') ORDER BY occurred_at,id",
    )
    .bind(command.invitation_id.expect("retained V1 fixture identity").to_string())
    .fetch_all(&runtime.github.sql)
    .await
    .unwrap();
    rows.into_iter().map(|(id,kind,at,metadata)| json!({"id":id,"kind":kind,"at":at,"metadata":serde_json::from_str::<Value>(&metadata).unwrap()})).collect()
}

async fn wait_facts(runtime: &Runtime, command: &CreateCommand, count: usize) -> Vec<Value> {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let rows = facts(runtime, command).await;
            if rows.len() >= count {
                return rows;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("retained transition audits must reach SQL")
}

fn held(command: &CreateCommand, epoch: u64) -> LinkControl {
    LinkControl::AutoPause {
        window: HoldWindow {
            epoch,
            window_start: command.approved_at - chrono::Duration::minutes(5),
            paused_at: Utc::now(),
        },
    }
}

#[tokio::test]
async fn transient_audits_survive_projection_outage_late_ack_and_later_confirmation() {
    let runtime = Runtime::start().await;
    runtime
        .faults
        .projection_unavailable
        .store(true, Ordering::SeqCst);
    let command = runtime
        .command(
            Scenario {
                writes: VecDeque::from([
                    WriteReply::Policy(
                        403,
                        "Inviting outside collaborators is restricted by organization policy.",
                    ),
                    WriteReply::Policy(
                        403,
                        "Inviting outside collaborators is restricted by organization policy.",
                    ),
                    WriteReply::Timeout,
                ]),
                ..Default::default()
            },
            Permission::Push,
        )
        .await;
    let first = runtime.create(&command).await.0;
    assert_eq!(
        first.audit_version,
        ghinvite_core::delivery_audit::DeliveryAuditVersion::CanonicalV1
    );
    assert!(matches!(first.outcome, CreateOutcome::Blocked { .. }));
    let second = runtime.create(&command).await.0;
    assert_eq!(second.outcome, first.outcome);
    assert!(second.revision > first.revision);
    let uncertain = runtime.create(&command).await.0;
    assert_eq!(uncertain.audit_version, first.audit_version);
    assert_eq!(uncertain.outcome, CreateOutcome::OutcomeUnknown);
    for _ in 0..2 {
        assert_eq!(
            runtime.create(&command).await.0.outcome,
            CreateOutcome::OutcomeUnknown
        );
    }
    assert!(
        facts(&runtime, &command).await.is_empty(),
        "SQL is withheld independently of the owner"
    );
    runtime.github.scenario.lock().unwrap().pending = Some("write");
    let confirmed = runtime.create(&command).await.0;
    assert_eq!(confirmed.audit_version, first.audit_version);
    assert_eq!(
        confirmed.outcome,
        CreateOutcome::Created { upstream_id: 99 }
    );
    runtime
        .faults
        .lose_projection_ack
        .store(true, Ordering::SeqCst);
    runtime
        .faults
        .projection_unavailable
        .store(false, Ordering::SeqCst);
    let rows = wait_facts(&runtime, &command, 2).await;
    assert_eq!(
        rows.len(),
        2,
        "repeated blocked/unknown polls are not audit transitions"
    );
    let blocked = rows
        .iter()
        .find(|row| row["kind"] == "delivery.blocked")
        .unwrap();
    assert_eq!(blocked["metadata"]["transition_revision"], first.revision);
    assert_eq!(
        blocked["metadata"]["transition"]["reason"],
        json!({"kind":"repository","reason":"outside_collaborators_restricted"})
    );
    let unknown = rows
        .iter()
        .find(|row| row["kind"] == "delivery.outcome_unknown")
        .unwrap();
    assert_eq!(
        unknown["metadata"]["transition_revision"],
        uncertain.revision
    );
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let revision: Option<i64> =
                sqlx::query_scalar("SELECT projection_revision FROM deliveries WHERE id=?")
                    .bind(command.delivery_key())
                    .fetch_optional(&runtime.github.sql)
                    .await
                    .unwrap();
            if revision == Some(confirmed.revision as i64) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(runtime.create(&command).await.0, confirmed);
    assert_eq!(
        facts(&runtime, &command).await,
        rows,
        "late SQL acknowledgment and create replay preserve fact IDs and times"
    );
    assert_eq!(operations(&runtime.trace(), "PUT", PUT_PATH).len(), 3);
    runtime.finish().await;
}

#[tokio::test]
async fn killed_owner_drains_its_retained_fact_before_publishing_a_newer_receipt() {
    let runtime = Runtime::start().await;
    let command = runtime
        .command(
            Scenario {
                writes: VecDeque::from([
                    WriteReply::Policy(
                        403,
                        "Inviting outside collaborators is restricted by organization policy.",
                    ),
                    WriteReply::Status(201),
                ]),
                ..Default::default()
            },
            Permission::Push,
        )
        .await;
    *runtime.faults.stage.lock().unwrap() = Some("after-receipt".into());
    let sent = runtime
        .harness
        .restate
        .invoke(
            &Call::object("RepositoryDelivery", &command.delivery_key(), "create").send(),
            Some(&json!(command)),
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
    let first = control::status(&runtime, &command).await.unwrap();
    assert!(matches!(
        first.create.outcome,
        CreateOutcome::Blocked { .. }
    ));
    let retained_before_kill = Utc::now();
    let durable: Value = runtime.harness.client.post(format!("{}/query",runtime.harness.restate.admin_url()))
        .header("accept","application/json")
        .json(&json!({"query":format!("SELECT key,value_utf8 FROM state WHERE service_name='RepositoryDelivery' AND service_key='{}'",command.delivery_key())}))
        .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    let state: Value = serde_json::from_str(
        durable["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["key"] == "repository_audit")
            .unwrap()["value_utf8"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        state["snapshot"],
        json!(first),
        "snapshot and audit share one authoritative record"
    );
    assert_eq!(state["cursor"]["revision"], first.revision);
    assert_eq!(state["pending"].as_array().unwrap().len(), 1);
    assert_eq!(state["pending"][0]["transition_revision"], first.revision);
    assert!(
        durable["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["key"] != "snapshot" && row["key"] != "receipt")
    );
    assert!(
        facts(&runtime, &command).await.is_empty(),
        "owner has retained the fact but has not sent it"
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
    runtime
        .faults
        .lose_projection_ack
        .store(true, Ordering::SeqCst);
    let confirmed = runtime.create(&command).await.0;
    assert_eq!(
        confirmed.outcome,
        CreateOutcome::Created { upstream_id: 99 }
    );
    let rows = wait_facts(&runtime, &command, 1).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["metadata"]["transition_revision"], first.revision);
    let fact_time = chrono::DateTime::parse_from_rfc3339(rows[0]["at"].as_str().unwrap()).unwrap();
    assert_eq!(
        fact_time,
        chrono::DateTime::parse_from_rfc3339(state["pending"][0]["at"].as_str().unwrap()).unwrap()
    );
    assert!(
        fact_time <= retained_before_kill,
        "recovery never replaces the original fact time"
    );
    assert_eq!(runtime.create(&command).await.0, confirmed);
    assert_eq!(facts(&runtime, &command).await, rows);
    assert_eq!(operations(&runtime.trace(), "PUT", PUT_PATH).len(), 2);
    runtime.finish().await;
}

#[tokio::test]
async fn held_epochs_and_release_are_retained_but_revoke_never_claims_release() {
    let runtime = Runtime::start().await;
    let command = runtime.command(Scenario::default(), Permission::Push).await;
    let first_hold = control::input(&command, 1, 1, held(&command, 1));
    control::control(&runtime, &command, &first_hold).await;
    let first = runtime.create(&command).await.0;
    let renewed = control::input(&command, 2, 2, held(&command, 2));
    control::control(&runtime, &command, &renewed).await;
    let newer = control::status(&runtime, &command).await.unwrap();
    assert!(
        newer.revision > first.revision,
        "a new hold epoch has its own immutable transition identity"
    );
    control::control(&runtime, &command, &renewed).await;
    let release = control::input(&command, 3, 2, LinkControl::Active);
    control::control(&runtime, &command, &release).await;
    assert!(matches!(
        runtime.create(&command).await.0.outcome,
        CreateOutcome::Created { .. }
    ));
    let rows = wait_facts(&runtime, &command, 3).await;
    assert_eq!(rows.len(), 3);
    let mut epochs: Vec<_> = rows
        .iter()
        .filter(|r| r["kind"] == "delivery.held")
        .map(|r| r["metadata"]["transition"]["hold_epoch"].as_u64().unwrap())
        .collect();
    epochs.sort_unstable();
    assert_eq!(epochs, [1, 2]);
    let released = rows
        .iter()
        .find(|r| r["kind"] == "delivery.released")
        .unwrap();
    assert_eq!(released["metadata"]["transition"]["hold_epoch"], 2);
    assert_eq!(operations(&runtime.trace(), "PUT", PUT_PATH).len(), 1);

    let aborted = runtime.command(Scenario::default(), Permission::Push).await;
    let LinkControl::AutoPause { window } = held(&aborted, 1) else {
        unreachable!()
    };
    control::control(
        &runtime,
        &aborted,
        &control::input(
            &aborted,
            1,
            1,
            LinkControl::AutoPause {
                window: window.clone(),
            },
        ),
    )
    .await;
    runtime.create(&aborted).await;
    control::control(
        &runtime,
        &aborted,
        &control::input(
            &aborted,
            2,
            1,
            LinkControl::Revoked {
                cancel_pending_invitations: false,
                decline_pending_requests: false,
                abort_hold: Some(window),
            },
        ),
    )
    .await;
    assert!(matches!(
        runtime.create(&aborted).await.0.outcome,
        CreateOutcome::Aborted { .. }
    ));
    let rows = wait_facts(&runtime, &aborted, 1).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["kind"], "delivery.held");
    assert!(operations(&runtime.trace(), "PUT", PUT_PATH).is_empty());
    runtime.finish().await;
}
