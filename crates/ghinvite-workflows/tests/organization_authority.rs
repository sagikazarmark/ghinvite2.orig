//! Actual retained org-owner authority; no org delivery is activated.
#![cfg(feature = "integration")]
use axum::{
    body::to_bytes,
    extract::{Request, State},
    response::{IntoResponse, Response},
};
use ghinvite_core::storage::{AppInstallationReadStorage, RecordStorage};
use ghinvite_e2e::{Call, Harness};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
#[derive(Default)]
struct Github {
    suspended: AtomicBool,
    unavailable: AtomicBool,
    roles: Mutex<BTreeMap<u64, (&'static str, &'static str)>>,
    mints: Mutex<Vec<Value>>,
}
async fn github(State(state): State<Arc<Github>>, req: Request) -> Response {
    let path = req.uri().path().to_owned();
    if path.starts_with("/app/installations/") {
        let id: u64 = path.split('/').nth(3).unwrap().parse().unwrap();
        let org = id == 2;
        if path.ends_with("/access_tokens") {
            let body: Value =
                serde_json::from_slice(&to_bytes(req.into_body(), 10000).await.unwrap()).unwrap();
            if org {
                assert!(body["permissions"].get("administration").is_none());
                assert!(matches!(
                    body["permissions"]["members"].as_str(),
                    Some("read" | "write")
                ));
            }
            state.mints.lock().unwrap().push(body);
            return (axum::http::StatusCode::CREATED, axum::Json(json!({"token":format!("fixture-{id}"), "expires_at":chrono::Utc::now()+chrono::Duration::hours(1)}))).into_response();
        }
        return axum::Json(json!({"id":id,"app_id":if org {456}else{123},
            "account":{"id":100,"login":"acme","type":"Organization"},
            "permissions":if org {json!({"members":"write","metadata":"read"})}else{json!({"administration":"write","metadata":"read"})},
            "repository_selection":"all","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z",
            "suspended_at": if org && state.suspended.load(Ordering::SeqCst) {json!("2026-01-02T00:00:00Z")}else{Value::Null}})).into_response();
    }
    if path == "/installation/repositories" {
        return axum::Json(json!({"total_count":1,"repositories":[{"id":10,"full_name":"acme/api","owner":{"id":100,"login":"acme"}}]})).into_response();
    }
    if path == "/orgs/acme" {
        if state.unavailable.load(Ordering::SeqCst) {
            return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        return axum::Json(json!({"id":100,"login":"acme","created_at":"2020-01-01T00:00:00Z"}))
            .into_response();
    }
    if let Some(id) = path
        .strip_prefix("/user/")
        .and_then(|s| s.parse::<u64>().ok())
    {
        return axum::Json(json!({"id":id,"login":format!("user-{id}")})).into_response();
    }
    if let Some(id) = path
        .strip_prefix("/orgs/acme/memberships/user-")
        .and_then(|s| s.parse::<u64>().ok())
    {
        let roles = state.roles.lock().unwrap();
        return match roles.get(&id) {
            Some((role, membership)) => axum::Json(json!({"role":role,"state":membership,"user":{"id":id,"login":format!("user-{id}")},"organization":{"id":100,"login":"acme"}})).into_response(),
            None => axum::http::StatusCode::NOT_FOUND.into_response(),
        };
    }
    panic!("unexpected fixture endpoint {path}")
}
async fn call(
    h: &Harness,
    service: &str,
    key: &str,
    method: &str,
    body: Option<&Value>,
    status: u16,
) -> Value {
    let r = h
        .restate
        .invoke(&Call::object(service, key, method), body, None)
        .await;
    assert_eq!(r.status, status, "{service}/{key}/{method}: {}", r.body);
    r.body
}
async fn decision(h: &Harness, actor: u64) -> Value {
    let request = ghinvite_core::RequestId::new();
    let link = ghinvite_core::InvitationLinkId::new();
    let now = chrono::Utc::now();
    let input = json!({"request":{"request_id":request,"link_id":link,"account_id":100,"requester_id":9,"justification":null,"state":"pending","admitted_at":now,"decision_deadline":now+chrono::Duration::days(7),"revision":1},"installation_id":1,"targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/api","grant":"write"}]},"approval_required":true});
    call(
        h,
        "InvitationRequest",
        &request.to_string(),
        "initialize",
        Some(&input),
        200,
    )
    .await;
    json!({"request_id":request,"link_id":link,"operation_id":ghinvite_core::RequestId::new(),"admin":{"account_id":100,"user_id":actor},"action":{"kind":"decline","reason":null}})
}
async fn decide(h: &Harness, input: &Value, status: u16) -> Value {
    call(
        h,
        "InvitationRequest",
        input["request_id"].as_str().unwrap(),
        "decide",
        Some(input),
        status,
    )
    .await
}
fn delegation(actor: u64, on: bool) -> Value {
    json!({"operation_id":ghinvite_core::RequestId::new(),"actor_user_id":actor,"on":on})
}
#[tokio::test]
async fn org_owner_supersedes_installer_with_one_settings_sequence_and_exact_receipts() {
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
    let state = ghinvite_workflows::AppState::new(storage.clone()).with_github_apps(Arc::new(
        ghinvite_workflows::RepositoryGithub::new(
            github_transport,
            &h.github_url,
            ghinvite_e2e::installed_apps(&h.github_url, true),
        )
        .unwrap(),
    ));
    h.restate
        .deploy(
            ghinvite_workflows::request_owner::bind(ghinvite_workflows::projection::bind(
                restate_sdk::endpoint::Endpoint::builder()
                    .bind(ghinvite_workflows::sweep_schedule::SweepSchedule::default()),
                storage.clone(),
            ))
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
    ghinvite_e2e::journal::JournalAudit::retain(&h, &["AccountInstallation", "InvitationRequest"])
        .await;
    let repo = ghinvite_e2e::setup_command(
        json!({"installation_id":1,"actor_user_id":7,"account_id":100,"account_login":"acme","account_type":"Organization","selected_repos":[],"installed_at":"2026-01-01T00:00:00Z"}),
    );
    let mut org = repo.clone();
    org["operation_id"] = json!(ghinvite_core::RequestId::new());
    org["verified"]["app"] = json!("org");
    org["verified"]["account"]["installation_id"] = json!(2);
    org["verified"]["permissions"] = json!({"members":"write","metadata":"read"});
    // Org-only accounts still write through the canonical repo:<account> owner.
    call(&h, "Installation", "org:2", "onboard", Some(&org), 200).await;
    let enable = delegation(8, true);
    assert_eq!(
        call(
            &h,
            "AccountInstallation",
            "repo:100",
            "set_delegation",
            Some(&enable),
            200
        )
        .await,
        json!({"Ok":null})
    );
    assert_eq!(
        call(
            &h,
            "AccountInstallation",
            "org:100",
            "set_delegation",
            Some(&enable),
            200
        )
        .await,
        json!({"Err":"org_app_required"})
    );
    call(&h, "Installation", "repo:1", "onboard", Some(&repo), 200).await;
    let event = json!({"delivery_id":"a".repeat(64),"action":"created","verified":repo["verified"],"sender_id":7,"sender_login":"user-7","updated_at":"2026-01-01T01:00:00Z"});
    call(
        &h,
        "Installation",
        "repo:1",
        "onboard_webhook",
        Some(&event),
        200,
    )
    .await;
    let disable = delegation(8, false);
    assert_eq!(
        call(
            &h,
            "AccountInstallation",
            "repo:100",
            "set_delegation",
            Some(&disable),
            200
        )
        .await,
        json!({"Ok":null})
    );
    for actor in [7, 9] {
        assert_eq!(
            call(
                &h,
                "AccountInstallation",
                "repo:100",
                "set_delegation",
                Some(&delegation(actor, true)),
                200
            )
            .await,
            json!({"Err":"not_owner"})
        );
        let input = decision(&h, actor).await;
        assert_eq!(decide(&h, &input, 200).await["outcome"], "not_authorized");
    }
    let owner_decision = decision(&h, 8).await;
    assert_eq!(decide(&h, &owner_decision, 200).await["outcome"], "applied");
    // Replay is decided before current authorization, without rewriting settings.
    assert_eq!(
        call(
            &h,
            "AccountInstallation",
            "repo:100",
            "set_delegation",
            Some(&enable),
            200
        )
        .await,
        json!({"Ok":null})
    );
    let mut conflict = enable.clone();
    conflict["on"] = json!(false);
    call(
        &h,
        "AccountInstallation",
        "repo:100",
        "set_delegation",
        Some(&conflict),
        409,
    )
    .await;
    let on = delegation(8, true);
    assert_eq!(
        call(
            &h,
            "AccountInstallation",
            "repo:100",
            "set_delegation",
            Some(&on),
            200
        )
        .await,
        json!({"Ok":null})
    );
    assert_eq!(
        decide(&h, &decision(&h, 7).await, 200).await["outcome"],
        "applied"
    );
    assert_eq!(
        decide(&h, &decision(&h, 9).await, 200).await["outcome"],
        "not_authorized"
    );
    github.suspended.store(true, Ordering::SeqCst);
    let suspended = delegation(8, false);
    assert_eq!(
        call(
            &h,
            "AccountInstallation",
            "repo:100",
            "set_delegation",
            Some(&suspended),
            200
        )
        .await,
        json!({"Err":"not_owner"})
    );
    assert_eq!(
        decide(&h, &decision(&h, 8).await, 200).await["outcome"],
        "not_authorized"
    );
    github.suspended.store(false, Ordering::SeqCst);
    github.unavailable.store(true, Ordering::SeqCst);
    let retry = delegation(8, false);
    call(
        &h,
        "AccountInstallation",
        "repo:100",
        "set_delegation",
        Some(&retry),
        503,
    )
    .await;
    let retry_decision = decision(&h, 8).await;
    decide(&h, &retry_decision, 503).await;
    github.unavailable.store(false, Ordering::SeqCst);
    assert_eq!(
        call(
            &h,
            "AccountInstallation",
            "repo:100",
            "set_delegation",
            Some(&retry),
            200
        )
        .await,
        json!({"Ok":null})
    );
    assert_eq!(decide(&h, &retry_decision, 200).await["outcome"], "applied");
    // Concurrent distinct operations serialize at the same revision owner.
    let a = delegation(8, true);
    let b = delegation(8, false);
    let (a, b) = tokio::join!(
        call(
            &h,
            "AccountInstallation",
            "repo:100",
            "set_delegation",
            Some(&a),
            200
        ),
        call(
            &h,
            "AccountInstallation",
            "repo:100",
            "set_delegation",
            Some(&b),
            200
        )
    );
    assert_eq!(a, json!({"Ok":null}));
    assert_eq!(b, json!({"Ok":null}));
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if storage
                .account_settings(100)
                .await
                .unwrap()
                .is_some_and(|s| s.revision == 6)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        storage
            .account_settings(100)
            .await
            .unwrap()
            .unwrap()
            .revision,
        6
    );
    github.roles.lock().unwrap().insert(8, ("member", "active"));
    assert_eq!(
        call(
            &h,
            "AccountInstallation",
            "repo:100",
            "set_delegation",
            Some(&delegation(8, true)),
            200
        )
        .await,
        json!({"Err":"not_owner"})
    );
    assert_eq!(
        call(
            &h,
            "AccountInstallation",
            "repo:100",
            "set_delegation",
            Some(&enable),
            200
        )
        .await,
        json!({"Ok":null})
    );
    assert_eq!(decide(&h, &owner_decision, 200).await["outcome"], "applied");
    let mut actor_conflict = enable.clone();
    actor_conflict["actor_user_id"] = json!(7);
    call(
        &h,
        "AccountInstallation",
        "repo:100",
        "set_delegation",
        Some(&actor_conflict),
        409,
    )
    .await;
    assert!(!github.mints.lock().unwrap().is_empty());
}
fn github_handler() -> axum::routing::MethodRouter<Arc<Github>> {
    axum::routing::any(github)
}

#[path = "organization_authority/budget_policy.rs"]
mod budget_policy;
