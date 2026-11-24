//! Real Restate owner transitions driven by typed GitHub HTTP observations.
//!
//! GitHub is the octoemu emulator ([`ghinvite_e2e::github`]): alice's
//! invitation, acceptance, role and expiry and the other invitations that
//! make the listing span two pages are GitHub's own state, changed through
//! the admin API. A failing second page, a failing repository read and an
//! unanswered write are fault rules.
#![cfg(feature = "integration")]
use chrono::Utc;
use ghinvite_core::{
    GithubInvitationId, InvitationLinkId, RequestId,
    delivery::{CreateCommand, CreateOutcome, DeliverySnapshot},
    storage::{ConsoleStorage, RecordStorage},
};
use ghinvite_e2e::github::Outcome;
use ghinvite_e2e::{Call, Harness};
use ghinvite_workflows::sweep_schedule::Schedule;
use octoemu::faults::{Fault, FaultAction};
use octoemu::seed::{RepositoryInvitation, Role, Seed, User};
use serde_json::{Value, json};
use std::sync::Arc;

/// 150 other users invited to `acme/api` by its owner, so its invitation
/// list spans two pages of 100.
fn crowd() -> Seed {
    (0..150).fold(Seed::new(), |seed, n| {
        seed.user(User::new(2000 + n, format!("other-{n}")))
            .repository_invitation(
                RepositoryInvitation::new(5000 + n, "acme/api", format!("other-{n}"), Role::Pull)
                    .inviter("creator"),
            )
    })
}

/// The second page of `acme/api`'s invitation list fails (the list's `Link`
/// follows the `/repositories/{id}/invitations` alias).
fn second_page_fails() -> Fault {
    Fault::on(
        "GET",
        "/repositories/10/invitations",
        FaultAction::respond_json(503, json!({"message": "Service Unavailable"})),
    )
    .query_contains("page=2")
}

/// `GET /repos/acme/api` answers 404.
fn repository_unavailable() -> Fault {
    Fault::on(
        "GET",
        "/repos/acme/api",
        FaultAction::respond_json(404, json!({"message": "Not Found"})),
    )
}

struct Runtime {
    harness: Harness,
    storage: Arc<ghinvite_storage_sqlx::SqlxStorage>,
    clock: Arc<std::sync::atomic::AtomicI64>,
    faults: Arc<ghinvite_workflows::delivery::DeliveryFaults>,
}
impl Runtime {
    async fn start() -> Self {
        let harness = Harness::start().await;
        let storage = Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::in_memory()
                .await
                .unwrap(),
        );
        for (user_id, login) in [(7, "creator"), (8, "alice")] {
            storage
                .upsert_user(&ghinvite_core::User {
                    user_id,
                    login: login.into(),
                    avatar_url: None,
                    github_created_at: None,
                    last_seen_at: Utc::now(),
                })
                .await
                .unwrap();
        }
        let github_transport = ghinvite_e2e::github_transport(harness.client.clone());
        let apps = Arc::new(
            ghinvite_workflows::RepositoryGithub::new(
                github_transport,
                &harness.github_url,
                ghinvite_e2e::repository_apps(&harness.github_url),
            )
            .unwrap(),
        );
        let mut state = ghinvite_workflows::AppState::new(storage.clone()).with_github_apps(apps);
        let clock = Arc::new(std::sync::atomic::AtomicI64::new(
            Utc::now().timestamp_millis(),
        ));
        state.sweep_clock = Some(clock.clone());
        let faults = Arc::new(ghinvite_workflows::delivery::DeliveryFaults::default());
        let builder = restate_sdk::endpoint::Endpoint::builder()
            .bind(ghinvite_workflows::availability::AccountInstallation {
                state: state.clone(),
            })
            .bind(ghinvite_workflows::availability::InstallationProjection {
                state: state.clone(),
            })
            .bind(ghinvite_workflows::reconcile::Reconcile {
                state: state.clone(),
            });
        // Settlement notifies the real request owner before untracking the
        // delivery. Register that production handler even though these isolated
        // delivery fixtures have no initialized parent request.
        let builder = ghinvite_workflows::request_owner::bind(builder);
        let endpoint =
            ghinvite_workflows::delivery::bind_with_faults(builder, state, faults.clone()).build();
        harness.restate.deploy(endpoint).await;
        let reply=harness.restate.invoke(&Call::object("AccountInstallation", "repo:100","onboard"),Some(&ghinvite_e2e::setup_command(json!({"app":"repo","installation_id":1,"actor_user_id":7,"account_id":100,"account_login":"acme","account_type":"Organization","selected_repos":"all","installed_at":Utc::now()}))),None).await;
        assert_eq!(reply.status, 200, "{reply:?}");
        Self {
            harness,
            storage,
            clock,
            faults,
        }
    }
    async fn command(&self) -> CreateCommand {
        let link:ghinvite_core::InvitationLink=serde_json::from_value(json!({"secret":{"secret_generation":1,"sha256":"2a".repeat(32)},
            "id":InvitationLinkId::new(),"installation_id":1,"account_id":100,"created_by":7,"created_at":Utc::now(),"expires_at":null,"max_uses":null,"uses_count":1,"approval_required":true,"description":"Settlement evidence","internal_note":null,"revoked_at":null,"revoked_by":null,"targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/api","grant":"write"}]}
        })).unwrap();
        let request:ghinvite_core::InvitationRequest=serde_json::from_value(json!({"id":RequestId::new(),"invitation_link_id":link.id,"requester_id":8,"justification":"Docs","state":"approved","decided_by":7,"decided_at":Utc::now(),"decline_reason":null,"created_at":Utc::now(),"decision_deadline":null})).unwrap();
        ghinvite_core::storage::projection::fixture::seed(
            self.storage.as_ref(),
            &link,
            std::slice::from_ref(&request),
            1,
        )
        .await
        .unwrap();
        CreateCommand {
            batch: None,
            budget_attribution: Default::default(),
            access_lifetime_seconds: None,
            budget_share_pct: 50,
            invitation_id: Some(GithubInvitationId::new()),
            delivery_generation: 1,
            link_id: Some(link.id),
            request_id: request.id,
            approval_id: format!("fixture/{}", request.id),
            account_id: 100,
            installation_id: 1,
            requester_id: 8,
            repo_id: 10,
            repo_full_name: "acme/api".into(),
            permission: ghinvite_core::Permission::Push,
            approved_at: request.decided_at.unwrap(),
        }
    }
    async fn owner(&self, command: &CreateCommand, handler: &str, input: &Value) -> Value {
        let reply = self
            .harness
            .restate
            .invoke(
                &Call::object("RepositoryDelivery", &command.delivery_key(), handler),
                (!input.is_null()).then_some(input),
                None,
            )
            .await;
        assert_eq!(reply.status, 200, "{handler}: {reply:?}");
        reply.body
    }
    async fn snapshot(&self, command: &CreateCommand) -> DeliverySnapshot {
        serde_json::from_value(self.owner(command, "status", &Value::Null).await).unwrap()
    }
    /// alice's open invitation to `acme/api`.
    fn invitation(&self) -> Option<u64> {
        self.harness
            .github()
            .invitations("alice")
            .into_iter()
            .find(|(_, repo)| repo == "acme/api")
            .map(|(id, _)| id)
    }
    /// `(method, path)` of every call since the log was last emptied.
    fn calls(&self) -> Vec<(String, String)> {
        self.harness
            .emulator()
            .calls()
            .into_iter()
            .map(|call| (call.method, call.path))
            .collect()
    }
    async fn projected(&self, expected: &DeliverySnapshot) {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let rows = self
                    .storage
                    .list_delivery_for_request(expected.create.command.request_id)
                    .await
                    .unwrap();
                if rows.iter().any(|row| row == expected) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("retained settlement projection must converge");
    }
}

