use super::*;
use ghinvite_core::delivery_control::LinkControl;

async fn invoke(runtime: &Runtime, command: &CreateCommand, handler: &str) {
    let reply = runtime
        .harness
        .restate
        .invoke(
            &Call::object("RepositoryDelivery", &command.delivery_key(), handler),
            None,
            None,
        )
        .await;
    assert_eq!(reply.status, 200, "{reply:?}");
}
async fn prepare(runtime: &Runtime, scenario: Scenario) -> (CreateCommand, CreateReceipt) {
    let command = runtime.command(scenario, Permission::Push).await;
    let active = control::input(&command, 1, 0, LinkControl::Active);
    control::control(runtime, &command, &active).await;
    let receipt = runtime.create(&command).await.0;
    assert!(matches!(receipt.outcome, CreateOutcome::Created { .. }));
    invoke(runtime, &command, "drive_cancellation").await;
    (command, receipt)
}
async fn revoke(runtime: &Runtime, command: &CreateCommand) {
    let revoke = control::input(
        command,
        2,
        0,
        LinkControl::Revoked {
            cancel_pending_invitations: true,
            decline_pending_requests: false,
            abort_hold: None,
        },
    );
    control::control(runtime, command, &revoke).await;
    invoke(runtime, command, "drive_cancellation").await;
}
async fn budget(runtime: &Runtime) -> Value {
    let reply = runtime
        .harness
        .restate
        .invoke(
            &Call::object("InviteBudget", "repo:100", "snapshot"),
            None,
            None,
        )
        .await;
    assert_eq!(reply.status, 200);
    reply.body
}
#[tokio::test]
async fn owned_cancel_uses_separate_fence_and_real_budget_preserving_create_receipt() {
    let runtime = Runtime::start_mode(true).await;
    let (command, receipt) = prepare(&runtime, Scenario::default()).await;
    revoke(&runtime, &command).await;
    let snapshot = control::status(&runtime, &command).await.unwrap();
    assert_eq!(snapshot.create, receipt);
    assert_eq!(
        snapshot.settlement.unwrap().state,
        ghinvite_core::InvitationState::Cancelled
    );
    for _ in 0..3 {
        invoke(&runtime, &command, "drive_cancellation").await;
    }
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
    let rows:Vec<(String,i64,i64)>=sqlx::query_as("SELECT step,attempt_generation,retryable FROM delivery_attempts WHERE delivery_key=? ORDER BY step").bind(command.delivery_key()).fetch_all(&runtime.github.sql).await.unwrap();
    assert_eq!(rows, vec![("cancel".into(), 1, 0), ("repo".into(), 1, 0)]);
    assert_eq!(budget(&runtime).await["content_used_minute"], 1);
}
#[tokio::test]
async fn unknown_delete_never_retries_or_refunds_and_double_401_releases() {
    let runtime = Runtime::start_mode(true).await;
    let (command, receipt) = prepare(
        &runtime,
        Scenario {
            deletes: VecDeque::from([WriteReply::Timeout]),
            ..Default::default()
        },
    )
    .await;
    revoke(&runtime, &command).await;
    for _ in 0..3 {
        invoke(&runtime, &command, "drive_cancellation").await;
    }
    let snapshot = control::status(&runtime, &command).await.unwrap();
    assert_eq!(snapshot.create, receipt);
    assert!(snapshot.settlement.is_none());
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
    assert_eq!(budget(&runtime).await["content_used_minute"], 1);

    let runtime = Runtime::start_mode(true).await;
    let (command, receipt) = prepare(
        &runtime,
        Scenario {
            deletes: VecDeque::from([WriteReply::Status(401), WriteReply::Status(401)]),
            ..Default::default()
        },
    )
    .await;
    revoke(&runtime, &command).await;
    let snapshot = control::status(&runtime, &command).await.unwrap();
    assert_eq!(snapshot.create, receipt);
    assert!(snapshot.settlement.is_none());
    let fence:(i64,i64)=sqlx::query_as("SELECT attempt_generation,retryable FROM delivery_attempts WHERE delivery_key=? AND step='cancel'").bind(command.delivery_key()).fetch_one(&runtime.github.sql).await.unwrap();
    assert_eq!(fence, (2, 1));
    assert_eq!(budget(&runtime).await["content_used_minute"], 0);
}

