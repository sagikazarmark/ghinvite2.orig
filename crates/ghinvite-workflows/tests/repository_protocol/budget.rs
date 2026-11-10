use super::*;

async fn snapshot(runtime: &Runtime) -> Value {
    let reply = runtime
        .harness
        .restate
        .invoke(
            &Call::object("InviteBudget", "repo:100", "snapshot"),
            None,
            None,
        )
        .await;
    assert_eq!(reply.status, 200, "{reply:?}");
    reply.body
}
fn invitations(snapshot: &Value) -> u64 {
    snapshot["buckets"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["bucket"]["kind"] == "repo")
        .map(|b| b["used_24h"].as_u64().unwrap())
        .sum()
}
async fn reserve_order(runtime: &Runtime) {
    let id = runtime.last_create.lock().unwrap().clone().unwrap();
    let journal = runtime.harness.restate.admin().journal(&id).await;
    let before = journal
        .iter()
        .find(|e| e.is_run() && e.name.as_deref() == Some("pre_read_repository"))
        .unwrap();
    let read_done = journal
        .iter()
        .find(|e| {
            e.entry_type == "Notification: Run" && e.run_completion_id == before.run_completion_id
        })
        .unwrap();
    let reserve = journal
        .iter()
        .find(|e| {
            e.entry_type == "Command: Call"
                && e.raw_contains("InviteBudget")
                && e.raw_contains("reserve")
        })
        .unwrap();
    let write = journal
        .iter()
        .find(|e| e.is_run() && e.name.as_deref() == Some("guarded_github_create"))
        .unwrap();
    assert!(
        read_done.index < reserve.index && reserve.index < write.index,
        "S1 completion -> S3 reserve -> S4 run"
    );
}

#[tokio::test]
async fn budget_s3_refunds_only_positive_put204_and_releases_pre_fence_no_effect() {
    for (roles, pending, response, content, daily) in [
        (vec!["none"], None, 201, 1, 1),
        (vec!["read"], None, 204, 1, 0),
        (vec!["none", "write"], None, 201, 0, 0),
        (vec!["none"], Some("read"), 200, 1, 0),
    ] {
        let runtime = Runtime::start_options(false, true).await;
        let command = runtime
            .command(
                Scenario {
                    roles,
                    pending,
                    writes: VecDeque::from([WriteReply::Status(response)]),
                    ..Default::default()
                },
                Permission::Push,
            )
            .await;
        let receipt = runtime.create(&command).await.0;
        assert!(receipt.outcome.confirmed(), "{receipt:?}");
        reserve_order(&runtime).await;
        let budget = snapshot(&runtime).await;
        assert_eq!(budget["content_used_hour"], content, "{budget}");
        assert_eq!(invitations(&budget), daily, "{budget}");
        if content == 0 {
            assert!(operations(&runtime.trace(), "PUT", PUT_PATH).is_empty());
            assert_eq!(runtime.fence(&command).await, None);
        }
        let replay = runtime.create(&command).await.0;
        assert_eq!(receipt, replay);
        assert_eq!(snapshot(&runtime).await["content_used_hour"], content);
        runtime.finish().await;
    }
}

#[tokio::test]
async fn budget_s3_lost_acknowledgements_reuse_one_slot_and_one_put() {
    let runtime = Runtime::start_options(false, true).await;
    let command = runtime.command(Scenario::default(), Permission::Push).await;
    runtime.faults.lose_http_results.store(3, Ordering::SeqCst);
    let receipt = runtime.create(&command).await.0;
    assert_eq!(receipt.outcome, CreateOutcome::Created { upstream_id: 99 });
    assert_eq!(runtime.faults.http_result_losses.load(Ordering::SeqCst), 3);
    reserve_order(&runtime).await;
    for _ in 0..3 {
        assert_eq!(runtime.create(&command).await.0, receipt);
    }
    assert_eq!(operations(&runtime.trace(), "PUT", PUT_PATH).len(), 1);
    let budget = snapshot(&runtime).await;
    assert_eq!(budget["content_used_hour"], 1);
    assert_eq!(invitations(&budget), 1);
    runtime.finish().await;
}

