//! Generated AC-DLV-3 acceptance through the actual org owner and InviteBudget.
use super::*;
#[path = "../common/delivery_sequence.rs"]
mod generated;
pub(super) use generated::WireReply;
use generated::{Action, Evidence, Oracle, Sample};
use std::{collections::BTreeSet, time::Duration};

pub(super) fn scripted_write(
    state: &Arc<Github>,
    method: &str,
    path: &str,
    body: &Value,
) -> Option<(Response, bool)> {
    if (method, path) != ("POST", "/orgs/acme/invitations") {
        return None;
    }
    let mut s = state.0.lock().unwrap();
    if !s.sequence_property {
        return None;
    }
    assert_eq!(body["invitee_id"], 8);
    assert_eq!(body["role"], "direct_member");
    assert_eq!(body["team_ids"], json!([5]));
    let reply = s
        .sequence_replies
        .pop_front()
        .expect("generated org response script exhausted");
    s.sequence_consumed.push(reply);
    s.trace.push((method.into(), path.into(), body.clone()));
    if reply == WireReply::Created {
        s.pending = true;
        s.org = Some(("member", "pending"));
        s.created_at.get_or_insert_with(Utc::now);
        s.attached = vec![5];
    }
    let status = axum::http::StatusCode::from_u16(reply.status()).unwrap();
    let response = match reply {
        WireReply::Created => (status, axum::Json(invitation(&s))).into_response(),
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
            axum::Json(reply.body()),
        )
            .into_response(),
        WireReply::SecondaryLimit => {
            (status, [("retry-after", "1")], axum::Json(reply.body())).into_response()
        }
        _ => (status, axum::Json(reply.body())).into_response(),
    };
    Some((response, reply == WireReply::Timeout))
}
async fn status(f: &Fixture, c: &OrganizationCreateCommand) -> OrganizationDeliverySnapshot {
    let response =
        f.h.restate
            .invoke(
                &Call::object("OrganizationDelivery", &c.delivery_key(), "status"),
                None,
                None,
            )
            .await;
    assert_eq!(response.status, 200, "{response:?}");
    serde_json::from_value(response.body).unwrap()
}
async fn sample(f: &Fixture, c: &OrganizationCreateCommand) -> Sample {
    let snapshot = status(f, c).await;
    let (consumed, calls, mutations) = {
        let s = f.github.0.lock().unwrap();
        let root = s
            .trace
            .iter()
            .filter(|t| {
                t.0 == "POST" && t.1 == "/orgs/acme/invitations"
                    || t.0 == "DELETE" && t.1 == "/orgs/acme/invitations/99"
            })
            .count();
        let team = s
            .trace
            .iter()
            .filter(|t| {
                matches!(t.0.as_str(), "PUT" | "DELETE")
                    && t.1 == "/orgs/acme/teams/team-5/memberships/alice"
            })
            .count();
        (
            s.sequence_consumed.clone(),
            s.trace
                .iter()
                .map(|t| json!({"method":t.0,"path":t.1,"body":t.2}))
                .collect::<Vec<_>>(),
            BTreeMap::from([("root".into(), root), ("team:5".into(), team)]),
        )
    };
    let budget = standard_call(f, "InviteBudget", "org:100", "snapshot", None).await;
    Sample {
        snapshot: json!(snapshot),
        mutations,
        consumed,
        audit_settlements: generated::settlement_facts(f.storage.as_ref()).await,
        counters: json!({"budget":budget,"calls":calls,"lost_run_acknowledgements":f.faults.lost_results.load(Ordering::SeqCst),"projection":f.storage.get_organization_delivery(&c.storage_id()).await.unwrap()}),
    }
}
async fn observe(f: &Fixture, c: &OrganizationCreateCommand, action: Action) {
    let created_at = {
        let mut s = f.github.0.lock().unwrap();
        match action {
            Action::Present => {
                s.pending = true;
                s.org = Some(("member", "pending"));
                s.attached = vec![5];
                s.created_at.get_or_insert_with(Utc::now);
            }
            Action::Accepted => {
                s.pending = false;
                s.org = Some(("member", "active"));
                s.teams.insert(5, ("member", "active"));
            }
            Action::Absent | Action::Expired => {
                s.pending = false;
                s.org = None;
                s.teams.clear();
            }
            _ => unreachable!(),
        }
        s.created_at.unwrap_or(c.approved_at)
    };
    let at = Utc::now();
    let mut observation = observed(c, at, action == Action::Present);
    if action == Action::Present {
        observation["pending"][0]["created_at"] = json!(created_at);
    }
    if action == Action::Expired {
        observation["failed"] = json!([{"invitation":{"upstream_id":99,"login":"alice","role":"direct_member","created_at":created_at,"team_count":1},"failed_at":at}]);
    }
    f.call(c, "observe_listed", observation).await;
}
#[tokio::test]
async fn ac_dlv_3_generated_organization_response_sequences_preserve_one_create_and_one_settlement()
{
    let mut consumed = BTreeSet::new();
    let mut controls = BTreeSet::new();
    let mut settlements = 0;
    let mut multi_response = 0;
    let mut seeds = vec![];
    for plan in generated::plans() {
        let mut evidence = Evidence::new("organization", plan.clone());
        let f = Fixture::new(Scenario {
            sequence_property: true,
            sequence_replies: plan.replies.iter().copied().collect(),
            ..Default::default()
        })
        .await;
        let command = f.command(&[5]).await;
        if plan.lose_run_acks {
            f.faults.lose_results.store(3, Ordering::SeqCst);
        }
        let mut oracle = Oracle::default();
        let mut previous: Option<Sample> = None;
        for (index, action) in plan.actions.iter().copied().enumerate() {
            evidence.before(index, action);
            match action {
                Action::Create | Action::Replay => {
                    f.create(&command).await;
                }
                Action::Status => {
                    status(&f, &command).await;
                }
                Action::Present | Action::Absent | Action::Accepted | Action::Expired => {
                    observe(&f, &command, action).await
                }
                Action::WaitDeadline => {
                    if let Some(sample) = &previous {
                        generated::wait_deadline(&sample.snapshot).await;
                    }
                }
            }
            let sample = sample(&f, &command).await;
            evidence.after(index, action, &sample, vec![]);
            if action == Action::Absent
                && previous
                    .as_ref()
                    .is_some_and(|s| s.snapshot["create"]["outcome"]["kind"] == "outcome_unknown")
                && sample.snapshot["create"]["outcome"]["kind"] != "outcome_unknown"
            {
                evidence.fail("absence resolved unknown org create");
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
                    let current = sample(&f, &command).await;
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
                    evidence.fail("retained org settlement did not project its durable audit fact")
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
            "seed did not issue forced initial org HTTP write"
        );
        assert_eq!(final_sample.consumed[0], plan.replies[0]);
        if plan.lose_run_acks {
            assert_eq!(f.faults.lost_results.load(Ordering::SeqCst), 3);
        }
        if final_sample.consumed.len() > 1 {
            multi_response += 1;
        }
        seeds.push(plan.seed);
        evidence.finish();
        f.h.finish().await;
    }
    generated::require_coverage(&consumed, &controls, settlements, multi_response);
    generated::write_summary(
        "organization",
        &seeds,
        &consumed,
        &controls,
        settlements,
        multi_response,
    );
}
