use super::*;
use ghinvite_core::{
    audit::EventType,
    storage::{AppInstallationReadStorage, AuditPosition, ConsoleStorage},
};
use std::sync::{
    Mutex,
    atomic::{AtomicI64, Ordering},
};
use std::time::Duration;

struct Runtime {
    h: Harness,
    storage: Arc<ghinvite_storage_sqlx::SqlxStorage>,
    fault: Arc<ghinvite_workflows::availability::InstallationAuditFault>,
    suspension: Arc<Mutex<Value>>,
    clock: Arc<AtomicI64>,
    setup: Value,
}
impl Runtime {
    async fn start() -> Self {
        Self::start_with_hook(true).await
    }
    async fn start_with_hook(hook: bool) -> Self {
        let suspension = Arc::new(Mutex::new(Value::Null));
        let observed = suspension.clone();
        let router=axum::Router::new().route("/app/installations/1",axum::routing::get(move || {let observed=observed.clone();async move {axum::Json(json!({"id":1,"account":{"id":100,"login":"acme","type":"Organization"},"app_id":123,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","repository_selection":"all","permissions":{"administration":"write","metadata":"read"},"suspended_at":observed.lock().unwrap().clone()}))}})).fallback_service(ghinvite_github::stub::router());
        let h = Harness::start_with_github_router(router).await;
        let storage = Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::in_memory()
                .await
                .unwrap(),
        );
        storage.run_migrations().await.unwrap();
        let github_transport = ghinvite_e2e::github_transport(h.client.clone());
        let mut state =
            ghinvite_workflows::AppState::new(storage.clone()).with_github_apps(Arc::new(
                ghinvite_workflows::RepositoryGithub::new(
                    github_transport,
                    &h.github_url,
                    ghinvite_e2e::repository_apps(&h.github_url),
                )
                .unwrap(),
            ));
        let fault = Arc::new(ghinvite_workflows::availability::InstallationAuditFault::default());
        let clock = Arc::new(AtomicI64::new(chrono::Utc::now().timestamp_millis()));
        state.installation_audit_fault = hook.then(|| fault.clone());
        state.installation_clock = Some(clock.clone());
        h.restate
            .deploy(
                restate_sdk::endpoint::Endpoint::builder()
                    .bind(ghinvite_workflows::sweep_schedule::SweepSchedule::default())
                    .bind(ghinvite_workflows::installation::Installation {
                        state: state.clone(),
                    })
                    .bind(ghinvite_workflows::availability::AccountInstallation {
                        state: state.clone(),
                    })
                    .bind(ghinvite_workflows::availability::InstallationProjection { state })
                    .build(),
            )
            .await;
        let setup = ghinvite_e2e::setup_command(
            json!({"installation_id":1,"account_id":100,"account_login":"acme","account_type":"Organization","selected_repos":[],"installed_at":"2026-01-01T00:00:00Z"}),
        );
        call(
            &h,
            "AccountInstallation",
            "repo:100",
            "onboard",
            Some(&setup),
            200,
        )
        .await;
        Self {
            h,
            storage,
            fault,
            suspension,
            clock,
            setup,
        }
    }
    fn event(&self, marker: &str, action: &str, at: &str) -> Value {
        json!({"delivery_id":marker.repeat(64),"action":action,"verified":self.setup["verified"],"sender_id":7,"sender_login":"installer","updated_at":at})
    }
    async fn event_call(&self, input: &Value, status: u16) {
        call(
            &self.h,
            "AccountInstallation",
            "repo:100",
            "onboard_webhook",
            Some(input),
            status,
        )
        .await;
    }
    async fn facts(&self, kind: EventType, count: usize) -> Vec<ghinvite_core::audit::AuditEvent> {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let events = self
                    .storage
                    .list_audit_events(100, Some(kind), AuditPosition::Latest)
                    .await
                    .unwrap()
                    .events;
                if events.len() >= count {
                    return events;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap()
    }
    async fn raw(&self) -> Value {
        let result:Value=self.h.client.post(format!("{}/query",self.h.restate.admin_url())).header("accept","application/json").json(&json!({"query":"SELECT key,value_utf8 FROM state WHERE service_name='AccountInstallation' AND service_key='repo:100'"})).send().await.unwrap().json().await.unwrap();
        let rows = result["rows"].as_array().unwrap();
        assert!(
            !rows
                .iter()
                .any(|v| v["key"] == "installation" || v["key"] == "delegation")
        );
        serde_json::from_str(
            rows.iter()
                .find(|v| v["key"] == "installation_audit")
                .unwrap()["value_utf8"]
                .as_str()
                .unwrap(),
        )
        .unwrap()
    }
    async fn kill_at_save(&self, handler: &str, input: &Value) -> Value {
        self.fault.armed.store(true, Ordering::SeqCst);
        let before = self.fault.paused.load(Ordering::SeqCst);
        let sent = self
            .h
            .restate
            .invoke(
                &Call::object("AccountInstallation", "repo:100", handler).send(),
                Some(input),
                None,
            )
            .await;
        assert_eq!(sent.status, 202);
        tokio::time::timeout(Duration::from_secs(15), async {
            while self.fault.paused.load(Ordering::SeqCst) == before {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let raw = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let raw = self.raw().await;
                if raw["pending"].is_object() {
                    return raw;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("atomic pending record must become durable before kill");
        self.h.restate.admin().kill(sent.invocation_id()).await;
        self.h
            .restate
            .admin()
            .await_status(sent.invocation_id(), &["completed"])
            .await;
        raw
    }
}

#[tokio::test]
async fn permission_facts_survive_atomic_kill_receipt_expiry_and_older_sources() {
    let r = Runtime::start().await;
    r.event_call(&r.event("a", "created", "2026-01-01T01:00:00Z"), 200)
        .await;
    let accepted = r.event("b", "new_permissions_accepted", "2026-01-01T02:00:00Z");
    let retained = r.kill_at_save("onboard_webhook", &accepted).await;
    assert_eq!(retained["source"]["updated_at"], accepted["updated_at"]);
    assert_eq!(
        retained["pending"]["audit_facts"][0]["revision"],
        retained["status"]["installation"]["revision"]
    );
    assert!(
        r.storage
            .list_audit_events(
                100,
                Some(EventType::InstallationPermissionsAccepted),
                AuditPosition::Latest
            )
            .await
            .unwrap()
            .events
            .is_empty()
    );
    r.event_call(&accepted, 200).await;
    let facts = r.facts(EventType::InstallationPermissionsAccepted, 1).await;
    assert_eq!(facts.len(), 1);
    assert_eq!(
        facts[0].occurred_at.to_rfc3339(),
        "2026-01-01T02:00:00+00:00"
    );
    assert!(facts[0].request_id.is_none());
    r.event_call(&accepted, 200).await;
    r.clock.fetch_add(
        chrono::Duration::days(8).num_milliseconds(),
        Ordering::SeqCst,
    );
    r.event_call(&accepted, 200).await;
    r.event_call(
        &r.event("c", "new_permissions_accepted", "2026-01-01T01:30:00Z"),
        200,
    )
    .await;
    let mut conflict = r.event("d", "new_permissions_accepted", "2026-01-01T02:00:00Z");
    conflict["sender_id"] = json!(8);
    r.event_call(&conflict, 409).await;
    assert_eq!(
        r.facts(EventType::InstallationPermissionsAccepted, 1).await,
        facts
    );
    r.event_call(
        &r.event("e", "new_permissions_accepted", "2026-01-01T03:00:00Z"),
        200,
    )
    .await;
    let both = r.facts(EventType::InstallationPermissionsAccepted, 2).await;
    assert_eq!(both.len(), 2);
    assert!(both.contains(&facts[0]));
    r.h.finish().await;
}

#[tokio::test]
async fn suspension_facts_use_verified_time_and_delegation_receipt_survives_kill() {
    let r = Runtime::start().await;
    r.event_call(&r.event("a", "created", "2026-01-01T01:00:00Z"), 200)
        .await;
    let input = json!({"operation_id":ghinvite_core::RequestId::new(),"actor_user_id":7,"on":true});
    let retained = r.kill_at_save("set_delegation", &input).await;
    assert_eq!(retained["delegation"]["revision"], 1);
    assert_eq!(retained["status"]["repo_admin_delegation"], true);
    assert!(retained["pending_receipt"].is_array());
    assert_eq!(
        call(
            &r.h,
            "AccountInstallation",
            "repo:100",
            "set_delegation",
            Some(&input),
            200
        )
        .await,
        json!({"Ok":null})
    );
    assert_eq!(r.raw().await["delegation"]["revision"], 1);
    let mut conflict = input.clone();
    conflict["on"] = json!(false);
    call(
        &r.h,
        "AccountInstallation",
        "repo:100",
        "set_delegation",
        Some(&conflict),
        409,
    )
    .await;
    let mut off = input.clone();
    off["operation_id"] = json!(ghinvite_core::RequestId::new());
    off["on"] = json!(false);
    call(
        &r.h,
        "AccountInstallation",
        "repo:100",
        "set_delegation",
        Some(&off),
        200,
    )
    .await;
    call(
        &r.h,
        "AccountInstallation",
        "repo:100",
        "set_delegation",
        Some(&input),
        200,
    )
    .await;
    assert_eq!(r.raw().await["delegation"]["revision"], 2);
    *r.suspension.lock().unwrap() = json!("2026-01-02T00:00:00Z");
    r.event_call(&r.event("b", "suspend", "2026-01-02T01:00:00Z"), 200)
        .await;
    let first = r.facts(EventType::InstallationSuspended, 1).await;
    assert_eq!(
        first[0].occurred_at.to_rfc3339(),
        "2026-01-02T00:00:00+00:00"
    );
    r.event_call(&r.event("c", "suspend", "2026-01-02T02:00:00Z"), 200)
        .await;
    assert_eq!(r.facts(EventType::InstallationSuspended, 1).await, first);
    *r.suspension.lock().unwrap() = Value::Null;
    r.event_call(&r.event("d", "unsuspend", "2026-01-03T00:00:00Z"), 200)
        .await;
    *r.suspension.lock().unwrap() = json!("2026-01-04T00:00:00Z");
    r.event_call(&r.event("e", "suspend", "2026-01-04T01:00:00Z"), 200)
        .await;
    let both = r.facts(EventType::InstallationSuspended, 2).await;
    assert_eq!(both.len(), 2);
    assert!(both.contains(&first[0]));
    assert_eq!(
        r.storage
            .app_installation(ghinvite_core::account::App::Repo, 1)
            .await
            .unwrap()
            .unwrap()
            .suspended_at
            .unwrap()
            .to_rfc3339(),
        "2026-01-04T00:00:00+00:00"
    );
    r.h.finish().await;
}

#[tokio::test]
async fn installation_handlers_walk_fixed_named_run_inventory() {
    use ghinvite_e2e::{RunPath, journal::JournalAudit};
    let r = Runtime::start_with_hook(false).await;
    let services = &["AccountInstallation", "InstallationProjection"];
    JournalAudit::retain(&r.h, services).await;
    let mut audit = JournalAudit::default();
    // Replay/getter/refusal cases are real handlers with retained nonempty journals.
    let scope = json!({"account_id":100,"repo_ids":[10]});
    let absent_scope = json!({"account_id":999,"repo_ids":[10]});
    let empty = json!(null);
    let invalid_projection = json!({"installation":null,"settings":null});
    let no_run = [
        ("onboard", r.setup.clone()),
        (
            "set_delegation",
            json!({"operation_id":ghinvite_core::RequestId::new(),"actor_user_id":99,"on":true}),
        ),
        (
            "set_org_cap",
            json!({"operation_id":ghinvite_core::RequestId::new(),"actor_user_id":7,"confirmed_established":false}),
        ),
        ("org_cap_policy", empty.clone()),
        ("status", empty.clone()),
        ("current_installation", empty.clone()),
        ("decision_authority", json!(7)),
        ("recheck_permissions", json!(99)),
        ("refresh", json!(99)),
        (
            "expire_webhook",
            json!({"payload_sha256":"a".repeat(64),"expires_at":chrono::Utc::now()}),
        ),
    ];
    for (handler, body) in no_run {
        let reply = audit
            .invoke(&r.h, "AccountInstallation", "repo:100", handler, &body)
            .await;
        assert_eq!(reply.status, 200, "{handler}: {}", reply.body);
    }
    assert_eq!(
        audit
            .invoke(
                &r.h,
                "AccountInstallation",
                "org:100",
                "org_authority",
                &json!(7)
            )
            .await
            .status,
        200
    );
    assert_eq!(
        audit
            .invoke(
                &r.h,
                "AccountInstallation",
                "org:100",
                "eligibility_installation",
                &empty
            )
            .await
            .status,
        503
    );
    assert_eq!(
        audit
            .invoke(&r.h, "AccountInstallation", "repo:999", "recheck", &empty)
            .await
            .status,
        200
    );
    assert_eq!(
        audit
            .invoke(
                &r.h,
                "AccountInstallation",
                "repo:999",
                "admission_evidence",
                &absent_scope
            )
            .await
            .status,
        503
    );
    assert_eq!(
        audit
            .invoke(
                &r.h,
                "AccountInstallation",
                "repo:999",
                "observe_admission_evidence",
                &absent_scope
            )
            .await
            .status,
        503
    );
    assert_eq!(
        audit
            .invoke(
                &r.h,
                "InstallationProjection",
                "repo:100",
                "apply",
                &invalid_projection
            )
            .await
            .status,
        400
    );
    let mut setup = r.setup.clone();
    setup["operation_id"] = json!(ghinvite_core::RequestId::new());
    assert_eq!(
        audit
            .invoke(&r.h, "AccountInstallation", "repo:100", "onboard", &setup)
            .await
            .status,
        200
    );
    let created = r.event("a", "created", "2026-01-01T01:00:00Z");
    for input in [
        &created,
        &created,
        &r.event("b", "new_permissions_accepted", "2026-01-01T02:00:00Z"),
    ] {
        assert_eq!(
            audit
                .invoke(
                    &r.h,
                    "AccountInstallation",
                    "repo:100",
                    "onboard_webhook",
                    input
                )
                .await
                .status,
            200
        );
    }
    let delegation =
        json!({"operation_id":ghinvite_core::RequestId::new(),"actor_user_id":7,"on":true});
    assert_eq!(
        audit
            .invoke(
                &r.h,
                "AccountInstallation",
                "repo:100",
                "set_delegation",
                &delegation
            )
            .await
            .status,
        200
    );
    for handler in ["recheck_permissions", "refresh"] {
        assert_eq!(
            audit
                .invoke(&r.h, "AccountInstallation", "repo:100", handler, &json!(1))
                .await
                .status,
            200
        );
    }
    assert_eq!(
        audit
            .invoke(&r.h, "AccountInstallation", "repo:100", "recheck", &empty)
            .await
            .status,
        200
    );
    for handler in ["admission_evidence", "observe_admission_evidence"] {
        assert_eq!(
            audit
                .invoke(&r.h, "AccountInstallation", "repo:100", handler, &scope)
                .await
                .status,
            200
        );
    }
    let installation = r
        .storage
        .app_installation(ghinvite_core::account::App::Repo, 1)
        .await
        .unwrap()
        .unwrap();
    let event = ghinvite_core::audit::AuditEvent {
        id: ghinvite_core::AuditEventId::new(),
        account_id: 100,
        occurred_at: chrono::Utc::now(),
        event_type: EventType::InstallationReposChanged,
        actor_kind: ghinvite_core::audit::ActorKind::Github,
        actor_id: None,
        target_kind: ghinvite_core::audit::TargetKind::Installation,
        target_id: "repo:1".into(),
        metadata: Value::Null,
        request_id: None,
    };
    let projection = json!({"installation":installation,"settings":null,"event":event});
    assert_eq!(
        audit
            .invoke(
                &r.h,
                "InstallationProjection",
                "repo:100",
                "apply",
                &projection
            )
            .await
            .status,
        200
    );
    let uninstall = json!({"app":"repo","installation_id":1,"uninstalled_at":chrono::Utc::now()});
    for _ in 0..2 {
        assert_eq!(
            audit
                .invoke(
                    &r.h,
                    "AccountInstallation",
                    "repo:100",
                    "uninstall",
                    &uninstall
                )
                .await
                .status,
            200
        );
    }
    const PATHS: &[RunPath] = &[
        RunPath::new("InstallationProjection", "apply", &[]),
        RunPath::new("InstallationProjection", "apply", &["project_installation"]),
        RunPath::new("AccountInstallation", "onboard", &[]),
        RunPath::new(
            "AccountInstallation",
            "onboard",
            &["verify_onboard_identity", "refresh_github_scope"],
        ),
        RunPath::new(
            "AccountInstallation",
            "onboard_webhook",
            &["webhook_receipt_time"],
        ),
        RunPath::new(
            "AccountInstallation",
            "onboard_webhook",
            &[
                "webhook_receipt_time",
                "verify_onboard_identity",
                "refresh_github_scope",
                "installation_event_time",
                "retain_installation_audit",
                "refresh_installation_credentials",
            ],
        ),
        RunPath::new(
            "AccountInstallation",
            "onboard_webhook",
            &[
                "webhook_receipt_time",
                "verify_onboard_identity",
                "refresh_github_scope",
                "refresh_installation_credentials",
            ],
        ),
        RunPath::new("AccountInstallation", "set_delegation", &[]),
        RunPath::new(
            "AccountInstallation",
            "set_delegation",
            &[
                "delegation_time",
                "installation_event_time",
                "retain_installation_audit",
            ],
        ),
        RunPath::new("AccountInstallation", "set_org_cap", &[]),
        RunPath::new("AccountInstallation", "org_cap_policy", &[]),
        RunPath::new("AccountInstallation", "status", &[]),
        RunPath::new("AccountInstallation", "current_installation", &[]),
        RunPath::new("AccountInstallation", "eligibility_installation", &[]),
        RunPath::new("AccountInstallation", "org_authority", &[]),
        RunPath::new("AccountInstallation", "decision_authority", &[]),
        RunPath::new("AccountInstallation", "recheck_permissions", &[]),
        RunPath::new(
            "AccountInstallation",
            "recheck_permissions",
            &[
                "verify_onboard_identity",
                "refresh_installation_credentials",
            ],
        ),
        RunPath::new("AccountInstallation", "refresh", &[]),
        RunPath::new("AccountInstallation", "refresh", &["refresh_github_scope"]),
        RunPath::new("AccountInstallation", "recheck", &[]),
        RunPath::new("AccountInstallation", "recheck", &["refresh_github_scope"]),
        RunPath::new("AccountInstallation", "expire_webhook", &[]),
        RunPath::new("AccountInstallation", "admission_evidence", &[]),
        RunPath::new(
            "AccountInstallation",
            "admission_evidence",
            &["evidence_time"],
        ),
        RunPath::new("AccountInstallation", "observe_admission_evidence", &[]),
        RunPath::new(
            "AccountInstallation",
            "observe_admission_evidence",
            &["refresh_github_scope", "evidence_time"],
        ),
        RunPath::new("AccountInstallation", "uninstall", &[]),
        RunPath::new(
            "AccountInstallation",
            "uninstall",
            &["installation_event_time", "retain_installation_audit"],
        ),
    ];
    let walked = audit.check(&r.h, services, PATHS).await;
    eprintln!("installation inventory: {walked:?}");
    r.h.finish().await;
}