async fn schedule(runtime: &Runtime) -> Schedule {
    let reply = runtime
        .harness
        .restate
        .invoke(
            &Call::object("SweepSchedule", "repo:100", "status"),
            None,
            None,
        )
        .await;
    assert_eq!(reply.status, 200, "{reply:?}");
    serde_json::from_value(reply.body).unwrap()
}
async fn registered(runtime: &Runtime, count: usize) -> Schedule {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let state = schedule(runtime).await;
            if state.tracked.values().map(|v| v.len()).sum::<usize>() == count {
                return state;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("owner registration must not wait for SQL")
}
async fn settled(runtime: &Runtime, command: &CreateCommand) -> DeliverySnapshot {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let snapshot = runtime.snapshot(command).await;
            if snapshot.settlement.is_some() {
                return snapshot;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("scheduled evidence must reach owner")
}
#[tokio::test]
async fn retained_schedule_pulls_forward_once_and_settles_without_sql_or_webhooks() {
    use ghinvite_e2e::journal::JournalAudit;
    use std::sync::atomic::Ordering::SeqCst;
    let runtime = Runtime::start().await;
    JournalAudit::retain(&runtime.harness, &["SweepSchedule", "Reconcile"]).await;
    let mut audit = JournalAudit::default();
    let idle = registered(&runtime, 0).await;
    assert_eq!(idle.cadence_seconds, 86_400);
    let run = |token| json!(token);
    runtime
        .clock
        .store(idle.next_run.unwrap().timestamp_millis(), SeqCst);
    let reply = audit
        .invoke(
            &runtime.harness,
            "SweepSchedule",
            "repo:100",
            "run",
            &run(idle.token),
        )
        .await;
    assert_eq!(reply.status, 200, "{reply:?}");
    let idle = schedule(&runtime).await;
    assert_eq!(idle.completed_runs, 1);
    runtime.clock.fetch_add(60_000, SeqCst); // last idle sweep at t - 1 minute
    let t = runtime.clock.load(SeqCst);
    runtime.faults.projection_unavailable.store(true, SeqCst);
    let first = runtime.command().await;
    runtime.owner(&first, "create", &json!(first)).await;
    let second = runtime.command().await;
    let second_receipt = runtime.owner(&second, "create", &json!(second)).await;
    assert_eq!(second_receipt["outcome"]["kind"], "already_pending");
    let active = registered(&runtime, 2).await;
    let foreign = runtime
        .harness
        .restate
        .invoke(
            &Call::object("SweepSchedule", "repo:200", "track"),
            Some(&json!({"target_id":10,"delivery_key":first.delivery_key()})),
            None,
        )
        .await;
    assert_eq!(
        foreign.status, 400,
        "another account cannot register this owner"
    );
    let org = runtime
        .harness
        .restate
        .invoke(
            &Call::object("SweepSchedule", "org:100", "ensure_scheduled"),
            None,
            None,
        )
        .await;
    assert_eq!(
        org.status, 400,
        "M1 organization polling is not silently enabled"
    );
    let live_registration = json!({"target_id":10,"delivery_key":first.delivery_key()});
    audit
        .invoke(
            &runtime.harness,
            "SweepSchedule",
            "repo:100",
            "untrack",
            &live_registration,
        )
        .await;
    audit
        .invoke(
            &runtime.harness,
            "SweepSchedule",
            "repo:100",
            "track",
            &live_registration,
        )
        .await;
    assert_eq!(
        registered(&runtime, 2).await.token,
        active.token,
        "stale untrack cannot hide a live owner; duplicate track cannot fork timers"
    );
    assert_eq!(
        active.tracked.len(),
        1,
        "two deliveries share one repository walk"
    );
    assert_eq!(active.next_run.unwrap().timestamp_millis(), t + 3_600_000);
    assert_eq!(
        active.token,
        idle.token + 1,
        "duplicate track does not spawn a chain"
    );
    assert!(
        runtime
            .storage
            .list_delivery_for_request(first.request_id)
            .await
            .unwrap()
            .is_empty(),
        "projection is unavailable while sweep registration succeeds"
    );
    assert!(runtime.faults.projection_failures.load(SeqCst) > 0);
    let before = runtime.calls().len();
    let reply = audit
        .invoke(
            &runtime.harness,
            "SweepSchedule",
            "repo:100",
            "run",
            &run(idle.token),
        )
        .await;
    assert_eq!(reply.status, 200);
    assert_eq!(schedule(&runtime).await.token, active.token);
    assert_eq!(runtime.calls().len(), before, "old idle timer is inert");
    runtime.clock.store(t + 5 * 60_000, SeqCst);
    // alice accepts (a write collaborator), and 150 others are invited, so
    // the list she is absent from spans two pages.
    let github = runtime.harness.github();
    github
        .accept("alice", runtime.invitation().expect("alice is invited"))
        .await;
    runtime.harness.emulator().seed(crowd()).unwrap();
    runtime.harness.emulator().clear_calls();
    let reply = audit
        .invoke(
            &runtime.harness,
            "SweepSchedule",
            "repo:100",
            "run",
            &run(active.token),
        )
        .await;
    assert_eq!(reply.status, 200);
    assert!(
        runtime.snapshot(&first).await.settlement.is_none(),
        "early run cannot change cadence"
    );
    runtime.clock.store(t + 3_600_000, SeqCst);
    let reply = audit
        .invoke(
            &runtime.harness,
            "SweepSchedule",
            "repo:100",
            "run",
            &run(active.token),
        )
        .await;
    assert_eq!(reply.status, 200, "{reply:?}");
    for command in [&first, &second] {
        let snapshot = settled(&runtime, command).await;
        assert_eq!(
            snapshot.settlement.unwrap().state,
            ghinvite_core::InvitationState::Accepted
        );
    }
    let state = registered(&runtime, 0).await;
    assert_eq!(state.completed_runs, 2);
    let pages = |page: &str| {
        runtime
            .calls()
            .iter()
            .filter(|(method, path)| method == "GET" && path.split('?').next() == Some(page))
            .count()
    };
    assert_eq!(
        (
            pages("/repos/acme/api/invitations"),
            pages("/repositories/10/invitations")
        ),
        (1, 1),
        "exactly one full two-page walk for both deliveries"
    );
    assert!(
        runtime
            .calls()
            .iter()
            .all(|(method, _)| method != "PUT" && method != "PATCH" && method != "DELETE")
    );
    // A duplicate fired token is inert; only the current token can advance.
    audit
        .invoke(
            &runtime.harness,
            "SweepSchedule",
            "repo:100",
            "run",
            &run(active.token),
        )
        .await;
    assert_eq!(schedule(&runtime).await.completed_runs, 2);
    runtime
        .clock
        .store(state.next_run.unwrap().timestamp_millis(), SeqCst);
    audit
        .invoke(
            &runtime.harness,
            "SweepSchedule",
            "repo:100",
            "run",
            &run(state.token),
        )
        .await;
    let idle_again = schedule(&runtime).await;
    assert_eq!(idle_again.cadence_seconds, 86_400);
    assert_eq!(idle_again.completed_runs, 3);
    assert_eq!(idle_again.token, state.token + 1);
    runtime.faults.projection_unavailable.store(false, SeqCst);
    for command in [&first, &second] {
        runtime.projected(&runtime.snapshot(command).await).await;
    }
    // Fixed current-handler table, including both stale and premature timer paths.
    // Other schedule handlers are covered below as concrete invocations.
    audit
        .invoke(
            &runtime.harness,
            "SweepSchedule",
            "repo:100",
            "ensure_scheduled",
            &Value::Null,
        )
        .await;
    audit
        .invoke(
            &runtime.harness,
            "SweepSchedule",
            "repo:100",
            "status",
            &Value::Null,
        )
        .await;
    let registration = json!({"target_id":10,"delivery_key":first.delivery_key()});
    audit
        .invoke(
            &runtime.harness,
            "SweepSchedule",
            "repo:100",
            "track",
            &registration,
        )
        .await;
    audit
        .invoke(
            &runtime.harness,
            "SweepSchedule",
            "repo:100",
            "untrack",
            &registration,
        )
        .await;
    assert!(
        schedule(&runtime).await.tracked.is_empty(),
        "late track cannot resurrect a settled owner"
    );
    // A failed walk of the last run counts once; its duplicate is inert.
    let failed = json!({"token":state.token,"target_id":10,"outcome":{"kind":"failed"}});
    for _ in 0..2 {
        audit
            .invoke(
                &runtime.harness,
                "SweepSchedule",
                "repo:100",
                "report",
                &failed,
            )
            .await;
    }
    assert_eq!(schedule(&runtime).await.health.failed_runs, 1);
    // Separate empty owner exercises the initial scheduling path.
    audit
        .invoke(
            &runtime.harness,
            "SweepSchedule",
            "repo:101",
            "ensure_scheduled",
            &Value::Null,
        )
        .await;
    const PATHS: &[ghinvite_e2e::RunPath] = &[
        ghinvite_e2e::RunPath::new("SweepSchedule", "ensure_scheduled", &["schedule_time"]),
        ghinvite_e2e::RunPath::new("SweepSchedule", "track", &["track_time"]),
        ghinvite_e2e::RunPath::new("SweepSchedule", "track", &[]),
        ghinvite_e2e::RunPath::new("SweepSchedule", "untrack", &["track_time"]),
        ghinvite_e2e::RunPath::new("SweepSchedule", "untrack", &[]),
        ghinvite_e2e::RunPath::new("SweepSchedule", "status", &[]),
        ghinvite_e2e::RunPath::new("SweepSchedule", "run", &[]),
        ghinvite_e2e::RunPath::new("SweepSchedule", "run", &["sweep_time"]),
        ghinvite_e2e::RunPath::new("SweepSchedule", "run", &["sweep_time", "schedule_time"]),
        ghinvite_e2e::RunPath::new("SweepSchedule", "report", &[]),
        ghinvite_e2e::RunPath::new("SweepSchedule", "report", &["report_time"]),
    ];
    audit
        .check(&runtime.harness, &["SweepSchedule"], PATHS)
        .await;
    runtime.harness.finish().await;
}
async fn sweep_now(runtime: &Runtime) {
    let schedule = schedule(runtime).await;
    let reply = runtime.harness.restate.invoke(&Call::service("Reconcile", "sweep"), Some(&json!({"account":{"app":"repo","account_id":100},"token":schedule.token,"tracked":schedule.tracked})), None).await;
    assert_eq!(reply.status, 200, "{reply:?}");
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let result: Value = runtime.harness.client.post(format!("{}/query", runtime.harness.restate.admin_url()))
                .json(&json!({"query":"SELECT id FROM sys_invocation WHERE status != 'completed' AND (target_service_name = 'Reconcile' OR (target_service_name = 'RepositoryDelivery' AND target_handler_name = 'reconcile'))"}))
                .header("accept", "application/json").send().await.unwrap().json().await.unwrap();
            if result["rows"].as_array().unwrap().is_empty() { return; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }).await.expect("one-way sweep fan-out completes");
}
#[tokio::test]
async fn scheduled_evidence_preserves_unknown_and_checks_same_sweep_access() {
    let runtime = Runtime::start().await;
    let command = runtime.command().await;
    runtime.owner(&command, "create", &json!(command)).await;
    registered(&runtime, 1).await;
    // alice accepts (a write collaborator), 150 others are invited, and the
    // list's second page fails.
    let github = runtime.harness.github();
    github
        .accept("alice", runtime.invitation().expect("alice is invited"))
        .await;
    runtime.harness.emulator().seed(crowd()).unwrap();
    let page_failure = github.rule("second page fails", second_page_fails());
    sweep_now(&runtime).await;
    assert!(
        runtime.snapshot(&command).await.settlement.is_none(),
        "a failed later page is not an empty complete listing"
    );
    assert!(
        runtime
            .harness
            .emulator()
            .fault(page_failure)
            .unwrap()
            .fired
            > 0
    );
    runtime.harness.emulator().remove_fault(page_failure);
    let unavailable = github.rule("repository unavailable", repository_unavailable());
    sweep_now(&runtime).await;
    assert!(runtime.harness.emulator().fault(unavailable).unwrap().fired > 0);
    assert!(
        runtime.snapshot(&command).await.settlement.is_none(),
        "absence with repository 404 is never acceptance"
    );
    runtime.harness.emulator().remove_fault(unavailable);
    github.collaborate("alice", Role::Pull).await;
    sweep_now(&runtime).await;
    assert_eq!(
        settled(&runtime, &command).await.settlement.unwrap().state,
        ghinvite_core::InvitationState::Declined,
        "base read cannot satisfy write"
    );
    registered(&runtime, 0).await;
    // alice has no access; the next write goes unanswered and is not applied.
    github.revoke("alice").await;
    github
        .outcome("acme/api", "alice", Outcome::Unanswered)
        .await;
    let unknown = runtime.command().await;
    assert_eq!(
        runtime.owner(&unknown, "create", &json!(unknown)).await["outcome"]["kind"],
        "outcome_unknown"
    );
    registered(&runtime, 1).await;
    // alice gains write access some other way; no invitation is open.
    github.collaborate("alice", Role::Push).await;
    let puts_before = github.writes();
    sweep_now(&runtime).await;
    assert!(
        matches!(
            runtime.snapshot(&unknown).await.create.outcome,
            CreateOutcome::OutcomeUnknown
        ),
        "absence plus access never clears the write fence"
    );
    // GitHub applies the App's write after all: alice, without access again,
    // has the App's open invitation.
    github.revoke("alice").await;
    let mut late = RepositoryInvitation::new(9001, "acme/api", "alice", Role::Push)
        .created_at(runtime.harness.emulator().now());
    late.inviter_app_id = Some(ghinvite_e2e::github::REPO_APP_ID);
    runtime
        .harness
        .emulator()
        .seed(Seed::new().repository_invitation(late))
        .unwrap();
    sweep_now(&runtime).await;
    assert!(matches!(
        runtime.snapshot(&unknown).await.create.outcome,
        CreateOutcome::Created { upstream_id: 9001 }
    ));
    github.expire(9001).await;
    sweep_now(&runtime).await;
    assert_eq!(
        settled(&runtime, &unknown).await.settlement.unwrap().state,
        ghinvite_core::InvitationState::Expired
    );
    registered(&runtime, 0).await;
    assert_eq!(
        github.writes(),
        puts_before,
        "unknown resolution is read only"
    );
    runtime.harness.finish().await;
}

#[tokio::test]
async fn operational_watermarks_survive_projection_outage_and_preserve_receipts() {
    use chrono::Duration;
    use ghinvite_core::storage::{OperationalReadStorage, operational::DeliverySignals};
    use ghinvite_workflows::settlement_policy::{Evidence, Observation};
    use std::sync::atomic::Ordering::SeqCst;

    async fn signals(
        runtime: &Runtime,
        at: chrono::DateTime<Utc>,
        expected_age: Option<i64>,
    ) -> DeliverySignals {
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                let report = runtime.storage.delivery_signals(at).await.unwrap();
                if report.counts.repository_deliveries == 1
                    && !report.incomplete()
                    && expected_age
                        .is_none_or(|age| report.counts.pending.oldest_age_seconds == Some(age))
                {
                    return report;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("telemetry must converge independently after SQL recovery")
    }

    let runtime = Runtime::start().await;
    runtime
        .harness
        .github()
        .outcome("acme/api", "alice", Outcome::Unanswered)
        .await;
    runtime.faults.projection_unavailable.store(true, SeqCst);
    let command = runtime.command().await;
    let receipt = runtime.owner(&command, "create", &json!(command)).await;
    assert_eq!(receipt["outcome"]["kind"], "outcome_unknown");
    let initial = runtime.snapshot(&command).await;
    let absent = Observation::new(
        &initial,
        Utc::now(),
        Evidence::Absent {
            availability_verified: true,
            role_name: None,
            direct_collaborator: None,
            membership: None,
        },
    );
    runtime.owner(&command, "reconcile", &json!(absent)).await;
    assert_eq!(
        runtime.snapshot(&command).await,
        initial,
        "observation changes no receipt or revision"
    );
    assert_eq!(
        runtime
            .storage
            .delivery_signals(Utc::now())
            .await
            .unwrap()
            .counts
            .repository_deliveries,
        0
    );
    assert!(runtime.faults.projection_failures.load(SeqCst) > 0);
    runtime.faults.projection_unavailable.store(false, SeqCst);
    let report_at = Utc::now() + Duration::hours(4);
    let before = signals(&runtime, report_at, None).await;
    assert_eq!(before.counts.unknown.total, 1);
    assert_eq!(before.counts.unknown.over_threshold, 1);
    assert_eq!(before.counts.pending.total, 0);
    // A retry cannot replace first-unknown time with this invocation's time.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    assert_eq!(
        runtime.owner(&command, "create", &json!(command)).await,
        receipt
    );
    assert_eq!(
        signals(&runtime, report_at, None).await.counts.unknown,
        before.counts.unknown
    );

    let resolved_at = Utc::now();
    let found = Observation::new(
        &initial,
        resolved_at,
        Evidence::FoundByInvitee {
            upstream_id: 99,
            permission: ghinvite_core::targets::EffectiveRole::Write,
        },
    );
    runtime.owner(&command, "reconcile", &json!(found)).await;
    let resolved = runtime.snapshot(&command).await;
    let report_at = resolved_at + Duration::hours(4);
    let report = signals(&runtime, report_at, Some(14_400)).await;
    assert_eq!(report.counts.unknown.total, 0);
    assert_eq!(report.counts.pending.over_threshold, 1);
    assert_eq!(
        resolved.create.outcome,
        CreateOutcome::Created { upstream_id: 99 }
    );

    let listed = Evidence::Listed {
        upstream_id: 99,
        expired: false,
        created_at: None,
    };
    let observed_at = resolved_at + Duration::hours(2);
    let valid = Observation::new(&resolved, observed_at, listed.clone());
    runtime.owner(&command, "reconcile", &json!(valid)).await;
    let report = signals(&runtime, report_at, Some(7200)).await;
    assert_eq!(
        report.counts.pending.over_threshold, 0,
        "exactly two cadences is not over the threshold"
    );
    assert_eq!(runtime.snapshot(&command).await, resolved);

    let mut stale = Observation::new(&resolved, resolved_at + Duration::hours(5), listed.clone());
    stale.expected_revision -= 1;
    runtime.owner(&command, "reconcile", &json!(stale)).await;
    let wrong = Observation::new(
        &resolved,
        stale.at,
        Evidence::Listed {
            upstream_id: 100,
            expired: false,
            created_at: None,
        },
    );
    runtime.owner(&command, "reconcile", &json!(wrong)).await;
    let incomplete = Observation::new(
        &resolved,
        stale.at,
        Evidence::Absent {
            availability_verified: true,
            role_name: None,
            direct_collaborator: None,
            membership: None,
        },
    );
    runtime
        .owner(&command, "reconcile", &json!(incomplete))
        .await;
    let older = Observation::new(&resolved, resolved_at, listed.clone());
    runtime.owner(&command, "reconcile", &json!(older)).await;
    let newer = Observation::new(&resolved, observed_at + Duration::minutes(1), listed);
    runtime.owner(&command, "reconcile", &json!(newer)).await;
    signals(&runtime, report_at, Some(7140)).await;
    // No-change listing evidence does not increment revision. A delayed older
    // absence with that same revision must not defeat the newer positive list.
    let stale_absence = Observation::new(
        &resolved,
        resolved_at + Duration::hours(1),
        Evidence::Absent {
            availability_verified: true,
            role_name: Some(ghinvite_core::targets::EffectiveRole::None),
            direct_collaborator: None,
            membership: None,
        },
    );
    runtime
        .owner(&command, "reconcile", &json!(stale_absence))
        .await;
    assert_eq!(
        runtime.snapshot(&command).await,
        resolved,
        "older same-revision absence cannot settle after a newer complete listing"
    );
    runtime.projected(&resolved).await;
    assert_eq!(runtime.harness.github().writes(), 1);
    runtime.harness.finish().await;
}

// --- #86 lifecycle reconciliation rehearsals -------------------------------

async fn schedule_of(runtime: &Runtime, key: &str) -> Option<Schedule> {
    let reply = runtime
        .harness
        .restate
        .invoke(&Call::object("SweepSchedule", key, "status"), None, None)
        .await;
    assert_eq!(reply.status, 200, "{reply:?}");
    serde_json::from_value(reply.body).unwrap()
}
async fn schedule_until(runtime: &Runtime, done: impl Fn(&Schedule) -> bool) -> Schedule {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let state = schedule(runtime).await;
            if done(&state) {
                return state;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("schedule must converge")
}
/// The web cron's one ingress call for these `repo:` accounts.
async fn watchdog(
    runtime: &Runtime,
    accounts: &[u64],
) -> ghinvite_core::sweep_health::WatchdogReport {
    let accounts = accounts
        .iter()
        .map(|id| json!({"app":"repo","account_id":id}))
        .collect::<Vec<_>>();
    let reply = runtime
        .harness
        .restate
        .invoke(
            &Call::service("SweepWatchdog", "check"),
            Some(&json!({ "accounts": accounts })),
            None,
        )
        .await;
    assert_eq!(reply.status, 200, "{reply:?}");
    serde_json::from_value(reply.body).unwrap()
}
async fn run_token(runtime: &Runtime, token: u64) {
    let reply = runtime
        .harness
        .restate
        .invoke(
            &Call::object("SweepSchedule", "repo:100", "run"),
            Some(&json!(token)),
            None,
        )
        .await;
    assert_eq!(reply.status, 200, "{reply:?}");
}
/// Every one-way sweep invocation has completed. Delayed schedule runs stay
/// `scheduled` until their real-time delay, which these tests never wait out.
async fn quiesce(runtime: &Runtime) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let result: Value = runtime.harness.client.post(format!("{}/query", runtime.harness.restate.admin_url()))
                .json(&json!({"query":"SELECT id FROM sys_invocation WHERE status NOT IN ('completed','scheduled') AND (target_service_name IN ('Reconcile','SweepSchedule','SweepWatchdog') OR (target_service_name = 'RepositoryDelivery' AND target_handler_name = 'reconcile'))"}))
                .header("accept", "application/json").send().await.unwrap().json().await.unwrap();
            if result["rows"].as_array().unwrap().is_empty() { return; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }).await.expect("one-way sweep fan-out completes");
}
fn listings(runtime: &Runtime) -> usize {
    runtime
        .calls()
        .iter()
        .filter(|(method, path)| {
            method == "GET" && path.split('?').next() == Some("/repos/acme/api/invitations")
        })
        .count()
}
fn at_millis(at: chrono::DateTime<Utc>) -> i64 {
    at.timestamp_millis()
}

