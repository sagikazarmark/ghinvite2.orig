//! Public ingress + real SQL + GitHub HTTP boundary acceptance for #56.
#![cfg(feature = "integration")]
#[path = "common/web_apps.rs"]
mod web_apps;

use ghinvite_core::storage::{ConsoleStorage, RecordStorage};
use ghinvite_core::{RequestId, storage::Storage};
use ghinvite_e2e::github::Outcome;
use restate_sdk::endpoint::{Endpoint, HandleOptions, ProtocolMode};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

const MEMBER_WEBHOOK_SECRET: &[u8] = b"member-webhook-runtime-fixture";

/// Own SDK tasks so the acceptance test can interrupt actual endpoint execution
/// after a journal boundary, then let Restate replay the original invocation.
async fn serve(
    axum::extract::State((endpoint, tasks)): axum::extract::State<(
        Endpoint,
        Arc<std::sync::Mutex<Vec<tokio::task::AbortHandle>>>,
    )>,
    request: axum::extract::Request,
) -> axum::response::Response {
    use axum::body::{Body, to_bytes};
    let (parts, body) = request.into_parts();
    let bytes = to_bytes(body, 1024 * 1024).await.unwrap();
    let task = tokio::spawn(async move {
        let response = endpoint.handle_with_options(
            axum::extract::Request::from_parts(parts, Body::from(bytes)),
            HandleOptions {
                protocol_mode: ProtocolMode::RequestResponse,
            },
        );
        let (parts, body) = response.into_parts();
        let bytes = to_bytes(Body::new(body), 1024 * 1024).await.unwrap();
        axum::response::Response::from_parts(parts, Body::from(bytes))
    });
    tasks.lock().unwrap().push(task.abort_handle());
    task.await.unwrap_or_else(|_| {
        axum::response::Response::builder()
            .status(503)
            .body(Body::from("fixture: endpoint interrupted"))
            .unwrap()
    })
}

async fn send_signed_member(
    client: &reqwest::Client,
    web_url: &str,
    delivery: &str,
    payload: &Value,
) -> reqwest::Response {
    use hmac::{Hmac, Mac};
    let body = payload.to_string();
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(MEMBER_WEBHOOK_SECRET).unwrap();
    mac.update(body.as_bytes());
    let digest: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    client
        .post(format!("{web_url}/webhooks/repo"))
        .header("content-type", "application/json")
        .header("x-github-event", "member")
        .header("x-github-delivery", delivery)
        .header("x-hub-signature-256", format!("sha256={digest}"))
        .body(body)
        .send()
        .await
        .unwrap()
}

