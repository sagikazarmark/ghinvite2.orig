//! Request/link protocol acceptance against a disposable real Restate runtime.
#![cfg(feature = "integration")]

use serde_json::{Value, json};
use std::time::Duration;

#[tokio::test]
async fn initialization_binds_context_and_verdict_survives_deadline() {
    let harness = ghinvite_e2e::Harness::start().await;
    let ingress = harness.restate.ingress_url().to_owned();
    let client = harness.client.clone();
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
    harness.restate.deploy(endpoint).await;
    let request_id = ghinvite_core::RequestId::new();
    let link_id = ghinvite_core::InvitationLinkId::new();
    let deadline = chrono::Utc::now() + chrono::Duration::seconds(3);
    let input = json!({
        "request": {"request_id":request_id,"link_id":link_id,"account_id":100,
            "requester_id":8,"justification":null,"state":"pending",
            "admitted_at":deadline - chrono::Duration::days(7),
            "decision_deadline":deadline,"revision":1},
        "installation_id":1,"targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/api","grant":"write"}]},
        "approval_required":true
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
    let owned: Value = call(
        "requester_status",
        json!({"request_id":request_id,"requester_id":8}),
    )
    .send()
    .await
    .unwrap()
    .error_for_status()
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(owned["request"]["request_id"], json!(request_id));
    assert_eq!(owned["request"]["state"], "pending");
    assert_eq!(owned["targets"], input["targets"]);
    assert_eq!(owned["repositories"], json!([]));
    assert!(owned["organization"].is_null());
    for foreign in [0, 9] {
        assert_eq!(
            call(
                "requester_status",
                json!({"request_id":request_id,"requester_id":foreign}),
            )
            .send()
            .await
            .unwrap()
            .status(),
            reqwest::StatusCode::NOT_FOUND
        );
    }
    let verdict_input = json!({"request_id":request_id,"attempt":{"scope_evidence":[],"secret_sha256":"2a".repeat(32),"secret_generation":1,
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
    let owned: Value = call(
        "requester_status",
        json!({"request_id":request_id,"requester_id":8}),
    )
    .send()
    .await
    .unwrap()
    .error_for_status()
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(owned["request"]["state"], "expired");
    assert_eq!(owned["targets"], input["targets"]);
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
    changed["targets"]["repos"][0]["grant"] = json!("read");
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
    harness.finish().await;
}
