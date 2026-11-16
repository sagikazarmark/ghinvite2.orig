//! Real retained request inbox/dispatch protocol with a delivery-boundary recorder.
//! S0/GitHub effects are independently tested on the actual RepositoryDelivery.
#![cfg(feature = "integration")]
use restate_sdk::{
    context::{ContextReadState, ContextWriteState, ObjectContext, SharedObjectContext},
    errors::TerminalError,
    serde::Json,
};
use serde_json::{Value, json};
use std::sync::Arc;

struct RepositoryDelivery;
#[restate_sdk::object]
impl RepositoryDelivery {
    #[handler]
    async fn control(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<ghinvite_workflows::delivery_control::DeliveryControlInput>,
    ) -> Result<(), TerminalError> {
        let mut events = ctx
            .get::<Json<Vec<Value>>>("events")
            .await?
            .map(|j| j.0)
            .unwrap_or_default();
        events.push(json!({"kind":"control","input":input}));
        ctx.set("events", Json(events));
        Ok(())
    }
    #[handler]
    async fn create(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<ghinvite_core::delivery::CreateCommand>,
    ) -> Result<(), TerminalError> {
        let mut events = ctx
            .get::<Json<Vec<Value>>>("events")
            .await?
            .map(|j| j.0)
            .unwrap_or_default();
        events.push(json!({"kind":"create","input":input}));
        ctx.set("events", Json(events));
        Ok(())
    }
    #[handler]
    async fn events(
        &self,
        ctx: SharedObjectContext<'_>,
    ) -> Result<Json<Vec<Value>>, TerminalError> {
        Ok(ctx
            .get::<Json<Vec<Value>>>("events")
            .await?
            .unwrap_or(Json(vec![])))
    }
}