/// Missed webhook, scheduler downtime and duplicate triggers: GitHub never
/// tells us alice accepted, no run fires for three days, and the hourly
/// watchdog then re-arms the chain with exactly one catch-up sweep.
#[tokio::test]
async fn watchdog_catches_up_once_after_scheduler_downtime_and_ignores_duplicates() {
    use chrono::Duration;
    use std::sync::atomic::Ordering::SeqCst;
    let runtime = Runtime::start().await;
    let command = runtime.command().await;
    runtime.owner(&command, "create", &json!(command)).await;
    let active = registered(&runtime, 1).await;
    // A healthy chain is left alone; an account that was never armed is armed.
    let report = watchdog(&runtime, &[100, 101, 100]).await;
    assert_eq!(
        (report.checked, report.missing, report.overdue),
        (2, 1, 0),
        "{report:?}"
    );
    assert_eq!(schedule(&runtime).await.token, active.token);
    quiesce(&runtime).await;
    assert!(
        schedule_of(&runtime, "repo:101")
            .await
            .is_some_and(|s| s.next_run.is_some())
    );
    // Missed webhook: alice accepts on GitHub. Repository member events never
    // dispatch settlement, so only the retained chain can notice.
    runtime
        .harness
        .github()
        .accept("alice", runtime.invitation().expect("alice is invited"))
        .await;
    // Scheduler downtime: neither the delayed run nor the cron fired for three
    // days, so 72 hourly runs were missed.
    let due = active.next_run.unwrap();
    runtime
        .clock
        .store(at_millis(due + Duration::days(3)), SeqCst);
    runtime.harness.emulator().clear_calls();
    let report = watchdog(&runtime, &[100]).await;
    assert_eq!((report.missing, report.overdue), (0, 1), "{report:?}");
    let oldest = report.oldest_overdue.expect("the overdue chain is named");
    assert_eq!((oldest.account.account_id, oldest.due_at), (100, due));
    assert_eq!(
        settled(&runtime, &command).await.settlement.unwrap().state,
        ghinvite_core::InvitationState::Accepted
    );
    let caught_up = registered(&runtime, 0).await;
    assert_eq!(
        caught_up.completed_runs, 1,
        "one catch-up sweep, not one per missed run"
    );
    assert_eq!(caught_up.health.rearmed_runs, 1);
    assert_eq!(caught_up.token, active.token + 2);
    assert_eq!(listings(&runtime), 1);
    // Duplicate and overlapping triggers: another check, the stale timer, the
    // spent catch-up token and a repeated ensure are all no-ops.
    let report = watchdog(&runtime, &[100]).await;
    assert_eq!((report.missing, report.overdue), (0, 0), "{report:?}");
    for token in [active.token, active.token + 1] {
        run_token(&runtime, token).await;
    }
    let reply = runtime
        .harness
        .restate
        .invoke(
            &Call::object("SweepSchedule", "repo:100", "ensure_scheduled"),
            None,
            None,
        )
        .await;
    assert_eq!(reply.status, 200, "{reply:?}");
    quiesce(&runtime).await;
    let after = schedule(&runtime).await;
    assert_eq!(
        (after.token, after.completed_runs, after.health.rearmed_runs),
        (caught_up.token, 1, 1)
    );
    assert_eq!(listings(&runtime), 1);
    runtime.harness.finish().await;
}