#[tokio::test]
async fn budget_s3_denial_never_mints_write_credentials_or_claims_fence() {
    let runtime = Runtime::start_options(false, true).await;
    let command = runtime.command(Scenario::default(), Permission::Push).await;
    let until = Utc::now() + chrono::Duration::hours(1);
    let close = runtime
        .harness
        .restate
        .invoke(
            &Call::object("InviteBudget", "repo:100", "record_limit"),
            Some(&json!({"bucket":{"kind":"repo","repo_id":10},"until":until})),
            None,
        )
        .await;
    assert_eq!(close.status, 200);
    let receipt = runtime.create(&command).await.0;
    assert!(matches!(receipt.outcome, CreateOutcome::Throttled { .. }));
    assert_eq!(runtime.fence(&command).await, None);
    assert!(operations(&runtime.trace(), "PUT", PUT_PATH).is_empty());
    assert_eq!(runtime.github.scenario.lock().unwrap().write_mints, 0);
    assert_eq!(snapshot(&runtime).await["content_used_hour"], 0);
    runtime.finish().await;
}

#[tokio::test]
async fn budget_s3_interrupted_patch_reservation_is_released_before_a_changed_plan() {
    for next in ["put", "satisfied", "abort"] {
        let runtime = Runtime::start_options(false, true).await;
        let command = runtime
            .command(
                Scenario {
                    pending: Some("read"),
                    ..Default::default()
                },
                Permission::Push,
            )
            .await;
        let active = control::input(
            &command,
            1,
            0,
            ghinvite_core::delivery_control::LinkControl::Active,
        );
        control::control(&runtime, &command, &active).await;
        *runtime.faults.stage.lock().unwrap() = Some("after-budget-reserve".into());
        let sent = runtime
            .harness
            .restate
            .invoke(
                &Call::object("RepositoryDelivery", &command.delivery_key(), "create").send(),
                Some(&json!(command)),
                None,
            )
            .await;
        assert_eq!(sent.status, 202, "{sent:?}");
        tokio::time::timeout(Duration::from_secs(10), async {
            while runtime.faults.interruptions.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(snapshot(&runtime).await["content_used_hour"], 1);
        assert_eq!(runtime.fence(&command).await, None);
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
        {
            let mut scenario = runtime.github.scenario.lock().unwrap();
            scenario.pending = None;
            scenario.roles = vec![if next == "satisfied" { "write" } else { "none" }];
        }
        if next == "abort" {
            let revoked = control::input(
                &command,
                2,
                0,
                ghinvite_core::delivery_control::LinkControl::Revoked {
                    cancel_pending_invitations: true,
                    decline_pending_requests: false,
                    abort_hold: None,
                },
            );
            control::control(&runtime, &command, &revoked).await;
            assert_eq!(
                snapshot(&runtime).await["content_used_hour"],
                0,
                "control releases before a later create"
            );
        }
        let result = runtime.create(&command).await.0;
        let budget = snapshot(&runtime).await;
        if next == "put" {
            assert!(matches!(result.outcome, CreateOutcome::Created { .. }));
            assert_eq!(budget["content_used_hour"], 1);
            assert_eq!(
                invitations(&budget),
                1,
                "PUT must receive a new RepoInvite reservation"
            );
            assert_eq!(operations(&runtime.trace(), "PUT", PUT_PATH).len(), 1);
        } else {
            assert_eq!(budget["content_used_hour"], 0);
            assert_eq!(invitations(&budget), 0);
            assert!(operations(&runtime.trace(), "PUT", PUT_PATH).is_empty());
            assert!(matches!(
                result.outcome,
                CreateOutcome::AlreadySatisfied { .. } | CreateOutcome::Aborted { .. }
            ));
        }
        runtime.finish().await;
    }
}

#[tokio::test]
async fn budget_s3_positive_limits_close_the_bucket_before_another_write() {
    for response in [WriteReply::RateLimited(30), WriteReply::Spammed] {
        let runtime = Runtime::start_options(false, true).await;
        let command = runtime
            .command(
                Scenario {
                    writes: VecDeque::from([response]),
                    ..Default::default()
                },
                Permission::Push,
            )
            .await;
        let first = runtime.create(&command).await.0;
        assert!(matches!(first.outcome, CreateOutcome::Throttled { .. }));
        assert_eq!(runtime.fence(&command).await, Some((1, 1)));
        let id = runtime.last_create.lock().unwrap().clone().unwrap();
        let journal = runtime.harness.restate.admin().journal(&id).await;
        let evidence = journal
            .iter()
            .find(|e| {
                e.entry_type == "Command: SetState" && e.raw_contains("create_budget_evidence")
            })
            .unwrap();
        let closure = journal
            .iter()
            .find(|e| e.entry_type == "Command: Call" && e.raw_contains("record_limit"))
            .unwrap();
        assert!(
            evidence.index < closure.index,
            "rate evidence persists before crossing object boundary"
        );
        let budget = snapshot(&runtime).await;
        assert_eq!(
            budget["limit_counts"]["spammed"],
            u32::from(matches!(response, WriteReply::Spammed))
        );
        assert_eq!(
            budget["limit_counts"]["rate_limited"],
            u32::from(matches!(response, WriteReply::RateLimited(_)))
        );
        let durable: Value = runtime.harness.client.post(format!("{}/query", runtime.harness.restate.admin_url()))
            .header("accept", "application/json")
            .json(&json!({"query":"SELECT value_utf8 FROM state WHERE service_name='InviteBudget' AND service_key='repo:100' AND key='budget'"}))
            .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
        let state: Value =
            serde_json::from_str(durable["rows"][0]["value_utf8"].as_str().unwrap()).unwrap();
        let retained = state["limit_observations"].as_object().unwrap();
        assert_eq!(retained.len(), 1, "one actual positive HTTP response");
        let observed = retained.values().next().unwrap().clone();
        assert!(
            observed["observation"]["id"]
                .as_str()
                .is_some_and(|id| id.len() == 64)
        );
        for _ in 0..2 {
            let replay = runtime
                .harness
                .restate
                .invoke(
                    &Call::object("InviteBudget", "repo:100", "record_limit"),
                    Some(&observed),
                    None,
                )
                .await;
            assert_eq!(replay.status, 200, "{replay:?}");
        }
        assert_eq!(
            snapshot(&runtime).await["limit_counts"],
            budget["limit_counts"],
            "separate recovery invocations must not count the same observation again"
        );
        let mut conflict = observed;
        conflict["observation"]["reason"] = json!(if matches!(response, WriteReply::Spammed) {
            "rate_limited"
        } else {
            "spammed"
        });
        let rejected = runtime
            .harness
            .restate
            .invoke(
                &Call::object("InviteBudget", "repo:100", "record_limit"),
                Some(&conflict),
                None,
            )
            .await;
        assert_eq!(rejected.status, 409);
        assert_eq!(
            snapshot(&runtime).await["limit_counts"],
            budget["limit_counts"]
        );
        assert_eq!(
            budget["content_used_hour"], 0,
            "positively rejected write releases unused charge"
        );
        if matches!(response, WriteReply::Spammed) {
            let bucket = budget["buckets"]
                .as_array()
                .unwrap()
                .iter()
                .find(|b| b["bucket"]["kind"] == "repo")
                .unwrap();
            let until: chrono::DateTime<Utc> =
                serde_json::from_value(bucket["closed_until"].clone()).unwrap();
            assert!(until > Utc::now() + chrono::Duration::hours(23));
        } else {
            let until: chrono::DateTime<Utc> =
                serde_json::from_value(budget["next_content_write_at"].clone()).unwrap();
            assert!(until > Utc::now() && until < Utc::now() + chrono::Duration::seconds(31));
        }
        let second = runtime.create(&command).await.0;
        assert!(matches!(second.outcome, CreateOutcome::Throttled { .. }));
        assert_eq!(operations(&runtime.trace(), "PUT", PUT_PATH).len(), 1);
        assert_eq!(runtime.fence(&command).await, Some((1, 1)));
        runtime.finish().await;
    }
}

#[tokio::test]
async fn budget_s3_unknown_and_lost_put204_keep_the_invitation_charge() {
    for response in [WriteReply::Timeout, WriteReply::Status(204)] {
        let runtime = Runtime::start_options(false, true).await;
        let command = runtime
            .command(
                Scenario {
                    writes: VecDeque::from([response]),
                    ..Default::default()
                },
                Permission::Push,
            )
            .await;
        if matches!(response, WriteReply::Status(204)) {
            runtime
                .faults
                .lose_http_result
                .store(true, Ordering::SeqCst);
        }
        let result = runtime.create(&command).await.0;
        assert_eq!(result.outcome, CreateOutcome::OutcomeUnknown);
        let budget = snapshot(&runtime).await;
        assert_eq!(budget["content_used_hour"], 1);
        assert_eq!(invitations(&budget), 1);
        assert_eq!(operations(&runtime.trace(), "PUT", PUT_PATH).len(), 1);
        assert_eq!(runtime.fence(&command).await, Some((1, 0)));
        runtime.finish().await;
    }
}

#[tokio::test]
async fn budget_s3_satisfied_uses_no_slot_and_mint_limits_release_unused_slot() {
    for limited in [false, true] {
        let runtime = Runtime::start_options(false, true).await;
        let command = runtime
            .command(
                Scenario {
                    roles: vec![if limited { "none" } else { "write" }],
                    write_mint_replies: if limited {
                        VecDeque::from([429])
                    } else {
                        VecDeque::new()
                    },
                    ..Default::default()
                },
                Permission::Push,
            )
            .await;
        let result = runtime.create(&command).await.0;
        assert!(matches!(
            result.outcome,
            CreateOutcome::AlreadySatisfied { .. } | CreateOutcome::Throttled { .. }
        ));
        let budget = snapshot(&runtime).await;
        assert_eq!(budget["content_used_hour"], 0);
        assert_eq!(invitations(&budget), 0);
        assert_eq!(runtime.fence(&command).await, None);
        assert!(operations(&runtime.trace(), "PUT", PUT_PATH).is_empty());
        if limited {
            let until: chrono::DateTime<Utc> =
                serde_json::from_value(budget["next_content_write_at"].clone()).unwrap();
            assert!(until > Utc::now() + chrono::Duration::hours(23));
        } else {
            let id = runtime.last_create.lock().unwrap().clone().unwrap();
            let journal = runtime.harness.restate.admin().journal(&id).await;
            assert!(!journal.iter().any(|e| e.entry_type == "Command: Call"
                && e.raw_contains("InviteBudget")
                && e.raw_contains("reserve")));
        }
        runtime.finish().await;
    }
}

#[tokio::test]
async fn budget_s3_installs_immutable_custom_share_before_charge() {
    let runtime = Runtime::start_options(false, true).await;
    let mut command = runtime.command(Scenario::default(), Permission::Push).await;
    command.budget_share_pct = 1;
    let first = runtime.create(&command).await.0;
    assert_eq!(first.outcome, CreateOutcome::Created { upstream_id: 99 });
    let id = runtime.last_create.lock().unwrap().clone().unwrap();
    let journal = runtime.harness.restate.admin().journal(&id).await;
    let policy = journal
        .iter()
        .find(|e| e.entry_type == "Command: Call" && e.raw_contains("set_link_share"))
        .unwrap();
    let charge = journal
        .iter()
        .find(|e| {
            e.entry_type == "Command: Call"
                && e.raw_contains("InviteBudget")
                && e.raw_contains("reserve")
        })
        .unwrap();
    assert!(policy.index < charge.index);
    assert_eq!(invitations(&snapshot(&runtime).await), 1);
    let mut next = command.clone();
    next.invitation_id = Some(GithubInvitationId::new());
    next.delivery_generation += 1;
    *runtime.github.scenario.lock().unwrap() = Scenario {
        key: Some(next.delivery_key()),
        ..Default::default()
    };
    let second = runtime.create(&next).await.0;
    assert!(matches!(
        second.outcome,
        CreateOutcome::Throttled {
            reason: Some(ghinvite_core::budget::RetryReason::LinkShare),
            ..
        }
    ));
    assert!(operations(&runtime.trace(), "PUT", PUT_PATH).is_empty());
    assert_eq!(runtime.fence(&next).await, None);
    assert_eq!(invitations(&snapshot(&runtime).await), 1);
    let changed = runtime
        .harness
        .restate
        .invoke(
            &Call::object("InviteBudget", "repo:100", "set_link_share"),
            Some(&json!({"link_id":command.link_id,"percent":100})),
            None,
        )
        .await;
    assert_eq!(changed.status, 409);
    runtime.finish().await;
}