#[tokio::test]
async fn retained_controls_before_initialization_bind_facts_and_precede_every_create() {
    let harness = ghinvite_e2e::Harness::start().await;
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let endpoint = ghinvite_workflows::request_owner::bind(ghinvite_workflows::projection::bind(
        restate_sdk::endpoint::Endpoint::builder(),
        storage,
    ))
    .bind(RepositoryDelivery)
    .build();
    harness.restate.deploy(endpoint).await;
    let request = ghinvite_core::RequestId::new();
    let link = ghinvite_core::InvitationLinkId::new();
    let admitted = chrono::Utc::now();
    let held_at = admitted + chrono::Duration::seconds(1);
    let base = format!(
        "{}/InvitationRequest/{request}",
        harness.restate.ingress_url()
    );
    let control = json!({"link_id":link,"request_id":request,"operation_id":ghinvite_core::RequestId::new(),
        "revision":2,"hold_epoch":2,"at":held_at,"admitted_at":admitted,"invitation_lifetime_seconds":3600,
        "control":{"kind":"auto_pause","window":{"epoch":2,"window_start":admitted-chrono::Duration::minutes(1),"paused_at":held_at}}});
    let call =
        |method: &str, input: Value| harness.client.post(format!("{base}/{method}")).json(&input);
    let response = call("apply_link_control", control.clone())
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    let replay: Value = call("apply_link_control", control.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(replay, json!({"outcome":"applied","revision":2}));
    let init = json!({"request":{"request_id":request,"link_id":link,"account_id":100,"requester_id":8,
        "justification":null,"state":"approved","admitted_at":admitted,"decision_deadline":null,"revision":1,
        "decision":{"decision_id":format!("request.approved/{request}"),"decided_by":null,"effective_at":admitted,"evaluated_at":admitted,"decline_reason":null}},
        "installation_id":1,"targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/api","grant":"write"},{"repo_id":11,"repo_full_name":"acme/web","grant":"read"}]},
        "approval_required":false,"invitation_lifetime_seconds":3600});
    let mut wrong = init.clone();
    wrong["invitation_lifetime_seconds"] = json!(604800);
    assert_eq!(
        call("initialize", wrong).send().await.unwrap().status(),
        409
    );
    let response = call("initialize", init.clone()).send().await.unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    let events = |repo: u64| {
        harness.client.post(format!(
            "{}/RepositoryDelivery/{request}:{repo}:1/events",
            harness.restate.ingress_url()
        ))
    };
    for repo in [10, 11] {
        let rows = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let response = events(repo).send().await.unwrap();
                let status = response.status();
                let body = response.text().await.unwrap();
                assert!(status.is_success(), "events {status}: {body}");
                let rows: Vec<Value> = serde_json::from_str(&body).unwrap();
                if rows.iter().any(|r| r["kind"] == "create") {
                    break rows;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("dispatch completes");
        assert_eq!(rows.last().unwrap()["kind"], "create");
        assert!(
            rows[..rows.len() - 1]
                .iter()
                .all(|r| r["kind"] == "control")
        );
        assert!(!rows[..rows.len() - 1].is_empty());
        assert_eq!(rows[0]["input"]["admitted_at"], json!(admitted));
        assert_eq!(rows[0]["input"]["invitation_lifetime_seconds"], 3600);
        assert_eq!(rows[0]["input"]["envelope"]["control"], control["control"]);
    }
    let mut release = control.clone();
    release["operation_id"] = json!(ghinvite_core::RequestId::new());
    release["revision"] = json!(3);
    release["control"] = json!({"kind":"active"});
    assert_eq!(
        call("apply_link_control", release.clone())
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    // A completed old receipt remains exact, but emits no delayed hold.
    assert_eq!(
        call("apply_link_control", control.clone())
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap(),
        replay
    );
    let mut stale = control.clone();
    stale["operation_id"] = json!(ghinvite_core::RequestId::new());
    assert_eq!(
        call("apply_link_control", stale.clone())
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap(),
        json!({"outcome":"ignored_stale","revision":3})
    );
    let mut changed = control.clone();
    changed["control"] = json!({"kind":"manual_pause"});
    assert_eq!(
        call("apply_link_control", changed)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let mut changed = release.clone();
    changed["operation_id"] = json!(ghinvite_core::RequestId::new());
    changed["admitted_at"] = json!(admitted + chrono::Duration::seconds(1));
    assert_eq!(
        call("apply_link_control", changed)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    for repo in [10, 11] {
        let rows = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let response = events(repo).send().await.unwrap();
                let status = response.status();
                let body = response.text().await.unwrap();
                assert!(status.is_success(), "events {status}: {body}");
                let rows: Vec<Value> = serde_json::from_str(&body).unwrap();
                if rows
                    .last()
                    .is_some_and(|r| r["input"]["envelope"]["revision"] == 3)
                {
                    break rows;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(rows.iter().filter(|r| r["kind"] == "create").count(), 1);
        assert_eq!(
            rows.iter()
                .filter(|r| r["input"]["envelope"]["revision"] == 3)
                .count(),
            1
        );
    }
    let mut competing_a = release.clone();
    competing_a["operation_id"] = json!(ghinvite_core::RequestId::new());
    competing_a["revision"] = json!(4);
    competing_a["control"] = json!({"kind":"manual_pause"});
    let mut competing_b = competing_a.clone();
    competing_b["operation_id"] = json!(ghinvite_core::RequestId::new());
    let (a, b) = tokio::join!(
        call("apply_link_control", competing_a).send(),
        call("apply_link_control", competing_b).send()
    );
    let mut statuses = [a.unwrap().status().as_u16(), b.unwrap().status().as_u16()];
    statuses.sort();
    assert_eq!(statuses, [200, 409], "equal revisions bind one full input");
    assert_eq!(
        call("apply_link_control", stale)
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap(),
        json!({"outcome":"ignored_stale","revision":3}),
        "stale receipt does not drift with current revision"
    );
    let snapshot: Value = call(
        "request_status",
        json!({"link_id":link,"request_id":request,"requester_id":8}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(
        snapshot, init["request"],
        "control never rewrites approved request or decision receipt"
    );
    assert_eq!(call("initialize", init).send().await.unwrap().status(), 200);
    // A pending request accepts the same inbox without approval or dispatch.
    let pending_id = ghinvite_core::RequestId::new();
    let mut pending_init = json!({"request":{"request_id":pending_id,"link_id":link,"account_id":100,"requester_id":9,
        "justification":null,"state":"pending","admitted_at":admitted,
        "decision_deadline":admitted+chrono::Duration::days(7),"revision":1},
        "installation_id":1,"targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/api","grant":"write"}]},
        "approval_required":true,"invitation_lifetime_seconds":3600});
    let mut pending_control = control.clone();
    pending_control["request_id"] = json!(pending_id);
    pending_control["operation_id"] = json!(ghinvite_core::RequestId::new());
    let pending_call = |method: &str, input: Value| {
        harness
            .client
            .post(format!(
                "{}/InvitationRequest/{pending_id}/{method}",
                harness.restate.ingress_url()
            ))
            .json(&input)
    };
    assert_eq!(
        pending_call("apply_link_control", pending_control)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        pending_call("initialize", pending_init.clone())
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let pending_rows = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let rows: Vec<Value> = harness
                .client
                .post(format!(
                    "{}/RepositoryDelivery/{pending_id}:10:1/events",
                    harness.restate.ingress_url()
                ))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if !rows.is_empty() {
                break rows;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(pending_rows.iter().all(|r| r["kind"] == "control"));
    let snapshot: Value = pending_call(
        "request_status",
        json!({"link_id":link,"request_id":pending_id,"requester_id":9}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(snapshot, pending_init["request"]);
    pending_init["invitation_lifetime_seconds"] = json!(604800);
    assert_eq!(
        pending_call("initialize", pending_init)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    harness.finish().await;
}

#[tokio::test]
async fn pending_cascade_cancels_once_projects_typed_reason_and_preserves_approval_receipts() {
    use ghinvite_core::storage::{
        AppInstallationStorage, ConsoleStorage, RecordStorage, projection::fixture::Seed,
    };
    let now =
        chrono::DateTime::from_timestamp_millis(chrono::Utc::now().timestamp_millis()).unwrap();
    // GitHub: the repository App installed on the user `owner` (100) for
    // `owner/api` (10).
    let github = {
        use octoemu::seed::{Access, App, Installation, Repo, Seed, User};
        Seed::new()
            .user(User::new(100, "owner"))
            .repo(Repo::new(10, "owner", "api").private())
            .app(
                App::new(
                    octoemu::compat::REPO_APP_ID,
                    "ghinvite-repo-fixture",
                    octoemu::compat::REPO_CLIENT_ID,
                    "fixture-client-secret",
                )
                .public_key(octoemu::compat::PUBLIC_KEY)
                .permission("administration", Access::Write)
                .permission("metadata", Access::Read),
            )
            .installation(
                Installation::new(1, octoemu::compat::REPO_APP_ID, "owner").selected(["owner/api"]),
            )
    };
    let harness = ghinvite_e2e::Harness::start_with_emulator(
        ghinvite_e2e::github_emulator_with(github).await,
    )
    .await;
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let account = ghinvite_core::Account {
        installation_id: 1,
        account_id: 100,
        account_login: "owner".into(),
        account_type: ghinvite_core::AccountType::User,
        installed_at: now,
        uninstalled_at: None,
        selected_repos: ghinvite_core::SelectedRepos::Subset(vec![10]),
    };
    storage
        .put_app_installation(&ghinvite_core::account::Installation::repo(
            (account).clone(),
        ))
        .await
        .unwrap();
    for user_id in [8, 100] {
        storage
            .upsert_user(&ghinvite_core::User {
                user_id,
                login: format!("user-{user_id}"),
                avatar_url: None,
                github_created_at: None,
                last_seen_at: now,
            })
            .await
            .unwrap();
    }
    let link = ghinvite_core::InvitationLink {
        batch_source: None,
        access_lifetime_seconds: None,
        email_domains: vec![],
        id: ghinvite_core::InvitationLinkId::new(),
        organization_installation_id: None,
        installation_id: 1,
        account_id: 100,
        created_by: 100,
        created_at: now - chrono::Duration::days(8),
        expires_at: None,
        max_uses: Some(100),
        uses_count: 0,
        secret: ghinvite_core::LinkSecretState {
            secret_generation: 1,
            sha256: [42; 32].into(),
        },
        approval_required: true,
        description: "Cascade".into(),
        internal_note: None,
        revoked_at: None,
        revoked_by: None,
        pause: None,
        invitation_lifetime_seconds: 3600,
        abuse_threshold_per_hour: None,
        budget_share_pct: 50,
        targets: ghinvite_core::targets::Targets {
            repos: vec![ghinvite_core::targets::RepoTarget {
                repo_id: 10,
                repo_full_name: "owner/api".into(),
                grant: ghinvite_core::targets::RepoRole::Write,
            }],
            ..Default::default()
        },
    };
    storage.seed_link(&link).await.unwrap();
    let github_transport = ghinvite_e2e::github_transport(harness.client.clone());
    let state = ghinvite_workflows::AppState::new(storage.clone()).with_github_apps(Arc::new(
        ghinvite_workflows::RepositoryGithub::new(
            github_transport,
            &harness.github_url,
            ghinvite_e2e::repository_apps(&harness.github_url),
        )
        .unwrap(),
    ));
    let faults = Arc::new(ghinvite_workflows::request_owner::Faults::default());
    *faults.clock.lock().unwrap() = Some(now);
    let endpoint = ghinvite_workflows::request_owner::bind_with_faults(
        ghinvite_workflows::projection::bind(
            restate_sdk::endpoint::Endpoint::builder(),
            storage.clone(),
        ),
        faults,
    )
    .bind(RepositoryDelivery)
    .bind(ghinvite_workflows::availability::AccountInstallation {
        state: state.clone(),
    })
    .bind(ghinvite_workflows::availability::InstallationProjection { state })
    .bind(ghinvite_workflows::sweep_schedule::SweepSchedule::default())
    .build();
    harness.restate.deploy(endpoint).await;
    let setup = ghinvite_e2e::setup_command(serde_json::to_value(&account).unwrap());
    let response = harness
        .client
        .post(format!(
            "{}/AccountInstallation/repo:100/onboard",
            harness.restate.ingress_url()
        ))
        .json(&setup)
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    let request = ghinvite_core::RequestId::new();
    let init = |request| {
        json!({"request":{"request_id":request,"link_id":link.id,"account_id":100,"requester_id":8,"justification":null,
        "state":"pending","admitted_at":now,"decision_deadline":now+chrono::Duration::days(7),"revision":1},
        "installation_id":1,"targets":link.targets,"approval_required":true,"invitation_lifetime_seconds":3600})
    };
    let control = |request| {
        json!({"link_id":link.id,"request_id":request,"operation_id":ghinvite_core::RequestId::new(),"revision":2,"hold_epoch":0,"at":now,
        "admitted_at":now,"invitation_lifetime_seconds":3600,"control":{"kind":"revoked","decline_pending_requests":true,"cancel_pending_invitations":false,"abort_hold":null}})
    };
    let call = |request: ghinvite_core::RequestId, method: &str, input: Value| {
        harness
            .client
            .post(format!(
                "{}/InvitationRequest/{request}/{method}",
                harness.restate.ingress_url()
            ))
            .json(&input)
    };
    let cancelled_control = control(request);
    assert_eq!(
        call(request, "apply_link_control", cancelled_control.clone())
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        call(request, "initialize", init(request))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let status = |request| json!({"link_id":link.id,"request_id":request,"requester_id":8});
    let cancelled: Value = call(request, "request_status", status(request))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cancelled["state"], "cancelled");
    assert_eq!(cancelled["decision"]["cancellation_reason"], "link_revoked");
    assert_eq!(cancelled["revision"], 2);
    let decide = |request| json!({"link_id":link.id,"request_id":request,"operation_id":ghinvite_core::RequestId::new(),"admin":{"account_id":100,"user_id":100},"action":{"kind":"approve"}});
    let rejected_decision = decide(request);
    let refused: Value = call(request, "decide", rejected_decision.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(refused["outcome"], "not_pending");
    for _ in 0..3 {
        assert_eq!(
            call(request, "apply_link_control", cancelled_control.clone())
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
    }
    assert_eq!(
        call(request, "request_status", status(request))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap(),
        cancelled
    );
    assert_eq!(
        harness
            .client
            .post(format!(
                "{}/InvitationRequest/{request}/tick_expire",
                harness.restate.ingress_url()
            ))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let approved = ghinvite_core::RequestId::new();
    assert_eq!(
        call(approved, "initialize", init(approved))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let approval = decide(approved);
    let receipt: Value = call(approved, "decide", approval.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(receipt["outcome"], "applied");
    assert_eq!(
        call(approved, "apply_link_control", control(approved))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        call(approved, "decide", approval)
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap(),
        receipt
    );
    assert_eq!(
        call(approved, "request_status", status(approved))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["state"],
        "approved"
    );
    assert_eq!(
        call(request, "decide", rejected_decision)
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap(),
        refused
    );
    let pending = ghinvite_core::RequestId::new();
    assert_eq!(
        call(pending, "initialize", init(pending))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        call(pending, "request_status", status(pending))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["state"],
        "pending"
    );
    assert_eq!(
        call(pending, "apply_link_control", control(pending))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        call(pending, "request_status", status(pending))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["state"],
        "cancelled"
    );
    // At the exact pending deadline, expiry is the pre-existing terminal
    // transition. A cascade must not relabel it as a revocation cancellation.
    let expired = ghinvite_core::RequestId::new();
    let mut expired_init = init(expired);
    expired_init["request"]["admitted_at"] = json!(now - chrono::Duration::days(7));
    expired_init["request"]["decision_deadline"] = json!(now);
    let mut expired_control = control(expired);
    expired_control["admitted_at"] = expired_init["request"]["admitted_at"].clone();
    assert_eq!(
        call(expired, "apply_link_control", expired_control)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        call(expired, "initialize", expired_init)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let expired_snapshot: Value = call(expired, "request_status", status(expired))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(expired_snapshot["state"], "expired");
    assert!(expired_snapshot["decision"]["cancellation_reason"].is_null());
    assert_eq!(expired_snapshot["decision"]["effective_at"], json!(now));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let row = storage.get_invitation_request(request).await.unwrap();
            let events = storage
                .list_audit_events(
                    100,
                    Some(ghinvite_core::audit::EventType::RequestCancelled),
                    ghinvite_core::storage::AuditPosition::Latest,
                )
                .await
                .unwrap()
                .events;
            if row
                .as_ref()
                .is_some_and(|r| r.state == ghinvite_core::RequestState::Cancelled)
                && events.len() == 2
            {
                assert_eq!(
                    row.unwrap().cancellation_reason,
                    Some(ghinvite_core::RequestCancellationReason::LinkRevoked)
                );
                assert_eq!(events[0].metadata, json!({"reason":"link_revoked"}));
                assert_eq!(events[1].metadata, json!({"reason":"link_revoked"}));
                assert_ne!(events[0].target_id, events[1].target_id);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("one cancellation audit per request and typed projections");
    harness.finish().await;
}