/// Rate limiting defers the chain with exponential backoff instead of failing
/// it; a non-throttled read failure counts once per run and is reported.
#[tokio::test]
async fn throttled_walks_defer_with_backoff_and_failed_walks_count_once() {
    use chrono::Duration;
    use octoemu::faults::Reply;
    use std::sync::atomic::Ordering::SeqCst;
    let runtime = Runtime::start().await;
    let command = runtime.command().await;
    runtime.owner(&command, "create", &json!(command)).await;
    let active = registered(&runtime, 1).await;
    let t = active.next_run.unwrap();
    let github = runtime.harness.github();
    let throttled = github.rule(
        "listing throttled",
        Fault::on(
            "GET",
            "/repos/acme/api/invitations",
            FaultAction::Respond(Reply::new(403).header("retry-after", "1").json(json!({
                "message": "You have exceeded a secondary rate limit. Please wait a few minutes before you try again.",
                "documentation_url": "https://docs.github.com/rest/overview/rate-limits-for-the-rest-api#about-secondary-rate-limits",
            }))),
        ),
    );
    runtime.clock.store(at_millis(t), SeqCst);
    run_token(&runtime, active.token).await;
    let first = schedule_until(&runtime, |s| s.health.throttled_runs == 1).await;
    assert_eq!(first.health.deferrals, 1);
    assert_eq!(
        first.health.failed_runs, 0,
        "throttling defers; it does not fail"
    );
    assert_eq!(first.health.deferred_until, Some(t + Duration::hours(1)));
    assert_eq!(
        first.token,
        active.token + 1,
        "the hourly successor already covers a first deferral"
    );
    // A duplicate report of that run neither counts nor defers again.
    let duplicate = json!({"token":active.token,"target_id":10,"outcome":{"kind":"throttled","retry_after_seconds":1}});
    let reply = runtime
        .harness
        .restate
        .invoke(
            &Call::object("SweepSchedule", "repo:100", "report"),
            Some(&duplicate),
            None,
        )
        .await;
    assert_eq!(reply.status, 200, "{reply:?}");
    assert_eq!(schedule(&runtime).await.health, first.health);
    // Throttled again: the backoff doubles past the cadence and replaces the timer.
    runtime
        .clock
        .store(at_millis(t + Duration::hours(1)), SeqCst);
    run_token(&runtime, first.token).await;
    let second = schedule_until(&runtime, |s| s.health.throttled_runs == 2).await;
    assert_eq!(second.health.deferrals, 2);
    assert_eq!(second.next_run, Some(t + Duration::hours(3)));
    assert_eq!(second.token, first.token + 2);
    run_token(&runtime, first.token + 1).await;
    assert_eq!(
        schedule(&runtime).await.token,
        second.token,
        "replaced timer is inert"
    );
    let report = watchdog(&runtime, &[100]).await;
    assert_eq!(report.overdue, 0, "a deferred chain is not overdue");
    assert!(report.failed.is_empty());
    // GitHub answers again: the deferred run reads, and the run after it
    // starts the backoff over.
    assert!(runtime.harness.emulator().remove_fault(throttled));
    runtime.harness.emulator().clear_calls();
    runtime
        .clock
        .store(at_millis(t + Duration::hours(3)), SeqCst);
    run_token(&runtime, second.token).await;
    quiesce(&runtime).await;
    assert_eq!(listings(&runtime), 1);
    let recovered = schedule(&runtime).await;
    assert_eq!(recovered.health.throttled_runs, 2);
    runtime
        .clock
        .store(at_millis(t + Duration::hours(4)), SeqCst);
    run_token(&runtime, recovered.token).await;
    quiesce(&runtime).await;
    let reset = schedule(&runtime).await;
    assert_eq!(
        (reset.health.deferrals, reset.health.deferred_until),
        (0, None)
    );
    // A failed later page is a failed sweep: counted once and reported.
    runtime.harness.emulator().seed(crowd()).unwrap();
    let page_failure = github.rule("second page fails", second_page_fails());
    runtime
        .clock
        .store(at_millis(t + Duration::hours(5)), SeqCst);
    run_token(&runtime, reset.token).await;
    let failed = schedule_until(&runtime, |s| s.health.failed_runs == 1).await;
    assert_eq!(failed.health.last_failed_token, Some(reset.token));
    assert!(
        runtime
            .harness
            .emulator()
            .fault(page_failure)
            .unwrap()
            .fired
            > 0
    );
    let report = watchdog(&runtime, &[100]).await;
    assert_eq!(report.failed.len(), 1, "{report:?}");
    assert_eq!(report.failed[0].failed_runs, 1);
    assert_eq!(
        report.failed[0].last_failed_at,
        t + Duration::hours(5),
        "the failure is dated by the schedule's clock"
    );
    assert!(runtime.snapshot(&command).await.settlement.is_none());
    runtime.harness.finish().await;
}

