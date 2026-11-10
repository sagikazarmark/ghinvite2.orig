use super::*;
use ghinvite_core::access_grant::*;
async fn invoke(runtime: &Runtime, key: &str, handler: &str, body: Value) -> Value {
    let reply = runtime
        .harness
        .restate
        .invoke(
            &Call::object("AccessGrant", key, handler),
            Some(&body),
            None,
        )
        .await;
    assert_eq!(reply.status, 200, "{reply:?}");
    reply.body
}
fn query(grant: &AccessGrantCommand) -> Value {
    json!(RemovalBarrierQuery {
        request_id: grant.request_id,
        target: grant.target.clone(),
        delivery_generation: grant.delivery_generation
    })
}
async fn accepted(
    runtime: &Runtime,
    lifetime: Option<u32>,
    deletes: VecDeque<WriteReply>,
) -> (
    CreateCommand,
    ghinvite_core::delivery::DeliverySnapshot,
    AccessGrantCommand,
) {
    accepted_with_stop(runtime, lifetime, deletes, false).await
}
async fn accepted_with_stop(
    runtime: &Runtime,
    lifetime: Option<u32>,
    deletes: VecDeque<WriteReply>,
    stop_before_registration: bool,
) -> (
    CreateCommand,
    ghinvite_core::delivery::DeliverySnapshot,
    AccessGrantCommand,
) {
    let mut command = runtime
        .command(
            Scenario {
                deletes,
                ..Default::default()
            },
            Permission::Push,
        )
        .await;
    command.access_lifetime_seconds = lifetime;
    if stop_before_registration {
        let query = RemovalBarrierQuery {
            request_id: command.request_id,
            target: GrantTarget::Repository {
                repo_id: command.repo_id,
                full_name: command.repo_full_name.clone(),
                grant: ghinvite_core::targets::RepoRole::Write,
            },
            delivery_generation: command.delivery_generation,
        };
        assert_eq!(
            invoke(
                runtime,
                &format!("{}:repo:{}", command.request_id, command.repo_id),
                "stop_for_reissue",
                json!(query)
            )
            .await,
            "no_removal"
        );
    }
    let created = runtime.create(&command).await;
    assert!(
        matches!(created.0.outcome, CreateOutcome::Created { .. }),
        "accepted fixture needs a confirmed owned invitation: {created:?}"
    );
    let snapshot = control::status(runtime, &command).await.unwrap();
    {
        let mut scenario = runtime.github.scenario.lock().unwrap();
        scenario.pending = None;
        scenario.roles = vec!["write"];
        scenario.role_reads = 0;
    }
    let observation = ghinvite_workflows::settlement_policy::Observation::new(
        &snapshot,
        Utc::now(),
        ghinvite_workflows::settlement_policy::Evidence::Absent {
            availability_verified: true,
            role_name: Some(EffectiveRole::Write),
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
    assert_eq!(reply.status, 200, "{reply:?}");
    let snapshot = control::status(runtime, &command).await.unwrap();
    assert_eq!(
        snapshot.settlement.as_ref().unwrap().state,
        ghinvite_core::InvitationState::Accepted
    );
    let grant = accepted_repository(&snapshot).unwrap().unwrap();
    // Registration also comes from the one-way producer. Exact explicit replay
    // proves schedule checks the actual accepted owner without modifying it.
    if runtime.access_faults.stage.lock().unwrap().as_deref() != Some("access-after-registration") {
        invoke(runtime, &grant.key(), "schedule", json!(grant)).await;
    }
    (command, snapshot, grant)
}
async fn remove(runtime: &Runtime, grant: &AccessGrantCommand) -> AccessGrantSnapshot {
    invoke(
        runtime,
        &grant.key(),
        "remove_now",
        json!(RemovalCommand {
            grant: grant.clone(),
            reason: RemovalReason::LinkRevoked
        }),
    )
    .await;
    serde_json::from_value(invoke(runtime, &grant.key(), "status", query(grant)).await).unwrap()
}
#[tokio::test]
async fn owned_accepted_access_removes_once_with_separate_fence_and_real_budget() {
    let runtime = Runtime::start_access().await;
    let (command, original, grant) =
        accepted(&runtime, None, VecDeque::from([WriteReply::Status(204)])).await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let removed = remove(&runtime, &grant).await;
    assert!(
        matches!(
            removed.outcome,
            RemovalOutcome::Removed {
                reason: RemovalReason::LinkRevoked,
                ..
            }
        ),
        "{removed:?}"
    );
    for _ in 0..3 {
        assert_eq!(remove(&runtime, &grant).await, removed);
    }
    assert_eq!(control::status(&runtime, &command).await.unwrap(), original);
    let trace = runtime.github.scenario.lock().unwrap().trace.clone();
    assert_eq!(
        trace
            .iter()
            .filter(|t| t.method == "DELETE" && t.path == PUT_PATH)
            .count(),
        1
    );
    let row: (String, i64) =
        sqlx::query_as("SELECT step,retryable FROM delivery_attempts WHERE delivery_key=?")
            .bind(format!("access:{}", grant.storage_id()))
            .fetch_one(&runtime.github.sql)
            .await
            .unwrap();
    assert_eq!(row, ("remove:repo:10".into(), 0));
    let reply = runtime
        .harness
        .restate
        .invoke(
            &Call::object("InviteBudget", "repo:100", "snapshot"),
            None,
            None,
        )
        .await;
    assert_eq!(reply.body["content_used_hour"], 2);
    wait_until(std::time::Duration::from_secs(10), || async {
        use ghinvite_core::storage::access_grants::AccessGrantStorage;
        runtime
            .storage
            .get_access_grant(&grant.storage_id())
            .await
            .unwrap()
            == Some(removed.clone())
    })
    .await;
}
#[tokio::test]
async fn promotion_skips_and_unknown_removal_never_repeats_or_refunds() {
    for unknown in [false, true] {
        let runtime = Runtime::start_access().await;
        let (command, original, grant) =
            accepted(&runtime, None, VecDeque::from([WriteReply::Timeout])).await;
        if !unknown {
            let mut scenario = runtime.github.scenario.lock().unwrap();
            scenario.roles = vec!["admin"];
            scenario.role_reads = 0;
        }
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let removed = remove(&runtime, &grant).await;
        if unknown {
            assert_eq!(removed.outcome, RemovalOutcome::OutcomeUnknown);
        } else {
            assert!(matches!(
                removed.outcome,
                RemovalOutcome::RemovalSkipped {
                    reason: RemovalSkip::RoleChanged { .. },
                    ..
                }
            ));
        }
        for _ in 0..3 {
            assert_eq!(remove(&runtime, &grant).await, removed);
        }
        assert_eq!(control::status(&runtime, &command).await.unwrap(), original);
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
            usize::from(unknown)
        );
        let reply = runtime
            .harness
            .restate
            .invoke(
                &Call::object("InviteBudget", "repo:100", "snapshot"),
                None,
                None,
            )
            .await;
        assert_eq!(reply.body["content_used_hour"], if unknown { 2 } else { 1 });
    }
}
#[tokio::test]
async fn actual_delayed_tick_expires_accepted_access_but_upgrade_has_no_grant() {
    let runtime = Runtime::start_access().await;
    let (command, original, grant) =
        accepted(&runtime, Some(2), VecDeque::from([WriteReply::Status(204)])).await;
    wait_until(Duration::from_secs(15), || async {
        let status = invoke(&runtime, &grant.key(), "status", query(&grant)).await;
        status["outcome"]["kind"] == "removed"
    })
    .await;
    let status: AccessGrantSnapshot =
        serde_json::from_value(invoke(&runtime, &grant.key(), "status", query(&grant)).await)
            .unwrap();
    let RemovalOutcome::Removed {
        reason: RemovalReason::AccessExpiry,
        at,
    } = status.outcome
    else {
        panic!("{status:?}")
    };
    assert!(at >= grant.remove_at().unwrap().unwrap());
    assert_eq!(control::status(&runtime, &command).await.unwrap(), original);
    let runtime = Runtime::start_access().await;
    let mut command = runtime
        .command(
            Scenario {
                roles: vec!["read"],
                writes: VecDeque::from([WriteReply::Status(204)]),
                ..Default::default()
            },
            Permission::Push,
        )
        .await;
    command.access_lifetime_seconds = Some(2);
    runtime.create(&command).await;
    let snapshot = control::status(&runtime, &command).await.unwrap();
    assert!(matches!(
        snapshot.create.outcome,
        CreateOutcome::Upgraded { .. }
    ));
    assert!(accepted_repository(&snapshot).unwrap().is_none());
    let target = GrantTarget::Repository {
        repo_id: 10,
        full_name: "acme/api".into(),
        grant: ghinvite_core::targets::RepoRole::Write,
    };
    let query = RemovalBarrierQuery {
        request_id: command.request_id,
        target,
        delivery_generation: 1,
    };
    assert_eq!(
        invoke(
            &runtime,
            &format!("{}:repo:10", command.request_id),
            "removal_barrier",
            json!(query)
        )
        .await,
        "no_removal"
    );
}

async fn wait_until<F, Fut>(duration: Duration, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + duration;
    loop {
        if check().await {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "access grant did not converge"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn reissue_absent_tombstone_blocks_delayed_accepted_registration_and_old_timer() {
    let runtime = Runtime::start_access().await;
    let (command, original, grant) = accepted_with_stop(
        &runtime,
        Some(1),
        VecDeque::from([WriteReply::Status(204)]),
        true,
    )
    .await;
    invoke(&runtime, &grant.key(), "schedule", json!(grant)).await;
    assert_eq!(
        invoke(&runtime, &grant.key(), "status", query(&grant)).await,
        Value::Null
    );
    invoke(
        &runtime,
        &grant.key(),
        "tick_remove",
        json!({"delivery_generation":1}),
    )
    .await;
    assert!(
        runtime
            .github
            .scenario
            .lock()
            .unwrap()
            .trace
            .iter()
            .all(|call| call.method != "DELETE")
    );
    assert_eq!(control::status(&runtime, &command).await.unwrap(), original);
    let mut changed = grant.clone();
    changed.target = GrantTarget::Repository {
        repo_id: 10,
        full_name: "acme/renamed".into(),
        grant: ghinvite_core::targets::RepoRole::Write,
    };
    let refused = runtime
        .harness
        .restate
        .invoke(
            &Call::object("AccessGrant", &grant.key(), "schedule"),
            Some(&json!(changed)),
            None,
        )
        .await;
    assert_eq!(refused.status, 409);
}
#[tokio::test]
async fn reissue_stop_cancels_retained_timer_without_charging_or_rewriting_acceptance() {
    let runtime = Runtime::start_access().await;
    let (command, original, grant) = accepted(
        &runtime,
        Some(3600),
        VecDeque::from([WriteReply::Status(204)]),
    )
    .await;
    for _ in 0..2 {
        assert_eq!(
            invoke(&runtime, &grant.key(), "stop_for_reissue", query(&grant)).await,
            "definitively_finished"
        );
    }
    let stopped: AccessGrantSnapshot =
        serde_json::from_value(invoke(&runtime, &grant.key(), "status", query(&grant)).await)
            .unwrap();
    assert!(matches!(
        stopped.outcome,
        RemovalOutcome::RemovalSkipped {
            reason: RemovalSkip::SupersededGeneration,
            ..
        }
    ));
    invoke(
        &runtime,
        &grant.key(),
        "tick_remove",
        json!({"delivery_generation":1}),
    )
    .await;
    invoke(&runtime, &grant.key(), "schedule", json!(grant)).await;
    assert_eq!(remove(&runtime, &grant).await, stopped);
    assert!(
        runtime
            .github
            .scenario
            .lock()
            .unwrap()
            .trace
            .iter()
            .all(|call| call.method != "DELETE")
    );
    assert_eq!(control::status(&runtime, &command).await.unwrap(), original);
}

#[tokio::test]
async fn fixed_access_grant_journals_cover_every_deployed_handler() {
    use ghinvite_e2e::{RunPath, journal::JournalAudit};
    const SERVICES: &[&str] = &["AccessGrant", "AccessGrantProjection"];
    const PATHS: &[RunPath] = &[
        RunPath::new("AccessGrant", "schedule", &[]),
        RunPath::new("AccessGrant", "schedule", &["access_schedule_time"]),
        RunPath::new("AccessGrant", "tick_remove", &["access_tick_time"]),
        RunPath::new("AccessGrant", "tick_remove", &[]),
        RunPath::new(
            "AccessGrant",
            "remove_now",
            &[
                "access_remove_time",
                "access_removal_read",
                "guarded_access_removal",
                "access_removal_completed",
            ],
        ),
        RunPath::new(
            "AccessGrant",
            "remove_now",
            &[
                "access_remove_time",
                "access_removal_read",
                "guarded_access_removal",
                "access_removal_completed",
                "release_access_removal_attempt",
            ],
        ),
        RunPath::new("AccessGrant", "remove_now", &[]),
        RunPath::new(
            "AccessGrant",
            "stop_for_reissue",
            &["access_reissue_removal_fence", "access_reissue_stop_time"],
        ),
        RunPath::new("AccessGrant", "stop_for_reissue", &[]),
        RunPath::new("AccessGrant", "status", &[]),
        RunPath::new("AccessGrant", "removal_progress", &[]),
        RunPath::new("AccessGrant", "removal_barrier", &[]),
        RunPath::new("AccessGrantProjection", "apply", &["project_access_grant"]),
    ];
    let runtime = Runtime::start_access().await;
    JournalAudit::retain(&runtime.harness, SERVICES).await;
    let mut audit = JournalAudit::default();
    let (_, _, grant) = accepted(&runtime, None, VecDeque::from([WriteReply::Status(204)])).await;
    for (handler, body) in [
        ("schedule", json!(grant)),
        (
            "tick_remove",
            json!({"delivery_generation": grant.delivery_generation}),
        ),
        ("status", query(&grant)),
        ("removal_progress", json!(grant)),
        ("removal_barrier", query(&grant)),
    ] {
        let reply = audit
            .invoke(
                &runtime.harness,
                "AccessGrant",
                &grant.key(),
                handler,
                &body,
            )
            .await;
        assert_eq!(reply.status, 200, "{reply:?}");
    }
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let removal = json!(RemovalCommand {
        grant: grant.clone(),
        reason: RemovalReason::LinkRevoked
    });
    for _ in 0..2 {
        let reply = audit
            .invoke(
                &runtime.harness,
                "AccessGrant",
                &grant.key(),
                "remove_now",
                &removal,
            )
            .await;
        assert_eq!(reply.status, 200, "{reply:?}");
    }
    let snapshot = invoke(&runtime, &grant.key(), "status", query(&grant)).await;
    assert_eq!(snapshot["outcome"]["kind"], "removed");
    let reply = audit
        .invoke(
            &runtime.harness,
            "AccessGrantProjection",
            &grant.storage_id(),
            "apply",
            &snapshot,
        )
        .await;
    assert_eq!(reply.status, 200, "{reply:?}");
    for handler in ["stop_for_reissue", "tick_remove"] {
        let body = if handler == "tick_remove" {
            json!({"delivery_generation": grant.delivery_generation})
        } else {
            query(&grant)
        };
        let reply = audit
            .invoke(
                &runtime.harness,
                "AccessGrant",
                &grant.key(),
                handler,
                &body,
            )
            .await;
        assert_eq!(reply.status, 200, "{reply:?}");
    }
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let (_, _, timed) = accepted(&runtime, Some(3600), VecDeque::new()).await;
    let reply = audit
        .invoke(
            &runtime.harness,
            "AccessGrant",
            &timed.key(),
            "schedule",
            &json!(timed),
        )
        .await;
    assert_eq!(reply.status, 200, "{reply:?}");
    let reply = audit
        .invoke(
            &runtime.harness,
            "AccessGrant",
            &timed.key(),
            "stop_for_reissue",
            &query(&timed),
        )
        .await;
    assert_eq!(reply.status, 200, "{reply:?}");
    assert_eq!(reply.body, "definitively_finished");
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let (_, _, refused) = accepted(&runtime, None, VecDeque::from([WriteReply::Status(403)])).await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let reply = audit
        .invoke(
            &runtime.harness,
            "AccessGrant",
            &refused.key(),
            "remove_now",
            &json!(RemovalCommand {
                grant: refused.clone(),
                reason: RemovalReason::LinkRevoked
            }),
        )
        .await;
    assert_eq!(reply.status, 200, "{reply:?}");
    assert_eq!(
        invoke(&runtime, &refused.key(), "status", query(&refused)).await["outcome"]["kind"],
        "blocked"
    );
    let walked = audit.check(&runtime.harness, SERVICES, PATHS).await;
    assert_eq!(walked.handlers, 8);
    assert_eq!(walked.paths, PATHS.len());
    eprintln!("AccessGrant fixed journal table: {walked}");
    runtime.finish().await;
}

async fn access_budget(runtime: &Runtime) -> Value {
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
#[tokio::test]
async fn killed_removal_recovers_reserved_slot_and_retained_effect_without_double_charge() {
    use std::sync::atomic::Ordering;
    for (stage, status) in [
        ("access-after-reserve", 204),
        ("access-after-completion", 204),
        ("access-after-completion", 403),
    ] {
        let runtime = Runtime::start_access().await;
        let (command, original, grant) =
            accepted(&runtime, None, VecDeque::from([WriteReply::Status(status)])).await;
        tokio::time::sleep(Duration::from_millis(1100)).await;
        *runtime.access_faults.stage.lock().unwrap() = Some(stage.into());
        let input = json!(RemovalCommand {
            grant: grant.clone(),
            reason: RemovalReason::LinkRevoked
        });
        let sent = runtime
            .harness
            .restate
            .invoke(
                &Call::object("AccessGrant", &grant.key(), "remove_now").send(),
                Some(&input),
                None,
            )
            .await;
        assert_eq!(sent.status, 202, "{sent:?}");
        tokio::time::timeout(Duration::from_secs(10), async {
            while runtime.access_faults.interruptions.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(access_budget(&runtime).await["content_used_hour"], 2);
        let before = runtime
            .github
            .scenario
            .lock()
            .unwrap()
            .trace
            .iter()
            .filter(|t| t.method == "DELETE")
            .count();
        assert_eq!(before, usize::from(stage == "access-after-completion"));
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
        *runtime.access_faults.stage.lock().unwrap() = None;
        if stage == "access-after-reserve" {
            let mut scenario = runtime.github.scenario.lock().unwrap();
            scenario.roles = vec!["admin"];
            scenario.role_reads = 0;
        }
        let recovered = remove(&runtime, &grant).await;
        match (stage, status) {
            ("access-after-reserve", _) => assert!(matches!(
                recovered.outcome,
                RemovalOutcome::RemovalSkipped {
                    reason: RemovalSkip::RoleChanged { .. },
                    ..
                }
            )),
            (_, 204) => assert!(matches!(recovered.outcome, RemovalOutcome::Removed { .. })),
            _ => assert!(matches!(recovered.outcome, RemovalOutcome::Blocked { .. })),
        }
        for _ in 0..3 {
            assert_eq!(remove(&runtime, &grant).await, recovered);
        }
        let after = runtime
            .github
            .scenario
            .lock()
            .unwrap()
            .trace
            .iter()
            .filter(|t| t.method == "DELETE")
            .count();
        assert_eq!(
            after, before,
            "new invocation must drain durable reservation/completion, not repeat DELETE"
        );
        let charged = if stage == "access-after-completion" && status == 204 {
            2
        } else {
            1
        };
        assert_eq!(access_budget(&runtime).await["content_used_hour"], charged);
        assert_eq!(control::status(&runtime, &command).await.unwrap(), original);
        runtime.finish().await;
    }
}
#[tokio::test]
async fn three_real_access_removal_run_retries_preserve_one_delete_and_uncertain_charge() {
    use std::sync::atomic::Ordering;
    let runtime = Runtime::start_access().await;
    let (command, original, grant) =
        accepted(&runtime, None, VecDeque::from([WriteReply::Status(204)])).await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    runtime
        .access_faults
        .lose_results
        .store(3, Ordering::SeqCst);
    let unknown = remove(&runtime, &grant).await;
    assert_eq!(runtime.access_faults.lost_results.load(Ordering::SeqCst), 3);
    assert_eq!(unknown.outcome, RemovalOutcome::OutcomeUnknown);
    for _ in 0..3 {
        assert_eq!(remove(&runtime, &grant).await, unknown);
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
    assert_eq!(access_budget(&runtime).await["content_used_hour"], 2);
    assert_eq!(
        invoke(&runtime, &grant.key(), "stop_for_reissue", query(&grant)).await,
        "may_have_written"
    );
    assert_eq!(control::status(&runtime, &command).await.unwrap(), original);
    runtime.finish().await;
}

#[tokio::test]
async fn reminted_access_removal_does_not_reuse_the_first_rejected_outcome() {
    for second in [502, 204, 401] {
        let runtime = Runtime::start_access().await;
        let (command, original, grant) = accepted(
            &runtime,
            None,
            VecDeque::from([WriteReply::Status(401), WriteReply::Status(second)]),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let result = remove(&runtime, &grant).await;
        if second == 502 {
            assert_eq!(result.outcome, RemovalOutcome::OutcomeUnknown);
        } else if second == 204 {
            assert!(matches!(result.outcome, RemovalOutcome::Removed { .. }));
        } else {
            assert!(matches!(
                result.outcome,
                RemovalOutcome::Blocked {
                    reason: RemovalBlocked::InstallationPermissions
                }
            ));
        }
        for _ in 0..3 {
            assert_eq!(remove(&runtime, &grant).await, result);
        }
        assert_eq!(
            access_budget(&runtime).await["content_used_hour"],
            if second == 401 { 1 } else { 2 },
            "only positively rejected final generation releases the budget"
        );
        let row:(i64,i64)=sqlx::query_as("SELECT attempt_generation,retryable FROM delivery_attempts WHERE delivery_key=? AND step='remove:repo:10'").bind(format!("access:{}",grant.storage_id())).fetch_one(&runtime.github.sql).await.unwrap();
        assert_eq!(row, (2, i64::from(second == 401)));
        let trace = runtime.github.scenario.lock().unwrap().trace.clone();
        let deletes = trace
            .iter()
            .filter(|t| t.method == "DELETE")
            .collect::<Vec<_>>();
        assert_eq!(deletes.len(), 2);
        assert_ne!(
            deletes[0].credential, deletes[1].credential,
            "401 must remint before the second generation"
        );
        assert_eq!(control::status(&runtime, &command).await.unwrap(), original);
        runtime.finish().await;
    }
}

async fn kill_access_checkpoint(runtime: &Runtime) -> String {
    let id = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(id) = runtime
                .access_faults
                .interrupted_invocation
                .lock()
                .unwrap()
                .clone()
            {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    runtime.harness.restate.admin().kill(&id).await;
    runtime
        .harness
        .restate
        .admin()
        .await_status(&id, &["completed"])
        .await;
    *runtime.access_faults.stage.lock().unwrap() = None;
    id
}
#[tokio::test]
async fn killed_registration_replay_repairs_head_and_arms_actual_delayed_removal() {
    let runtime = Runtime::start_access().await;
    *runtime.access_faults.stage.lock().unwrap() = Some("access-after-registration".into());
    let (command, original, grant) =
        accepted(&runtime, Some(3), VecDeque::from([WriteReply::Status(204)])).await;
    kill_access_checkpoint(&runtime).await;
    let retained: AccessGrantSnapshot =
        serde_json::from_value(invoke(&runtime, &grant.key(), "status", query(&grant)).await)
            .unwrap();
    assert_eq!(retained.outcome, RemovalOutcome::Scheduled);
    invoke(&runtime, &grant.key(), "schedule", json!(grant)).await;
    // No manual tick: exact registration replay must repair the durable head
    // and arm the real Restate delayed send after the interrupted first write.
    wait_until(Duration::from_secs(10), || async {
        let snapshot: AccessGrantSnapshot =
            serde_json::from_value(invoke(&runtime, &grant.key(), "status", query(&grant)).await)
                .unwrap();
        matches!(
            snapshot.outcome,
            RemovalOutcome::Removed {
                reason: RemovalReason::AccessExpiry,
                ..
            }
        )
    })
    .await;
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
    assert_eq!(control::status(&runtime, &command).await.unwrap(), original);
    runtime.finish().await;
}
#[tokio::test]
async fn terminal_replays_repair_projection_after_killed_terminal_owner_commit() {
    use ghinvite_core::storage::access_grants::AccessGrantStorage;
    for handler in ["remove_now", "tick_remove", "stop_for_reissue"] {
        let runtime = Runtime::start_access().await;
        let (command, original, grant) =
            accepted(&runtime, None, VecDeque::from([WriteReply::Status(204)])).await;
        wait_until(Duration::from_secs(10), || async {
            runtime
                .storage
                .get_access_grant(&grant.storage_id())
                .await
                .unwrap()
                .is_some()
        })
        .await;
        tokio::time::sleep(Duration::from_millis(1100)).await;
        *runtime.access_faults.stage.lock().unwrap() = Some("access-after-terminal".into());
        let removal = json!(RemovalCommand {
            grant: grant.clone(),
            reason: RemovalReason::LinkRevoked
        });
        let sent = runtime
            .harness
            .restate
            .invoke(
                &Call::object("AccessGrant", &grant.key(), "remove_now").send(),
                Some(&removal),
                None,
            )
            .await;
        assert_eq!(sent.status, 202);
        assert_eq!(kill_access_checkpoint(&runtime).await, sent.invocation_id());
        let final_owner: AccessGrantSnapshot =
            serde_json::from_value(invoke(&runtime, &grant.key(), "status", query(&grant)).await)
                .unwrap();
        assert!(matches!(
            final_owner.outcome,
            RemovalOutcome::Removed { .. }
        ));
        assert_eq!(
            runtime
                .storage
                .get_access_grant(&grant.storage_id())
                .await
                .unwrap()
                .unwrap()
                .outcome,
            RemovalOutcome::Scheduled
        );
        let body = match handler {
            "remove_now" => removal,
            "tick_remove" => json!({"delivery_generation":1}),
            _ => query(&grant),
        };
        invoke(&runtime, &grant.key(), handler, body.clone()).await;
        wait_until(Duration::from_secs(10), || async {
            runtime
                .storage
                .get_access_grant(&grant.storage_id())
                .await
                .unwrap()
                == Some(final_owner.clone())
        })
        .await;
        for _ in 0..2 {
            invoke(&runtime, &grant.key(), handler, body.clone()).await;
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
        assert_eq!(access_budget(&runtime).await["content_used_hour"], 2);
        assert_eq!(control::status(&runtime, &command).await.unwrap(), original);
        runtime.finish().await;
    }
}
