//! Real Restate proof: setup identity, signed installer ordering and delegation.
#![cfg(feature = "integration")]
use axum::response::IntoResponse as _;
use ghinvite_e2e::{Call, Harness};
use serde_json::{Value, json};
use std::sync::Arc;

async fn call(
    h: &Harness,
    service: &str,
    key: &str,
    handler: &str,
    body: Option<&Value>,
    status: u16,
) -> Value {
    let result = h
        .restate
        .invoke(&Call::object(service, key, handler), body, None)
        .await;
    assert_eq!(result.status, status, "{handler}: {}", result.body);
    result.body
}
// Initialize through the actual request owner, then exercise direct ingress decide.
async fn pending_decision(h: &Harness, account: u64, actor: u64) -> Value {
    let request_id = ghinvite_core::RequestId::new();
    let link_id = ghinvite_core::InvitationLinkId::new();
    let now = chrono::Utc::now();
    let input = json!({"request":{"request_id":request_id,"link_id":link_id,"account_id":account,
        "requester_id":8,"justification":null,"state":"pending","admitted_at":now,
        "decision_deadline":now+chrono::Duration::days(7),"revision":1},
        "installation_id":1,"targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/api","grant":"write"}]},"approval_required":true});
    call(
        h,
        "InvitationRequest",
        &request_id.to_string(),
        "initialize",
        Some(&input),
        200,
    )
    .await;
    json!({"request_id":request_id,"link_id":link_id,"operation_id":ghinvite_core::RequestId::new(),
        "admin":{"account_id":account,"user_id":actor},"action":{"kind":"decline","reason":null}})
}
async fn decide(h: &Harness, input: &Value) -> Value {
    call(
        h,
        "InvitationRequest",
        input["request_id"].as_str().unwrap(),
        "decide",
        Some(input),
        200,
    )
    .await
}
/// How the GitHub fixture answers the app-authenticated read of
/// installation 1 (PD-8): as the stub does, 404, or one 502 and then 404.
const INSTALLED: u8 = 0;
const GONE: u8 = 1;
const GONE_AFTER_ONE_FAILURE: u8 = 2;
fn deletable_installation(answer: Arc<std::sync::atomic::AtomicU8>) -> axum::Router {
    use std::sync::atomic::Ordering::SeqCst;
    ghinvite_github::stub::router().layer(axum::middleware::from_fn(
        move |request: axum::extract::Request, next: axum::middleware::Next| {
            let answer = answer.clone();
            async move {
                if request.method() == axum::http::Method::GET
                    && request.uri().path() == "/app/installations/1"
                {
                    match answer.load(SeqCst) {
                        GONE => return axum::http::StatusCode::NOT_FOUND.into_response(),
                        GONE_AFTER_ONE_FAILURE => {
                            answer.store(GONE, SeqCst);
                            return axum::http::StatusCode::BAD_GATEWAY.into_response();
                        }
                        _ => {}
                    }
                }
                next.run(request).await
            }
        },
    ))
}
#[tokio::test]
async fn setup_cannot_claim_installing_owner_and_signed_owner_changes_are_ordered() {
    let installation_one = Arc::new(std::sync::atomic::AtomicU8::new(INSTALLED));
    let h =
        Harness::start_with_github_router(deletable_installation(installation_one.clone())).await;
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    storage.run_migrations().await.unwrap();
    use ghinvite_core::storage::RecordStorage;
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
    let mut state = ghinvite_workflows::AppState::new(storage.clone()).with_github_apps(Arc::new(
        ghinvite_workflows::RepositoryGithub::new(
            github_transport,
            &h.github_url,
            ghinvite_e2e::repository_apps(&h.github_url),
        )
        .unwrap(),
    ));
    let receipt_now = chrono::Utc::now();
    let clock = Arc::new(std::sync::atomic::AtomicI64::new(
        receipt_now.timestamp_millis(),
    ));
    state.installation_clock = Some(clock.clone());
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
    ghinvite_e2e::journal::JournalAudit::retain(&h, &["InvitationRequest"]).await;
    let setup = ghinvite_e2e::setup_command(
        json!({"installation_id":1,"actor_user_id":8,"account_id":100,"account_login":"acme","account_type":"Organization","selected_repos":[],"installed_at":"2026-01-01T00:00:00Z"}),
    );
    call(&h, "Installation", "1", "onboard", Some(&setup), 400).await;
    call(&h, "Installation", "repo:1", "onboard", Some(&setup), 200).await;
    let initial = call(&h, "AccountInstallation", "repo:100", "status", None, 200).await;
    assert_eq!(initial["installation"]["installed_by_user_id"], Value::Null);
    assert_eq!(initial["repo_admin_delegation"], false);
    let missing = pending_decision(&h, 999, 7).await;
    assert_eq!(decide(&h, &missing).await["outcome"], "not_authorized");
    let unauthorized = pending_decision(&h, 100, 8).await;
    let refused = decide(&h, &unauthorized).await;
    assert_eq!(refused["outcome"], "not_authorized");
    assert_eq!(refused["request"]["state"], "pending");
    assert_eq!(refused["request"]["revision"], 1);
    assert!(refused["request"].get("decision").is_none());
    let delegate = |actor, on| json!({"operation_id":ghinvite_core::RequestId::new(),"actor_user_id":actor,"on":on});
    assert_eq!(
        call(
            &h,
            "AccountInstallation",
            "repo:100",
            "set_delegation",
            Some(&delegate(8, true)),
            200
        )
        .await,
        json!({"Err":"not_owner"})
    );
    let event = |marker: &str, sender: u64, updated: &str| json!({"delivery_id":marker.repeat(64),"action":"created","verified":setup["verified"],"sender_id":sender,"sender_login":format!("user-{sender}"),"updated_at":updated});
    let first = event("a", 7, "2026-01-01T01:00:00Z");
    call(
        &h,
        "Installation",
        "repo:1",
        "onboard_webhook",
        Some(&first),
        200,
    )
    .await;
    call(
        &h,
        "Installation",
        "repo:1",
        "onboard_webhook",
        Some(&first),
        200,
    )
    .await;
    let installer_decision = pending_decision(&h, 100, 7).await;
    let installer_receipt = decide(&h, &installer_decision).await;
    assert_eq!(installer_receipt["outcome"], "applied");
    let enable = delegate(7, true);
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
            "repo:100",
            "set_delegation",
            Some(&enable),
            200
        )
        .await,
        json!({"Ok":null})
    );
    assert_eq!(
        decide(&h, &unauthorized).await,
        refused,
        "refusal replay binds the old operation after delegation enables"
    );
    let mut fresh = unauthorized.clone();
    fresh["operation_id"] = json!(ghinvite_core::RequestId::new());
    let delegated_receipt = decide(&h, &fresh).await;
    assert_eq!(delegated_receipt["outcome"], "applied");
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
    let replacement = event("b", 9, "2026-01-01T02:00:00Z");
    call(
        &h,
        "Installation",
        "repo:1",
        "onboard_webhook",
        Some(&replacement),
        200,
    )
    .await;
    call(
        &h,
        "Installation",
        "repo:1",
        "onboard_webhook",
        Some(&event("c", 8, "2026-01-01T01:30:00Z")),
        200,
    )
    .await;
    call(
        &h,
        "Installation",
        "repo:1",
        "onboard_webhook",
        Some(&event("d", 8, "2026-01-01T02:00:00Z")),
        409,
    )
    .await;
    call(&h, "Installation", "repo:1", "onboard", Some(&setup), 200).await;
    let state = call(&h, "AccountInstallation", "repo:100", "status", None, 200).await;
    assert_eq!(state["installation"]["installed_by_user_id"], 9);
    assert_eq!(state["repo_admin_delegation"], true);
    assert_eq!(
        call(
            &h,
            "AccountInstallation",
            "repo:100",
            "set_delegation",
            Some(&delegate(7, false)),
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
            Some(&delegate(9, false)),
            200
        )
        .await,
        json!({"Ok":null})
    );
    assert_eq!(
        decide(&h, &fresh).await,
        delegated_receipt,
        "success replay survives delegation withdrawal"
    );
    assert_eq!(
        decide(&h, &installer_decision).await,
        installer_receipt,
        "original owner success survives owner change"
    );
    let old_owner = pending_decision(&h, 100, 7).await;
    assert_eq!(decide(&h, &old_owner).await["outcome"], "not_authorized");
    let new_owner = pending_decision(&h, 100, 9).await;
    assert_eq!(decide(&h, &new_owner).await["outcome"], "applied");
    call(
        &h,
        "Installation",
        "repo:1",
        "recheck_permissions",
        None,
        200,
    )
    .await;
    use ghinvite_core::storage::AppInstallationReadStorage;
    let stored = storage
        .app_installation(ghinvite_core::account::App::Repo, 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.installed_by_user_id, Some(9));
    assert!(stored.permissions.contains_key("administration"));
    let other = call(&h, "AccountInstallation", "org:100", "status", None, 200).await;
    assert_eq!(other["installation"], Value::Null);
    // Exercise seven-day cleanup with the production journaled clock seam.
    // The actual callback is invoked after advancing the injected reading;
    // production schedules the same callback through a durable seven-day send.
    let receipt_key = format!("webhook/{}", "a".repeat(64));
    let state_keys = || async {
        h.client.post(format!("{}/query",h.restate.admin_url())).header("accept","application/json").json(&json!({"query":"SELECT key FROM state WHERE service_name='AccountInstallation' AND service_key='repo:100'"})).send().await.unwrap().json::<Value>().await.unwrap()
    };
    assert!(
        state_keys().await["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["key"] == receipt_key)
    );
    clock.store(
        (receipt_now + chrono::Duration::days(7)).timestamp_millis(),
        std::sync::atomic::Ordering::SeqCst,
    );
    let expiry = json!({"payload_sha256":"a".repeat(64),"expires_at":chrono::DateTime::from_timestamp_millis(receipt_now.timestamp_millis()).unwrap()+chrono::Duration::days(7)});
    call(
        &h,
        "AccountInstallation",
        "repo:100",
        "expire_webhook",
        Some(&expiry),
        200,
    )
    .await;
    assert!(
        !state_keys().await["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["key"] == receipt_key)
    );
    call(
        &h,
        "Installation",
        "repo:1",
        "onboard_webhook",
        Some(&first),
        200,
    )
    .await;
    let after_expired_replay =
        call(&h, "AccountInstallation", "repo:100", "status", None, 200).await;
    assert_eq!(
        after_expired_replay["installation"]["installed_by_user_id"],
        9
    );
    // An older cleanup callback cannot erase the renewed receipt.
    call(
        &h,
        "AccountInstallation",
        "repo:100",
        "expire_webhook",
        Some(&expiry),
        200,
    )
    .await;
    assert!(
        state_keys().await["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["key"] == receipt_key)
    );
    // PD-8: GitHub still reports installation 1, so a signed deletion
    // retires nothing; the delivery is retained as handled.
    let mut unconfirmed = event("9", 9, "2026-01-01T03:00:00Z");
    unconfirmed["action"] = json!("deleted");
    call(
        &h,
        "Installation",
        "repo:1",
        "onboard_webhook",
        Some(&unconfirmed),
        200,
    )
    .await;
    let live = call(&h, "AccountInstallation", "repo:100", "status", None, 200).await;
    assert_eq!(live["account"]["installation_id"], 1);
    assert_eq!(
        live["installation"]["account"]["uninstalled_at"],
        Value::Null
    );
    assert!(
        state_keys().await["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["key"] == format!("webhook/{}", "9".repeat(64)))
    );
    let installation_keys = h.client.post(format!("{}/query",h.restate.admin_url())).header("accept","application/json").json(&json!({"query":"SELECT key FROM state WHERE service_name='Installation' AND service_key='repo:1'"})).send().await.unwrap().json::<Value>().await.unwrap();
    assert!(
        installation_keys["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["key"] != "uninstalled"),
        "an unconfirmed deletion leaves the installation owner live"
    );
    // GitHub's 404 confirms the next deletion; a transient failure first is
    // retried rather than answered.
    installation_one.store(GONE_AFTER_ONE_FAILURE, std::sync::atomic::Ordering::SeqCst);
    let mut deleted = event("e", 9, "2026-01-01T03:00:00Z");
    deleted["action"] = json!("deleted");
    call(
        &h,
        "Installation",
        "repo:1",
        "onboard_webhook",
        Some(&deleted),
        200,
    )
    .await;
    call(
        &h,
        "Installation",
        "repo:1",
        "onboard_webhook",
        Some(&event("f", 7, "2026-01-01T04:00:00Z")),
        200,
    )
    .await;
    assert_eq!(
        installation_one.load(std::sync::atomic::Ordering::SeqCst),
        GONE,
        "the transient failure was served and retried"
    );
    let final_state = call(&h, "AccountInstallation", "repo:100", "status", None, 200).await;
    assert_eq!(final_state["account"], Value::Null);
    assert_eq!(final_state["installation"]["installed_by_user_id"], 9);
    let removed = pending_decision(&h, 100, 9).await;
    assert_eq!(decide(&h, &removed).await["outcome"], "not_authorized");
    let mut reinstalled = setup.clone();
    reinstalled["operation_id"] = json!(ghinvite_core::RequestId::new());
    reinstalled["verified"]["account"]["installation_id"] = json!(2);
    call(
        &h,
        "Installation",
        "repo:2",
        "onboard",
        Some(&reinstalled),
        200,
    )
    .await;
    let after_setup = pending_decision(&h, 100, 9).await;
    assert_eq!(
        decide(&h, &after_setup).await["outcome"],
        "not_authorized",
        "reinstall cannot inherit the former installer"
    );
    let mut new_signed = event("1", 8, "2026-01-02T00:00:00Z");
    new_signed["verified"] = reinstalled["verified"].clone();
    call(
        &h,
        "Installation",
        "repo:2",
        "onboard_webhook",
        Some(&new_signed),
        200,
    )
    .await;
    // Explicit current-handler named-run table: fresh refusal/replay has no run;
    // an authorized fresh decline journals the time and decision calculation.
    // This covers decide only, not every Request handler or cross-release replay.
    let replacement_owner = pending_decision(&h, 100, 8).await;
    let prior_owner = pending_decision(&h, 100, 9).await;
    let mut invocations = vec![];
    let mut journals = std::collections::BTreeMap::new();
    for (input, expected) in [
        (&replacement_owner, "applied"),
        (&prior_owner, "not_authorized"),
        (&replacement_owner, "applied"),
    ] {
        let reply = h
            .restate
            .invoke(
                &Call::object(
                    "InvitationRequest",
                    input["request_id"].as_str().unwrap(),
                    "decide",
                ),
                Some(input),
                None,
            )
            .await;
        assert_eq!(reply.status, 200);
        assert_eq!(reply.body["outcome"], expected);
        let invocation_id = reply.invocation_id().to_owned();
        let journal = h.restate.admin().journal(&invocation_id).await;
        assert!(!journal.is_empty(), "missing retained decision journal");
        invocations.push((
            invocation_id.clone(),
            h.restate.admin().invocation(&invocation_id).await,
        ));
        journals.insert(invocation_id, journal);
    }
    let handlers = h
        .restate
        .admin()
        .handlers()
        .await
        .into_iter()
        .filter(|v| v.service == "InvitationRequest" && v.name == "decide")
        .collect::<Vec<_>>();
    const DECIDE_PATHS: &[ghinvite_e2e::RunPath] = &[
        ghinvite_e2e::RunPath::new("InvitationRequest", "decide", &[]),
        ghinvite_e2e::RunPath::new(
            "InvitationRequest",
            "decide",
            &["decision_time", "record_decision"],
        ),
    ];
    ghinvite_e2e::Table::new(DECIDE_PATHS)
        .check(&handlers, &invocations, &journals)
        .unwrap();
    h.finish().await;
}

#[path = "installation_authority/audit.rs"]
mod audit;
