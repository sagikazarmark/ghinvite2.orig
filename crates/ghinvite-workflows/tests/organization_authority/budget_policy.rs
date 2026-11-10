use super::*;

fn command(actor: u64, confirmed: bool) -> Value {
    json!({"operation_id":ghinvite_core::RequestId::new(),"actor_user_id":actor,"confirmed_established":confirmed})
}
#[tokio::test]
async fn org_budget_policy_requires_live_owner_and_replays_without_reverting_later_policy() {
    let github = Arc::new(Github::default());
    github.roles.lock().unwrap().extend([
        (7, ("member", "active")),
        (8, ("admin", "active")),
        (9, ("admin", "pending")),
    ]);
    let h = Harness::start_with_github_router(
        axum::Router::new()
            .fallback(github_handler())
            .with_state(github.clone()),
    )
    .await;
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    for id in [7, 8, 9] {
        storage
            .upsert_user(&ghinvite_core::User {
                user_id: id,
                login: format!("user-{id}"),
                avatar_url: None,
                github_created_at: None,
                last_seen_at: chrono::Utc::now(),
            })
            .await
            .unwrap();
    }
    let github_transport = ghinvite_e2e::github_transport(h.client.clone());
    let apps = Arc::new(
        ghinvite_workflows::RepositoryGithub::new(
            github_transport,
            &h.github_url,
            ghinvite_e2e::installed_apps(&h.github_url, true),
        )
        .unwrap(),
    );
    let state = ghinvite_workflows::AppState::new(storage.clone())
        .with_github_apps(apps)
        .with_invite_budget(storage.clone());
    h.restate
        .deploy(ghinvite_workflows::build_endpoint(state, storage.clone(), None).unwrap())
        .await;
    let mut org = ghinvite_e2e::setup_command(
        json!({"installation_id":2,"actor_user_id":7,"account_id":100,"account_login":"acme","account_type":"Organization","selected_repos":[],"installed_at":"2026-01-01T00:00:00Z"}),
    );
    org["verified"]["app"] = json!("org");
    org["verified"]["permissions"] = json!({"members":"write","metadata":"read"});
    call(&h, "Installation", "org:2", "onboard", Some(&org), 200).await;
    for actor in [7, 9] {
        let denied = command(actor, true);
        assert_eq!(
            call(
                &h,
                "AccountInstallation",
                "repo:100",
                "set_org_cap",
                Some(&denied),
                200
            )
            .await,
            json!({"Err":"not_owner"})
        );
    }
    let absent = command(8, true);
    assert_eq!(
        call(
            &h,
            "AccountInstallation",
            "repo:101",
            "set_org_cap",
            Some(&absent),
            200
        )
        .await,
        json!({"Err":"org_app_required"})
    );
    let raised = command(8, true);
    github.unavailable.store(true, Ordering::SeqCst);
    call(
        &h,
        "AccountInstallation",
        "repo:100",
        "set_org_cap",
        Some(&raised),
        503,
    )
    .await;
    github.unavailable.store(false, Ordering::SeqCst);
    let original = call(
        &h,
        "AccountInstallation",
        "repo:100",
        "set_org_cap",
        Some(&raised),
        200,
    )
    .await;
    assert_eq!(original["Ok"]["revision"], 1);
    let budget = call(&h, "InviteBudget", "org:100", "snapshot", None, 200).await;
    assert_eq!(budget["buckets"][0]["cap"], 500);
    let lowered = command(8, false);
    assert_eq!(
        call(
            &h,
            "AccountInstallation",
            "repo:100",
            "set_org_cap",
            Some(&lowered),
            200
        )
        .await["Ok"]["revision"],
        2
    );
    github.roles.lock().unwrap().insert(8, ("member", "active"));
    assert_eq!(
        call(
            &h,
            "AccountInstallation",
            "repo:100",
            "set_org_cap",
            Some(&raised),
            200
        )
        .await,
        original
    );
    assert_eq!(
        call(&h, "InviteBudget", "org:100", "snapshot", None, 200).await["buckets"][0]["cap"],
        50
    );
    let mut changed = raised.clone();
    changed["confirmed_established"] = json!(false);
    call(
        &h,
        "AccountInstallation",
        "repo:100",
        "set_org_cap",
        Some(&changed),
        409,
    )
    .await;
    assert_eq!(
        call(
            &h,
            "AccountInstallation",
            "repo:100",
            "set_org_cap",
            Some(&command(8, true)),
            200
        )
        .await,
        json!({"Err":"not_owner"})
    );
    let view = call(
        &h,
        "AccountInstallation",
        "repo:100",
        "org_cap_policy",
        None,
        200,
    )
    .await;
    assert_eq!(view["enabled"], true);
    assert_eq!(view["policy"]["command"], lowered);
    let audit = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            use ghinvite_core::storage::ConsoleStorage;
            let events = storage
                .list_audit_events(
                    100,
                    Some(ghinvite_core::audit::EventType::OrganizationInviteCapChanged),
                    Default::default(),
                )
                .await;
            if let Ok(events) = events
                && events.events.len() == 2
            {
                break events;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    let audit = audit.expect("two immutable cap audits");
    assert_ne!(audit.events[0].id, audit.events[1].id);
    assert!(audit.events.iter().all(|e| e.actor_id == Some(8)));
    h.finish().await;
}

#[derive(Clone)]
struct BudgetBoundary {
    endpoint: restate_sdk::endpoint::Endpoint,
    unavailable: Arc<AtomicBool>,
    refused: Arc<std::sync::atomic::AtomicUsize>,
}
async fn budget_boundary(State(state): State<BudgetBoundary>, request: Request) -> Response {
    if request.uri().path() == "/invoke/InviteBudget/set_org_cap"
        && state.unavailable.load(Ordering::SeqCst)
    {
        state.refused.fetch_add(1, Ordering::SeqCst);
        return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    // The actual SDK endpoint and actual budget authority still execute every
    // accepted request. Only this test's transport boundary can fail a call.
    state
        .endpoint
        .handle_with_options(
            request,
            restate_sdk::endpoint::HandleOptions {
                protocol_mode: restate_sdk::endpoint::ProtocolMode::RequestResponse,
            },
        )
        .map(axum::body::Body::new)
}
struct BoundaryTask(tokio::task::JoinHandle<()>);
impl Drop for BoundaryTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn await_refused(boundary: &BudgetBoundary, after: usize) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while boundary.refused.load(Ordering::SeqCst) <= after {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("actual budget call reached unavailable endpoint boundary");
}

#[tokio::test]
async fn completed_policy_replays_during_unrelated_pending_change_and_fresh_command_drains_it() {
    let github = Arc::new(Github::default());
    github.roles.lock().unwrap().insert(8, ("admin", "active"));
    let h = Harness::start_with_github_router(
        axum::Router::new()
            .fallback(github_handler())
            .with_state(github.clone()),
    )
    .await;
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    storage
        .upsert_user(&ghinvite_core::User {
            user_id: 8,
            login: "user-8".into(),
            avatar_url: None,
            github_created_at: None,
            last_seen_at: chrono::Utc::now(),
        })
        .await
        .unwrap();
    let github_transport = ghinvite_e2e::github_transport(h.client.clone());
    let apps = Arc::new(
        ghinvite_workflows::RepositoryGithub::new(
            github_transport,
            &h.github_url,
            ghinvite_e2e::installed_apps(&h.github_url, true),
        )
        .unwrap(),
    );
    let state = ghinvite_workflows::AppState::new(storage.clone())
        .with_github_apps(apps)
        .with_invite_budget(storage.clone());
    let boundary = BudgetBoundary {
        endpoint: ghinvite_workflows::build_endpoint(state, storage.clone(), None).unwrap(),
        unavailable: Arc::new(AtomicBool::new(false)),
        refused: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = axum::Router::new()
        .fallback(budget_boundary)
        .with_state(boundary.clone());
    let task = BoundaryTask(tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    }));
    h.restate
        .admin()
        .register(&format!("http://{address}"))
        .await;
    ghinvite_e2e::journal::JournalAudit::retain(&h, &["AccountInstallation"]).await;
    let mut org = ghinvite_e2e::setup_command(
        json!({"installation_id":2,"account_id":100,"account_login":"acme","account_type":"Organization","selected_repos":[],"installed_at":"2026-01-01T00:00:00Z"}),
    );
    org["verified"]["app"] = json!("org");
    org["verified"]["permissions"] = json!({"members":"write","metadata":"read"});
    call(&h, "Installation", "org:2", "onboard", Some(&org), 200).await;
    let a = command(8, true);
    let first = call(
        &h,
        "AccountInstallation",
        "repo:100",
        "set_org_cap",
        Some(&a),
        200,
    )
    .await;
    assert_eq!(first["Ok"]["revision"], 1);

    boundary.unavailable.store(true, Ordering::SeqCst);
    let b = command(8, false);
    let sent = h
        .restate
        .invoke(
            &Call::object("AccountInstallation", "repo:100", "set_org_cap").send(),
            Some(&b),
            None,
        )
        .await;
    assert_eq!(sent.status, 202);
    await_refused(&boundary, 0).await;
    let journal = h.restate.admin().journal(sent.invocation_id()).await;
    let retained = journal
        .iter()
        .find(|e| e.entry_type == "Command: SetState" && e.raw_contains("org_cap_pending"))
        .expect("B retained before budget RPC");
    let budget_call = journal
        .iter()
        .find(|e| e.entry_type == "Command: Call" && e.raw_contains("InviteBudget"))
        .expect("real budget RPC");
    assert!(retained.index < budget_call.index);
    h.restate.admin().kill(sent.invocation_id()).await;
    h.restate
        .admin()
        .await_status(sent.invocation_id(), &["completed"])
        .await;
    // The completed receipt is independent of both currently unavailable
    // dependencies and does not need to finish B as a side effect.
    github.unavailable.store(true, Ordering::SeqCst);
    let replay = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        call(
            &h,
            "AccountInstallation",
            "repo:100",
            "set_org_cap",
            Some(&a),
            200,
        ),
    )
    .await
    .expect("A replay must not wait for B's unavailable budget");
    assert_eq!(replay, first);
    let mut conflicting_a = a.clone();
    conflicting_a["confirmed_established"] = json!(false);
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        call(
            &h,
            "AccountInstallation",
            "repo:100",
            "set_org_cap",
            Some(&conflicting_a),
            409,
        ),
    )
    .await
    .expect("A conflict is likewise independent of B");
    assert_eq!(
        call(
            &h,
            "AccountInstallation",
            "repo:100",
            "org_cap_policy",
            None,
            200
        )
        .await["policy"]["command"],
        a
    );

    let c = command(8, true);
    let refusals = boundary.refused.load(Ordering::SeqCst);
    let sent_c = h
        .restate
        .invoke(
            &Call::object("AccountInstallation", "repo:100", "set_org_cap").send(),
            Some(&c),
            None,
        )
        .await;
    assert_eq!(sent_c.status, 202);
    await_refused(&boundary, refusals).await;
    boundary.unavailable.store(false, Ordering::SeqCst);
    h.restate
        .admin()
        .await_status(sent_c.invocation_id(), &["completed"])
        .await;
    let failed_c = h
        .client
        .get(format!(
            "{}/restate/invocation/{}/attach",
            h.restate.ingress_url(),
            sent_c.invocation_id()
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(
        failed_c.status(),
        503,
        "fresh C must recheck live authority after draining B"
    );
    // A fresh command drains B before checking its own live authorization.
    // GitHub is still unavailable, so C cannot have changed the policy.
    let retained_b = call(
        &h,
        "AccountInstallation",
        "repo:100",
        "org_cap_policy",
        None,
        200,
    )
    .await;
    assert_eq!(retained_b["policy"]["command"], b);
    assert_eq!(retained_b["policy"]["revision"], 2);
    assert_eq!(
        call(&h, "InviteBudget", "org:100", "snapshot", None, 200).await["buckets"][0]["cap"],
        50
    );
    let replay_b = call(
        &h,
        "AccountInstallation",
        "repo:100",
        "set_org_cap",
        Some(&b),
        200,
    )
    .await;
    assert_eq!(replay_b["Ok"], retained_b["policy"]);
    github.unavailable.store(false, Ordering::SeqCst);
    let third = call(
        &h,
        "AccountInstallation",
        "repo:100",
        "set_org_cap",
        Some(&c),
        200,
    )
    .await;
    assert_eq!(third["Ok"]["revision"], 3);
    assert_eq!(
        call(
            &h,
            "AccountInstallation",
            "repo:100",
            "set_org_cap",
            Some(&b),
            200
        )
        .await,
        replay_b
    );
    assert_eq!(
        call(&h, "InviteBudget", "org:100", "snapshot", None, 200).await["buckets"][0]["cap"],
        500
    );
    // Retry of the pending operation itself must use the receipt completion
    // just created, not perform a second authority check or allocate revision5.
    boundary.unavailable.store(true, Ordering::SeqCst);
    let d = command(8, false);
    let before_d = boundary.refused.load(Ordering::SeqCst);
    let sent_d = h
        .restate
        .invoke(
            &Call::object("AccountInstallation", "repo:100", "set_org_cap").send(),
            Some(&d),
            None,
        )
        .await;
    assert_eq!(sent_d.status, 202);
    await_refused(&boundary, before_d).await;
    h.restate.admin().kill(sent_d.invocation_id()).await;
    h.restate
        .admin()
        .await_status(sent_d.invocation_id(), &["completed"])
        .await;
    boundary.unavailable.store(false, Ordering::SeqCst);
    github.unavailable.store(true, Ordering::SeqCst);
    let fourth = call(
        &h,
        "AccountInstallation",
        "repo:100",
        "set_org_cap",
        Some(&d),
        200,
    )
    .await;
    assert_eq!(fourth["Ok"]["revision"], 4);
    assert_eq!(fourth["Ok"]["command"], d);
    assert_eq!(
        call(
            &h,
            "AccountInstallation",
            "repo:100",
            "set_org_cap",
            Some(&d),
            200
        )
        .await,
        fourth
    );
    assert_eq!(
        call(&h, "InviteBudget", "org:100", "snapshot", None, 200).await["buckets"][0]["cap"],
        50
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            use ghinvite_core::storage::ConsoleStorage;
            let events = storage
                .list_audit_events(
                    100,
                    Some(ghinvite_core::audit::EventType::OrganizationInviteCapChanged),
                    Default::default(),
                )
                .await
                .unwrap();
            if events.events.len() == 4 {
                let operations = events
                    .events
                    .iter()
                    .map(|e| e.metadata["operation_id"].as_str().unwrap().to_owned())
                    .collect::<std::collections::BTreeSet<_>>();
                assert_eq!(
                    operations,
                    [&a, &b, &c, &d]
                        .map(|v| v["operation_id"].as_str().unwrap().to_owned())
                        .into_iter()
                        .collect()
                );
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("exactly one immutable audit per applied operation");
    h.finish().await;
    drop(task);
}
