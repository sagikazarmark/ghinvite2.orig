//! #60: availability through public commands and a real GitHub HTTP boundary.
#![cfg(feature = "integration")]
use ghinvite_core::RequestId;
use ghinvite_core::storage::Storage;
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
    for user_id in [7, 8, 12] {
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
                let data = data.lock().unwrap();
                let path = request.uri().path();
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
        client
            .post(format!("{ingress}/InvitationLink/{link}/{handler}"))
            .json(&input)
    };
    let creation = json!({"version":1,"link_id":link,"account_id":100,"installation_id":9,"admin":{"account_id":100,"user_id":7},
        "description":"Availability","max_uses":2,"approval_required":true,"permission":"pull","repos":[{"repo_id":10,"repo_full_name":"acme/api"},{"repo_id":11,"repo_full_name":"acme/web"}]});
    assert!(
        call("create", creation)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let result: Value = call(
        "admit",
        json!({"version":1,"link_id":link,"requester_id":8,"operation_id":RequestId::new()}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(
        result["result"],
        json!({"kind":"rejected","reason":"installation_unavailable"})
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
    let attempt =
        json!({"version":1,"link_id":link,"requester_id":8,"operation_id":RequestId::new()});
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
    let accepted_attempt =
        json!({"version":1,"link_id":link,"requester_id":8,"operation_id":RequestId::new()});
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
    let fresh =
        json!({"version":1,"link_id":link,"requester_id":12,"operation_id":RequestId::new()});
    assert_eq!(
        call("admit", fresh.clone()).send().await.unwrap().status(),
        503
    );
    let status: Value = client
        .post(format!("{ingress}/AccountInstallation/100/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["observation"]["kind"], "unknown");
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
    let approved: Value = call("decide", json!({"version":1,"link_id":link,"request_id":accepted["result"]["request_id"],"operation_id":RequestId::new(),"admin":{"account_id":100,"user_id":7},"action":{"kind":"approve"}})).send().await.unwrap().json().await.unwrap();
    assert_eq!(approved["request"]["state"], "approved");
    assert_eq!(
        approved["request"]["decision_deadline"],
        pending["decision_deadline"]
    );
    let plan: Value = call("prepare_dispatch", query.clone())
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
                "{ingress}/GithubCreate/{}/create",
                command["invitation_id"].as_str().unwrap()
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
    // #66: a repository webhook is acknowledged by its durable send, so the
    // account object's first storage adoption failure must retain recovery
    // instead of dropping acknowledged work. Installation 40 is the shape
    // adoption exists for: its command already retains the numeric account
    // binding, while account 300 has never been observed and must adopt the
    // existing row.
    observed.lock().unwrap()["repos"] = json!([10, 11]);
    storage
        .insert_installation(&ghinvite_core::Account {
            installation_id: 40,
            account_id: 300,
            account_login: "adopted".into(),
            account_type: ghinvite_core::AccountType::Organization,
            installed_at: chrono::Utc::now(),
            uninstalled_at: None,
            selected_repos: ghinvite_core::SelectedRepos::Subset(vec![]),
        })
        .await
        .unwrap();
    let seeded = client
        .post(format!("{admin}/services/Installation/state"))
        .json(&json!({"object_key":"40","new_state":{"account_id":serde_json::to_vec(&300u64).unwrap()}}))
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
            .json(&json!({"version":1,"link_id":adopted_link,"account_id":300,"installation_id":40,"admin":{"account_id":300,"user_id":7},
                "description":"Adopted","max_uses":2,"approval_required":true,"permission":"pull","repos":[{"repo_id":10,"repo_full_name":"acme/api"}]}))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    // Installation reads fail while GitHub stays reachable.
    sqlx::query("ALTER TABLE installations RENAME TO installations_offline")
        .execute(&failures)
        .await
        .unwrap();
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
    // The command also completes rather than holding its own exclusivity
    // through indefinite retries, and a duplicate event joins the retained
    // continuation instead of competing with it.
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
    // A delayed event for an identity this account never adopts is retained
    // just as safely.
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
    // Admission's synchronous observation still fails promptly with an
    // infrastructure result, holding no exclusivity and recording no rejection.
    let started = std::time::Instant::now();
    let adopted_attempt = json!({"version":1,"link_id":adopted_link,"requester_id":8,"operation_id":RequestId::new()});
    assert_eq!(
        client
            .post(format!("{ingress}/InvitationLink/{adopted_link}/admit"))
            .json(&adopted_attempt)
            .send()
            .await
            .unwrap()
            .status(),
        503
    );
    assert_eq!(
        client
            .post(format!("{ingress}/AccountInstallation/300/status"))
            .send()
            .await
            .unwrap()
            .status(),
        503
    );
    assert!(started.elapsed() < Duration::from_secs(15));
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
    // The attempt that met the outage was never decided, so the same operation
    // still admits once the observation is available.
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
    observed.lock().unwrap()["account_id"] = json!(200);
    let foreign =
        json!({"version":1,"link_id":link,"requester_id":15,"operation_id":RequestId::new()});
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
        call("prepare_dispatch", query)
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
        json!({"version":1,"link_id":link,"requester_id":15,"operation_id":RequestId::new()}),
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
        json!({"version":1,"link_id":link,"requester_id":15,"operation_id":RequestId::new()}),
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