/// Reinstall: the account keeps its one chain, and its next run settles a
/// delivery created under the retired installation through the replacement.
#[tokio::test]
async fn reinstall_keeps_one_chain_and_settles_through_the_replacement() {
    use std::sync::atomic::Ordering::SeqCst;
    let runtime = Runtime::start().await;
    let command = runtime.command().await;
    runtime.owner(&command, "create", &json!(command)).await;
    let active = registered(&runtime, 1).await;
    let emulator = runtime.harness.emulator();
    // On GitHub the App is uninstalled (1) and installed again (as 3).
    assert!(emulator.uninstall(1));
    emulator
        .install(ghinvite_e2e::github::installed(
            3,
            ghinvite_e2e::github::REPO_APP_ID,
        ))
        .unwrap();
    let onboard = runtime
        .harness
        .restate
        .invoke(
            &Call::object("AccountInstallation", "repo:100", "onboard"),
            Some(&ghinvite_e2e::setup_command(json!({"app":"repo","installation_id":3,"actor_user_id":7,"account_id":100,"account_login":"acme","account_type":"Organization","selected_repos":"all","installed_at":Utc::now()}))),
            None,
        )
        .await;
    assert_eq!(onboard.status, 200, "{onboard:?}");
    let uninstall = runtime
        .harness
        .restate
        .invoke(
            &Call::object("AccountInstallation", "repo:100", "uninstall"),
            Some(&json!({"app":"repo","installation_id":1,"uninstalled_at":Utc::now()})),
            None,
        )
        .await;
    assert_eq!(uninstall.status, 200, "{uninstall:?}");
    quiesce(&runtime).await;
    assert_eq!(
        schedule(&runtime).await.token,
        active.token,
        "onboarding the replacement re-asks for the chain without forking it"
    );
    runtime
        .harness
        .github()
        .accept("alice", runtime.invitation().expect("alice is invited"))
        .await;
    runtime.harness.emulator().clear_calls();
    runtime
        .clock
        .store(at_millis(active.next_run.unwrap()), SeqCst);
    run_token(&runtime, active.token).await;
    assert_eq!(
        settled(&runtime, &command).await.settlement.unwrap().state,
        ghinvite_core::InvitationState::Accepted
    );
    // The observation verified the replacement installation with GitHub; the
    // retired one was never consulted.
    let calls = runtime.calls();
    assert!(
        calls.contains(&("GET".into(), "/app/installations/3".into())),
        "{calls:?}"
    );
    assert!(
        calls
            .iter()
            .all(|(_, path)| !path.starts_with("/app/installations/1")),
        "{calls:?}"
    );
    runtime.harness.finish().await;
}

