//! #60: availability through public commands and a real GitHub HTTP boundary.
#![cfg(feature = "integration")]
use ghinvite_core::RequestId;
use ghinvite_core::storage::{AuditStorage, ConsoleStorage, RecordStorage};
use restate_sdk::http_server::HttpServer;
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

#[tokio::test]
async fn availability_preserves_admission_and_pending_decisions() {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap();
    let database =
        std::env::temp_dir().join(format!("ghinvite-availability-{}.db", RequestId::new()));
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::at_path(&database)
            .await
            .unwrap(),
    );
    storage.run_migrations().await.unwrap();
    let failures = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new().filename(&database),
    )
    .await
    .unwrap();
    for user_id in [7, 8, 12, 31, 32, 33] {
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
    // `adopted` is #66's pre-existing installation, observed alongside `id` so
    // the account it belongs to is the only one it can answer for.
    let observed = Arc::new(Mutex::new(
        json!({"id":9,"adopted":40,"repos":[10],"failed":false}),
    ));
    let stub = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", stub.local_addr().unwrap());
    let data = observed.clone();
    let stub_server = tokio::spawn(async move {
        axum::serve(stub, axum::Router::new().fallback(move |request: axum::extract::Request| {
            let data = data.clone();
            async move {
                let mut data = data.lock().unwrap();
                let path = request.uri().path();
                // Counted so a scenario can say how many times GitHub was read,
                // not merely what it answered. A repository listing is counted
                // once per observation, at its first page.
                let observation = if path == "/installation/repositories" && !request.uri().query().unwrap_or_default().contains("page=2") { Some("repo_reads") }
                else if path.starts_with("/app/installations/") && !path.ends_with("/access_tokens") { Some("identity_reads") }
                else { None };
                let (status, body) = if data["failed"] == true { (503, json!({})) }
                else if path.ends_with("/access_tokens") { (201, json!({"token":"fixture","expires_at":"2099-01-01T00:00:00Z"})) }
                else if path == "/installation/repositories" && data["rate_limited"] == true { (403, json!({"message":"API rate limit exceeded"})) }
                else if path == "/installation/repositories" {
                    let page = if request.uri().query().unwrap_or_default().contains("page=2") { 1 } else { 0 };
                    let repos = data["repos"].as_array().unwrap();
                    (200, json!({"total_count":repos.len(),"repositories":repos.iter().skip(page * 100).take(100).map(|id|json!({"id":id,"full_name":"acme/api","private":true})).collect::<Vec<_>>()}))
                }
                else if path == format!("/app/installations/{}", data["adopted"]) { (200, json!({"id":data["adopted"],"account":{"id":300,"login":"adopted","type":"Organization"},"suspended_at":null})) }
                else if path == format!("/app/installations/{}", data["id"]) { (200, json!({"id":data["id"],"account":{"id":data.get("account_id").unwrap_or(&json!(100)),"login":"renamed","type":"Organization"},"suspended_at":null})) }
                else { (404, json!({})) };
                if let Some(counter) = observation {
                    let read = data[counter].as_u64().unwrap_or(0);
                    data[counter] = json!(read + 1);
                }
                (axum::http::StatusCode::from_u16(status).unwrap(), axum::Json(body))
            }
        })).await.unwrap();
    });
    let state = ghinvite_workflows::AppState::new(
        storage.clone(),
        Arc::new(
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
            .with_base(base),
        ),
    );
    let endpoint = ghinvite_workflows::build_endpoint(state, storage.clone(), None).unwrap();
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
        .json(&json!({"uri":format!("http://{host}:{port}")}))
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    let link = ghinvite_core::InvitationLinkId::new().to_string();
    let call = |handler: &str, input: Value| {
        let route = if matches!(handler, "request_status" | "decide" | "approved_plan") {
            format!(
                "InvitationRequest/{}/{handler}",
                input["request_id"].as_str().unwrap()
            )
        } else {
            format!("InvitationLink/{link}/{handler}")
        };
        client.post(format!("{ingress}/{route}")).json(&input)
    };
    let creation = json!({"link_id":link,"account_id":100,"installation_id":9,"admin":{"account_id":100,"user_id":7},
        "description":"Availability","max_uses":2,"approval_required":true,"permission":"pull","repos":[{"repo_id":10,"repo_full_name":"acme/api"},{"repo_id":11,"repo_full_name":"acme/web"}]});
    assert!(
        call("create", creation)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let result = call(
        "admit",
        json!({"link_id":link,"requester_id":8,"operation_id":RequestId::new()}),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(
        result.status(),
        503,
        "missing retained authority is uncertain"
    );
    let onboard = json!({"installation_id":9,"actor_user_id":7,"account_id":100,"account_login":"acme","account_type":"Organization","selected_repos":"all","installed_at":chrono::Utc::now()});
    let installation = |id: u64, handler: &str, input: Value| {
        client
            .post(format!("{ingress}/Installation/{id}/{handler}"))
            .json(&input)
    };
    let response = installation(9, "onboard", onboard.clone())
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    observed.lock().unwrap()["repos"] = json!([10]);
    let response = installation(
        9,
        "repos_changed",
        json!({"installation_id":9,"selected_repos":"all"}),
    )
    .send()
    .await
    .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    let attempt = json!({"link_id":link,"requester_id":8,"operation_id":RequestId::new()});
    let unavailable: Value = call("admit", attempt.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        unavailable["result"],
        json!({"kind":"rejected","reason":"repository_unavailable"})
    );
    observed.lock().unwrap()["repos"] = json!([10, 11]);
    let replay: Value = call("admit", attempt)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(replay, unavailable);
    let accepted_attempt = json!({"link_id":link,"requester_id":8,"operation_id":RequestId::new()});
    observed.lock().unwrap()["rate_limited"] = json!(true);
    assert_eq!(
        call("admit", accepted_attempt.clone())
            .send()
            .await
            .unwrap()
            .status(),
        503
    );
    observed.lock().unwrap()["rate_limited"] = json!(false);
    let accepted: Value = call("admit", accepted_attempt.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(accepted["result"]["state"], "pending");
    // Pause installation SQL projection, not its authoritative commands. Neither
    // new admission nor subsequent pending decisions may wait for that write.
    sqlx::query("CREATE TRIGGER fixture_outage BEFORE UPDATE ON installations BEGIN SELECT RAISE(ABORT, 'fixture: storage unavailable'); END").execute(&failures).await.unwrap();
    observed.lock().unwrap()["failed"] = json!(true);
    assert!(
        installation(
            9,
            "repos_changed",
            json!({"installation_id":9,"selected_repos":"all"})
        )
        .send()
        .await
        .unwrap()
        .status()
        .is_success()
    );
    // Recorded acceptance remains recoverable even when GitHub cannot be read.
    assert_eq!(
        call("admit", accepted_attempt.clone())
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap(),
        accepted
    );
    // The outage reaches the account through the refresh its own webhook
    // acknowledged. Only then has this account nothing recent and available to
    // admit on, so admission reads GitHub again and fails through promptly.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let status: Value = client
                .post(format!("{ingress}/AccountInstallation/100/status"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if status["observation"]["kind"] == "unknown" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let fresh = json!({"link_id":link,"requester_id":12,"operation_id":RequestId::new()});
    assert_eq!(
        call("admit", fresh.clone()).send().await.unwrap().status(),
        503
    );
    let query =
        json!({"link_id":link,"request_id":accepted["result"]["request_id"],"requester_id":8});
    let pending: Value = call("request_status", query.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        pending["decision_deadline"],
        accepted["result"]["decision_deadline"]
    );
    let approved: Value = call("decide", json!({"link_id":link,"request_id":accepted["result"]["request_id"],"operation_id":RequestId::new(),"admin":{"account_id":100,"user_id":7},"action":{"kind":"approve"}})).send().await.unwrap().json().await.unwrap();
    assert_eq!(approved["request"]["state"], "approved");
    assert_eq!(
        approved["request"]["decision_deadline"],
        pending["decision_deadline"]
    );
    let plan: Value = call("approved_plan", query.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(plan["commands"].as_array().unwrap().len(), 2);
    for command in plan["commands"].as_array().unwrap() {
        let receipt: Value = client
            .post(format!(
                "{ingress}/RepositoryDelivery/{}:{}/create",
                command["request_id"].as_str().unwrap(),
                command["repo_id"]
            ))
            .json(command)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(receipt["outcome"]["kind"], "blocked");
    }
    sqlx::query("DROP TRIGGER fixture_outage")
        .execute(&failures)
        .await
        .unwrap();
    observed.lock().unwrap()["failed"] = json!(false);
    // Invoke the same public command the durable refresh timer uses. Restoration
    // must repair delivery prerequisites even without another GitHub webhook.
    assert!(
        client
            .post(format!("{ingress}/AccountInstallation/100/recheck"))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if storage.get_installation(9).await.unwrap().is_some_and(|a| {
                a.selected_repos == ghinvite_core::SelectedRepos::Subset(vec![10, 11])
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    observed.lock().unwrap()["failed"] = json!(false);
    // Fresh onboarding, refresh and admission use retained context while the
    // entire installation projection is unreadable. No SQL adoption is needed.
    observed.lock().unwrap()["repos"] = json!([10, 11]);
    sqlx::query("ALTER TABLE installations RENAME TO installations_offline")
        .execute(&failures)
        .await
        .unwrap();
    let seeded = installation(40, "onboard", json!({"installation_id":40,"actor_user_id":7,"account_id":300,
        "account_login":"adopted","account_type":"Organization","selected_repos":"all","installed_at":chrono::Utc::now()}))
        .send()
        .await
        .unwrap();
    assert!(
        seeded.status().is_success(),
        "{}",
        seeded.text().await.unwrap()
    );
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let state: Value = client
                .post(format!("{admin}/query"))
                .header("accept", "application/json")
                .json(&json!({"query":"SELECT key FROM state WHERE service_name = 'Installation' AND service_key = '40'"}))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if !state["rows"].as_array().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    // An account-scoped link admits through the same observation admission uses.
    let adopted_link = ghinvite_core::InvitationLinkId::new().to_string();
    assert!(
        client
            .post(format!("{ingress}/InvitationLink/{adopted_link}/create"))
            .json(&json!({"link_id":adopted_link,"account_id":300,"installation_id":40,"admin":{"account_id":300,"user_id":7},
                "description":"Adopted","max_uses":2,"approval_required":true,"permission":"pull","repos":[{"repo_id":10,"repo_full_name":"acme/api"}]}))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    // Installation reads fail while GitHub stays reachable.
    let repos_changed = json!({"installation_id":40,"selected_repos":"all"});
    // The webhook acknowledges the event with a durable send, exactly as the
    // repository-change command does.
    assert!(
        client
            .post(format!("{ingress}/Installation/40/repos_changed/send"))
            .json(&repos_changed)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    // Duplicate refreshes complete without waiting for projection.
    for _ in 0..2 {
        assert!(
            installation(40, "repos_changed", repos_changed.clone())
                .send()
                .await
                .unwrap()
                .status()
                .is_success()
        );
    }
    // A delayed event for an identity this account never onboarded is harmless.
    assert!(
        client
            .post(format!("{ingress}/AccountInstallation/300/refresh"))
            .json(&json!(41))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    // Retained authority remains usable during the projection outage.
    let started = std::time::Instant::now();
    let adopted_attempt =
        json!({"link_id":adopted_link,"requester_id":8,"operation_id":RequestId::new()});
    assert_eq!(
        client
            .post(format!("{ingress}/InvitationLink/{adopted_link}/admit"))
            .json(&adopted_attempt)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .post(format!("{ingress}/AccountInstallation/300/status"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert!(started.elapsed() < Duration::from_secs(15));
    // An unrelated uninstall retains a tombstone without affecting this account.
    let uninstall_41 = json!({"installation_id":41,"uninstalled_at":chrono::Utc::now()});
    for _ in 0..2 {
        assert!(
            client
                .post(format!("{ingress}/AccountInstallation/300/uninstall"))
                .json(&uninstall_41)
                .send()
                .await
                .unwrap()
                .status()
                .is_success()
        );
    }
    let retries: Value = client
        .post(format!("{admin}/query"))
        .header("accept", "application/json")
        .json(&json!({"query":"SELECT id FROM sys_invocation WHERE target_service_name = 'AccountInstallation' AND target_service_key = '300' AND target_handler_name = 'retry_uninstall' AND status <> 'completed'"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(retries["rows"].as_array().unwrap().len(), 0, "{retries}");
    // An unusable account key is refused, not retried as if it were an outage.
    assert_eq!(
        client
            .post(format!("{ingress}/AccountInstallation/0/uninstall"))
            .json(&uninstall_41)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    sqlx::query("ALTER TABLE installations_offline RENAME TO installations")
        .execute(&failures)
        .await
        .unwrap();
    // Restoration alone converges: no further webhook and no user action.
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if storage
                .get_installation(40)
                .await
                .unwrap()
                .is_some_and(|a| {
                    a.selected_repos == ghinvite_core::SelectedRepos::Subset(vec![10, 11])
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    let adopted: Value = client
        .post(format!("{ingress}/AccountInstallation/300/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(adopted["account"]["installation_id"], 40);
    assert_eq!(adopted["observation"]["repo_ids"], json!([10, 11]));
    // Admission replay retains the successful result after projection repair.
    assert_eq!(
        client
            .post(format!("{ingress}/InvitationLink/{adopted_link}/admit"))
            .json(&adopted_attempt)
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["result"]["kind"],
        "accepted"
    );
    // A delayed event for a retired identity observes nothing, so recovered
    // scope survives it.
    assert!(
        installation(
            40,
            "uninstall",
            json!({"installation_id":40,"uninstalled_at":chrono::Utc::now()})
        )
        .send()
        .await
        .unwrap()
        .status()
        .is_success()
    );
    assert!(
        client
            .post(format!("{ingress}/AccountInstallation/300/refresh"))
            .json(&json!(40))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let retired: Value = client
        .post(format!("{ingress}/AccountInstallation/300/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(retired["account"].is_null());
    assert_eq!(
        storage
            .get_installation(40)
            .await
            .unwrap()
            .unwrap()
            .selected_repos,
        ghinvite_core::SelectedRepos::Subset(vec![10, 11])
    );
    // New installation arrives before the old uninstall. Numeric account identity
    // survives a renamed account and obsolete events cannot mutate the replacement.
    observed.lock().unwrap()["id"] = json!(20);
    let mut replacement = onboard.clone();
    replacement["installation_id"] = json!(20);
    replacement["account_login"] = json!("untrusted-old-name");
    let response = installation(20, "onboard", replacement.clone())
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    let uninstall = json!({"installation_id":9,"uninstalled_at":chrono::Utc::now()});
    for _ in 0..2 {
        assert!(
            installation(9, "uninstall", uninstall.clone())
                .send()
                .await
                .unwrap()
                .status()
                .is_success()
        );
        assert!(
            installation(9, "onboard", onboard.clone())
                .send()
                .await
                .unwrap()
                .status()
                .is_success()
        );
        assert!(
            installation(
                9,
                "repos_changed",
                json!({"installation_id":9,"selected_repos":[]})
            )
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
        );
    }
    let status: Value = client
        .post(format!("{ingress}/AccountInstallation/100/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["account"]["installation_id"], 20);
    assert_eq!(status["account"]["account_id"], 100);
    assert_eq!(status["account"]["account_login"], "renamed");
    assert_eq!(status["observation"]["repo_ids"], json!([10, 11]));
    // Concurrent/stale payloads cannot overwrite a current authoritative refresh,
    // including repositories beyond the first GitHub page.
    observed.lock().unwrap()["repos"] = json!((10..=110).collect::<Vec<_>>());
    let (first, second) = tokio::join!(
        installation(
            20,
            "repos_changed",
            json!({"installation_id":20,"selected_repos":[]})
        )
        .send(),
        installation(
            20,
            "repos_changed",
            json!({"installation_id":20,"selected_repos":[999]})
        )
        .send()
    );
    assert!(first.unwrap().status().is_success());
    assert!(second.unwrap().status().is_success());
    let status: Value = client
        .post(format!("{ingress}/AccountInstallation/100/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        status["observation"]["repo_ids"].as_array().unwrap().len(),
        101
    );
    assert_eq!(status["observation"]["repo_ids"][100], 110);
    // A scope the account's recent observation does not cover is never refused
    // from that observation: the refusal is read from GitHub first, because it
    // is retained permanently.
    let scoped_link = ghinvite_core::InvitationLinkId::new().to_string();
    let scoped = |handler: &str, input: Value| {
        client
            .post(format!("{ingress}/InvitationLink/{scoped_link}/{handler}"))
            .json(&input)
    };
    assert!(
        scoped(
            "create",
            json!({"link_id":scoped_link,"account_id":100,"installation_id":20,"admin":{"account_id":100,"user_id":7},
                "description":"Recent observation","max_uses":3,"approval_required":true,"permission":"pull",
                "repos":[{"repo_id":10,"repo_full_name":"acme/api"},{"repo_id":200,"repo_full_name":"acme/extra"}]})
        )
        .send()
        .await
        .unwrap()
        .status()
        .is_success()
    );
    let scoped_attempt = |requester_id: u64| json!({"link_id":scoped_link,"requester_id":requester_id,"operation_id":RequestId::new()});
    observed.lock().unwrap()["repo_reads"] = json!(0);
    let refused: Value = scoped("admit", scoped_attempt(15))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        refused["result"],
        json!({"kind":"rejected","reason":"repository_unavailable"})
    );
    assert_eq!(
        observed.lock().unwrap()["repo_reads"],
        json!(1),
        "a rejection rests on a live read"
    );
    // GitHub gains the repository without a webhook. The attempts below still
    // read GitHub once between them — the first, whose scope the retained
    // observation does not cover — and the rest rest on what it observed.
    observed.lock().unwrap()["repos"] = json!((10..=110).chain([200]).collect::<Vec<_>>());
    observed.lock().unwrap()["repo_reads"] = json!(0);
    for requester_id in [31, 32, 33] {
        let admitted: Value = scoped("admit", scoped_attempt(requester_id))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(admitted["result"]["kind"], "accepted", "{admitted}");
    }
    assert_eq!(
        observed.lock().unwrap()["repo_reads"],
        json!(1),
        "one observation admits every attempt it covers"
    );
    observed.lock().unwrap()["account_id"] = json!(200);
    // A changed installation identity reaches the account through its own
    // observation; admission then confirms the refusal against GitHub.
    assert!(
        client
            .post(format!("{ingress}/AccountInstallation/100/recheck"))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    observed.lock().unwrap()["identity_reads"] = json!(0);
    let foreign = json!({"link_id":link,"requester_id":15,"operation_id":RequestId::new()});
    assert_eq!(
        call("admit", foreign)
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["result"]["reason"],
        "installation_unavailable"
    );
    assert!(
        observed.lock().unwrap()["identity_reads"].as_u64().unwrap() > 0,
        "a rejection rests on a live read"
    );
    observed.lock().unwrap()["account_id"] = json!(100);
    let restored: Value = call("admit", fresh)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(restored["result"]["kind"], "accepted");
    assert_eq!(
        call("admit", accepted_attempt.clone())
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap(),
        accepted
    );
    assert_eq!(
        call("request_status", query.clone())
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["state"],
        "approved"
    );
    assert_eq!(
        call("approved_plan", query)
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap(),
        plan
    );
    let link_query = json!({"link_id":link,"admin":{"account_id":100,"user_id":7}});
    let link_status: Value = call("link_status", link_query.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(link_status["uses"], 2);
    assert_eq!(link_status["creation"]["installation_id"], 9);
    assert_eq!(
        link_status["creation"]["repos"].as_array().unwrap().len(),
        2
    );
    let exhausted: Value = call(
        "admit",
        json!({"link_id":link,"requester_id":15,"operation_id":RequestId::new()}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(exhausted["result"]["reason"], "exhausted");
    call("revoke", link_query).send().await.unwrap();
    assert!(
        installation(20, "onboard", replacement.clone())
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let revoked: Value = call(
        "admit",
        json!({"link_id":link,"requester_id":15,"operation_id":RequestId::new()}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(revoked["result"]["reason"], "revoked");
    // Uninstall before any onboarding leaves a durable tombstone.
    assert!(
        installation(
            30,
            "uninstall",
            json!({"installation_id":30,"uninstalled_at":chrono::Utc::now()})
        )
        .send()
        .await
        .unwrap()
        .status()
        .is_success()
    );
    observed.lock().unwrap()["id"] = json!(30);
    replacement["installation_id"] = json!(30);
    assert!(
        installation(30, "onboard", replacement)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let status: Value = client
        .post(format!("{ingress}/AccountInstallation/100/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["account"]["installation_id"], 20);
    server.abort();
    let events = storage
        .list_audit_events(100, None, ghinvite_core::storage::AuditPosition::Latest)
        .await
        .unwrap()
        .events;
    for event in events.iter().filter(|event| {
        matches!(
            event.event_type,
            ghinvite_core::audit::EventType::InstallationCreated
                | ghinvite_core::audit::EventType::InstallationReposChanged
                | ghinvite_core::audit::EventType::InstallationUninstalled
        )
    }) {
        storage.audit(event).await.unwrap();
        let mut conflict = event.clone();
        conflict.account_id = 999;
        assert!(storage.audit(&conflict).await.is_err());
    }
    assert_eq!(
        storage
            .list_audit_events(100, None, ghinvite_core::storage::AuditPosition::Latest)
            .await
            .unwrap()
            .events
            .len(),
        events.len()
    );
    stub_server.abort();
    failures.close().await;
    std::fs::remove_file(database).unwrap();
}
