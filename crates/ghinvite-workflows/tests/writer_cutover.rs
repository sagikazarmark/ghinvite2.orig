//! Operator import/activation contract against a disposable real runtime.
#![cfg(feature = "integration")]
use restate_sdk::{endpoint::Endpoint, http_server::HttpServer};
use serde_json::{Value, json};
use std::time::Duration;

#[tokio::test]
async fn partial_import_stays_closed_and_identical_manifest_resumes() {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap();
    let faults = std::sync::Arc::new(ghinvite_workflows::admission_v1::Faults::default());
    let builder =
        ghinvite_workflows::admission_v1::bind_with_faults(Endpoint::builder(), faults.clone());
    let builder = ghinvite_workflows::request_lifecycle_v1::bind(builder);
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        HttpServer::new(builder.build())
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
    let id = ghinvite_core::InvitationLinkId::new();
    let call = |handler: &str, input: Value| {
        client
            .post(format!("{ingress}/InvitationLinkV1/{id}/{handler}"))
            .json(&input)
    };
    let link = json!({"link_id":id,"creation":{"version":1,"link_id":id,"admin":{"account_id":100,"user_id":7},"account_id":100,"installation_id":9,"description":"Imported","permission":"push","approval_required":true,"repos":[{"repo_id":10,"repo_full_name":"acme/api"}]},"invitation_code":"importcode123456","created_at":"2026-01-01T00:00:00Z","uses":0,"revision":1});
    let begin = json!({"version":1,"migration_id":"fixture-migration","manifest_checksum":"a".repeat(64),"checkpoint":"coordinated-fixture","link":link,"request_checksums":[]});
    let response = call("begin_import", begin.clone()).send().await.unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    let replay: Value = call("begin_import", begin.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(replay["phase"], "importing");
    assert_eq!(replay["imported_requests"], 0);
    let query = json!({"link_id":id,"admin":{"account_id":100,"user_id":7}});
    assert_eq!(
        call("link_status", query.clone())
            .send()
            .await
            .unwrap()
            .status(),
        503
    );
    let mut conflict = begin.clone();
    conflict["manifest_checksum"] = json!("b".repeat(64));
    assert_eq!(
        call("begin_import", conflict)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let activate = json!({"migration_id":"fixture-migration","manifest_checksum":"a".repeat(64),"projection_verified":true});
    let response = call("activate_import", activate.clone())
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    let status: Value = call("link_status", query)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["uses"], 0);
    assert_eq!(status["invitation_code"], "importcode123456");
    assert_eq!(
        call("begin_import", begin)
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["phase"],
        "active"
    );

    // A second link transfers a historical pending deadline and accepted replay
    // input without granting a fresh seven days or consuming another use.
    let second = ghinvite_core::InvitationLinkId::new();
    let request = ghinvite_core::RequestId::new();
    let mut old_link = link.clone();
    old_link["link_id"] = json!(second);
    old_link["creation"]["link_id"] = json!(second);
    old_link["invitation_code"] = json!("secondcode123456");
    old_link["uses"] = json!(1);
    let imported = json!({"request":{"request_id":request,"link_id":second,"account_id":100,"requester_id":8,"justification":"access","state":"pending","admitted_at":"2026-01-01T00:00:00Z","decision_deadline":"2099-01-02T00:00:00Z","revision":1},"legacy_input":{"version":0,"link_id":second,"operation_id":request,"requester_id":8,"justification":"access"},"plan":null,"receipts":[]});
    use sha2::{Digest, Sha256};
    let digest = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&imported).unwrap())
    );
    let second_call = |handler: &str, input: Value| {
        client
            .post(format!("{ingress}/InvitationLinkV1/{second}/{handler}"))
            .json(&input)
    };
    let begin = json!({"version":1,"migration_id":"fixture-migration","manifest_checksum":"a".repeat(64),"checkpoint":"coordinated-fixture","link":old_link,"request_checksums":[digest]});
    assert!(
        second_call("begin_import", begin)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    assert_eq!(
        second_call("activate_import", activate.clone())
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let item = json!({"migration_id":"fixture-migration","manifest_checksum":"a".repeat(64),"index":0,"record":imported});
    faults.arm("migration-after-item");
    let pending = second_call("import_request", item.clone());
    let importing = tokio::spawn(async move { pending.send().await.unwrap() });
    tokio::time::timeout(Duration::from_secs(10), async {
        while faults.attempts() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    faults.release();
    let response = importing.await.unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    let replay: Value = second_call("import_request", item)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(replay["imported_requests"], 1);
    assert!(
        second_call("activate_import", activate)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let replay: Value = second_call("admit", imported["legacy_input"].clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(replay["result"]["request_id"], request.to_string());
    assert_eq!(
        replay["result"]["decision_deadline"],
        "2099-01-02T00:00:00Z"
    );
    let mut changed = imported["legacy_input"].clone();
    changed["justification"] = json!("changed");
    assert_eq!(
        second_call("admit", changed).send().await.unwrap().status(),
        409
    );
    // Transferred pending workflow gets the same identity/input, once only.
    let response = second_call(
        "handoff_import",
        json!({"migration_id":"fixture-migration","manifest_checksum":"a".repeat(64),"index":0}),
    )
    .send()
    .await
    .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    let response: Value = client.post(format!("{admin}/query")).header("accept", "application/json")
        .json(&json!({"query":format!("SELECT id, pinned_deployment_id, status FROM sys_invocation WHERE target_service_name = 'InvitationRequestV1' AND target_service_key = '{request}'")}))
        .send().await.unwrap().json().await.unwrap();
    let invocation = response["rows"][0]["id"]
        .as_str()
        .expect("inventoried pending invocation");
    let response = client
        .delete(format!("{admin}/invocations/{invocation}"))
        .query(&[("mode", "kill")])
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    // Operational kill must not turn a still-pending business request cancelled.
    let status: Value = second_call(
        "request_status",
        json!({"link_id":second,"request_id":request,"requester_id":8}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(status["state"], "pending");
    // The cutover endpoint keeps the legacy service contract discoverable but
    // rejects new/queued legacy commands before any database or GitHub work.
    let database_path = std::env::temp_dir().join(format!(
        "ghinvite-cutover-{}.db",
        ghinvite_core::RequestId::new()
    ));
    let storage = std::sync::Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::at_path(&database_path)
            .await
            .unwrap(),
    );
    storage.run_migrations().await.unwrap();
    let github = std::sync::Arc::new(ghinvite_github::InstallationClient::new(
        std::sync::Arc::new(ghinvite_github::mocks::MockTransport::scripted(vec![])),
        ghinvite_github::jwt::AppJwtSigner::from_pem(
            123,
            include_str!("../../ghinvite-github/src/jwt_test_key.pem"),
        )
        .unwrap(),
    ));
    let storage_for_fence = storage.clone();
    let endpoint = ghinvite_workflows::build_cutover_endpoint(
        ghinvite_workflows::AppState::new(storage.clone(), github),
        storage,
        None,
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let cutover_server = tokio::spawn(async move {
        HttpServer::new(endpoint)
            .serve_with_cancel(listener, std::future::pending::<()>())
            .await;
    });
    let registered = client
        .post(format!("{admin}/deployments"))
        .json(&json!({"uri":format!("http://{host}:{port}"),"force":true}))
        .send()
        .await
        .unwrap();
    assert!(
        registered.status().is_success(),
        "{}",
        registered.text().await.unwrap()
    );
    let late = client
        .post(format!("{ingress}/InvitationRequest/{request}/decide"))
        .json(&json!({"Approve":{"decided_by":7,"decided_at":"2026-01-01T00:00:00Z"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(late.status(), 410);
    // Import approved delivery facts before any fanout. A Sent create is final;
    // Sending stays uncertain and creates a durable SQL HTTP fence.
    let third = ghinvite_core::InvitationLinkId::new();
    let rid = ghinvite_core::RequestId::new();
    let iid = ghinvite_core::GithubInvitationId::new();
    let command = json!({"version":1,"invitation_id":iid,"link_id":third,"request_id":rid,"approval_id":"legacy-approved","account_id":100,"installation_id":9,"requester_id":8,"repo_id":10,"repo_full_name":"acme/api","permission":"push","approved_at":"2026-01-01T00:00:00Z"});
    let receipt =
        json!({"command":command,"outcome":{"kind":"created","upstream_id":1234},"revision":1});
    let receiver = |id: ghinvite_core::GithubInvitationId, handler: &str, input: Value| {
        client
            .post(format!("{ingress}/GithubCreateV1/{id}/{handler}"))
            .json(&input)
    };
    let imported_receipt = json!({"migration_id":"fixture-migration","manifest_checksum":"a".repeat(64),"receipt":receipt});
    assert!(
        receiver(iid, "import_receipt", imported_receipt.clone())
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    assert!(
        receiver(iid, "import_receipt", imported_receipt)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let actual: Value = client
        .post(format!("{ingress}/GithubCreateV1/{iid}/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(actual, receipt);
    let unknown_id = ghinvite_core::GithubInvitationId::new();
    let mut unknown = receipt.clone();
    unknown["command"]["invitation_id"] = json!(unknown_id);
    unknown["outcome"] = json!({"kind":"outcome_unknown"});
    let response = receiver(unknown_id,"import_receipt",json!({"migration_id":"fixture-migration","manifest_checksum":"a".repeat(64),"receipt":unknown})).send().await.unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    use ghinvite_core::storage::Storage;
    assert!(
        storage_for_fence
            .delivery_attempt_exists(unknown_id)
            .await
            .unwrap()
    );
    let ingress_for_cli = ingress.clone();
    let database_for_cli = database_path.clone();
    tokio::task::spawn_blocking(move || {
        let output = std::process::Command::new("python3")
            .args(["-m", "unittest", "discover", "-s", "tests/migration", "-v"])
            .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
            .env("CUTOVER_REHEARSAL_INGRESS", ingress_for_cli)
            .env("CUTOVER_REHEARSAL_DATABASE", database_for_cli)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    })
    .await
    .unwrap();
    cutover_server.abort();
    server.abort();
}
