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
        ghinvite_e2e::RunPath::new("SweepSchedule", "ensure_scheduled", &[]),
        ghinvite_e2e::RunPath::new("SweepSchedule", "ensure_scheduled", &["schedule_time"]),
        ghinvite_e2e::RunPath::new("SweepSchedule", "track", &["track_time"]),
        ghinvite_e2e::RunPath::new("SweepSchedule", "track", &[]),
        ghinvite_e2e::RunPath::new("SweepSchedule", "untrack", &["track_time"]),
        ghinvite_e2e::RunPath::new("SweepSchedule", "untrack", &[]),
        ghinvite_e2e::RunPath::new("SweepSchedule", "status", &[]),
        ghinvite_e2e::RunPath::new("SweepSchedule", "run", &[]),
        ghinvite_e2e::RunPath::new("SweepSchedule", "run", &["sweep_time"]),
        ghinvite_e2e::RunPath::new("SweepSchedule", "run", &["sweep_time", "schedule_time"]),
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
