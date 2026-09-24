//! Approved delivery's public Restate/HTTP seam, with read projections withheld.
#![cfg(feature = "integration")]
use ghinvite_core::storage::{ConsoleStorage, RecordStorage};
use restate_sdk::{endpoint::Endpoint, http_server::HttpServer};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

#[tokio::test]
async fn approved_delivery_progresses_without_projected_parents() {
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
    let stub_server = tokio::spawn(async move {
        axum::serve(stub, ghinvite_github::stub::router())
            .await
            .unwrap()
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
    let endpoint = ghinvite_workflows::delivery::bind(builder, state).build();
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
        &format!("{ingress}/AccountInstallation/100/recheck"),
        Value::Null,
    )
    .await;
    assert_eq!(
        client
            .post(format!(
                "{ingress}/AccountInstallation/100/current_installation"
            ))
            .send()
            .await
            .unwrap()
            .status(),
        503
    );
    let eligibility = post(
        &client,
        &format!("{ingress}/AccountInstallation/100/eligibility"),
        json!({"account_id":100,"repo_ids":[10]}),
    )
    .await;
    assert_eq!(eligibility["kind"], "unknown");
    post(
        &client,
        &format!("{base}/installation-repositories"),
        json!([10]),
    )
    .await;
    let onboard = json!({"installation_id":9,"actor_user_id":7,"account_id":100,"account_login":"acme","account_type":"Organization","selected_repos":"all","installed_at":chrono::Utc::now()});
    let response = client
        .post(format!("{ingress}/Installation/9/onboard"))
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
    let response = call("create", json!({"link_id":link,"account_id":100,"installation_id":9,"admin":{"account_id":100,"user_id":7},"description":"Withheld parents","approval_required":false,"permission":"push","repos":[{"repo_id":10,"repo_full_name":"acme/api"},{"repo_id":11,"repo_full_name":"acme/web"}]})).send().await.unwrap();
    assert!(response.status().is_success());
    let admitted: Value = call(
        "admit",
        json!({"link_id":link,"requester_id":8,"operation_id":ghinvite_core::RequestId::new()}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
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
    let command = plan["commands"][0].clone();
    let id = command["invitation_id"].as_str().unwrap();
    let delivery_key = format!(
        "{}:{}",
        command["request_id"].as_str().unwrap(),
        command["repo_id"]
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
    // Settlement is retained beside the original create even while every
    // projected parent is unavailable. Replays return that original receipt.
    post(
        &client,
        &format!("{ingress}/RepositoryDelivery/{delivery_key}/on_webhook"),
        json!({"invitation_id":id,"action":"accepted","at":"2026-09-24T12:00:00Z"}),
    )
    .await;
    let snapshot = post(
        &client,
        &format!("{ingress}/RepositoryDelivery/{delivery_key}/status"),
        Value::Null,
    )
    .await;
    assert_eq!(snapshot["create"], receipt);
    assert_eq!(snapshot["settlement"]["state"], "accepted");
    let replay = post(
        &client,
        &format!("{ingress}/RepositoryDelivery/{delivery_key}/create"),
        command.clone(),
    )
    .await;
    assert_eq!(replay, receipt);
    assert!(storage.get_installation(9).await.unwrap().is_none());
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
        "{ingress}/RepositoryDelivery/{}:{}/create",
        blocked_command["request_id"].as_str().unwrap(),
        blocked_command["repo_id"]
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
        &format!("{ingress}/Installation/9/uninstall"),
        json!({"installation_id":9,"uninstalled_at":chrono::Utc::now()}),
    )
    .await;
    post(
        &client,
        &format!("{base}/installation-repositories"),
        json!([10, 11]),
    )
    .await;
    let mut replacement = onboard.clone();
    replacement["installation_id"] = json!(19);
    post(
        &client,
        &format!("{ingress}/Installation/19/onboard"),
        replacement,
    )
    .await;
    post(
        &client,
        &format!("{ingress}/Installation/9/repos_changed"),
        json!({"installation_id":9,"selected_repos":[]}),
    )
    .await;
    post(
        &client,
        &format!("{ingress}/Installation/9/uninstall"),
        json!({"installation_id":9,"uninstalled_at":chrono::Utc::now()}),
    )
    .await;
    post(
        &client,
        &format!("{base}/installation-identity"),
        json!({"id":200,"login":"acme","type":"Organization"}),
    )
    .await;
    assert_eq!(
        post(&client, &blocked_url, blocked_command.clone()).await["outcome"]["kind"],
        "blocked"
    );
    assert_eq!(puts(&client, &base).await, 1);
    post(
        &client,
        &format!("{base}/installation-identity"),
        json!({"id":100,"login":"acme","type":"Organization"}),
    )
    .await;
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
    post(&client, &format!("{owner_url}/cancel"), json!({"invitation_id":blocked_command["invitation_id"],"installation_id":9,"by_user":7,"at":chrono::Utc::now()})).await;
    assert_eq!(
        post(&client, &format!("{owner_url}/status"), Value::Null).await,
        pending
    );
    // Competing valid withdrawal/webhook commands choose one retained winner.
    let webhook_url = format!("{owner_url}/on_webhook");
    let cancel_url = format!("{owner_url}/cancel");
    tokio::join!(
        post(
            &client,
            &cancel_url,
            json!({"invitation_id":blocked_command["invitation_id"],"installation_id":19,"by_user":7,"at":chrono::Utc::now()})
        ),
        post(
            &client,
            &webhook_url,
            json!({"invitation_id":blocked_command["invitation_id"],"action":"accepted","at":chrono::Utc::now()})
        )
    );
    let winner = post(&client, &format!("{owner_url}/status"), Value::Null).await;
    assert!(matches!(
        winner["settlement"]["state"].as_str(),
        Some("accepted" | "cancelled")
    ));
    assert_eq!(winner["create"], resumed);
    post(&client, &webhook_url, json!({"invitation_id":blocked_command["invitation_id"],"action":"declined","at":chrono::Utc::now()})).await;
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
                last_seen_at: chrono::Utc::now(),
            })
            .await
            .unwrap();
    }
    tokio::time::timeout(Duration::from_secs(30), async {
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
    post(&client, &format!("{ingress}/InvitationLink/{manual}/create"), json!({"link_id":manual,"account_id":100,"installation_id":19,"admin":{"account_id":100,"user_id":7},"description":"Stale pending","approval_required":true,"permission":"push","repos":[{"repo_id":10,"repo_full_name":"acme/api"}]})).await;
    let admission = post(
        &client,
        &format!("{ingress}/InvitationLink/{manual}/admit"),
        json!({"link_id":manual,"requester_id":8,"operation_id":ghinvite_core::RequestId::new()}),
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
    post(&client, &format!("{ingress}/InvitationRequest/{request}/decide"), json!({"link_id":manual,"request_id":request,"operation_id":ghinvite_core::RequestId::new(),"admin":{"account_id":100,"user_id":7},"action":{"kind":"approve"}})).await;
    let manual_plan = post(
        &client,
        &format!("{ingress}/InvitationRequest/{request}/approved_plan"),
        json!({"link_id":manual,"request_id":request,"requester_id":8}),
    )
    .await;
    let manual_command = manual_plan["commands"][0].clone();
    let manual_url = format!(
        "{ingress}/RepositoryDelivery/{}:{}/create",
        manual_command["request_id"].as_str().unwrap(),
        manual_command["repo_id"]
    );
    let unavailable = post(&client, &manual_url, manual_command.clone()).await;
    assert_eq!(
        unavailable["outcome"]["kind"], "blocked",
        "fence outage before claim releases owner exclusivity"
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
    sqlx::query("DELETE FROM delivery_attempts WHERE invitation_id = ?")
        .bind(manual_command["invitation_id"].as_str().unwrap())
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
