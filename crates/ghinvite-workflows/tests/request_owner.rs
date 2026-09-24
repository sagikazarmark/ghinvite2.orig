//! Request/link protocol acceptance against a disposable real Restate runtime.
#![cfg(feature = "integration")]

use serde_json::{Value, json};
use std::time::Duration;

#[tokio::test]
async fn initialization_binds_context_and_verdict_survives_deadline() {
    let ingress =
        std::env::var("RESTATE_INGRESS_URL").expect("bash scripts/test-restate.sh request_owner");
    let admin = std::env::var("RESTATE_ADMIN_URL").unwrap();
    let host = std::env::var("RESTATE_ENDPOINT_HOST").unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap();
    let faults = std::sync::Arc::new(ghinvite_workflows::request_owner::Faults::default());
    let storage = std::sync::Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let builder =
        ghinvite_workflows::projection::bind(restate_sdk::endpoint::Endpoint::builder(), storage);
    let endpoint = ghinvite_workflows::request_owner::bind_with_faults(builder, faults.clone())
        .bind(ghinvite_workflows::request_owner::AdmissionProtocol)
        .build();
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        restate_sdk::http_server::HttpServer::new(endpoint)
            .serve(listener)
            .await;
    });
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
    let request_id = ghinvite_core::RequestId::new();
    let link_id = ghinvite_core::InvitationLinkId::new();
    let deadline = chrono::Utc::now() + chrono::Duration::seconds(3);
    let input = json!({
        "request": {"request_id":request_id,"link_id":link_id,"account_id":100,
            "requester_id":8,"justification":null,"state":"pending",
            "admitted_at":deadline - chrono::Duration::days(7),
            "decision_deadline":deadline,"revision":1},
        "installation_id":1,"repos":[{"repo_id":10,"repo_full_name":"acme/api"}],
        "permission":"push","approval_required":true
    });
    let call = |method: &str, input: Value| {
        let target = if matches!(method, "initialize" | "eligibility") {
            format!("AdmissionProtocol/{link_id}")
        } else {
            format!("InvitationRequest/{request_id}")
        };
        client
            .post(format!("{ingress}/{target}/{method}"))
            .json(&input)
    };
    *faults.stage.lock().unwrap() = Some("initialization-recorded".into());
    let sending = call("initialize", input.clone());
    let pending = tokio::spawn(async move { sending.send().await.unwrap() });
    tokio::time::timeout(Duration::from_secs(5), async {
        while faults.reached.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("interrupted initialization");
    assert!(
        !pending.is_finished(),
        "the link cannot acknowledge before initialization finishes"
    );
    *faults.stage.lock().unwrap() = None;
    let response = pending.await.unwrap();
    assert!(
        response.status().is_success(),
        "initialize: {}",
        response.text().await.unwrap()
    );
    let verdict_input = json!({"request_id":request_id,"attempt":{
        "link_id":link_id,"operation_id":ghinvite_core::RequestId::new(),"requester_id":8}});
    *faults.stage.lock().unwrap() = Some("verdict-recorded".into());
    faults.reached.store(0, std::sync::atomic::Ordering::SeqCst);
    let sending = call("eligibility", verdict_input.clone());
    let pending = tokio::spawn(async move { sending.send().await.unwrap() });
    tokio::time::timeout(Duration::from_secs(5), async {
        while faults.reached.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("interrupted after verdict retention");
    tokio::time::sleep(Duration::from_secs(4)).await;
    *faults.stage.lock().unwrap() = None;
    let response = pending.await.unwrap();
    assert!(
        response.status().is_success(),
        "eligibility: {}",
        response.text().await.unwrap()
    );
    let verdict: Value = response.json().await.unwrap();
    assert_eq!(verdict["blocking"], true);
    let replay: Value = call("eligibility", verdict_input.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        replay, verdict,
        "the original blocking verdict binds the attempt across its deadline"
    );
    let mut fresh = verdict_input.clone();
    fresh["attempt"]["operation_id"] = json!(ghinvite_core::RequestId::new());
    let fresh: Value = call("eligibility", fresh)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(fresh["blocking"], false);
    assert_eq!(fresh["state"], "expired");
    assert!(
        call("initialize", input.clone())
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let status: Value = call(
        "request_status",
        json!({"link_id":link_id,"request_id":request_id,"requester_id":8}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(
        status["state"], "expired",
        "initialization cannot reset terminal state"
    );
    let mut changed = input;
    changed["permission"] = json!("pull");
    assert_eq!(
        call("initialize", changed).send().await.unwrap().status(),
        409
    );
    let mut changed = verdict_input;
    changed["attempt"]["justification"] = json!("changed input");
    assert_eq!(
        call("eligibility", changed).send().await.unwrap().status(),
        409
    );
    server.abort();
}