/// Expiration: GitHub still lists the known invitation eight days after it
/// was created, without (yet) flagging it expired. The next run's fresh
/// observation settles it Expired.
#[tokio::test]
async fn still_listed_past_the_seven_day_lifetime_settles_expired() {
    use chrono::Duration;
    use std::sync::atomic::Ordering::SeqCst;
    let runtime = Runtime::start().await;
    let command = runtime.command().await;
    let receipt = runtime.owner(&command, "create", &json!(command)).await;
    let upstream = receipt["outcome"]["upstream_id"].as_u64().unwrap();
    let active = registered(&runtime, 1).await;
    // Inside its lifetime a run observes it pending and changes nothing.
    runtime
        .clock
        .store(at_millis(active.next_run.unwrap()), SeqCst);
    run_token(&runtime, active.token).await;
    quiesce(&runtime).await;
    assert!(runtime.snapshot(&command).await.settlement.is_none());
    let next = schedule(&runtime).await;
    // The octoemu clock cannot jump a week (App JWT skew), so GitHub's aged
    // listing is a response rule.
    let created = Utc::now() - Duration::days(8);
    runtime.harness.github().rule(
        "aged unflagged listing",
        Fault::on(
            "GET",
            "/repos/acme/api/invitations",
            FaultAction::respond_json(
                200,
                json!([{"id":upstream,"invitee":{"id":8,"login":"alice"},"permissions":"write","created_at":created,"expired":false}]),
            ),
        ),
    );
    runtime
        .clock
        .store(at_millis(next.next_run.unwrap()), SeqCst);
    run_token(&runtime, next.token).await;
    let settlement = settled(&runtime, &command).await.settlement.unwrap();
    assert_eq!(settlement.state, ghinvite_core::InvitationState::Expired);
    assert_eq!(
        settlement.event.metadata["evidence"]["kind"],
        "listed_past_lifetime"
    );
    registered(&runtime, 0).await;
    runtime.harness.finish().await;
}