/// Real web route and Restate adapter, with an HTTP proxy that loses one send ack.
async fn observation_ingress_with_webhook_guard(
    storage: Arc<ghinvite_storage_sqlx::SqlxStorage>,
    ingress: &str,
    client: reqwest::Client,
) -> (
    String,
    String,
    Arc<std::sync::Mutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
    tokio::task::JoinHandle<()>,
) {
    use axum::{
        body::Body,
        extract::Json,
        response::{IntoResponse, Response},
        routing::post,
    };
    let invocations = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let recorded = invocations.clone();
    let ingress = ingress.to_owned();
    let proxy = axum::Router::new().route(
        "/DeliveryObservation/observe/send",
        post(move |Json(input): Json<Value>| {
            let client = client.clone();
            let ingress = ingress.clone();
            let recorded = recorded.clone();
            async move {
                let response = client
                    .post(format!("{ingress}/DeliveryObservation/observe/send"))
                    .json(&input)
                    .send()
                    .await
                    .unwrap();
                assert!(response.status().is_success());
                let ack: Value = response.json().await.unwrap();
                let lose_ack = {
                    let mut recorded = recorded.lock().unwrap();
                    recorded.push(ack["invocationId"].as_str().unwrap().to_owned());
                    recorded.len() == 1
                };
                if lose_ack {
                    Response::builder().status(502).body(Body::empty()).unwrap()
                } else {
                    Json(ack).into_response()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_url = format!("http://{}", listener.local_addr().unwrap());
    let proxy_server = tokio::spawn(async move {
        axum::serve(listener, proxy).await.unwrap();
    });
    let restate = Arc::new(ghinvite_web::RestateClient::new(proxy_url.clone()).unwrap());
    // Webhooks only: GitHub is never called.
    let state = ghinvite_web::AppState::new(
        storage,
        ghinvite_web::WebGithub::new(
            Arc::new(ghinvite_github::mocks::MockTransport::scripted(vec![])),
            &Default::default(),
            vec![],
        )
        .unwrap(),
        restate,
        ghinvite_web::WebConfig {
            webhooks: Some(web_apps::webhooks(
                std::str::from_utf8(MEMBER_WEBHOOK_SECRET).unwrap(),
            )),
            ..ghinvite_web::WebConfig::for_local_dev_with_secret([7; 32])
        },
    );
    let app = ghinvite_web::routes::webhook::router().with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let web_url = format!("http://{}", listener.local_addr().unwrap());
    let web_server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (web_url, proxy_url, invocations, web_server, proxy_server)
}

#[tokio::test]
async fn retained_create_survives_sent_replay_and_conflicts() {
    let harness =
        ghinvite_e2e::Harness::start_with_emulator(ghinvite_e2e::github_emulator_for(9).await)
            .await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap();
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let now = chrono::Utc::now();
    for (user_id, login) in [(7, "creator"), (8, "alice")] {
        storage
            .upsert_user(&ghinvite_core::User {
                user_id,
                login: login.into(),
                avatar_url: None,
                github_created_at: None,
                last_seen_at: now,
            })
            .await
            .unwrap();
    }
    // GitHub: the suites' emulator world, the repository App installed as
    // 9. Fault rules inject what GitHub would not answer on its own:
    // unavailable credentials, a reassigned repository and a throttled
    // observation.
    let emulator = harness.emulator();
    let github = harness.github();
    let base = harness.github_url.clone();
    use octoemu::faults::{Fault, FaultAction, Reply};
    let credentials_unavailable = || {
        emulator
            .add_fault(
                Fault::new("/**", FaultAction::respond(401))
                    .credential(octoemu::calls::CredentialKind::InstallationToken),
            )
            .unwrap()
    };
    let apps = Arc::new(
        ghinvite_workflows::RepositoryGithub::new(
            ghinvite_e2e::github_transport(client.clone()),
            &base,
            ghinvite_e2e::repository_apps(&base),
        )
        .unwrap(),
    );
    let state = ghinvite_workflows::AppState::new(storage.clone()).with_github_apps(apps);
    let builder = ghinvite_workflows::admission::bind_protocol_fixture(Endpoint::builder());
    let builder = ghinvite_workflows::projection::bind(builder, storage.clone());
    let workflow_faults = Arc::new(ghinvite_workflows::request_owner::Faults::default());
    let builder =
        ghinvite_workflows::request_owner::bind_with_faults(builder, workflow_faults.clone());
    let faults = Arc::new(ghinvite_workflows::delivery::DeliveryFaults::default());
    faults
        .lose_http_result
        .store(true, std::sync::atomic::Ordering::SeqCst);
    faults
        .lose_projection_ack
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let builder = builder
        .bind(ghinvite_workflows::installation::Installation {
            state: state.clone(),
        })
        .bind(ghinvite_workflows::availability::AccountInstallation {
            state: state.clone(),
        })
        .bind(ghinvite_workflows::availability::InstallationProjection {
            state: state.clone(),
        })
        .bind(ghinvite_workflows::reconcile::Reconcile {
            state: state.clone(),
        });
    let endpoint =
        ghinvite_workflows::delivery::bind_with_faults(builder, state, faults.clone()).build();
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let tasks = Arc::new(std::sync::Mutex::new(Vec::new()));
    let app = axum::Router::new()
        .fallback(serve)
        .with_state((endpoint, tasks.clone()));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let admin = harness.restate.admin_url().to_owned();
    let ingress = harness.restate.ingress_url().to_owned();
    let host = std::env::var("RESTATE_ENDPOINT_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    for _ in 0..100 {
        if client.get(format!("{admin}/health")).send().await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let response = client
        .post(format!("{admin}/deployments"))
        .json(&json!({"uri": format!("http://{host}:{port}")}))
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    let link = ghinvite_core::InvitationLinkId::new();
    let call = |service: &str, key: String, handler: &str, input: Value| {
        let key = if matches!(service, "Installation" | "AccountInstallation") && !key.contains(':')
        {
            format!("repo:{key}")
        } else {
            key
        };
        client
            .post(format!("{ingress}/{service}/{key}/{handler}"))
            .json(&input)
    };
    let creation = json!({"max_uses":1000,"secret_sha256":"2a".repeat(32),"link_id":link,"account_id":100,"installation_id":9,"admin":{"account_id":100,"user_id":7},
        "description":"Delivery test","approval_required":false,"targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/api","grant":"write"}]}});
    let setup = ghinvite_e2e::setup_command(
        json!({"app":"repo","installation_id":9,"actor_user_id":7,"account_id":100,"account_login":"acme","account_type":"Organization","selected_repos":"all","installed_at":now}),
    );
    let response = call("Installation", "9".into(), "onboard", setup.clone())
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    call(
        "Installation",
        "9".into(),
        "onboard_webhook",
        ghinvite_e2e::signed_owner_command(&setup, 7),
    )
    .send()
    .await
    .unwrap()
    .error_for_status()
    .unwrap();
    let response = call("InvitationLink", link.to_string(), "create", creation)
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    let receipt: Value = call(
        "InvitationLink",
        link.to_string(),
        "admit",
        json!({"scope_evidence":[],"secret_sha256":"2a".repeat(32),"secret_generation":1,"link_id":link,"requester_id":8,"operation_id":RequestId::new()}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let request = receipt["result"]["request_id"].as_str().unwrap();
    let query = json!({"link_id":link,"request_id":request,"requester_id":8});
    let plan: Value = call(
        "InvitationRequest",
        request.to_owned(),
        "approved_plan",
        query,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let command = plan["commands"][0].clone();
    let id = delivery_key(&command);
    let completed = client
        .post(format!("{ingress}/InvitationRequest/{request}/dispatch"))
        .send()
        .await
        .unwrap();
    assert!(completed.status().is_success());
    let checkpoint: Value = call(
        "InvitationRequest",
        request.to_owned(),
        "delivery_status",
        json!({"link_id":link,"request_id":request,"requester_id":8}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert!(
        checkpoint["submitted"][0]["invocation_id"]
            .as_str()
            .is_some(),
        "workflow must durably submit: {checkpoint}"
    );
    let response = call("RepositoryDelivery", id.clone(), "create", command.clone())
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    let result: Value = response.json().await.unwrap();
    assert_eq!(result["outcome"]["kind"], "created");
    let original_event = assert_create_audit(&*storage, &result).await.unwrap();
    let replay: Value = call("RepositoryDelivery", id.clone(), "create", command.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(replay, result);
    assert_eq!(
        faults
            .http_result_losses
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    let mut changed = command.clone();
    changed["permission"] = json!("admin");
    assert_eq!(
        call("RepositoryDelivery", id, "create", changed)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    assert_eq!(github.writes(), 1);
    for (outcome, expected) in [
        (Outcome::AlreadyCollaborator, "upgraded"),
        (Outcome::TerminalFailure, "failed"),
        (Outcome::TransientOnce, "outcome_unknown"),
        (Outcome::CreatedResponseLost, "outcome_unknown"),
        (Outcome::Unanswered, "outcome_unknown"),
    ] {
        github.reset().await;
        github.outcome("acme/api", "alice", outcome).await;
        let link = ghinvite_core::InvitationLinkId::new();
        call("InvitationLink",link.to_string(),"create",json!({"max_uses":1000,"secret_sha256":"2a".repeat(32),"link_id":link,"account_id":100,"installation_id":9,"admin":{"account_id":100,"user_id":7},
            "description":"Other result","approval_required":false,"targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/api","grant":"write"}]}})).send().await.unwrap();
        let admitted: Value = call(
            "InvitationLink",
            link.to_string(),
            "admit",
            json!({"scope_evidence":[],"secret_sha256":"2a".repeat(32),"secret_generation":1,"link_id":link,"requester_id":8,"operation_id":RequestId::new()}),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        let request = admitted["result"]["request_id"].as_str().unwrap();
        let plan: Value = call(
            "InvitationRequest",
            request.to_owned(),
            "approved_plan",
            json!({"link_id":link,"request_id":request,"requester_id":8}),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        let command = plan["commands"][0].clone();
        let id = delivery_key(&command);
        let result: Value = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let snapshot: Value = client
                    .post(format!(
                        "{ingress}/RepositoryDelivery/{request}:10:1/status"
                    ))
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                if !snapshot.is_null() {
                    break snapshot["create"].clone();
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(result["outcome"]["kind"], expected);
        // A pending snapshot can become confirmed before its audit is queried.
        // Assert the audit against the replay below, after the owner has had the
        // opportunity to recover a lost response from positive GitHub evidence.
        let replay: Value = call("RepositoryDelivery", id, "create", command)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if outcome == Outcome::CreatedResponseLost {
            assert_eq!(replay["outcome"]["kind"], "created");
        } else {
            assert_eq!(replay["outcome"], result["outcome"]);
        }
        assert_eq!(github.writes(), 1, "{outcome:?}");
        assert_create_audit(&*storage, &replay).await;
        let rows = storage
            .list_delivery_for_request(request.parse().unwrap())
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(&rows[0].create).unwrap()["outcome"],
            replay["outcome"]
        );
    }
    tokio::time::timeout(Duration::from_secs(15), async { loop {
        let response: Value = client.post(format!("{admin}/query")).header("accept","application/json")
            .json(&json!({"query":format!("SELECT id FROM sys_invocation WHERE target_service_name = 'InvitationRequest' AND target_service_key = '{request}'")}))
            .send().await.unwrap().json().await.unwrap();
        if response["rows"].as_array().unwrap().is_empty() { break; }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }}).await.expect("actual workflow retention cleanup");
    let response = client
        .post(format!("{ingress}/DeliveryRecovery/recover"))
        .json(&json!({"link_id":link,"request_id":request,"requester_id":8}))
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    let outcomes = storage
        .list_delivery_for_request(request.parse().unwrap())
        .await
        .unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(serde_json::to_value(&outcomes[0].create).unwrap(), result);
    // The independent lifecycle can decline without resetting create authority.
    let response = call(
        "RepositoryDelivery",
        format!("{request}:10:1"),
        "reconcile",
        json!({"delivery_key":delivery_key(&command),"expected_revision":outcomes[0].revision,"at":chrono::Utc::now(),"evidence":{"kind":"absent","availability_verified":true,"role_name":"none","direct_collaborator":null,"membership":null}}),
    )
    .send()
    .await
    .unwrap();
    assert!(response.status().is_success());
    let replay: Value = call(
        "RepositoryDelivery",
        delivery_key(&command),
        "create",
        command.clone(),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(replay, result);
    assert_eq!(
        assert_create_audit(&*storage, &replay).await.unwrap(),
        original_event
    );
    assert_eq!(
        storage
            .get_delivery(&delivery_key(&command))
            .await
            .unwrap()
            .unwrap()
            .settlement
            .unwrap()
            .state,
        ghinvite_core::InvitationState::Declined
    );
    assert_eq!(github.writes(), 1);
    // Partial fan-out: a durable first send stalls before its link checkpoint;
    // link commands remain available and explicit recovery uses the same plan.
    github.reset().await;
    workflow_faults
        .pause_after_send
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let partial_link = ghinvite_core::InvitationLinkId::new();
    call("InvitationLink",partial_link.to_string(),"create",json!({"max_uses":1000,"secret_sha256":"2a".repeat(32),"link_id":partial_link,"account_id":100,"installation_id":9,"admin":{"account_id":100,"user_id":7},
        "description":"Partial fanout","approval_required":false,"targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/api","grant":"write"},{"repo_id":11,"repo_full_name":"acme/web","grant":"write"}]}})).send().await.unwrap();
    let admitted: Value = call(
        "InvitationLink",
        partial_link.to_string(),
        "admit",
        json!({"scope_evidence":[],"secret_sha256":"2a".repeat(32),"secret_generation":1,"link_id":partial_link,"requester_id":8,"operation_id":RequestId::new()}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let partial_request = admitted["result"]["request_id"].as_str().unwrap();
    let query = json!({"link_id":partial_link,"request_id":partial_request,"requester_id":8});
    tokio::time::timeout(Duration::from_secs(10), async {
        while workflow_faults
            .sent_before_pause
            .load(std::sync::atomic::Ordering::SeqCst)
            == 0
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let partial_plan: Value = call(
        "InvitationRequest",
        partial_request.to_owned(),
        "approved_plan",
        query.clone(),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let first = partial_plan["commands"][0].clone();
    call(
        "RepositoryDelivery",
        delivery_key(&first),
        "create",
        first.clone(),
    )
    .send()
    .await
    .unwrap();
    let revocation = json!({"link_id":partial_link,"admin":{"account_id":100,"user_id":7},
        "operation_id":RequestId::new(),"cascade":{"decline_pending_requests":false,
        "cancel_pending_invitations":false,"remove_granted_access":false}});
    for _ in 0..2 {
        let revoked: Value = call(
            "InvitationLink",
            partial_link.to_string(),
            "revoke",
            revocation.clone(),
        )
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
        assert_eq!(
            revoked,
            json!({"Ok":{"addressed_requests":0,"addressed_deliveries":0}})
        );
    }
    let checkpoint: Value = call(
        "InvitationRequest",
        partial_request.to_owned(),
        "delivery_status",
        query.clone(),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(checkpoint["submitted"], json!([]));
    github.select(9, &[10]).await;
    assert!(
        call(
            "AccountInstallation",
            "repo:100".into(),
            "refresh",
            json!(9)
        )
        .send()
        .await
        .unwrap()
        .status()
        .is_success()
    );
    workflow_faults
        .pause_after_send
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let recovered = client
        .post(format!("{ingress}/DeliveryRecovery/recover"))
        .json(&query)
        .send()
        .await
        .unwrap();
    assert!(recovered.status().is_success());
    workflow_faults
        .pause_after_send
        .store(false, std::sync::atomic::Ordering::SeqCst);
    client
        .post(format!(
            "{ingress}/InvitationRequest/{partial_request}/dispatch"
        ))
        .send()
        .await
        .unwrap();
    for command in partial_plan["commands"].as_array().unwrap() {
        let response = call(
            "RepositoryDelivery",
            delivery_key(command),
            "create",
            command.clone(),
        )
        .send()
        .await
        .unwrap();
        assert!(response.status().is_success());
        let result: Value = response.json().await.unwrap();
        assert_eq!(
            result["outcome"]["kind"],
            if command["repo_id"] == 10 {
                "created"
            } else {
                "blocked"
            }
        );
    }
    github.select(9, &[10, 11]).await;
    assert!(
        call(
            "AccountInstallation",
            "repo:100".into(),
            "refresh",
            json!(9)
        )
        .send()
        .await
        .unwrap()
        .status()
        .is_success()
    );
    // alice renames, and her new login names another account (99) when it
    // is resolved, then her own again.
    github.rename("alice", "renamed").await;
    let recycled = github.recycled_login("renamed", 99);
    let second = partial_plan["commands"][1].clone();
    let mismatch: Value = call(
        "RepositoryDelivery",
        delivery_key(&second),
        "create",
        second.clone(),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(mismatch["outcome"]["kind"], "blocked");
    emulator.remove_fault(recycled);
    for command in partial_plan["commands"].as_array().unwrap() {
        let result: Value = call(
            "RepositoryDelivery",
            delivery_key(command),
            "create",
            command.clone(),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        assert_eq!(result["outcome"]["kind"], "created");
    }
    assert_eq!(github.writes(), 2);
    assert!(github.called("PUT", "/repos/acme/web/collaborators/renamed"));
    let checkpoint: Value = call(
        "InvitationRequest",
        partial_request.to_owned(),
        "delivery_status",
        query,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(checkpoint["submitted"].as_array().unwrap().len(), 2);
    assert_eq!(checkpoint["plan"], partial_plan);
    let progress: Value = call(
        "InvitationRequest",
        partial_request.to_owned(),
        "delivery_progress",
        json!({"link_id":partial_link,"request_id":partial_request,"requester_id":8}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(
        progress,
        json!([{"repo_id":10,"stage":"submitted"},{"repo_id":11,"stage":"submitted"}])
    );
    // A fresh GitHub side: renamed is alice again, without invitations or
    // access, and the installation is back on all of acme.
    github.reset().await;
    github.rename("renamed", "alice").await;
    github.select_all(9).await;
    // Explicit 403 proves rejection, so restoration may safely try again.
    emulator
        .seed(octoemu::seed::Seed::new().user(octoemu::seed::User::new(84, "user-84")))
        .unwrap();
    workflow_faults
        .pause_before_dispatch
        .store(true, std::sync::atomic::Ordering::SeqCst);
    github
        .outcome("acme/api", "user-84", Outcome::AccessLostOnce)
        .await;
    let rejected_link = ghinvite_core::InvitationLinkId::new();
    call("InvitationLink",rejected_link.to_string(),"create",json!({"max_uses":1000,"secret_sha256":"2a".repeat(32),"link_id":rejected_link,"account_id":100,"installation_id":9,"admin":{"account_id":100,"user_id":7},
        "description":"Access rejection","approval_required":false,"targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/api","grant":"write"}]}})).send().await.unwrap();
    storage
        .upsert_user(&ghinvite_core::User {
            user_id: 84,
            login: "user-84".into(),
            avatar_url: None,
            github_created_at: None,
            last_seen_at: now,
        })
        .await
        .unwrap();
    let admitted: Value = call(
        "InvitationLink",
        rejected_link.to_string(),
        "admit",
        json!({"scope_evidence":[],"secret_sha256":"2a".repeat(32),"secret_generation":1,"link_id":rejected_link,"requester_id":84,"operation_id":RequestId::new()}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let rejected_request = admitted["result"]["request_id"].as_str().unwrap();
    let query = json!({"link_id":rejected_link,"request_id":rejected_request,"requester_id":84});
    let progress: Value = call(
        "InvitationRequest",
        rejected_request.to_owned(),
        "delivery_progress",
        query.clone(),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(progress, json!([{"repo_id":10,"stage":"planned"}]));
    let plan: Value = call(
        "InvitationRequest",
        rejected_request.to_owned(),
        "approved_plan",
        query.clone(),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let progress: Value = call(
        "InvitationRequest",
        rejected_request.to_owned(),
        "delivery_progress",
        query,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(progress, json!([{"repo_id":10,"stage":"planned"}]));
    let command = plan["commands"][0].clone();
    let delivery_id = delivery_key(&command);
    let id = delivery_id.as_str();
    let rejected: Value = call("RepositoryDelivery", id.into(), "create", command.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(rejected["outcome"]["kind"], "blocked");
    assert_create_audit(&*storage, &rejected).await;
    // This sweep observes older Alice invitations; the blocked user-84
    // create is ineligible.
    run_sweep(&client, &ingress, &admin, &*storage).await;
    assert_eq!(
        storage
            .get_delivery(id)
            .await
            .unwrap()
            .unwrap()
            .create
            .outcome,
        ghinvite_core::delivery::CreateOutcome::Blocked {
            reason: ghinvite_core::delivery::BlockedReason::InstallationUnavailable
        }
    );
    let resumed: Value = call("RepositoryDelivery", id.into(), "create", command.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resumed["outcome"]["kind"], "created");
    assert_create_audit(&*storage, &resumed).await;
    let sent = storage.get_delivery(id).await.unwrap().unwrap();
    assert_eq!(upstream(&sent), resumed["outcome"]["upstream_id"].as_u64());
    // Retained delayed evidence arrives after read-only observer settlement. SQL audit
    // failure stalls only projection; the retained winner is already observable.
    storage.debug_set_audit_failure(true).await.unwrap();
    let (web_url, proxy_url, invocations, web_server, proxy_server) =
        observation_ingress_with_webhook_guard(storage.clone(), &ingress, client.clone()).await;
    let payload = json!({"action":"added","installation":{"id":9},
        "repository":{"id":10,"owner":{"id":100}},"member":{"id":84},"sender":{"id":7}});
    // Obsolete signed member events cannot dispatch effects.
    assert_eq!(
        send_signed_member(&client, &web_url, "member-1", &payload)
            .await
            .status(),
        202
    );
    assert!(invocations.lock().unwrap().is_empty());
    let before: Value = client
        .post(format!(
            "{ingress}/RepositoryDelivery/{}:10:1/status",
            command["request_id"].as_str().unwrap()
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // The proxy durably submits read-only observation, then loses its acknowledgement.
    let first = client
        .post(format!("{proxy_url}/DeliveryObservation/observe/send"))
        .json(&before)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 502);
    assert_eq!(invocations.lock().unwrap().len(), 1);
    // A matching projected candidate is not sufficient: its exact invitation
    // is still pending, so observation cannot accept it.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let pending: Value = client
        .post(format!(
            "{ingress}/RepositoryDelivery/{}:10:1/status",
            command["request_id"].as_str().unwrap()
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(pending["settlement"].is_null());
    // The invitee declines, and is a write collaborator anyway (added
    // directly).
    github.decline("user-84", upstream(&sent).unwrap()).await;
    github
        .collaborate("user-84", octoemu::seed::Role::Push)
        .await;
    emulator
        .add_fault(
            Fault::on(
                "GET",
                "/repos/acme/api/invitations",
                FaultAction::Respond(Reply::new(429).header("retry-after", "1").json(json!({}))),
            )
            .once(),
        )
        .unwrap();
    let retry = client
        .post(format!("{proxy_url}/DeliveryObservation/observe/send"))
        .json(&pending)
        .send()
        .await
        .unwrap();
    assert_eq!(retry.status(), 200);
    assert_eq!(invocations.lock().unwrap().len(), 2);
    let accepted_invocation = invocations.lock().unwrap()[1].clone();
    assert!(
        client
            .get(format!(
                "{ingress}/restate/invocation/{accepted_invocation}/attach"
            ))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let snapshot: Value = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let snapshot: Value = client
                .post(format!(
                    "{ingress}/RepositoryDelivery/{}:10:1/status",
                    command["request_id"].as_str().unwrap()
                ))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if snapshot["settlement"]["state"] == "accepted" {
                break snapshot;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(snapshot["settlement"]["state"], "accepted", "{snapshot}");
    assert_eq!(snapshot["create"], resumed);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(is_pending_delivery(
        &storage
            .get_delivery(&sent.create.command.delivery_key())
            .await
            .unwrap()
            .unwrap()
    ));
    storage.debug_set_audit_failure(false).await.unwrap();
    let invocation_id = invocations.lock().unwrap()[1].clone();
    let attached = client
        .get(format!(
            "{ingress}/restate/invocation/{invocation_id}/attach"
        ))
        .send()
        .await
        .unwrap();
    assert!(
        attached.status().is_success(),
        "{}",
        attached.text().await.unwrap()
    );
    let evidence = json!({
        "delivery_key":sent.create.command.delivery_key(),
        "expected_revision":resumed["revision"],"at":chrono::Utc::now(),
        "evidence":{"kind":"absent","availability_verified":true,"role_name":"none","direct_collaborator":null,"membership":null}
    });
    let (a, b, duplicate) = tokio::join!(
        call(
            "RepositoryDelivery",
            id.into(),
            "reconcile",
            evidence.clone()
        )
        .send(),
        call("RepositoryDelivery", id.into(), "reconcile", evidence).send(),
        send_signed_member(&client, &web_url, "member-2", &payload)
    );
    assert!(a.unwrap().status().is_success());
    assert!(b.unwrap().status().is_success());
    assert_eq!(duplicate.status(), 202);
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if storage
                .get_delivery(&sent.create.command.delivery_key())
                .await
                .unwrap()
                .unwrap()
                .settlement
                .is_some_and(|settlement| {
                    settlement.state == ghinvite_core::InvitationState::Accepted
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("settlement projection converges independently");
    // Unsupported reordered removal is not contradictory settlement evidence.
    let mut removed = payload.clone();
    removed["action"] = json!("removed");
    assert_eq!(
        send_signed_member(&client, &web_url, "member-3", &removed)
            .await
            .status(),
        202
    );
    assert_eq!(
        storage
            .get_delivery(&sent.create.command.delivery_key())
            .await
            .unwrap()
            .unwrap()
            .settlement
            .unwrap()
            .state,
        ghinvite_core::InvitationState::Accepted
    );
    let events = storage
        .list_audit_events(
            100,
            Some(ghinvite_core::audit::EventType::InvitationAccepted),
            ghinvite_core::storage::AuditPosition::Latest,
        )
        .await
        .unwrap()
        .events;
    assert_eq!(
        events.iter().filter(|event| event.target_id == id).count(),
        1
    );
    let event = events.iter().find(|event| event.target_id == id).unwrap();
    assert_eq!(event.actor_kind, ghinvite_core::audit::ActorKind::System);
    assert_eq!(
        event.metadata,
        json!({"reconciled":true,"evidence":{"kind":"effective_role","role_name":"write"}})
    );
    let completed_invocations = invocations.lock().unwrap().clone();
    for invocation in completed_invocations {
        assert!(
            client
                .get(format!("{ingress}/restate/invocation/{invocation}/attach"))
                .send()
                .await
                .unwrap()
                .status()
                .is_success()
        );
    }
    web_server.abort();
    proxy_server.abort();
    assert_eq!(
        storage
            .get_delivery(id)
            .await
            .unwrap()
            .unwrap()
            .settlement
            .unwrap()
            .state,
        ghinvite_core::InvitationState::Accepted
    );
    assert_eq!(github.writes(), 2);
    for role in ["triage", "maintain"] {
        github.reset().await;
        github
            .outcome("acme/api", "alice", Outcome::Unanswered)
            .await;
        let link = ghinvite_core::InvitationLinkId::new();
        call("InvitationLink",link.to_string(),"create",json!({"max_uses":1000,"secret_sha256":"2a".repeat(32),"link_id":link,"account_id":100,"installation_id":9,"admin":{"account_id":100,"user_id":7},
            "description":"Role reconciliation","approval_required":false,"targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/api","grant":if role == "push" { "write" } else if role == "pull" { "read" } else { role }}]}})).send().await.unwrap();
        let admitted: Value = call(
            "InvitationLink",
            link.to_string(),
            "admit",
            json!({"scope_evidence":[],"secret_sha256":"2a".repeat(32),"secret_generation":1,"link_id":link,"requester_id":8,"operation_id":RequestId::new()}),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        let plan: Value = call(
            "InvitationRequest",
            admitted["result"]["request_id"]
                .as_str()
                .unwrap()
                .to_owned(),
            "approved_plan",
            json!({"link_id":link,"request_id":admitted["result"]["request_id"],"requester_id":8}),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        let command = plan["commands"][0].clone();
        let delivery_id = delivery_key(&command);
        let id = delivery_id.as_str();
        let unknown: Value = call("RepositoryDelivery", id.into(), "create", command.clone())
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(unknown["outcome"]["kind"], "outcome_unknown");
        github
            .collaborate("alice", octoemu::seed::Role::from(role.to_owned()))
            .await;
        let reconciled: Value = call("RepositoryDelivery", id.into(), "create", command.clone())
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            reconciled["outcome"]["kind"], "outcome_unknown",
            "role evidence without invitation presence cannot resolve uncertain creation"
        );
        assert_eq!(github.writes(), 1);
    }
    // #65: approval and scope belong to A even when delivery resumes through B.
    github.reset().await;
    let historical_link = ghinvite_core::InvitationLinkId::new();
    let created = call("InvitationLink", historical_link.to_string(), "create", json!({"max_uses":1000,"secret_sha256":"2a".repeat(32),
        "link_id":historical_link,"account_id":100,"installation_id":9,
        "admin":{"account_id":100,"user_id":7},"description":"Reinstalled account",
        "approval_required":false,"targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/api","grant":"write"}]}
    })).send().await.unwrap();
    assert!(created.status().is_success());
    let admitted: Value = call(
        "InvitationLink",
        historical_link.to_string(),
        "admit",
        json!({"scope_evidence":[],"secret_sha256":"2a".repeat(32),"secret_generation":1,
            "link_id":historical_link,"requester_id":8,"operation_id":RequestId::new()
        }),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let query = json!({"link_id":historical_link,"request_id":admitted["result"]["request_id"],"requester_id":8});
    let plan: Value = call(
        "InvitationRequest",
        query["request_id"].as_str().unwrap().to_owned(),
        "approved_plan",
        query.clone(),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let command = plan["commands"][0].clone();
    let delivery_id = delivery_key(&command);
    let id = delivery_id.as_str();
    assert!(
        call(
            "Installation",
            "9".into(),
            "uninstall",
            json!({"app":"repo","installation_id":9,"uninstalled_at":chrono::Utc::now()})
        )
        .send()
        .await
        .unwrap()
        .status()
        .is_success()
    );
    let blocked: Value = call("RepositoryDelivery", id.into(), "create", command.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(blocked["outcome"]["kind"], "blocked");
    // On GitHub the App is uninstalled and installed again as 19.
    assert!(emulator.uninstall(9));
    emulator
        .install(octoemu::seed::Installation::new(19, 123, "acme"))
        .unwrap();
    assert!(call("Installation", "19".into(), "onboard", ghinvite_e2e::setup_command(json!({"app":"repo","installation_id":19,"actor_user_id":7,"account_id":100,
        "account_login":"acme","account_type":"Organization","selected_repos":"all","installed_at":chrono::Utc::now()})))
        .send().await.unwrap().status().is_success());
    let delivered: Value = call("RepositoryDelivery", id.into(), "create", command.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(delivered["outcome"]["kind"], "created");
    assert_eq!(delivered["command"], command);
    let original_link = storage
        .get_invitation_link_by_id(historical_link)
        .await
        .unwrap()
        .unwrap();
    let original_request = storage
        .get_invitation_request(command["request_id"].as_str().unwrap().parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    let original_deliveries = storage
        .list_delivery_for_request(original_request.id)
        .await
        .unwrap();
    let original_audit = assert_create_audit(&*storage, &delivered).await.unwrap();
    // GitHub acceptance without a webhook: remove pending evidence, expose access.
    // The invitee declines (the stub took an unauthenticated DELETE).
    github
        .decline(
            "alice",
            delivered["outcome"]["upstream_id"].as_u64().unwrap(),
        )
        .await;
    github.collaborate("alice", octoemu::seed::Role::Push).await;
    // A stale/corrupt local installation mapping must not grant another account
    // authority to observe this request, even if its repo/login evidence
    // matches: GitHub reports the installation under another account (200),
    // an injected answer.
    let other_account = emulator
        .add_fault(
            Fault::on(
                "GET",
                "/app/installations/{installation_id}",
                FaultAction::custom(|request| {
                    let id = request
                        .params
                        .get("installation_id")
                        .and_then(|id| id.parse::<u64>().ok());
                    Reply::new(200).json(json!({"id":id,"account":{"id":200,"login":"acme","type":"Organization"},"app_id":123,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","repository_selection":"all","permissions":{"administration":"write","metadata":"read"},"suspended_at":null}))
                }),
            )
            .label("installation under another account"),
        )
        .unwrap();
    run_sweep(&client, &ingress, &admin, &*storage).await;
    assert!(
        is_pending_delivery(&storage.get_delivery(id).await.unwrap().unwrap()),
        "another account cannot supply observation authority"
    );
    emulator.remove_fault(other_account);
    let unavailable = credentials_unavailable();
    let window = emulator.calls().last().map_or(0, |call| call.seq);
    run_sweep(&client, &ingress, &admin, &*storage).await;
    assert!(
        is_pending_delivery(&storage.get_delivery(id).await.unwrap().unwrap()),
        "unavailable credentials must preserve recoverability"
    );
    // Every refused call carried installation 19's token.
    let tokens: Vec<_> = emulator
        .calls()
        .into_iter()
        .filter(|call| {
            call.seq > window && call.fault.as_ref().is_some_and(|f| f.rule == unavailable)
        })
        .map(|call| call.identity)
        .collect();
    assert!(!tokens.is_empty());
    assert!(
        tokens.iter().all(|identity| matches!(
            identity,
            Some(octoemu::calls::Identity::Installation {
                installation_id: 19,
                ..
            })
        )),
        "{tokens:?}"
    );
    emulator.remove_fault(unavailable);
    let reassigned = emulator
        .add_fault(Fault::on(
            "GET",
            "/repos/acme/api",
            FaultAction::respond_json(200, json!({"id":999,"full_name":"acme/api","private":true})),
        ))
        .unwrap();
    run_sweep(&client, &ingress, &admin, &*storage).await;
    assert!(
        is_pending_delivery(&storage.get_delivery(id).await.unwrap().unwrap()),
        "a reused repository name cannot supply evidence"
    );
    emulator.remove_fault(reassigned);
    run_sweep(&client, &ingress, &admin, &*storage).await;
    let settled = storage.get_delivery(id).await.unwrap().unwrap();
    assert_eq!(
        settled.settlement.as_ref().unwrap().state,
        ghinvite_core::InvitationState::Accepted,
        "active replacement sweeps must discover the historical invitation"
    );
    assert_eq!(
        upstream(&settled),
        delivered["outcome"]["upstream_id"].as_u64()
    );
    assert_eq!(
        storage
            .get_invitation_link_by_id(historical_link)
            .await
            .unwrap()
            .unwrap(),
        original_link
    );
    assert_eq!(original_link.installation_id, 9);
    assert_eq!(
        storage
            .get_invitation_request(original_request.id)
            .await
            .unwrap()
            .unwrap(),
        original_request
    );
    assert_eq!(
        storage
            .list_delivery_for_request(original_request.id)
            .await
            .unwrap()
            .into_iter()
            .map(|s| s.create)
            .collect::<Vec<_>>(),
        original_deliveries
            .into_iter()
            .map(|s| s.create)
            .collect::<Vec<_>>()
    );
    let retained_plan: Value = call(
        "InvitationRequest",
        query["request_id"].as_str().unwrap().to_owned(),
        "approved_plan",
        query,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(retained_plan, plan);
    let events = storage
        .list_audit_events(100, None, ghinvite_core::storage::AuditPosition::Latest)
        .await
        .unwrap()
        .events;
    assert!(events.contains(&original_audit));
    let settlements: Vec<_> = events
        .iter()
        .filter(|event| {
            event.target_id == id
                && event.event_type == ghinvite_core::audit::EventType::InvitationAccepted
        })
        .collect();
    assert_eq!(settlements.len(), 1);
    assert_eq!(
        settlements[0].metadata,
        json!({"reconciled":true,"evidence":{"kind":"effective_role","role_name":"write"}})
    );
    // A rate limit is GitHub declining to answer: it blocks rather than failing
    // the invitation, and its recheck follows GitHub's own retry guidance rather
    // than the slow dependency cadence. The object lock is released for that
    // wait, so the status reads below do not queue behind the retry.
    faults
        .projection_unavailable
        .store(true, std::sync::atomic::Ordering::SeqCst);
    github.reset().await;
    let throttled_link = ghinvite_core::InvitationLinkId::new();
    call("InvitationLink", throttled_link.to_string(), "create", json!({"max_uses":1000,"secret_sha256":"2a".repeat(32),
        "link_id":throttled_link,"account_id":100,"installation_id":19,
        "admin":{"account_id":100,"user_id":7},"description":"Throttled delivery",
        "approval_required":false,"targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/api","grant":"write"}]}
    })).send().await.unwrap();
    let key = throttled_link.to_string();
    let admit = json!({"scope_evidence":[],"secret_sha256":"2a".repeat(32),"secret_generation":1,"link_id":throttled_link,"requester_id":8,"operation_id":RequestId::new()});
    let admitted = call("InvitationLink", key.clone(), "admit", admit);
    let admitted: Value = admitted.send().await.unwrap().json().await.unwrap();
    let query = json!({"link_id":throttled_link,"request_id":admitted["result"]["request_id"],"requester_id":8});
    let plan = call(
        "InvitationRequest",
        query["request_id"].as_str().unwrap().to_owned(),
        "approved_plan",
        query,
    );
    let plan: Value = plan.send().await.unwrap().json().await.unwrap();
    let command = plan["commands"][0].clone();
    let delivery_id = delivery_key(&command);
    let id = delivery_id.as_str();
    github
        .outcome("acme/api", "alice", Outcome::ThrottledOnce)
        .await;
    faults
        .lose_release_ack
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let throttled: Value = call("RepositoryDelivery", id.into(), "create", command.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(throttled["outcome"]["kind"], "throttled");
    let until = throttled["outcome"]["until"]
        .as_str()
        .unwrap()
        .parse::<chrono::DateTime<chrono::Utc>>()
        .unwrap();
    assert!(
        until
            >= command["approved_at"]
                .as_str()
                .unwrap()
                .parse::<chrono::DateTime<chrono::Utc>>()
                .unwrap()
    );
    let sending = storage.get_delivery(id).await.unwrap();
    assert!(sending.is_none(), "receipt projection remains unavailable");
    // Explicit recovery is an exclusive command too. It returns while SQL is
    // stalled and keeps the original pending timer instead of adding an hour.
    let unavailable = credentials_unavailable();
    let redriven: Value = call("RepositoryDelivery", id.into(), "create", command.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(redriven["outcome"]["kind"], "blocked");
    assert_eq!(
        faults
            .release_ack_losses
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_eq!(redriven["revision"], 2);
    for _ in 0..2 {
        let stale = call(
            "RepositoryDelivery",
            id.into(),
            "recheck",
            json!({"command":command,"generation":0}),
        )
        .send()
        .await
        .unwrap();
        assert!(stale.status().is_success());
    }
    let retained: Value = client
        .post(format!(
            "{ingress}/RepositoryDelivery/{}:10:1/status",
            command["request_id"].as_str().unwrap()
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        retained["create"], redriven,
        "stale wakes cannot consume the scheduled generation"
    );
    emulator.remove_fault(unavailable);
    // The stub asked for one second; the recheck that wait scheduled delivers
    // without any further command from us.
    let mut recovered = Value::Null;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let status = client.post(format!(
            "{ingress}/RepositoryDelivery/{}:10:1/status",
            command["request_id"].as_str().unwrap()
        ));
        recovered = status.send().await.unwrap().json().await.unwrap();
        recovered = recovered["create"].clone();
        if recovered["outcome"]["kind"] == "created" {
            break;
        }
    }
    assert_eq!(recovered["outcome"]["kind"], "created", "{recovered}");
    assert!(
        faults
            .projection_failures
            .load(std::sync::atomic::Ordering::SeqCst)
            > 0
    );
    let replay: Value = call("RepositoryDelivery", id.into(), "create", command.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        replay, recovered,
        "confirmed replay must finish during projection outage"
    );
    assert!(storage.get_delivery(id).await.unwrap().is_none());
    faults
        .projection_unavailable
        .store(false, std::sync::atomic::Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if storage
                .get_delivery(id)
                .await
                .unwrap()
                .is_some_and(|row| is_pending_delivery(&row))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("receipt projection converges after repair");
    let delivered = storage.get_delivery(id).await.unwrap();
    let upstream = upstream(&delivered.unwrap());
    assert_eq!(upstream, recovered["outcome"]["upstream_id"].as_u64());
    assert_create_audit(&*storage, &recovered).await.unwrap();
    // Replayed/stale wakes cannot reopen confirmed delivery or perform a PUT.
    for generation in [1, 1, 0] {
        let response = call(
            "RepositoryDelivery",
            id.into(),
            "recheck",
            json!({"command":command,"generation":generation}),
        )
        .send()
        .await
        .unwrap();
        assert!(response.status().is_success());
    }
    let status: Value = client
        .post(format!(
            "{ingress}/RepositoryDelivery/{}:10:1/status",
            command["request_id"].as_str().unwrap()
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["create"], recovered);
    assert_eq!(
        status["revision"], 3,
        "only the original timer advances the redriven delivery"
    );
    let writes = github.write_statuses();
    assert_eq!(
        writes.len(),
        2,
        "one throttled PUT and one successful PUT across outage, repair and stale wakes"
    );
    assert_eq!(writes.iter().filter(|status| **status == 201).count(), 1);

    for stage in [
        "after-receipt",
        "after-projection-send",
        "after-recheck-send",
    ] {
        github.reset().await;
        let link = ghinvite_core::InvitationLinkId::new();
        call("InvitationLink", link.to_string(), "create", json!({"max_uses":1000,"secret_sha256":"2a".repeat(32),
            "link_id":link,"account_id":100,"installation_id":19,
            "admin":{"account_id":100,"user_id":7},"description":"Interrupted delivery",
            "approval_required":false,"targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/api","grant":"write"}]}
        })).send().await.unwrap();
        let admitted: Value = call(
            "InvitationLink",
            link.to_string(),
            "admit",
            json!({"scope_evidence":[],"secret_sha256":"2a".repeat(32),"secret_generation":1,
                "link_id":link,"requester_id":8,"operation_id":RequestId::new()
            }),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        let plan: Value = call(
            "InvitationRequest",
            admitted["result"]["request_id"]
                .as_str()
                .unwrap()
                .to_owned(),
            "approved_plan",
            json!({
                "link_id":link,"request_id":admitted["result"]["request_id"],"requester_id":8
            }),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        let command = plan["commands"][0].clone();
        let delivery_id = delivery_key(&command);
        let id = delivery_id.as_str();
        if stage == "after-recheck-send" {
            github
                .outcome("acme/api", "alice", Outcome::ThrottledOnce)
                .await;
        }
        *faults.stage.lock().unwrap() = Some(stage.into());
        faults
            .interruptions
            .store(0, std::sync::atomic::Ordering::SeqCst);
        let request = call("RepositoryDelivery", id.into(), "create", command.clone());
        let response = tokio::spawn(async move { request.send().await.unwrap() });
        tokio::time::timeout(Duration::from_secs(10), async {
            while faults
                .interruptions
                .load(std::sync::atomic::Ordering::SeqCst)
                < 2
            {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("delivery paused at journal boundary");
        let competing = if stage == "after-receipt" {
            // Acceptance hints now require fresh role evidence. Simulate the
            // actual upstream transition while the owner remains interrupted.
            let world = emulator.state()["world"].clone();
            let upstream_id = world["repository_invitations"]
                .as_array()
                .unwrap()
                .iter()
                .find(|invitation| {
                    invitation["repo"] == "acme/api" && invitation["invitee"] == "alice"
                })
                .unwrap()["id"]
                .as_u64()
                .unwrap();
            github.decline("alice", upstream_id).await;
            github.collaborate("alice", octoemu::seed::Role::Push).await;
            let observed: Value = client
                .post(format!("{ingress}/RepositoryDelivery/{id}/status"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let ack: Value = client
                .post(format!("{ingress}/DeliveryObservation/observe/send"))
                .json(&observed)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            Some(ack["invocationId"].as_str().unwrap().to_owned())
        } else {
            None
        };
        let retained: Value = client
            .post(format!(
                "{ingress}/RepositoryDelivery/{}:10:1/status",
                command["request_id"].as_str().unwrap()
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        *faults.stage.lock().unwrap() = None;
        let mut aborted = 0;
        for task in tasks.lock().unwrap().drain(..) {
            if !task.is_finished() {
                task.abort();
                aborted += 1;
            }
        }
        assert!(aborted > 0, "interrupt live SDK execution");
        let result: Value = response.await.unwrap().json().await.unwrap();
        assert_eq!(
            result, retained["create"],
            "replay keeps outcome, identity and timestamp at {stage}"
        );
        let recovered = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let status: Value = client
                    .post(format!(
                        "{ingress}/RepositoryDelivery/{}:10:1/status",
                        command["request_id"].as_str().unwrap()
                    ))
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                if status["create"]["outcome"]["kind"] == "created" {
                    break status["create"].clone();
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("interrupted continuation completes");
        if let Some(invocation) = competing {
            assert!(
                client
                    .get(format!("{ingress}/restate/invocation/{invocation}/attach"))
                    .send()
                    .await
                    .unwrap()
                    .status()
                    .is_success()
            );
            let snapshot: Value = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let snapshot: Value = client
                        .post(format!(
                            "{ingress}/RepositoryDelivery/{}:10:1/status",
                            command["request_id"].as_str().unwrap()
                        ))
                        .send()
                        .await
                        .unwrap()
                        .json()
                        .await
                        .unwrap();
                    if snapshot["settlement"]["state"] == "accepted" {
                        break snapshot;
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .expect("accepted hint is verified and reconciled after owner replay");
            assert_eq!(snapshot["create"], recovered);
            assert_eq!(snapshot["settlement"]["state"], "accepted");
        }
        assert_create_audit(&*storage, &recovered).await.unwrap();
        let writes = github.write_statuses();
        assert_eq!(
            writes.len(),
            if stage == "after-recheck-send" { 2 } else { 1 }
        );
        assert_eq!(writes.iter().filter(|status| **status == 201).count(), 1);
    }
    server.abort();
    harness.finish().await;
}

async fn run_sweep(client: &reqwest::Client, ingress: &str, admin: &str, storage: &impl Storage) {
    // Read the actual retained sweep set. The new service queues observations;
    // wait for those public handlers and their changed projections, not a sleep.
    let state: Value = client
        .post(format!("{ingress}/SweepSchedule/repo:100/status"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let response = client.post(format!("{ingress}/Reconcile/sweep"))
        .json(&json!({"account":{"app":"repo","account_id":100},"token":state["token"],"tracked":state["tracked"]}))
        .send().await.unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let result: Value = client.post(format!("{admin}/query")).header("accept","application/json")
                .json(&json!({"query":"SELECT id FROM sys_invocation WHERE status != 'completed' AND (target_service_name = 'Reconcile' OR (target_service_name = 'RepositoryDelivery' AND target_handler_name = 'reconcile'))"}))
                .send().await.unwrap().json().await.unwrap();
            if result["rows"].as_array().unwrap().is_empty() { break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        for keys in state["tracked"].as_object().unwrap().values() {
            for key in keys.as_array().unwrap() {
                let snapshot: Value=client.post(format!("{ingress}/RepositoryDelivery/{}/status",key.as_str().unwrap())).send().await.unwrap().json().await.unwrap();
                if snapshot["settlement"].is_null() { continue; }
                let delivery_key=key.as_str().unwrap();
                loop {
                    if let Some(row)=storage.get_delivery(delivery_key).await.unwrap()
                        && serde_json::to_value(row.settlement.as_ref().unwrap().state).unwrap()==snapshot["settlement"]["state"] {break;}
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
        }
    }).await.expect("scheduled fan-out and retained projections complete");
}

async fn assert_create_audit(
    storage: &impl Storage,
    receipt: &Value,
) -> Option<ghinvite_core::audit::AuditEvent> {
    use ghinvite_core::{
        audit::{ActorKind, EventType, TargetKind},
        storage::AuditPosition,
    };
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let rows = storage
                .list_delivery_for_request(
                    receipt["command"]["request_id"]
                        .as_str()
                        .unwrap()
                        .parse()
                        .unwrap(),
                )
                .await
                .unwrap();
            if rows.iter().any(|row| {
                row.create.command.delivery_key() == delivery_key(&receipt["command"])
                    && row.revision >= receipt["revision"].as_u64().unwrap()
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("delivery read projection catches up");
    let events = storage
        .list_audit_events(100, None, AuditPosition::Latest)
        .await
        .unwrap()
        .events;
    let events: Vec<_> = events
        .into_iter()
        .filter(|event| event.target_id == delivery_key(&receipt["command"]))
        // Keep create facts separate from settlement and transient audit facts.
        .filter(|event| {
            matches!(
                event.event_type,
                EventType::InvitationSent
                    | EventType::DeliveryAlreadySatisfied
                    | EventType::DeliveryUpgraded
                    | EventType::DeliveryAlreadyPending
                    | EventType::InvitationSendFailed
            )
        })
        .collect();
    let (kind, actor) = match receipt["outcome"]["kind"].as_str().unwrap() {
        "created" => (EventType::InvitationSent, ActorKind::System),
        "already_satisfied" => (EventType::DeliveryAlreadySatisfied, ActorKind::Github),
        "upgraded" => (EventType::DeliveryUpgraded, ActorKind::System),
        "already_pending" => (EventType::DeliveryAlreadyPending, ActorKind::System),
        "failed" => (EventType::InvitationSendFailed, ActorKind::System),
        _ => {
            assert!(events.is_empty(), "receipt={receipt}; events={events:?}");
            return None;
        }
    };
    assert_eq!(events.len(), 1);
    let event = events.into_iter().next().unwrap();
    assert_eq!(event.event_type, kind);
    assert_eq!(event.actor_kind, actor);
    assert_eq!(event.actor_id, None);
    assert_eq!(event.target_kind, TargetKind::RepositoryDelivery);
    assert_eq!(
        event.occurred_at,
        receipt["confirmed_at"]
            .as_str()
            .unwrap()
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap()
    );
    assert!(
        event.occurred_at
            >= receipt["command"]["approved_at"]
                .as_str()
                .unwrap()
                .parse::<chrono::DateTime<chrono::Utc>>()
                .unwrap()
    );
    assert!(event.request_id.is_none());
    Some(event)
}

fn delivery_key(command: &Value) -> String {
    let command: ghinvite_core::delivery::CreateCommand =
        serde_json::from_value(command.clone()).unwrap();
    command.delivery_key()
}
fn is_pending_delivery(snapshot: &ghinvite_core::delivery::DeliverySnapshot) -> bool {
    snapshot.settlement.is_none()
        && matches!(
            snapshot.create.outcome,
            ghinvite_core::delivery::CreateOutcome::Created { .. }
        )
}

fn upstream(snapshot: &ghinvite_core::delivery::DeliverySnapshot) -> Option<u64> {
    match snapshot.create.outcome {
        ghinvite_core::delivery::CreateOutcome::Created { upstream_id }
        | ghinvite_core::delivery::CreateOutcome::AlreadyPending { upstream_id, .. } => {
            Some(upstream_id)
        }
        _ => None,
    }
}
