//! Approved delivery's public Restate/HTTP seam, with read projections withheld.
#![cfg(feature = "integration")]
use ghinvite_core::storage::AppInstallationReadStorage as _;
use ghinvite_core::storage::{ConsoleStorage, RecordStorage};
use restate_sdk::{endpoint::Endpoint, http_server::HttpServer};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

#[tokio::test]
async fn approved_delivery_progresses_without_projected_parents() {
    let harness = ghinvite_e2e::Harness::start().await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let database =
        std::env::temp_dir().join(format!("approved-{}.db", ghinvite_core::RequestId::new()));
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::at_path(&database)
            .await
            .unwrap(),
    );
    let sql = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new().filename(&database),
    )
    .await
    .unwrap();
    sqlx::query("CREATE TRIGGER hold_installation BEFORE INSERT ON installations BEGIN SELECT RAISE(ABORT, 'projection withheld'); END")
        .execute(&sql).await.unwrap();
    let stub = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", stub.local_addr().unwrap());
    let lose_fence_after_put = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observation_reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fail_observations = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let wrong_repository_owner = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let router = {
        let sql = sql.clone();
        let wrong_owner = wrong_repository_owner.clone();
        let lose_fence = lose_fence_after_put.clone();
        let reads = observation_reads.clone();
        let fail = fail_observations.clone();
        ghinvite_github::stub::router().layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let sql = sql.clone();
                let wrong_owner = wrong_owner.clone();
                let lose_fence = lose_fence.clone();
                let reads = reads.clone();
                let fail = fail.clone();
                async move {
                    if request.uri().path()=="/repos/acme/web" && wrong_owner.load(std::sync::atomic::Ordering::SeqCst) {
                        return axum::response::Response::builder().status(200).header("content-type","application/json").body(axum::body::Body::from(json!({"id":11,"full_name":"acme/web","owner":{"id":200,"login":"acme"}}).to_string())).unwrap();
                    }
                    let put = request.method() == axum::http::Method::PUT;
                    if request.method() == axum::http::Method::GET
                        && request.uri().path() == "/repos/acme/api/invitations"
                    {
                        reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        if fail.load(std::sync::atomic::Ordering::SeqCst) {
                            return axum::response::Response::builder()
                                .status(429)
                                .header("retry-after", "1")
                                .body(axum::body::Body::from("{}"))
                                .unwrap();
                        }
                    }
                    let response = next.run(request).await;
                    if put && lose_fence.swap(false, std::sync::atomic::Ordering::SeqCst) {
                        sqlx::query(
                            "ALTER TABLE delivery_attempts RENAME TO delivery_attempts_offline",
                        )
                        .execute(&sql)
                        .await
                        .unwrap();
                    }
                    response
                }
            },
        ))
    };
    let stub_server = tokio::spawn(async move { axum::serve(stub, router).await.unwrap() });
    let state = ghinvite_workflows::AppState::new(storage.clone()).with_github_apps(Arc::new(
        ghinvite_workflows::RepositoryGithub::new(
            ghinvite_e2e::github_transport(client.clone()),
            &base,
            ghinvite_e2e::repository_apps(&base),
        )
        .unwrap(),
    ));
    let builder = Endpoint::builder()
        .bind(ghinvite_workflows::installation::Installation {
            state: state.clone(),
        })
        .bind(ghinvite_workflows::availability::AccountInstallation {
            state: state.clone(),
        })
        .bind(ghinvite_workflows::availability::InstallationProjection {
            state: state.clone(),
        });
    let builder = ghinvite_workflows::admission::bind_protocol_fixture(builder);
    let builder = ghinvite_workflows::projection::bind(builder, storage.clone());
    let builder = ghinvite_workflows::request_owner::bind(builder);
    let faults = Arc::new(ghinvite_workflows::delivery::DeliveryFaults::default());
    let endpoint =
        ghinvite_workflows::delivery::bind_with_faults(builder, state, faults.clone()).build();
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        HttpServer::new(endpoint)
            .serve_with_cancel(listener, std::future::pending::<()>())
            .await;
    });
    let admin = harness.restate.admin_url().to_owned();
    let ingress = harness.restate.ingress_url().to_owned();
    let host = std::env::var("RESTATE_ENDPOINT_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let response = client
        .post(format!("{admin}/deployments"))
        .json(&json!({"uri":format!("http://{host}:{port}")}))
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    // An early/stale timer must not manufacture known-uninstalled authority.
    post(
        &client,
        &format!("{ingress}/AccountInstallation/repo:100/recheck"),
        Value::Null,
    )
    .await;
    assert_eq!(
        client
            .post(format!(
                "{ingress}/AccountInstallation/repo:100/current_installation"
            ))
            .send()
            .await
            .unwrap()
            .status(),
        503
    );
    assert_eq!(
        client
            .post(format!(
                "{ingress}/AccountInstallation/repo:100/admission_evidence"
            ))
            .json(&json!({"account_id":100,"repo_ids":[10]}))
            .send()
            .await
            .unwrap()
            .status(),
        503
    );
    post(
        &client,
        &format!("{base}/installation-repositories"),
        json!([10]),
    )
    .await;
    let onboard = ghinvite_e2e::setup_command(
        json!({"app":"repo","installation_id":9,"actor_user_id":7,"account_id":100,"account_login":"acme","account_type":"Organization","selected_repos":"all","installed_at":chrono::Utc::now()}),
    );
    let response = client
        .post(format!("{ingress}/Installation/repo:9/onboard"))
        .json(&onboard)
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    let link = ghinvite_core::InvitationLinkId::new();
    let call = |handler: &str, input: Value| {
        let route = if handler == "approved_plan" {
            format!(
                "InvitationRequest/{}/approved_plan",
                input["request_id"].as_str().unwrap()
            )
        } else {
            format!("InvitationLink/{link}/{handler}")
        };
        client.post(format!("{ingress}/{route}")).json(&input)
    };
    let response = call("create", json!({"max_uses":1000,"secret_sha256":"2a".repeat(32),"link_id":link,"account_id":100,"installation_id":9,"admin":{"account_id":100,"user_id":7},"description":"Withheld parents","approval_required":false,"targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/api","grant":"write"},{"repo_id":11,"repo_full_name":"acme/web","grant":"write"}]}})).send().await.unwrap();
    assert!(response.status().is_success());
    let created: Value = response.json().await.unwrap();
    assert!(created["Ok"].is_object());
    let admitted: Value = call(
        "admit",
        json!({"scope_evidence":[],"secret_sha256":"2a".repeat(32),"secret_generation":1,"link_id":link,"requester_id":8,"operation_id":ghinvite_core::RequestId::new()}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(admitted["result"]["kind"], "accepted");
    let plan: Value = call(
        "approved_plan",
        json!({"link_id":link,"request_id":admitted["result"]["request_id"],"requester_id":8}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(plan["commands"].as_array().unwrap().len(), 2);
    let command = plan["commands"][0].clone();
    assert!(command.get("invitation_id").is_none());
    let delivery_key = format!(
        "{}:{}:{}",
        command["request_id"].as_str().unwrap(),
        command["repo_id"],
        command["delivery_generation"]
    );
    let response = client
        .post(format!(
            "{ingress}/RepositoryDelivery/{delivery_key}/create"
        ))
        .json(&command)
        .send()
        .await
        .expect("delivery must complete while parents are withheld");
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    let receipt: Value = response.json().await.unwrap();
    assert_eq!(receipt["outcome"]["kind"], "created");
    assert_eq!(receipt["command"], command);
    client
        .delete(format!(
            "{base}/repos/acme/api/invitations/{}",
            receipt["outcome"]["upstream_id"]
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    post(
        &client,
        &format!("{base}/identity"),
        json!({"login":"alice","addressed_id":8,"role_name":"write"}),
    )
    .await;
    // Settlement is retained beside the original create even while every
    // projected parent is unavailable. Replays return that original receipt.
    let pending = post(
        &client,
        &format!("{ingress}/RepositoryDelivery/{delivery_key}/status"),
        Value::Null,
    )
    .await;
    post(
        &client,
        &format!("{ingress}/DeliveryObservation/observe"),
        pending,
    )
    .await;
    let snapshot = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = post(
                &client,
                &format!("{ingress}/RepositoryDelivery/{delivery_key}/status"),
                Value::Null,
            )
            .await;
            if snapshot["settlement"]["state"] == "accepted" {
                break snapshot;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("read-only acceptance verification completes");
    assert_eq!(snapshot["create"], receipt);
    assert_eq!(snapshot["settlement"]["state"], "accepted");
    let replay = post(
        &client,
        &format!("{ingress}/RepositoryDelivery/{delivery_key}/create"),
        command.clone(),
    )
    .await;
    assert_eq!(replay, receipt);
    assert!(
        storage
            .app_installation(ghinvite_core::account::App::Repo, 9)
            .await
            .map(|value| value.map(|installation| installation.account))
            .unwrap()
            .is_none()
    );
    assert!(storage.get_user(8).await.unwrap().is_none());
    assert!(
        storage
            .get_invitation_link_by_id(link)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        storage
            .get_invitation_request(command["request_id"].as_str().unwrap().parse().unwrap())
            .await
            .unwrap()
            .is_none()
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
    let blocked_command = plan["commands"][1].clone();
    let blocked_url = format!(
        "{ingress}/RepositoryDelivery/{}:{}:{}/create",
        blocked_command["request_id"].as_str().unwrap(),
        blocked_command["repo_id"],
        blocked_command["delivery_generation"]
    );
    let blocked = post(&client, &blocked_url, blocked_command.clone()).await;
    assert_eq!(blocked["outcome"]["kind"], "blocked");
    assert_eq!(blocked["command"], blocked_command);
    // Exact first-use command validation: a minted arbitrary ID is not another
    // delivery obligation, and changed input cannot replace a bound command.
    let mut unauthorized = command.clone();
    unauthorized["invitation_id"] = json!(ghinvite_core::GithubInvitationId::new());
    let response = client
        .post(format!(
            "{ingress}/RepositoryDelivery/{delivery_key}/create"
        ))
        .json(&unauthorized)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 409);
    let mut changed = blocked_command.clone();
    changed["permission"] = json!("admin");
    assert_eq!(
        client
            .post(&blocked_url)
            .json(&changed)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    // Same-account reinstall resumes the original command, even while SQL is
    // entirely absent. Delayed old events cannot retire the replacement.
    post(
        &client,
        &format!("{ingress}/Installation/repo:9/uninstall"),
        json!({"app":"repo","installation_id":9,"uninstalled_at":chrono::Utc::now()}),
    )
    .await;
    post(
        &client,
        &format!("{base}/installation-repositories"),
        json!([10, 11]),
    )
    .await;
    let mut replacement = onboard.clone();
    replacement["operation_id"] = json!(ghinvite_core::RequestId::new());
    replacement["verified"]["account"]["installation_id"] = json!(19);
    post(
        &client,
        &format!("{ingress}/Installation/repo:19/onboard"),
        replacement.clone(),
    )
    .await;
    post(
        &client,
        &format!("{ingress}/Installation/repo:9/repos_changed"),
        json!({"app":"repo","installation_id":9,"selected_repos":[]}),
    )
    .await;
    post(
        &client,
        &format!("{ingress}/Installation/repo:9/uninstall"),
        json!({"app":"repo","installation_id":9,"uninstalled_at":chrono::Utc::now()}),
    )
    .await;
    // The installation-token repository read must bind the numeric owner.
    post(
        &client,
        &format!("{base}/identity"),
        json!({"login":"alice","addressed_id":8}),
    )
    .await;
    wrong_repository_owner.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        post(&client, &blocked_url, blocked_command.clone()).await["outcome"]["kind"],
        "blocked"
    );
    assert_eq!(puts(&client, &base).await, 1);
    wrong_repository_owner.store(false, std::sync::atomic::Ordering::SeqCst);
    post(
        &client,
        &format!("{base}/identity"),
        json!({"login":"renamed","addressed_id":99}),
    )
    .await;
    assert_eq!(
        post(&client, &blocked_url, blocked_command.clone()).await["outcome"]["kind"],
        "blocked"
    );
    assert_eq!(puts(&client, &base).await, 1);
    post(
        &client,
        &format!("{base}/identity"),
        json!({"login":"renamed","addressed_id":8}),
    )
    .await;
    let resumed = post(&client, &blocked_url, blocked_command.clone()).await;
    assert_eq!(resumed["outcome"]["kind"], "created");
    assert_eq!(resumed["command"], blocked_command);
    let owner_url = blocked_url.trim_end_matches("/create");
    let pending = post(&client, &format!("{owner_url}/status"), Value::Null).await;
    // A withdrawal authorized for the retired installation cannot use its replacement.
    refuse_cancel(&client, &format!("{owner_url}/cancel"), json!({"invitation_id":json!(ghinvite_core::GithubInvitationId::new()),"installation_id":9,"by_user":7,"at":chrono::Utc::now()})).await;
    assert_eq!(
        post(&client, &format!("{owner_url}/status"), Value::Null).await,
        pending
    );
    // A refused legacy cancellation cannot override exact canonical observation evidence.
    // Positive fenced cancellation races live in repository_protocol/cancellation.
    let evidence_url = format!("{owner_url}/reconcile");
    let declined = absence(&pending, "none");
    let cancel_url = format!("{owner_url}/cancel");
    tokio::join!(
        refuse_cancel(
            &client,
            &cancel_url,
            json!({"invitation_id":json!(ghinvite_core::GithubInvitationId::new()),"installation_id":19,"by_user":7,"at":chrono::Utc::now()})
        ),
        post(&client, &evidence_url, declined.clone())
    );
    let winner = post(&client, &format!("{owner_url}/status"), Value::Null).await;
    assert_eq!(winner["settlement"]["state"], "declined");
    assert_eq!(winner["create"], resumed);
    post(&client, &evidence_url, declined).await;
    assert_eq!(
        post(&client, &format!("{owner_url}/status"), Value::Null).await,
        winner
    );
    assert_eq!(
        post(&client, &blocked_url, blocked_command.clone()).await,
        resumed
    );
    let ledger: Value = client
        .get(format!("{base}/calls"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        ledger["requests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["path"] == "/repos/acme/web/collaborators/renamed" && c["method"] == "PUT")
    );
    assert!(
        ledger["requests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["path"] == "/app/installations/19/access_tokens")
    );
    assert_eq!(puts(&client, &base).await, 2);
    // Restore only the missing prerequisites: independently queued projections
    // establish all rows, receipts and immutable audit without another PUT.
    sqlx::query("DROP TRIGGER hold_installation")
        .execute(&sql)
        .await
        .unwrap();
    for user_id in [7, 8] {
        storage
            .upsert_user(&ghinvite_core::User {
                user_id,
                login: format!("user-{user_id}"),
                avatar_url: None,
                github_created_at: None,
                last_seen_at: chrono::Utc::now(),
            })
            .await
            .unwrap();
    }
    // Installation, link, request and delivery each have independent retry
    // backoff after the outage; allow the dependency chain to catch up.
    tokio::time::timeout(Duration::from_secs(75), async {
        loop {
            if storage
                .list_delivery_for_request(command["request_id"].as_str().unwrap().parse().unwrap())
                .await
                .unwrap()
                .len()
                == 2
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("independent receipt projection converges");
    assert_eq!(puts(&client, &base).await, 2);

    // Authoritative manual approval progresses while SQL still says Pending.
    client.delete(format!("{base}/reset")).send().await.unwrap();
    post(
        &client,
        &format!("{base}/identity"),
        json!({"login":"renamed","addressed_id":8}),
    )
    .await;
    let manual = ghinvite_core::InvitationLinkId::new();
    post(&client, &format!("{ingress}/InvitationLink/{manual}/create"), json!({"max_uses":1000,"secret_sha256":"2a".repeat(32),"link_id":manual,"account_id":100,"installation_id":19,"admin":{"account_id":100,"user_id":7},"description":"Stale pending","approval_required":true,"targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/api","grant":"write"}]}})).await;
    let admission = post(
        &client,
        &format!("{ingress}/InvitationLink/{manual}/admit"),
        json!({"scope_evidence":[],"secret_sha256":"2a".repeat(32),"secret_generation":1,"link_id":manual,"requester_id":8,"operation_id":ghinvite_core::RequestId::new()}),
    )
    .await;
    let request = admission["result"]["request_id"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        while storage
            .get_invitation_request(request.parse().unwrap())
            .await
            .unwrap()
            .is_none()
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    sqlx::query("CREATE TRIGGER hold_approval BEFORE UPDATE ON invitation_requests BEGIN SELECT RAISE(ABORT, 'stale pending'); END").execute(&sql).await.unwrap();
    // Fence outage holds the effect, not approval. The original invocation
    // resumes when the fence is restored; its identity is never reminted.
    sqlx::query("ALTER TABLE delivery_attempts RENAME TO delivery_attempts_offline")
        .execute(&sql)
        .await
        .unwrap();
    let signed = client
        .post(format!("{ingress}/Installation/repo:19/onboard_webhook"))
        .json(&ghinvite_e2e::signed_owner_command(&replacement, 7))
        .send()
        .await
        .unwrap();
    assert!(
        signed.status().is_success(),
        "{}",
        signed.text().await.unwrap()
    );
    post(&client, &format!("{ingress}/InvitationRequest/{request}/decide"), json!({"link_id":manual,"request_id":request,"operation_id":ghinvite_core::RequestId::new(),"admin":{"account_id":100,"user_id":7},"action":{"kind":"approve"}})).await;
    let manual_plan = post(
        &client,
        &format!("{ingress}/InvitationRequest/{request}/approved_plan"),
        json!({"link_id":manual,"request_id":request,"requester_id":8}),
    )
    .await;
    let manual_command = manual_plan["commands"][0].clone();
    let manual_url = format!(
        "{ingress}/RepositoryDelivery/{}:{}:{}/create",
        manual_command["request_id"].as_str().unwrap(),
        manual_command["repo_id"],
        manual_command["delivery_generation"]
    );
    assert_eq!(
        client
            .post(&manual_url)
            .json(&manual_command)
            .send()
            .await
            .unwrap()
            .status(),
        503,
        "fence outage before claim releases owner exclusivity"
    );
    assert_eq!(puts(&client, &base).await, 0);
    let recheck_url = manual_url.replace("/create", "/recheck");
    // Each failed wake must arrange a new generation even though preflight
    // failures publish no receipt. Redrive each old payload as a new invocation.
    for generation in [1, 2] {
        let wake = json!({"command":manual_command,"generation":generation});
        assert_eq!(
            client
                .post(&recheck_url)
                .json(&wake)
                .send()
                .await
                .unwrap()
                .status(),
            503,
            "the current wake performs the unavailable preflight"
        );
        for _ in 0..2 {
            post(&client, &recheck_url, wake.clone()).await;
        }
    }
    assert!(
        post(
            &client,
            &manual_url.replace("/create", "/status"),
            Value::Null
        )
        .await
        .is_null()
    );
    assert_eq!(puts(&client, &base).await, 0);
    assert_eq!(
        storage
            .get_invitation_request(request.parse().unwrap())
            .await
            .unwrap()
            .unwrap()
            .state,
        ghinvite_core::RequestState::Pending
    );
    post(
        &client,
        &format!("{base}/outcomes"),
        json!({"owner":"acme","repo":"api","user":"renamed","outcome":"created_then_declined"}),
    )
    .await;
    sqlx::query("ALTER TABLE delivery_attempts_offline RENAME TO delivery_attempts")
        .execute(&sql)
        .await
        .unwrap();
    // Recovery rechecks the fence before attempting the write.
    post(
        &client,
        &recheck_url,
        json!({"command":manual_command,"generation":3}),
    )
    .await;
    let recovered = post(&client, &manual_url, manual_command.clone()).await;
    assert_eq!(recovered["outcome"]["kind"], "outcome_unknown");
    let unknown = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let status = post(
                &client,
                &manual_url.replace("/create", "/status"),
                Value::Null,
            )
            .await;
            if status["create"]["outcome"]["kind"] == "outcome_unknown" {
                break status["create"].clone();
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(unknown["command"], manual_command);
    assert_eq!(puts(&client, &base).await, 1);
    // An absent read result and even loss of the fence row do not permit an
    // unknown retained receipt to write. Projected parents are unreadable too.
    sqlx::query("DELETE FROM delivery_attempts WHERE delivery_key = ? AND step = 'repo'")
        .bind(
            manual_url
                .split("/RepositoryDelivery/")
                .nth(1)
                .unwrap()
                .trim_end_matches("/create"),
        )
        .execute(&sql)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE invitation_requests RENAME TO invitation_requests_offline")
        .execute(&sql)
        .await
        .unwrap();
    assert_eq!(
        post(&client, &manual_url, manual_command.clone()).await["outcome"]["kind"],
        "outcome_unknown"
    );
    assert_eq!(puts(&client, &base).await, 1);
    sqlx::query("ALTER TABLE invitation_requests_offline RENAME TO invitation_requests")
        .execute(&sql)
        .await
        .unwrap();
    sqlx::query("DROP TRIGGER hold_approval")
        .execute(&sql)
        .await
        .unwrap();
    // Applied GitHub PUT + lost Restate result + failed fence read on replay.
    // The receipt must retain uncertainty even though no earlier receipt exists.
    post(
        &client,
        &format!("{base}/identity"),
        json!({"login":"recovered-user","addressed_id":8}),
    )
    .await;
    lose_fence_after_put.store(true, std::sync::atomic::Ordering::SeqCst);
    faults
        .lose_http_result
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let recovery_link = ghinvite_core::InvitationLinkId::new();
    post(&client, &format!("{ingress}/InvitationLink/{recovery_link}/create"), json!({"max_uses":1000,"secret_sha256":"2a".repeat(32),"link_id":recovery_link,"account_id":100,"installation_id":19,"admin":{"account_id":100,"user_id":7},"description":"Lost create acknowledgement","approval_required":false,"targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/api","grant":"write"}]}})).await;
    let recovery_admission = post(&client, &format!("{ingress}/InvitationLink/{recovery_link}/admit"), json!({"scope_evidence":[],"secret_sha256":"2a".repeat(32),"secret_generation":1,"link_id":recovery_link,"requester_id":8,"operation_id":ghinvite_core::RequestId::new()})).await;
    assert_eq!(recovery_admission["result"]["kind"], "accepted");
    let recovery_plan = post(&client, &format!("{ingress}/InvitationRequest/{}/approved_plan", recovery_admission["result"]["request_id"].as_str().unwrap()), json!({"link_id":recovery_link,"request_id":recovery_admission["result"]["request_id"],"requester_id":8})).await;
    let uncertain_command = recovery_plan["commands"][0].clone();
    assert!(uncertain_command.get("invitation_id").is_none());
    let owner = format!(
        "{ingress}/RepositoryDelivery/{}:10:{}",
        uncertain_command["request_id"].as_str().unwrap(),
        uncertain_command["delivery_generation"]
    );
    // Admission already sent create. Read its retained first result without
    // issuing a second create, which legitimately performs positive recovery.
    let uncertain = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let snapshot = post(&client, &format!("{owner}/status"), Value::Null).await;
            if !snapshot.is_null() {
                break snapshot["create"].clone();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("lost-result invocation retains uncertainty");
    assert_eq!(uncertain["outcome"]["kind"], "outcome_unknown");
    assert_eq!(
        faults
            .http_result_losses
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_eq!(puts(&client, &base).await, 2);
    sqlx::query("ALTER TABLE delivery_attempts_offline RENAME TO delivery_attempts")
        .execute(&sql)
        .await
        .unwrap();
    let confirmed = post(
        &client,
        &format!("{owner}/create"),
        uncertain_command.clone(),
    )
    .await;
    assert_eq!(confirmed["outcome"]["kind"], "created");
    assert_eq!(puts(&client, &base).await, 2);

    // A real delayed observation retry wakes after another event has settled.
    let sent = post(&client, &format!("{owner}/status"), Value::Null).await;
    fail_observations.store(true, std::sync::atomic::Ordering::SeqCst);
    let before = observation_reads.load(std::sync::atomic::Ordering::SeqCst);
    post(
        &client,
        &format!("{ingress}/DeliveryObservation/observe"),
        sent.clone(),
    )
    .await;
    assert_eq!(
        observation_reads.load(std::sync::atomic::Ordering::SeqCst),
        before + 1
    );
    refuse_cancel(&client, &format!("{owner}/cancel"), json!({"invitation_id":json!(ghinvite_core::GithubInvitationId::new()),"installation_id":19,"by_user":7,"at":chrono::Utc::now()})).await;
    post(
        &client,
        &format!("{owner}/reconcile"),
        absence(&sent, "none"),
    )
    .await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    // Also redrive the stale observation and a repeated member event explicitly.
    post(
        &client,
        &format!("{ingress}/DeliveryObservation/observe"),
        sent.clone(),
    )
    .await;
    post(
        &client,
        &format!("{owner}/reconcile"),
        absence(&sent, "write"),
    )
    .await;
    assert_eq!(
        observation_reads.load(std::sync::atomic::Ordering::SeqCst),
        before + 1
    );
    assert_eq!(
        post(&client, &format!("{owner}/status"), Value::Null).await["settlement"]["state"],
        "declined"
    );
    let calls: Value = client
        .get(format!("{base}/calls"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        !calls["requests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|call| call["method"] == "DELETE")
    );
    harness.finish().await;
    server.abort();
    stub_server.abort();
}

async fn post(client: &reqwest::Client, url: &str, input: Value) -> Value {
    let request = client.post(url);
    let response = if input.is_null() {
        request
    } else {
        request.json(&input)
    }
    .send()
    .await
    .unwrap();
    assert!(
        response.status().is_success(),
        "{url}: {}",
        response.text().await.unwrap()
    );
    let body = response.text().await.unwrap();
    if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&body).unwrap()
    }
}

async fn puts(client: &reqwest::Client, base: &str) -> usize {
    let calls: Value = client
        .get(format!("{base}/calls"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    calls["requests"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["method"] == "PUT")
        .count()
}

async fn refuse_cancel(client: &reqwest::Client, url: &str, input: Value) {
    let response = client.post(url).json(&input).send().await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
}

fn absence(snapshot: &Value, role: &str) -> Value {
    let command = &snapshot["create"]["command"];
    json!({"delivery_key":format!("{}:{}:{}",command["request_id"].as_str().unwrap(),command["repo_id"],command["delivery_generation"]),"expected_revision":snapshot["revision"],"at":chrono::Utc::now(),"evidence":{"kind":"absent","availability_verified":true,"role_name":role,"direct_collaborator":null,"membership":null}})
}
