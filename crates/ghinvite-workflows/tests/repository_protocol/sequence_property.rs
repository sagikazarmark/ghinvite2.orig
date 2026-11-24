//! Generated AC-DLV-3 acceptance through the actual repository owner.
use super::*;
#[path = "../common/delivery_sequence.rs"]
mod generated;
pub(super) use generated::WireReply;
use generated::{Action, Evidence, Oracle, Sample};
use ghinvite_core::{delivery::DeliverySnapshot, storage::ConsoleStorage};
use ghinvite_workflows::settlement_policy::{Evidence as ObservationEvidence, Observation};
use std::collections::{BTreeMap, BTreeSet};

/// Only generated cases opt into this provider seam. Existing fixed fixtures
/// keep their own write scripts and response semantics unchanged.
pub(super) fn scripted_write(
    state: &Arc<Github>,
    method: &str,
    path: &str,
    body: &Value,
    fence: Option<(i64, i64)>,
    credential: &Option<(String, usize)>,
) -> Option<(Response, bool)> {
    if !matches!((method, path), ("PUT", PUT_PATH) | ("PATCH", PATCH_PATH)) {
        return None;
    }
    let mut s = state.scenario.lock().unwrap();
    if !s.sequence_property {
        return None;
    }
    assert_eq!(credential.as_ref().map(|c| c.0.as_str()), Some("write"));
    assert_eq!(
        *body,
        if method == "PUT" {
            json!({"permission":"push"})
        } else {
            json!({"permissions":"write"})
        }
    );
    assert!(
        fence.is_some_and(|(_, retryable)| retryable == 0),
        "actual HTTP write must hold the physical fence"
    );
    let reply = s
        .sequence_replies
        .pop_front()
        .expect("generated response script exhausted");
    s.sequence_consumed.push(reply);
    s.trace.push(Trace {
        method: method.into(),
        path: path.into(),
        body: body.clone(),
        fence,
        credential: credential.clone(),
    });
    if reply == WireReply::Created {
        s.pending = Some("write");
        s.created_at.get_or_insert_with(Utc::now);
    }
    let status = axum::http::StatusCode::from_u16(reply.status()).unwrap();
    let response = match reply {
        WireReply::Created => (
            status,
            Json({
                let mut i = invitation("write");
                i["created_at"] = json!(s.created_at);
                i
            }),
        )
            .into_response(),
        WireReply::NoContent => status.into_response(),
        WireReply::PrimaryLimit => (
            status,
            [
                ("x-ratelimit-remaining", "0".into()),
                (
                    "x-ratelimit-reset",
                    (Utc::now().timestamp() + 1).to_string(),
                ),
            ],
            Json(reply.body()),
        )
            .into_response(),
        WireReply::SecondaryLimit => {
            (status, [("retry-after", "1")], Json(reply.body())).into_response()
        }
        _ => (status, Json(reply.body())).into_response(),
    };
    Some((response, reply == WireReply::Timeout))
}
async fn status(runtime: &Runtime, command: &CreateCommand) -> DeliverySnapshot {
    let response = runtime
        .harness
        .restate
        .invoke(
            &Call::object("RepositoryDelivery", &command.delivery_key(), "status"),
            None::<&Value>,
            None,
        )
        .await;
    assert_eq!(response.status, 200, "{response:?}");
    serde_json::from_value(response.body).unwrap()
}
async fn sample(runtime: &Runtime, command: &CreateCommand) -> Sample {
    let snapshot = status(runtime, command).await;
    let (consumed, calls, presence_listings) = {
        let s = runtime.github.scenario.lock().unwrap();
        (
            s.sequence_consumed.clone(),
            s.trace
                .iter()
                .map(|t| json!({"method":t.method,"path":t.path,"body":t.body,"fence":t.fence}))
                .collect::<Vec<_>>(),
            s.presence_listings,
        )
    };
    let mutations = runtime
        .trace()
        .iter()
        .filter(|t| {
            matches!(
                (t.method.as_str(), t.path.as_str()),
                ("PUT", PUT_PATH)
                    | ("PATCH", PATCH_PATH)
                    | ("DELETE", PUT_PATH)
                    | ("DELETE", PATCH_PATH)
            )
        })
        .count();
    Sample {
        snapshot: json!(snapshot),
        mutations: BTreeMap::from([("root".into(), mutations)]),
        consumed,
        audit_settlements: generated::settlement_facts(runtime.storage.as_ref()).await,
        counters: json!({"fence":runtime.fence(command).await,"calls":calls,"presence_listings":presence_listings,"lost_run_acknowledgements":runtime.faults.http_result_losses.load(Ordering::SeqCst),"projection":runtime.storage.get_delivery(&command.delivery_key()).await.unwrap()}),
    }
}
async fn observe(runtime: &Runtime, command: &CreateCommand, action: Action) {
    let snapshot = status(runtime, command).await;
    let evidence = match action {
        Action::Present => {
            if matches!(snapshot.create.outcome, CreateOutcome::OutcomeUnknown) {
                ObservationEvidence::FoundByInvitee {
                    upstream_id: 99,
                    permission: EffectiveRole::Write,
                }
            } else {
                ObservationEvidence::Listed {
                    upstream_id: 99,
                    expired: false,
                    created_at: None,
                }
            }
        }
        Action::Expired => ObservationEvidence::Listed {
            upstream_id: 99,
            expired: true,
            created_at: None,
        },
        Action::Accepted => ObservationEvidence::Absent {
            availability_verified: true,
            role_name: Some(EffectiveRole::Write),
            direct_collaborator: Some(true),
            membership: None,
        },
        Action::Absent => ObservationEvidence::Absent {
            availability_verified: true,
            role_name: Some(EffectiveRole::None),
            direct_collaborator: Some(false),
            membership: None,
        },
        _ => unreachable!(),
    };
    {
        let mut s = runtime.github.scenario.lock().unwrap();
        s.role_reads = 0;
        match action {
            Action::Present | Action::Expired => {
                s.pending = Some("write");
                s.created_at = Some(Utc::now());
                s.roles = vec!["none"];
            }
            Action::Accepted => {
                s.pending = None;
                s.roles = vec!["write"];
            }
            Action::Absent => {
                s.pending = None;
                s.roles = vec!["none"];
            }
            _ => unreachable!(),
        }
    }
    let observation =
        serde_json::to_value(Observation::new(&snapshot, Utc::now(), evidence)).unwrap();
    let response = runtime
        .harness
        .restate
        .invoke(
            &Call::object("RepositoryDelivery", &command.delivery_key(), "reconcile"),
            Some(&observation),
            None,
        )
        .await;
    assert_eq!(response.status, 200, "{response:?}");
}
#[tokio::test]
async fn ac_dlv_3_generated_repository_response_sequences_preserve_one_create_and_one_settlement() {
    let mut consumed = BTreeSet::new();
    let mut controls = BTreeSet::new();
    let mut settlements = 0;
    let mut multi_response = 0;
    let mut seeds = vec![];
    for plan in generated::plans() {
        let mut evidence = Evidence::new("repository", plan.clone());
        let runtime = Runtime::start().await;
        let command = runtime
            .command(
                Scenario {
                    sequence_property: true,
                    sequence_replies: plan.replies.iter().copied().collect(),
                    writes: VecDeque::new(),
                    ..Default::default()
                },
                Permission::Push,
            )
            .await;
        if plan.lose_run_acks {
            runtime.faults.lose_http_results.store(3, Ordering::SeqCst);
        }
        let mut oracle = Oracle::default();
        let mut previous: Option<Sample> = None;
        for (index, action) in plan.actions.iter().copied().enumerate() {
            evidence.before(index, action);
            match action {
                Action::Create | Action::Replay => {
                    runtime.create(&command).await;
                }
                Action::Status => {
                    status(&runtime, &command).await;
                }
                Action::Present | Action::Absent | Action::Accepted | Action::Expired => {
                    observe(&runtime, &command, action).await
                }
                Action::WaitDeadline => {
                    if let Some(sample) = &previous {
                        generated::wait_deadline(&sample.snapshot).await;
                    }
                }
            }
            let sample = sample(&runtime, &command).await;
            evidence.after(index, action, &sample, vec![]);
            // A pending recheck wake runs its own read-only recovery, so an
            // unknown create may resolve during this step from a listing that
            // still showed the invitation. Only absence must never resolve it.
            if action == Action::Absent
                && previous.as_ref().is_some_and(|s| {
                    s.snapshot["create"]["outcome"]["kind"] == "outcome_unknown"
                        && s.counters["presence_listings"] == sample.counters["presence_listings"]
                })
                && sample.snapshot["create"]["outcome"]["kind"] != "outcome_unknown"
            {
                evidence.fail("absence resolved unknown create");
            }
            if let Err(error) = oracle.check(&sample) {
                evidence.after(index, action, &sample, vec![]);
                evidence.fail(&error);
            }
            let checks = oracle.controls(&sample);
            controls.extend(checks.iter().map(|s| s.to_string()));
            evidence.after(index, action, &sample, checks);
            consumed.extend(sample.consumed.iter().copied());
            previous = Some(sample);
        }
        let mut final_sample = previous.unwrap();
        if !final_sample.snapshot["settlement"].is_null() {
            settlements += 1;
        }
        if generated::has_settlement(&final_sample.snapshot) {
            let result = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let current = sample(&runtime, &command).await;
                    if generated::audits_complete(&current) {
                        break current;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await;
            match result {
                Ok(current) => final_sample = current,
                Err(_) => {
                    evidence.fail("retained settlement did not project its durable audit fact")
                }
            }
            if let Err(error) = oracle.check(&final_sample) {
                evidence.fail(&error);
            }
            evidence.after(
                plan.actions.len(),
                Action::Status,
                &final_sample,
                oracle.controls(&final_sample),
            );
        }
        assert!(
            !final_sample.consumed.is_empty(),
            "seed did not issue its forced initial HTTP write"
        );
        assert_eq!(final_sample.consumed[0], plan.replies[0]);
        if plan.lose_run_acks {
            assert_eq!(runtime.faults.http_result_losses.load(Ordering::SeqCst), 3);
        }
        if final_sample.consumed.len() > 1 {
            multi_response += 1;
        }
        seeds.push(plan.seed);
        evidence.finish();
        runtime.finish().await;
    }
    generated::require_coverage(&consumed, &controls, settlements, multi_response);
    generated::write_summary(
        "repository",
        &seeds,
        &consumed,
        &controls,
        settlements,
        multi_response,
    );
}
