//! Public ingress + real SQL + GitHub HTTP boundary acceptance for #56.
#![cfg(feature = "integration")]
use ghinvite_core::storage::{ConsoleStorage, DeliveryStorage, InstallationStorage, RecordStorage};
use ghinvite_core::{RequestId, storage::Storage};
use restate_sdk::{endpoint::Endpoint, http_server::HttpServer};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

const MEMBER_WEBHOOK_SECRET: &[u8] = b"member-webhook-runtime-fixture";

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
        .post(format!("{web_url}/webhooks/github"))
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
async fn member_webhook_ingress(
    storage: Arc<ghinvite_storage_sqlx::SqlxStorage>,
    ingress: &str,
    client: reqwest::Client,
) -> (
    String,
    Arc<std::sync::Mutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
    tokio::task::JoinHandle<()>,
) {
    use axum::{
        body::Body,
        extract::{Json, Path},
        response::{IntoResponse, Response},
        routing::post,
    };
    let invocations = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let recorded = invocations.clone();
    let ingress = ingress.to_owned();
    let proxy = axum::Router::new().route(
        "/GithubInvitation/{id}/on_webhook/send",
        post(move |Path(id): Path<String>, Json(input): Json<Value>| {
            let client = client.clone();
            let ingress = ingress.clone();
            let recorded = recorded.clone();
            async move {
                let response = client
                    .post(format!("{ingress}/GithubInvitation/{id}/on_webhook/send"))
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
    let restate = Arc::new(ghinvite_web::RestateClient::new(proxy_url).unwrap());
    let state = ghinvite_web::AppState::new(
        storage,
        Arc::new(ghinvite_github::mocks::MockTransport::scripted(vec![])),
        Arc::new(ghinvite_web::RestateCommands::new(restate.clone())),
        restate,
        ghinvite_web::WebConfig {
            webhook_secret: MEMBER_WEBHOOK_SECRET.to_vec(),
            ..ghinvite_web::WebConfig::for_local_dev_with_secret([7; 32])
        },
    );
    let app = ghinvite_web::routes::webhook::router().with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let web_url = format!("http://{}", listener.local_addr().unwrap());
    let web_server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (web_url, invocations, web_server, proxy_server)
}

#[tokio::test]
async fn retained_create_survives_sent_replay_and_conflicts() {
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
    storage
        .insert_installation(&ghinvite_core::Account {
            installation_id: 9,
            account_id: 100,
            account_login: "acme".into(),
            account_type: ghinvite_core::AccountType::Organization,
            installed_at: now,
            uninstalled_at: None,
            selected_repos: ghinvite_core::SelectedRepos::All,
        })
        .await
        .unwrap();
    for (user_id, login) in [(7, "creator"), (8, "alice")] {
        storage
            .upsert_user(&ghinvite_core::User {
                user_id,
                login: login.into(),
                avatar_url: None,
                last_seen_at: now,
            })
            .await
            .unwrap();
    }
    let stub = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", stub.local_addr().unwrap());
    let credentials_unavailable = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let repository_reassigned = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let github_tokens = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let unavailable = credentials_unavailable.clone();
    let reassigned = repository_reassigned.clone();
    let tokens = github_tokens.clone();
    let router = ghinvite_github::stub::router().layer(axum::middleware::from_fn(
        move |request: axum::extract::Request, next: axum::middleware::Next| {
            let unavailable = unavailable.clone();
            let reassigned = reassigned.clone();
            let tokens = tokens.clone();
            async move {
                if let Some(token) = request
                    .headers()
                    .get("authorization")
                    .and_then(|h| h.to_str().ok())
                    && token.starts_with("Bearer test-token-for-")
                {
                    tokens.lock().unwrap().push(token.to_owned());
                    if unavailable.load(std::sync::atomic::Ordering::SeqCst) {
                        return axum::response::Response::builder()
                            .status(401)
                            .body(axum::body::Body::empty())
                            .unwrap();
                    }
                }
                if request.uri().path() == "/repos/acme/api"
                    && reassigned.load(std::sync::atomic::Ordering::SeqCst)
                {
                    return axum::response::IntoResponse::into_response(axum::Json(
                        json!({"id":999,"full_name":"acme/api","private":true}),
                    ));
                }
                next.run(request).await
            }
        },
    ));
    let stub_task = tokio::spawn(async move {
        axum::serve(stub, router).await.unwrap();
    });
    let github = Arc::new(
        ghinvite_github::InstallationClient::new(
            Arc::new(ghinvite_github::transport::ReqwestTransport::with_client(
                client.clone(),
            )),
            ghinvite_github::jwt::AppJwtSigner::from_pem(
                123,
                include_str!("../../ghinvite-github/src/jwt_test_key.pem"),
            )
            .unwrap(),
        )
        .with_base(&base),
    );
    let state = ghinvite_workflows::AppState::new(storage.clone(), github);
    let builder = ghinvite_workflows::admission::bind_protocol_fixture(Endpoint::builder());
    let builder = ghinvite_workflows::projection::bind(builder, storage.clone());
    let workflow_faults =
        Arc::new(ghinvite_workflows::request_lifecycle::WorkflowFaults::default());
    let builder =
        ghinvite_workflows::request_lifecycle::bind_with_faults(builder, workflow_faults.clone());
    let faults = Arc::new(ghinvite_workflows::delivery::DeliveryFaults::default());
    faults
        .lose_http_result
        .store(true, std::sync::atomic::Ordering::SeqCst);
    faults
        .lose_projection_ack
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let builder = builder
        .bind(ghinvite_workflows::github_invitation::GithubInvitation {
            state: state.clone(),
        })
        .bind(ghinvite_workflows::reconcile::Reconcile {
            state: state.clone(),
        });
    let endpoint =
        ghinvite_workflows::delivery::bind_with_faults(builder, state, faults.clone()).build();
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        HttpServer::new(endpoint)
            .serve_with_cancel(listener, std::future::pending::<()>())
            .await;
    });
    let admin = std::env::var("RESTATE_ADMIN_URL").unwrap();
    let ingress = std::env::var("RESTATE_INGRESS_URL").unwrap();
    let host = std::env::var("RESTATE_ENDPOINT_HOST").unwrap();
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
        client
            .post(format!("{ingress}/{service}/{key}/{handler}"))
            .json(&input)
    };
    let creation = json!({"link_id":link,"account_id":100,"installation_id":9,"admin":{"account_id":100,"user_id":7},
        "description":"Delivery test","approval_required":false,"permission":"push","repos":[{"repo_id":10,"repo_full_name":"acme/api"}]});
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
        json!({"link_id":link,"requester_id":8,"operation_id":RequestId::new()}),
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
        "InvitationLink",
        link.to_string(),
        "prepare_dispatch",
        query,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let command = plan["commands"][0].clone();
    let id = command["invitation_id"].as_str().unwrap().to_owned();
    let completed = client
        .get(format!(
            "{ingress}/restate/workflow/InvitationRequest/{request}/attach"
        ))
        .send()
        .await
        .unwrap();
    assert!(completed.status().is_success());
    let checkpoint: Value = call(
        "InvitationLink",
        link.to_string(),
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
    let response = call("GithubCreate", id.clone(), "create", command.clone())
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
    let replay: Value = call("GithubCreate", id.clone(), "create", command.clone())
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
        call("GithubCreate", id, "create", changed)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let calls: Value = client
        .get(format!("{base}/calls"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        calls["requests"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["method"] == "PUT")
            .count(),
        1
    );
    for (stub_outcome, expected) in [
        ("already_collaborator", "already_collaborator"),
        ("terminal_failure", "failed"),
        ("transient_once", "outcome_unknown"),
        ("created_response_lost", "outcome_unknown"),
        ("created_then_declined", "outcome_unknown"),
    ] {
        client.delete(format!("{base}/reset")).send().await.unwrap();
        client
            .post(format!("{base}/outcomes"))
            .json(&json!({"owner":"acme","repo":"api","user":"alice","outcome":stub_outcome}))
            .send()
            .await
            .unwrap();
        let link = ghinvite_core::InvitationLinkId::new();
        call("InvitationLink",link.to_string(),"create",json!({"link_id":link,"account_id":100,"installation_id":9,"admin":{"account_id":100,"user_id":7},
            "description":"Other result","approval_required":false,"permission":"push","repos":[{"repo_id":10,"repo_full_name":"acme/api"}]})).send().await.unwrap();
        let admitted: Value = call(
            "InvitationLink",
            link.to_string(),
            "admit",
            json!({"link_id":link,"requester_id":8,"operation_id":RequestId::new()}),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        let request = admitted["result"]["request_id"].as_str().unwrap();
        let plan: Value = call(
            "InvitationLink",
            link.to_string(),
            "prepare_dispatch",
            json!({"link_id":link,"request_id":request,"requester_id":8}),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        let command = plan["commands"][0].clone();
        let id = command["invitation_id"].as_str().unwrap().to_owned();
        let result: Value = call("GithubCreate", id.clone(), "create", command.clone())
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(result["outcome"]["kind"], expected);
        assert_create_audit(&*storage, &result).await;
        let replay: Value = call("GithubCreate", id, "create", command)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if stub_outcome == "created_response_lost" {
            assert_eq!(replay["outcome"]["kind"], "created");
        } else {
            assert_eq!(replay["outcome"], result["outcome"]);
        }
        let calls: Value = client
            .get(format!("{base}/calls"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            calls["requests"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|c| c["method"] == "PUT")
                .count(),
            1,
            "{stub_outcome}"
        );
        let rows = storage
            .list_delivery_for_request(request.parse().unwrap())
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(&rows[0]).unwrap()["outcome"],
            replay["outcome"]
        );
        assert_create_audit(&*storage, &replay).await;
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
    assert_eq!(serde_json::to_value(&outcomes[0]).unwrap(), result);
    // The independent lifecycle can decline without resetting create authority.
    let invitation_id: ghinvite_core::GithubInvitationId =
        command["invitation_id"].as_str().unwrap().parse().unwrap();
    let sent = storage
        .get_github_invitation(invitation_id)
        .await
        .unwrap()
        .unwrap();
    storage
        .settle_github_invitation(&ghinvite_core::storage::settlement::Settlement {
            expected: sent,
            state: ghinvite_core::InvitationState::Declined,
            event: ghinvite_core::audit::AuditEvent {
                id: ghinvite_core::AuditEventId::new(),
                account_id: 100,
                occurred_at: chrono::Utc::now(),
                event_type: ghinvite_core::audit::EventType::InvitationDeclined,
                actor_kind: ghinvite_core::audit::ActorKind::Github,
                actor_id: None,
                target_kind: ghinvite_core::audit::TargetKind::GithubInvitation,
                target_id: invitation_id.to_string(),
                metadata: serde_json::Value::Null,
                request_id: None,
            },
        })
        .await
        .unwrap();
    let replay: Value = call(
        "GithubCreate",
        command["invitation_id"].as_str().unwrap().into(),
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
            .get_github_invitation(command["invitation_id"].as_str().unwrap().parse().unwrap())
            .await
            .unwrap()
            .unwrap()
            .state,
        ghinvite_core::InvitationState::Declined
    );
    let signal = json!({"link_id":link,"request_id":request,"revision":plan["input"]["request"]["revision"],"decision_id":command["approval_id"]});
    let needed: Value = call(
        "InvitationLink",
        link.to_string(),
        "notification_needed",
        signal.clone(),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(needed, false);
    assert!(
        call("InvitationRequest", request.into(), "notify", signal)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let promise: Value = client
        .post(format!(
            "{ingress}/InvitationRequest/{request}/notification_status"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        promise,
        Value::Null,
        "late delivery recreated an orphan promise"
    );
    let calls: Value = client
        .get(format!("{base}/calls"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        calls["requests"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["method"] == "PUT")
            .count(),
        1
    );
    // Partial fan-out: a durable first send stalls before its link checkpoint;
    // link commands remain available and explicit recovery uses the same plan.
    client.delete(format!("{base}/reset")).send().await.unwrap();
    workflow_faults
        .pause_after_send
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let partial_link = ghinvite_core::InvitationLinkId::new();
    call("InvitationLink",partial_link.to_string(),"create",json!({"link_id":partial_link,"account_id":100,"installation_id":9,"admin":{"account_id":100,"user_id":7},
        "description":"Partial fanout","approval_required":false,"permission":"push","repos":[{"repo_id":10,"repo_full_name":"acme/api"},{"repo_id":11,"repo_full_name":"acme/web"}]})).send().await.unwrap();
    let admitted: Value = call(
        "InvitationLink",
        partial_link.to_string(),
        "admit",
        json!({"link_id":partial_link,"requester_id":8,"operation_id":RequestId::new()}),
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
        "InvitationLink",
        partial_link.to_string(),
        "prepare_dispatch",
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
        "GithubCreate",
        first["invitation_id"].as_str().unwrap().into(),
        "create",
        first.clone(),
    )
    .send()
    .await
    .unwrap();
    assert!(
        call(
            "InvitationLink",
            partial_link.to_string(),
            "revoke",
            json!({"link_id":partial_link,"admin":{"account_id":100,"user_id":7}})
        )
        .send()
        .await
        .unwrap()
        .status()
        .is_success()
    );
    let checkpoint: Value = call(
        "InvitationLink",
        partial_link.to_string(),
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
    storage
        .update_installation_repos(9, &ghinvite_core::SelectedRepos::Subset(vec![10]))
        .await
        .unwrap();
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
        .get(format!(
            "{ingress}/restate/workflow/InvitationRequest/{partial_request}/attach"
        ))
        .send()
        .await
        .unwrap();
    for command in partial_plan["commands"].as_array().unwrap() {
        let response = call(
            "GithubCreate",
            command["invitation_id"].as_str().unwrap().into(),
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
    storage
        .update_installation_repos(9, &ghinvite_core::SelectedRepos::All)
        .await
        .unwrap();
    client
        .post(format!("{base}/identity"))
        .json(&json!({"login":"renamed","addressed_id":99}))
        .send()
        .await
        .unwrap();
    let second = partial_plan["commands"][1].clone();
    let mismatch: Value = call(
        "GithubCreate",
        second["invitation_id"].as_str().unwrap().into(),
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
    client
        .post(format!("{base}/identity"))
        .json(&json!({"login":"renamed","addressed_id":8}))
        .send()
        .await
        .unwrap();
    for command in partial_plan["commands"].as_array().unwrap() {
        let result: Value = call(
            "GithubCreate",
            command["invitation_id"].as_str().unwrap().into(),
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
    let calls: Value = client
        .get(format!("{base}/calls"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        calls["requests"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["method"] == "PUT")
            .count(),
        2
    );
    assert!(
        calls["requests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["method"] == "PUT" && c["path"] == "/repos/acme/web/collaborators/renamed")
    );
    let checkpoint: Value = call(
        "InvitationLink",
        partial_link.to_string(),
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
        "InvitationLink",
        partial_link.to_string(),
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
    client.delete(format!("{base}/reset")).send().await.unwrap();
    // Explicit 403 proves rejection, so restoration may safely try again.
    client
        .post(format!("{base}/identity"))
        .json(&json!({"login":"user-84","addressed_id":84}))
        .send()
        .await
        .unwrap();
    workflow_faults
        .pause_before_dispatch
        .store(true, std::sync::atomic::Ordering::SeqCst);
    client
        .post(format!("{base}/outcomes"))
        .json(&json!({"owner":"acme","repo":"api","user":"user-84","outcome":"access_lost_once"}))
        .send()
        .await
        .unwrap();
    let rejected_link = ghinvite_core::InvitationLinkId::new();
    call("InvitationLink",rejected_link.to_string(),"create",json!({"link_id":rejected_link,"account_id":100,"installation_id":9,"admin":{"account_id":100,"user_id":7},
        "description":"Access rejection","approval_required":false,"permission":"push","repos":[{"repo_id":10,"repo_full_name":"acme/api"}]})).send().await.unwrap();
    storage
        .upsert_user(&ghinvite_core::User {
            user_id: 84,
            login: "user-84".into(),
            avatar_url: None,
            last_seen_at: now,
        })
        .await
        .unwrap();
    let admitted: Value = call(
        "InvitationLink",
        rejected_link.to_string(),
        "admit",
        json!({"link_id":rejected_link,"requester_id":84,"operation_id":RequestId::new()}),
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
        "InvitationLink",
        rejected_link.to_string(),
        "delivery_progress",
        query.clone(),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(progress, json!([{"repo_id":10,"stage":"approved"}]));
    let plan: Value = call(
        "InvitationLink",
        rejected_link.to_string(),
        "prepare_dispatch",
        query.clone(),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let progress: Value = call(
        "InvitationLink",
        rejected_link.to_string(),
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
    let id = command["invitation_id"].as_str().unwrap();
    let rejected: Value = call("GithubCreate", id.into(), "create", command.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(rejected["outcome"]["kind"], "blocked");
    assert_create_audit(&*storage, &rejected).await;
    // The stub exposes one addressed identity at a time. This sweep observes
    // older Alice invitations; the blocked user-84 create is ineligible.
    client
        .post(format!("{base}/identity"))
        .json(&json!({"login":"alice","addressed_id":8}))
        .send()
        .await
        .unwrap();
    let sweep = client
        .post(format!("{ingress}/Reconcile/daily_run"))
        .json(&json!({"at":chrono::Utc::now()}))
        .send()
        .await
        .unwrap();
    assert!(
        sweep.status().is_success(),
        "{}",
        sweep.text().await.unwrap()
    );
    assert_eq!(
        storage
            .get_github_invitation(id.parse().unwrap())
            .await
            .unwrap()
            .unwrap()
            .state,
        ghinvite_core::InvitationState::Sending
    );
    client
        .post(format!("{base}/identity"))
        .json(&json!({"login":"user-84","addressed_id":84}))
        .send()
        .await
        .unwrap();
    let resumed: Value = call("GithubCreate", id.into(), "create", command.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resumed["outcome"]["kind"], "created");
    assert_create_audit(&*storage, &resumed).await;
    let sent = storage
        .get_github_invitation(id.parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        sent.github_invitation_id,
        resumed["outcome"]["upstream_id"].as_u64()
    );
    // Retained delayed evidence arrives after webhook settlement. Real SQL audit
    // failure keeps the public command retrying; the eventual commit is atomic.
    storage.debug_set_audit_failure(true).await.unwrap();
    let (web_url, invocations, web_server, proxy_server) =
        member_webhook_ingress(storage.clone(), &ingress, client.clone()).await;
    let payload = json!({"action":"added","installation":{"id":9},
        "repository":{"id":10,"owner":{"id":100}},"member":{"id":84},"sender":{"id":7}});
    // The proxy durably submits to real Restate, then loses the first acknowledgement.
    let first = send_signed_member(&client, &web_url, "member-1", &payload).await;
    assert_eq!(first.status(), 500);
    assert_eq!(invocations.lock().unwrap().len(), 1);
    let retry = send_signed_member(&client, &web_url, "member-1", &payload).await;
    assert_eq!(retry.status(), 204);
    assert_eq!(invocations.lock().unwrap().len(), 2);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        storage
            .get_github_invitation(sent.id)
            .await
            .unwrap()
            .unwrap()
            .state,
        ghinvite_core::InvitationState::Sent
    );
    storage.debug_set_audit_failure(false).await.unwrap();
    let invocation_id = invocations.lock().unwrap()[0].clone();
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
    let evidence = json!({"expected":sent,"accepted":false,"at":chrono::Utc::now()});
    let (a, b, duplicate) = tokio::join!(
        call("GithubInvitation", id.into(), "reconcile", evidence.clone()).send(),
        call("GithubInvitation", id.into(), "reconcile", evidence).send(),
        send_signed_member(&client, &web_url, "member-2", &payload)
    );
    assert!(a.unwrap().status().is_success());
    assert!(b.unwrap().status().is_success());
    assert_eq!(duplicate.status(), 204);
    // Unsupported reordered removal is not contradictory settlement evidence.
    let mut removed = payload.clone();
    removed["action"] = json!("removed");
    assert_eq!(
        send_signed_member(&client, &web_url, "member-3", &removed)
            .await
            .status(),
        204
    );
    assert_eq!(
        storage
            .get_github_invitation(sent.id)
            .await
            .unwrap()
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
    assert_eq!(event.actor_kind, ghinvite_core::audit::ActorKind::Github);
    assert_eq!(event.metadata, json!({"action":"accepted"}));
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
            .get_github_invitation(id.parse().unwrap())
            .await
            .unwrap()
            .unwrap()
            .state,
        ghinvite_core::InvitationState::Accepted
    );
    let calls: Value = client
        .get(format!("{base}/calls"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        calls["requests"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["method"] == "PUT")
            .count(),
        2
    );
    for role in ["triage", "maintain"] {
        client.delete(format!("{base}/reset")).send().await.unwrap();
        client.post(format!("{base}/outcomes")).json(&json!({"owner":"acme","repo":"api","user":"alice","outcome":"created_then_declined"})).send().await.unwrap();
        let link = ghinvite_core::InvitationLinkId::new();
        call("InvitationLink",link.to_string(),"create",json!({"link_id":link,"account_id":100,"installation_id":9,"admin":{"account_id":100,"user_id":7},
            "description":"Role reconciliation","approval_required":false,"permission":role,"repos":[{"repo_id":10,"repo_full_name":"acme/api"}]})).send().await.unwrap();
        let admitted: Value = call(
            "InvitationLink",
            link.to_string(),
            "admit",
            json!({"link_id":link,"requester_id":8,"operation_id":RequestId::new()}),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        let plan: Value = call(
            "InvitationLink",
            link.to_string(),
            "prepare_dispatch",
            json!({"link_id":link,"request_id":admitted["result"]["request_id"],"requester_id":8}),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        let command = plan["commands"][0].clone();
        let id = command["invitation_id"].as_str().unwrap();
        let unknown: Value = call("GithubCreate", id.into(), "create", command.clone())
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(unknown["outcome"]["kind"], "outcome_unknown");
        client
            .post(format!("{base}/identity"))
            .json(&json!({"login":"alice","role_name":role}))
            .send()
            .await
            .unwrap();
        let reconciled: Value = call("GithubCreate", id.into(), "create", command.clone())
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(reconciled["outcome"]["kind"], "already_collaborator");
        let calls: Value = client
            .get(format!("{base}/calls"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            calls["requests"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|c| c["method"] == "PUT")
                .count(),
            1
        );
    }
    // #65: approval and scope belong to A even when delivery resumes through B.
    client.delete(format!("{base}/reset")).send().await.unwrap();
    let historical_link = ghinvite_core::InvitationLinkId::new();
    let created = call("InvitationLink", historical_link.to_string(), "create", json!({
        "link_id":historical_link,"account_id":100,"installation_id":9,
        "admin":{"account_id":100,"user_id":7},"description":"Reinstalled account",
        "approval_required":false,"permission":"push","repos":[{"repo_id":10,"repo_full_name":"acme/api"}]
    })).send().await.unwrap();
    assert!(created.status().is_success());
    let admitted: Value = call(
        "InvitationLink",
        historical_link.to_string(),
        "admit",
        json!({
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
        "InvitationLink",
        historical_link.to_string(),
        "prepare_dispatch",
        query.clone(),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let command = plan["commands"][0].clone();
    let id = command["invitation_id"].as_str().unwrap();
    storage
        .mark_installation_uninstalled(9, chrono::Utc::now())
        .await
        .unwrap();
    let blocked: Value = call("GithubCreate", id.into(), "create", command.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(blocked["outcome"]["kind"], "blocked");
    let mut replacement = storage.get_installation(9).await.unwrap().unwrap();
    replacement.installation_id = 19;
    replacement.installed_at = chrono::Utc::now();
    replacement.uninstalled_at = None;
    storage.insert_installation(&replacement).await.unwrap();
    let delivered: Value = call("GithubCreate", id.into(), "create", command.clone())
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
    client
        .delete(format!(
            "{base}/repos/acme/api/invitations/{}",
            delivered["outcome"]["upstream_id"].as_u64().unwrap()
        ))
        .send()
        .await
        .unwrap();
    client
        .post(format!("{base}/identity"))
        .json(&json!({"login":"alice","addressed_id":8,"role_name":"write"}))
        .send()
        .await
        .unwrap();
    // A stale/corrupt local installation mapping must not grant another account
    // authority to observe this request, even if its repo/login evidence matches.
    client
        .post(format!("{base}/installation-identity"))
        .json(&json!({"id":200,"login":"acme","type":"Organization"}))
        .send()
        .await
        .unwrap();
    run_sweep(&client, &ingress).await;
    assert_eq!(
        storage
            .get_github_invitation(id.parse().unwrap())
            .await
            .unwrap()
            .unwrap()
            .state,
        ghinvite_core::InvitationState::Sent,
        "another account cannot supply observation authority"
    );
    client
        .post(format!("{base}/installation-identity"))
        .json(&json!({"id":100,"login":"acme","type":"Organization"}))
        .send()
        .await
        .unwrap();
    credentials_unavailable.store(true, std::sync::atomic::Ordering::SeqCst);
    github_tokens.lock().unwrap().clear();
    run_sweep(&client, &ingress).await;
    assert_eq!(
        storage
            .get_github_invitation(id.parse().unwrap())
            .await
            .unwrap()
            .unwrap()
            .state,
        ghinvite_core::InvitationState::Sent,
        "unavailable credentials must preserve recoverability"
    );
    let tokens = github_tokens.lock().unwrap().clone();
    assert!(!tokens.is_empty());
    assert!(
        tokens
            .iter()
            .all(|token| token == "Bearer test-token-for-19"),
        "{tokens:?}"
    );
    credentials_unavailable.store(false, std::sync::atomic::Ordering::SeqCst);
    repository_reassigned.store(true, std::sync::atomic::Ordering::SeqCst);
    run_sweep(&client, &ingress).await;
    assert_eq!(
        storage
            .get_github_invitation(id.parse().unwrap())
            .await
            .unwrap()
            .unwrap()
            .state,
        ghinvite_core::InvitationState::Sent,
        "a reused repository name cannot supply evidence"
    );
    repository_reassigned.store(false, std::sync::atomic::Ordering::SeqCst);
    run_sweep(&client, &ingress).await;
    let settled = storage
        .get_github_invitation(id.parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        settled.state,
        ghinvite_core::InvitationState::Accepted,
        "active replacement sweeps must discover the historical invitation"
    );
    assert_eq!(
        settled.github_invitation_id,
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
            .unwrap(),
        original_deliveries
    );
    let retained_plan: Value = call(
        "InvitationLink",
        historical_link.to_string(),
        "prepare_dispatch",
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
    assert_eq!(settlements[0].metadata, json!({"reconciled":true}));
    // A rate limit is GitHub declining to answer: it blocks rather than failing
    // the invitation, and its recheck follows GitHub's own retry guidance rather
    // than the slow dependency cadence. The object lock is released for that
    // wait, so the status reads below do not queue behind the retry.
    let throttled_link = ghinvite_core::InvitationLinkId::new();
    call("InvitationLink", throttled_link.to_string(), "create", json!({
        "link_id":throttled_link,"account_id":100,"installation_id":19,
        "admin":{"account_id":100,"user_id":7},"description":"Throttled delivery",
        "approval_required":false,"permission":"push","repos":[{"repo_id":10,"repo_full_name":"acme/api"}]
    })).send().await.unwrap();
    let key = throttled_link.to_string();
    let admit = json!({"link_id":throttled_link,"requester_id":8,"operation_id":RequestId::new()});
    let admitted = call("InvitationLink", key.clone(), "admit", admit);
    let admitted: Value = admitted.send().await.unwrap().json().await.unwrap();
    let query = json!({"link_id":throttled_link,"request_id":admitted["result"]["request_id"],"requester_id":8});
    let plan = call("InvitationLink", key, "prepare_dispatch", query);
    let plan: Value = plan.send().await.unwrap().json().await.unwrap();
    let command = plan["commands"][0].clone();
    let id = command["invitation_id"].as_str().unwrap();
    client
        .post(format!("{base}/outcomes"))
        .json(&json!({"owner":"acme","repo":"api","user":"alice","outcome":"throttled_once"}))
        .send()
        .await
        .unwrap();
    let throttled: Value = call("GithubCreate", id.into(), "create", command.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        throttled["outcome"],
        json!({"kind":"blocked","reason":"GitHub throttled delivery"})
    );
    let sending = storage
        .get_github_invitation(id.parse().unwrap())
        .await
        .unwrap();
    assert_eq!(
        sending.unwrap().state,
        ghinvite_core::InvitationState::Sending,
        "a rate limit never settles an invitation"
    );
    // The stub asked for one second; the recheck that wait scheduled delivers
    // without any further command from us.
    let mut recovered = Value::Null;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let status = client.post(format!("{ingress}/GithubCreate/{id}/status"));
        recovered = status.send().await.unwrap().json().await.unwrap();
        if recovered["outcome"]["kind"] == "created" {
            break;
        }
    }
    assert_eq!(recovered["outcome"]["kind"], "created", "{recovered}");
    let delivered = storage
        .get_github_invitation(id.parse().unwrap())
        .await
        .unwrap();
    let upstream = delivered.unwrap().github_invitation_id;
    assert_eq!(upstream, recovered["outcome"]["upstream_id"].as_u64());
    assert_create_audit(&*storage, &recovered).await.unwrap();
    server.abort();
    stub_task.abort();
}

async fn run_sweep(client: &reqwest::Client, ingress: &str) {
    let response = client
        .post(format!("{ingress}/Reconcile/daily_run"))
        .json(&json!({"at": chrono::Utc::now()}))
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
}

async fn assert_create_audit(
    storage: &impl Storage,
    receipt: &Value,
) -> Option<ghinvite_core::audit::AuditEvent> {
    use ghinvite_core::{
        audit::{ActorKind, EventType, TargetKind},
        storage::AuditPosition,
    };
    let events = storage
        .list_audit_events(100, None, AuditPosition::Latest)
        .await
        .unwrap()
        .events;
    let events: Vec<_> = events
        .into_iter()
        .filter(|event| event.target_id == receipt["command"]["invitation_id"].as_str().unwrap())
        // Only create outcomes; a later settlement publishes its own event.
        .filter(|event| event.event_type != EventType::InvitationDeclined)
        .collect();
    let (kind, actor) = match receipt["outcome"]["kind"].as_str().unwrap() {
        "created" => (EventType::InvitationSent, ActorKind::System),
        "already_collaborator" => (EventType::InvitationAccepted, ActorKind::Github),
        "failed" => (EventType::InvitationSendFailed, ActorKind::System),
        _ => {
            assert!(events.is_empty());
            return None;
        }
    };
    assert_eq!(events.len(), 1);
    let event = events.into_iter().next().unwrap();
    assert_eq!(event.event_type, kind);
    assert_eq!(event.actor_kind, actor);
    assert_eq!(event.actor_id, None);
    assert_eq!(event.target_kind, TargetKind::GithubInvitation);
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