#[tokio::test]
async fn three_cancel_run_losses_preserve_one_delete_and_spent_slot() {
    let runtime = Runtime::start_mode(true).await;
    let (command, receipt) = prepare(&runtime, Scenario::default()).await;
    runtime
        .faults
        .lose_cancel_results
        .store(3, Ordering::SeqCst);
    revoke(&runtime, &command).await;
    assert_eq!(runtime.faults.lost_cancel_results.load(Ordering::SeqCst), 3);
    for _ in 0..3 {
        invoke(&runtime, &command, "drive_cancellation").await;
    }
    let snapshot = control::status(&runtime, &command).await.unwrap();
    assert_eq!(snapshot.create, receipt);
    assert!(snapshot.settlement.is_none());
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
    assert_eq!(budget(&runtime).await["content_used_minute"], 1);
    for role in [EffectiveRole::None, EffectiveRole::Write] {
        let observation = ghinvite_workflows::settlement_policy::Observation::new(
            &snapshot,
            Utc::now(),
            ghinvite_workflows::settlement_policy::Evidence::Absent {
                availability_verified: true,
                role_name: Some(role.clone()),
                direct_collaborator: None,
                membership: None,
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
        assert_eq!(reply.status, 200);
        let current = control::status(&runtime, &command).await.unwrap();
        assert_eq!(current.create, receipt);
        if role == EffectiveRole::None {
            assert!(
                current.settlement.is_none(),
                "absence cannot relabel an uncertain DELETE as decline"
            );
        } else {
            assert_eq!(
                current.settlement.unwrap().state,
                ghinvite_core::InvitationState::Accepted
            );
        }
    }
    assert_eq!(
        budget(&runtime).await["content_used_minute"],
        1,
        "uncertain cancellation remains charged after later acceptance"
    );
}

#[tokio::test]
async fn actual_delayed_own_expiry_cancels_pending_but_preserves_unobserved_acceptance() {
    for accepted in [false, true] {
        let runtime = Runtime::start_mode(true).await;
        let command = runtime
            .command(
                Scenario {
                    created_at: Some(Utc::now() - chrono::Duration::hours(2)),
                    accept_after_metadata: accepted,
                    ..Default::default()
                },
                Permission::Push,
            )
            .await;
        let mut active = control::input(&command, 1, 0, LinkControl::Active);
        active.invitation_lifetime_seconds = 3600;
        control::control(&runtime, &command, &active).await;
        let receipt = runtime.create(&command).await.0;
        let snapshot = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let snapshot = control::status(&runtime, &command).await.unwrap();
                if snapshot.settlement.is_some() {
                    break snapshot;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .unwrap();
        let notice = final_notice::wait(&runtime, &command).await;
        assert_eq!(
            notice["notice"],
            json!({"kind":"repository","target_id":10,"delivery_generation":1})
        );
        assert_eq!(notice["snapshot"], json!(snapshot));
        assert_eq!(
            notice["result"]["result"],
            if accepted {
                "satisfied"
            } else {
                "lapsed_invite"
            }
        );
        assert_eq!(snapshot.create, receipt);
        assert_eq!(
            snapshot.settlement.as_ref().unwrap().state,
            if accepted {
                ghinvite_core::InvitationState::Accepted
            } else {
                ghinvite_core::InvitationState::Cancelled
            }
        );
        if !accepted {
            assert_eq!(
                snapshot.settlement.unwrap().event.metadata["reason"],
                "own_expiry"
            );
        }
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
            usize::from(!accepted)
        );
    }
}

#[tokio::test]
async fn interrupted_unused_cancellation_reservation_is_recovered_without_double_charge() {
    let runtime = Runtime::start_mode(true).await;
    let (command, receipt) = prepare(&runtime, Scenario::default()).await;
    runtime
        .faults
        .cancel_after_reserve
        .store(true, Ordering::SeqCst);
    let revoke = control::input(
        &command,
        2,
        0,
        LinkControl::Revoked {
            cancel_pending_invitations: true,
            decline_pending_requests: false,
            abort_hold: None,
        },
    );
    control::control(&runtime, &command, &revoke).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while runtime.faults.cancel_after_reserve.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
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
        0
    );
    assert_eq!(budget(&runtime).await["content_used_minute"], 1);
    invoke(&runtime, &command, "drive_cancellation").await;
    let snapshot = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let snapshot = control::status(&runtime, &command).await.unwrap();
            if snapshot.settlement.is_some() {
                break snapshot;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(snapshot.create, receipt);
    assert_eq!(budget(&runtime).await["content_used_minute"], 1);
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
}

#[tokio::test]
async fn positive_acceptance_wins_before_cancel_and_late_events_preserve_cancel_winner() {
    let runtime = Runtime::start_mode(true).await;
    let (command, receipt) = prepare(&runtime, Scenario::default()).await;
    {
        let mut state = runtime.github.scenario.lock().unwrap();
        state.pending = None;
        state.roles = vec!["write"];
    }
    revoke(&runtime, &command).await;
    let accepted = control::status(&runtime, &command).await.unwrap();
    assert_eq!(accepted.create, receipt);
    assert_eq!(
        accepted.settlement.unwrap().state,
        ghinvite_core::InvitationState::Accepted
    );
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
    assert_eq!(budget(&runtime).await["content_used_minute"], 0);

    let runtime = Runtime::start_mode(true).await;
    let (command, receipt) = prepare(&runtime, Scenario::default()).await;
    revoke(&runtime, &command).await;
    let winner = control::status(&runtime, &command).await.unwrap();
    assert_eq!(
        winner.settlement.as_ref().unwrap().event.metadata["evidence"],
        "delete204"
    );
    let reply = runtime
        .harness
        .restate
        .invoke(
            &Call::object("RepositoryDelivery", &command.delivery_key(), "on_webhook"),
            Some(
                &json!({"invitation_id":command.invitation_id,"action":"declined","at":Utc::now()}),
            ),
            None,
        )
        .await;
    assert_eq!(reply.status, 200);
    assert_eq!(control::status(&runtime, &command).await.unwrap(), winner);
    assert_eq!(runtime.create(&command).await.0, receipt);
}

#[tokio::test]
async fn received_404_requires_fresh_reach_and_acceptance_evidence() {
    for (reply, expected) in [
        (WriteReply::Status(404), None),
        (
            WriteReply::Policy(404, "gone"),
            Some(ghinvite_core::InvitationState::Cancelled),
        ),
        (
            WriteReply::Policy(404, "accepted"),
            Some(ghinvite_core::InvitationState::Accepted),
        ),
    ] {
        let runtime = Runtime::start_mode(true).await;
        let (command, receipt) = prepare(
            &runtime,
            Scenario {
                deletes: VecDeque::from([reply]),
                ..Default::default()
            },
        )
        .await;
        revoke(&runtime, &command).await;
        let snapshot = control::status(&runtime, &command).await.unwrap();
        assert_eq!(snapshot.create, receipt);
        assert_eq!(snapshot.settlement.as_ref().map(|s| s.state), expected);
        if expected == Some(ghinvite_core::InvitationState::Cancelled) {
            assert_eq!(
                snapshot.settlement.unwrap().event.metadata["evidence"],
                "not_found_after_verified_reach"
            );
        }
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
    }
}

#[tokio::test]
async fn retained_cancellation_result_finishes_accounting_after_owner_interruption() {
    for reply in [
        WriteReply::Status(204),
        WriteReply::RateLimited(90),
        WriteReply::Spammed,
        WriteReply::Policy(422, "validation refused"),
    ] {
        let limited = matches!(reply, WriteReply::RateLimited(_) | WriteReply::Spammed);
        let refused = limited || matches!(reply, WriteReply::Policy(422, _));
        let runtime = Runtime::start_mode(true).await;
        let (command, receipt) = prepare(
            &runtime,
            Scenario {
                deletes: VecDeque::from([reply]),
                ..Default::default()
            },
        )
        .await;
        runtime
            .faults
            .cancel_after_result
            .store(true, Ordering::SeqCst);
        let revoked = control::input(
            &command,
            2,
            0,
            LinkControl::Revoked {
                cancel_pending_invitations: true,
                decline_pending_requests: false,
                abort_hold: None,
            },
        );
        control::control(&runtime, &command, &revoked).await;
        tokio::time::timeout(Duration::from_secs(10), async {
            while runtime.faults.cancel_after_result.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            control::status(&runtime, &command)
                .await
                .unwrap()
                .settlement
                .is_none()
        );
        assert_eq!(budget(&runtime).await["content_used_hour"], 1);
        invoke(&runtime, &command, "drive_cancellation").await;
        let snapshot = control::status(&runtime, &command).await.unwrap();
        assert_eq!(snapshot.create, receipt);
        let accounting = budget(&runtime).await;
        assert_eq!(
            accounting["limit_counts"]["spammed"],
            u32::from(matches!(reply, WriteReply::Spammed))
        );
        assert_eq!(
            accounting["limit_counts"]["rate_limited"],
            u32::from(matches!(reply, WriteReply::RateLimited(_)))
        );
        if refused {
            assert!(snapshot.settlement.is_none());
            assert_eq!(accounting["content_used_hour"], 0);
            if limited {
                let until: chrono::DateTime<Utc> =
                    serde_json::from_value(accounting["next_content_write_at"].clone()).unwrap();
                assert!(until > Utc::now() + chrono::Duration::seconds(60));
            }
            let fence: (i64,i64) = sqlx::query_as("SELECT attempt_generation,retryable FROM delivery_attempts WHERE delivery_key=? AND step='cancel'")
                .bind(command.delivery_key()).fetch_one(&runtime.github.sql).await.unwrap();
            assert_eq!(fence, (1, 1));
        } else {
            assert_eq!(
                snapshot.settlement.unwrap().state,
                ghinvite_core::InvitationState::Cancelled
            );
            assert_eq!(accounting["content_used_hour"], 1);
        }
        for _ in 0..3 {
            invoke(&runtime, &command, "drive_cancellation").await;
        }
        assert_eq!(
            runtime
                .trace()
                .iter()
                .filter(|t| t.method == "DELETE")
                .count(),
            1
        );
        assert_eq!(runtime.create(&command).await.0, receipt);
        runtime.finish().await;
    }
}

#[tokio::test]
async fn cancellation_read_rate_limit_closes_content_without_claiming_cancel_fence() {
    let runtime = Runtime::start_mode(true).await;
    let (command, receipt) = prepare(&runtime, Scenario::default()).await;
    runtime
        .github
        .scenario
        .lock()
        .unwrap()
        .installation_read_replies
        .push_back(429);
    revoke(&runtime, &command).await;
    let snapshot = control::status(&runtime, &command).await.unwrap();
    assert_eq!(snapshot.create, receipt);
    assert!(snapshot.settlement.is_none());
    let accounting = budget(&runtime).await;
    let until: chrono::DateTime<Utc> =
        serde_json::from_value(accounting["next_content_write_at"].clone()).unwrap();
    assert!(until > Utc::now() + chrono::Duration::seconds(60));
    assert_eq!(accounting["content_used_hour"], 0);
    assert_eq!(
        runtime
            .trace()
            .iter()
            .filter(|t| t.method == "DELETE")
            .count(),
        0
    );
    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM delivery_attempts WHERE delivery_key=? AND step='cancel'",
    )
    .bind(command.delivery_key())
    .fetch_one(&runtime.github.sql)
    .await
    .unwrap();
    assert_eq!(count.0, 0);
    runtime.finish().await;
}
