//! Public ingress + real SQL + GitHub HTTP boundary acceptance for #56.
#![cfg(feature = "integration")]
use ghinvite_core::{RequestId, storage::Storage};
use restate_sdk::{endpoint::Endpoint, http_server::HttpServer};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

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
                if let Some(token) = request.headers().get("authorization").and_then(|h| h.to_str().ok())
                    && token.starts_with("Bearer test-token-for-") {
                    tokens.lock().unwrap().push(token.to_owned());
                    if unavailable.load(std::sync::atomic::Ordering::SeqCst) {
                        return axum::response::Response::builder().status(401).body(axum::body::Body::empty()).unwrap();
                    }
                }
                if request.uri().path() == "/repos/acme/api" && reassigned.load(std::sync::atomic::Ordering::SeqCst) {
                    return axum::response::IntoResponse::into_response(axum::Json(json!({"id":999,"full_name":"acme/api","private":true})));
                }
                next.run(request).await
            }
        },
    ));
    let stub_task = tokio::spawn(async move {
        axum::serve(stub, router)
            .await
            .unwrap();
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
    let builder = ghinvite_workflows::admission_v1::bind_protocol_fixture(Endpoint::builder());
    let builder = ghinvite_workflows::projection_v1::bind(builder, storage.clone());
    let workflow_faults =
        Arc::new(ghinvite_workflows::request_lifecycle_v1::WorkflowFaults::default());
    let builder = ghinvite_workflows::request_lifecycle_v1::bind_with_faults(
        builder,
        workflow_faults.clone(),
    );
    let faults = Arc::new(ghinvite_workflows::delivery_v1::DeliveryFaults::default());
    faults
        .lose_http_result
        .store(true, std::sync::atomic::Ordering::SeqCst);
    faults
        .lose_projection_ack
        .store(true, std::sync::atomic::Ordering::SeqCst);
    use ghinvite_workflows::{github_invitation::GithubInvitation as _, reconcile::Reconcile as _};
    let builder = builder
        .bind(
            ghinvite_workflows::github_invitation::GithubInvitationImpl {
                state: state.clone(),
            }
            .serve(),
        )
        .bind(
            ghinvite_workflows::reconcile::ReconcileImpl {
                state: state.clone(),
            }
            .serve(),
        );
    let endpoint =
        ghinvite_workflows::delivery_v1::bind_with_faults(builder, state, faults.clone()).build();
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
    let creation = json!({"version":1,"link_id":link,"account_id":100,"installation_id":9,"admin":{"account_id":100,"user_id":7},
        "description":"Delivery test","approval_required":false,"permission":"push","repos":[{"repo_id":10,"repo_full_name":"acme/api"}]});
    let response = call("InvitationLinkV1", link.to_string(), "create", creation)
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    let receipt: Value = call(
        "InvitationLinkV1",
        link.to_string(),
        "admit",
        json!({"version":1,"link_id":link,"requester_id":8,"operation_id":RequestId::new()}),
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
        "InvitationLinkV1",
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
            "{ingress}/restate/workflow/InvitationRequestV1/{request}/attach"
        ))
        .send()
        .await
        .unwrap();
    assert!(completed.status().is_success());
    let checkpoint: Value = call(
        "InvitationLinkV1",
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
    let response = call("GithubCreateV1", id.clone(), "create", command.clone())
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
    let replay: Value = call("GithubCreateV1", id.clone(), "create", command.clone())
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
        call("GithubCreateV1", id, "create", changed)
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
        call("InvitationLinkV1",link.to_string(),"create",json!({"version":1,"link_id":link,"account_id":100,"installation_id":9,"admin":{"account_id":100,"user_id":7},
            "description":"Other result","approval_required":false,"permission":"push","repos":[{"repo_id":10,"repo_full_name":"acme/api"}]})).send().await.unwrap();
        let admitted: Value = call(
            "InvitationLinkV1",
            link.to_string(),
            "admit",
            json!({"version":1,"link_id":link,"requester_id":8,"operation_id":RequestId::new()}),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        let request = admitted["result"]["request_id"].as_str().unwrap();
        let plan: Value = call(
            "InvitationLinkV1",
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
        let result: Value = call("GithubCreateV1", id.clone(), "create", command.clone())
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(result["outcome"]["kind"], expected);
        assert_create_audit(&*storage, &result).await;
        let replay: Value = call("GithubCreateV1", id, "create", command)
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
            .json(&json!({"query":format!("SELECT id FROM sys_invocation WHERE target_service_name = 'InvitationRequestV1' AND target_service_key = '{request}'")}))
            .send().await.unwrap().json().await.unwrap();
        if response["rows"].as_array().unwrap().is_empty() { break; }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }}).await.expect("actual workflow retention cleanup");
    let response = client
        .post(format!("{ingress}/DeliveryRecoveryV1/recover"))
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
    storage
        .update_github_invitation(&ghinvite_core::storage::GithubInvitationUpdate {
            id: command["invitation_id"].as_str().unwrap().parse().unwrap(),
            state: ghinvite_core::InvitationState::Declined,
            github_invitation_id: None,
            error_message: None,
            updated_at: chrono::Utc::now(),
        })
        .await
        .unwrap();
    let replay: Value = call(
        "GithubCreateV1",
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
        "InvitationLinkV1",
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
        call("InvitationRequestV1", request.into(), "notify", signal)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let promise: Value = client
        .post(format!(
            "{ingress}/InvitationRequestV1/{request}/notification_status"
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
    call("InvitationLinkV1",partial_link.to_string(),"create",json!({"version":1,"link_id":partial_link,"account_id":100,"installation_id":9,"admin":{"account_id":100,"user_id":7},
        "description":"Partial fanout","approval_required":false,"permission":"push","repos":[{"repo_id":10,"repo_full_name":"acme/api"},{"repo_id":11,"repo_full_name":"acme/web"}]})).send().await.unwrap();
    let admitted: Value = call("InvitationLinkV1",partial_link.to_string(),"admit",json!({"version":1,"link_id":partial_link,"requester_id":8,"operation_id":RequestId::new()})).send().await.unwrap().json().await.unwrap();
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
        "InvitationLinkV1",
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
    assert!(
        call(
            "InvitationLinkV1",
            partial_link.to_string(),
            "retain_dispatch",
            partial_plan.clone()
        )
        .send()
        .await
        .unwrap()
        .status()
        .is_success()
    );
    let mut changed_plan = partial_plan.clone();
    changed_plan["commands"][0]["invitation_id"] = json!(ghinvite_core::GithubInvitationId::new());
    assert_eq!(
        call(
            "InvitationLinkV1",
            partial_link.to_string(),
            "retain_dispatch",
            changed_plan
        )
        .send()
        .await
        .unwrap()
        .status(),
        409
    );
    let first = partial_plan["commands"][0].clone();
    call(
        "GithubCreateV1",
        first["invitation_id"].as_str().unwrap().into(),
        "create",
        first.clone(),
    )
    .send()
    .await
    .unwrap();
    assert!(
        call(
            "InvitationLinkV1",
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
        "InvitationLinkV1",
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
        .post(format!("{ingress}/DeliveryRecoveryV1/recover"))
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
            "{ingress}/restate/workflow/InvitationRequestV1/{partial_request}/attach"
        ))
        .send()
        .await
        .unwrap();
    for command in partial_plan["commands"].as_array().unwrap() {
        let response = call(
            "GithubCreateV1",
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
        "GithubCreateV1",
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
            "GithubCreateV1",
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
        "InvitationLinkV1",
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
        "InvitationLinkV1",
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
    // Import a known Sent identity before first fan-out. Preparation must never
    // allocate replacement IDs, even when the SQL receipt predates Restate.
    client.delete(format!("{base}/reset")).send().await.unwrap();
    workflow_faults
        .pause_before_dispatch
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let imported_link = ghinvite_core::InvitationLinkId::new();
    call("InvitationLinkV1",imported_link.to_string(),"create",json!({"version":1,"link_id":imported_link,"account_id":100,"installation_id":9,"admin":{"account_id":100,"user_id":7},
        "description":"Imported dispatch","approval_required":false,"permission":"push","repos":[{"repo_id":10,"repo_full_name":"acme/api"}]})).send().await.unwrap();
    let admitted: Value = call("InvitationLinkV1",imported_link.to_string(),"admit",json!({"version":1,"link_id":imported_link,"requester_id":8,"operation_id":RequestId::new()})).send().await.unwrap().json().await.unwrap();
    let imported_request = admitted["result"]["request_id"].as_str().unwrap();
    let query = json!({"link_id":imported_link,"request_id":imported_request,"requester_id":8});
    let snapshot: Value = call(
        "InvitationLinkV1",
        imported_link.to_string(),
        "request_status",
        query.clone(),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let imported_id = ghinvite_core::GithubInvitationId::new();
    let command = json!({"version":1,"invitation_id":imported_id,"link_id":imported_link,"request_id":imported_request,
        "approval_id":snapshot["decision"]["decision_id"],"account_id":100,"installation_id":9,"requester_id":8,
        "repo_id":10,"repo_full_name":"acme/api","permission":"push","approved_at":snapshot["decision"]["effective_at"]});
    let imported_plan = json!({"dispatch_id":format!("v1/dispatch/{imported_request}"),"input":{"version":1,"request":snapshot,
        "installation_id":9,"repos":[{"repo_id":10,"repo_full_name":"acme/api"}],"permission":"push","approval_required":false},"commands":[command]});
    let response = call(
        "InvitationLinkV1",
        imported_link.to_string(),
        "retain_dispatch",
        imported_plan.clone(),
    )
    .send()
    .await
    .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while storage
            .get_invitation_request(imported_request.parse().unwrap())
            .await
            .unwrap()
            .is_none()
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    storage
        .insert_github_invitation(&ghinvite_core::GithubInvitation {
            id: imported_id,
            invitation_request_id: imported_request.parse().unwrap(),
            repo_id: 10,
            github_invitation_id: Some(99123),
            state: ghinvite_core::InvitationState::Sent,
            error_message: None,
            created_at: now,
            updated_at: now,
        })
        .await
        .unwrap();
    workflow_faults
        .pause_before_dispatch
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let result: Value = call(
        "GithubCreateV1",
        imported_id.to_string(),
        "create",
        imported_plan["commands"][0].clone(),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(
        result["outcome"],
        json!({"kind":"created","upstream_id":99123})
    );
    assert_eq!(result["recovered"], true);
    assert_create_audit(&*storage, &result).await;
    // A retained pre-#64 receipt gains a stable recovery observation on replay.
    let mut old = result.clone();
    old.as_object_mut().unwrap().remove("confirmed_at");
    old.as_object_mut().unwrap().remove("recovered");
    let old_id = ghinvite_core::GithubInvitationId::new();
    old["command"]["invitation_id"] = json!(old_id);
    let imported = call(
        "GithubCreateV1",
        old_id.to_string(),
        "import_receipt",
        json!({
            "migration_id":"audit-recovery", "manifest_checksum":"a".repeat(64), "receipt":old
        }),
    )
    .send()
    .await
    .unwrap();
    assert!(imported.status().is_success());
    let recovered: Value = call(
        "GithubCreateV1",
        old_id.to_string(),
        "create",
        old["command"].clone(),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(recovered["recovered"], true);
    let event = assert_create_audit(&*storage, &recovered).await.unwrap();
    let replay: Value = call(
        "GithubCreateV1",
        old_id.to_string(),
        "create",
        old["command"].clone(),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(replay, recovered);
    assert_eq!(
        assert_create_audit(&*storage, &replay).await.unwrap(),
        event
    );
    let retained: Value = call(
        "InvitationLinkV1",
        imported_link.to_string(),
        "prepare_dispatch",
        query,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(retained, imported_plan);
    let calls: Value = client
        .get(format!("{base}/calls"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(calls["count"], 0);
    // Explicit 403 proves rejection, so restoration may safely try again.
    workflow_faults
        .pause_before_dispatch
        .store(true, std::sync::atomic::Ordering::SeqCst);
    client
        .post(format!("{base}/outcomes"))
        .json(&json!({"owner":"acme","repo":"api","user":"alice","outcome":"access_lost_once"}))
        .send()
        .await
        .unwrap();
    let rejected_link = ghinvite_core::InvitationLinkId::new();
    call("InvitationLinkV1",rejected_link.to_string(),"create",json!({"version":1,"link_id":rejected_link,"account_id":100,"installation_id":9,"admin":{"account_id":100,"user_id":7},
        "description":"Access rejection","approval_required":false,"permission":"push","repos":[{"repo_id":10,"repo_full_name":"acme/api"}]})).send().await.unwrap();
    let admitted: Value = call("InvitationLinkV1",rejected_link.to_string(),"admit",json!({"version":1,"link_id":rejected_link,"requester_id":8,"operation_id":RequestId::new()})).send().await.unwrap().json().await.unwrap();
    let rejected_request = admitted["result"]["request_id"].as_str().unwrap();
    let query = json!({"link_id":rejected_link,"request_id":rejected_request,"requester_id":8});
    let progress: Value = call(
        "InvitationLinkV1",
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
        "InvitationLinkV1",
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
        "InvitationLinkV1",
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
    let rejected: Value = call("GithubCreateV1", id.into(), "create", command.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(rejected["outcome"]["kind"], "blocked");
    assert_create_audit(&*storage, &rejected).await;
    let sweep = client
        .post(format!("{ingress}/Reconcile/daily_run_v1"))
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
    let resumed: Value = call("GithubCreateV1", id.into(), "create", command.clone())
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
    let response = call(
        "GithubInvitation",
        id.into(),
        "on_webhook_v1/send",
        json!({"invitation_id":id,"action":"accepted","at":chrono::Utc::now()}),
    )
    .send()
    .await
    .unwrap();
    assert!(response.status().is_success());
    let invocation: Value = response.json().await.unwrap();
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
    let attached = client
        .get(format!(
            "{ingress}/restate/invocation/{}/attach",
            invocation["invocationId"].as_str().unwrap()
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
    let (a, b) = tokio::join!(
        call(
            "GithubInvitation",
            id.into(),
            "reconcile_v1",
            evidence.clone()
        )
        .send(),
        call("GithubInvitation", id.into(), "reconcile_v1", evidence).send()
    );
    assert!(a.unwrap().status().is_success());
    assert!(b.unwrap().status().is_success());
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
        call("InvitationLinkV1",link.to_string(),"create",json!({"version":1,"link_id":link,"account_id":100,"installation_id":9,"admin":{"account_id":100,"user_id":7},
            "description":"Role reconciliation","approval_required":false,"permission":role,"repos":[{"repo_id":10,"repo_full_name":"acme/api"}]})).send().await.unwrap();
        let admitted: Value = call(
            "InvitationLinkV1",
            link.to_string(),
            "admit",
            json!({"version":1,"link_id":link,"requester_id":8,"operation_id":RequestId::new()}),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        let plan: Value = call(
            "InvitationLinkV1",
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
        let unknown: Value = call("GithubCreateV1", id.into(), "create", command.clone())
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
        let reconciled: Value = call("GithubCreateV1", id.into(), "create", command.clone())
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
    let created = call("InvitationLinkV1", historical_link.to_string(), "create", json!({
        "version":1,"link_id":historical_link,"account_id":100,"installation_id":9,
        "admin":{"account_id":100,"user_id":7},"description":"Reinstalled account",
        "approval_required":false,"permission":"push","repos":[{"repo_id":10,"repo_full_name":"acme/api"}]
    })).send().await.unwrap();
    assert!(created.status().is_success());
    let admitted: Value = call("InvitationLinkV1", historical_link.to_string(), "admit", json!({
        "version":1,"link_id":historical_link,"requester_id":8,"operation_id":RequestId::new()
    })).send().await.unwrap().json().await.unwrap();
    let query = json!({"link_id":historical_link,"request_id":admitted["result"]["request_id"],"requester_id":8});
    let plan: Value = call("InvitationLinkV1", historical_link.to_string(), "prepare_dispatch", query.clone())
        .send().await.unwrap().json().await.unwrap();
    let command = plan["commands"][0].clone();
    let id = command["invitation_id"].as_str().unwrap();
    storage.mark_installation_uninstalled(9, chrono::Utc::now()).await.unwrap();
    let blocked: Value = call("GithubCreateV1", id.into(), "create", command.clone())
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(blocked["outcome"]["kind"], "blocked");
    let mut replacement = storage.get_installation(9).await.unwrap().unwrap();
    replacement.installation_id = 19;
    replacement.installed_at = chrono::Utc::now();
    replacement.uninstalled_at = None;
    storage.insert_installation(&replacement).await.unwrap();
    let delivered: Value = call("GithubCreateV1", id.into(), "create", command.clone())
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(delivered["outcome"]["kind"], "created");
    assert_eq!(delivered["command"], command);
    let original_link = storage.get_invitation_link_by_id(historical_link).await.unwrap().unwrap();
    let original_request = storage.get_invitation_request(command["request_id"].as_str().unwrap().parse().unwrap()).await.unwrap().unwrap();
    let original_deliveries = storage.list_delivery_for_request(original_request.id).await.unwrap();
    let original_audit = assert_create_audit(&*storage, &delivered).await.unwrap();
    // GitHub acceptance without a webhook: remove pending evidence, expose access.
    client.delete(format!("{base}/repos/acme/api/invitations/{}", delivered["outcome"]["upstream_id"].as_u64().unwrap()))
        .send().await.unwrap();
    client.post(format!("{base}/identity")).json(&json!({"login":"alice","addressed_id":8,"role_name":"write"}))
        .send().await.unwrap();
    // A stale/corrupt local installation mapping must not grant another account
    // authority to observe this request, even if its repo/login evidence matches.
    client.post(format!("{base}/installation-identity"))
        .json(&json!({"id":200,"login":"acme","type":"Organization"}))
        .send().await.unwrap();
    run_sweep(&client, &ingress).await;
    assert_eq!(storage.get_github_invitation(id.parse().unwrap()).await.unwrap().unwrap().state,
        ghinvite_core::InvitationState::Sent, "another account cannot supply observation authority");
    client.post(format!("{base}/installation-identity"))
        .json(&json!({"id":100,"login":"acme","type":"Organization"}))
        .send().await.unwrap();
    credentials_unavailable.store(true, std::sync::atomic::Ordering::SeqCst);
    github_tokens.lock().unwrap().clear();
    run_sweep(&client, &ingress).await;
    assert_eq!(storage.get_github_invitation(id.parse().unwrap()).await.unwrap().unwrap().state,
        ghinvite_core::InvitationState::Sent, "unavailable credentials must preserve recoverability");
    let tokens = github_tokens.lock().unwrap().clone();
    assert!(!tokens.is_empty());
    assert!(tokens.iter().all(|token| token == "Bearer test-token-for-19"), "{tokens:?}");
    credentials_unavailable.store(false, std::sync::atomic::Ordering::SeqCst);
    repository_reassigned.store(true, std::sync::atomic::Ordering::SeqCst);
    run_sweep(&client, &ingress).await;
    assert_eq!(storage.get_github_invitation(id.parse().unwrap()).await.unwrap().unwrap().state,
        ghinvite_core::InvitationState::Sent, "a reused repository name cannot supply evidence");
    repository_reassigned.store(false, std::sync::atomic::Ordering::SeqCst);
    run_sweep(&client, &ingress).await;
    let settled = storage.get_github_invitation(id.parse().unwrap()).await.unwrap().unwrap();
    assert_eq!(settled.state, ghinvite_core::InvitationState::Accepted,
        "active replacement sweeps must discover the historical invitation");
    assert_eq!(settled.github_invitation_id, delivered["outcome"]["upstream_id"].as_u64());
    assert_eq!(storage.get_invitation_link_by_id(historical_link).await.unwrap().unwrap(), original_link);
    assert_eq!(original_link.installation_id, 9);
    assert_eq!(storage.get_invitation_request(original_request.id).await.unwrap().unwrap(), original_request);
    assert_eq!(storage.list_delivery_for_request(original_request.id).await.unwrap(), original_deliveries);
    let retained_plan: Value = call("InvitationLinkV1", historical_link.to_string(), "prepare_dispatch", query)
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(retained_plan, plan);
    let events = storage.list_audit_events(100, None, ghinvite_core::storage::AuditPosition::Latest).await.unwrap().events;
    assert!(events.contains(&original_audit));
    let settlements: Vec<_> = events.iter().filter(|event| event.target_id == id && event.event_type == ghinvite_core::audit::EventType::InvitationAccepted).collect();
    assert_eq!(settlements.len(), 1);
    assert_eq!(settlements[0].metadata, json!({"reconciled":true}));
    server.abort();
    stub_task.abort();
}

async fn run_sweep(client: &reqwest::Client, ingress: &str) {
    let response = client
        .post(format!("{ingress}/Reconcile/daily_run_v1"))
        .json(&json!({"at": chrono::Utc::now()}))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{}", response.text().await.unwrap());
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
