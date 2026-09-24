//! Real link/request owners: replay, arbitration, races and retained business state.
#![cfg(feature = "integration")]
use ghinvite_core::{InvitationLinkId, RequestId};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

struct Runtime {
    client: reqwest::Client,
    ingress: String,
    admin: String,
    link_faults: Arc<ghinvite_workflows::admission::Faults>,
    request_faults: Arc<ghinvite_workflows::request_owner::Faults>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Runtime {
    async fn start() -> Self {
        let ingress = std::env::var("RESTATE_INGRESS_URL")
            .expect("bash scripts/test-restate.sh authoritative_admission");
        let admin = std::env::var("RESTATE_ADMIN_URL").unwrap();
        let host = std::env::var("RESTATE_ENDPOINT_HOST").unwrap();
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(25))
            .build()
            .unwrap();
        let link_faults = Arc::new(ghinvite_workflows::admission::Faults::default());
        let request_faults = Arc::new(ghinvite_workflows::request_owner::Faults::default());
        let storage = Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::in_memory()
                .await
                .unwrap(),
        );
        let github = Arc::new(ghinvite_github::InstallationClient::new(
            Arc::new(ghinvite_github::transport::ReqwestTransport::with_client(
                client.clone(),
            )),
            ghinvite_github::jwt::AppJwtSigner::from_pem(
                123,
                include_str!("../../ghinvite-github/src/jwt_test_key.pem"),
            )
            .unwrap(),
        ));
        let state = ghinvite_workflows::AppState::new(storage.clone(), github);
        let builder = ghinvite_workflows::admission::bind_with_faults(
            restate_sdk::endpoint::Endpoint::builder(),
            link_faults.clone(),
        );
        let builder =
            ghinvite_workflows::request_owner::bind_with_faults(builder, request_faults.clone());
        let builder = ghinvite_workflows::projection::bind(builder, storage);
        let endpoint = ghinvite_workflows::delivery::bind(builder, state.clone())
            .bind(ghinvite_workflows::availability::AccountInstallation { state })
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
        Self {
            client,
            ingress,
            admin,
            link_faults,
            request_faults,
            server,
        }
    }
    fn call(
        &self,
        service: &str,
        key: impl ToString,
        handler: &str,
        input: &Value,
    ) -> reqwest::RequestBuilder {
        let request = self.client.post(format!(
            "{}/{}/{}/{}",
            self.ingress,
            service,
            key.to_string(),
            handler
        ));
        if input.is_null() {
            request
        } else {
            request.json(input)
        }
    }
    async fn ok(&self, service: &str, key: impl ToString, handler: &str, input: &Value) -> Value {
        let response = self
            .call(service, key, handler, input)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let text = response.text().await.unwrap();
        assert!(status.is_success(), "{handler}: {status} {text}");
        if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap()
        }
    }
    async fn create(&self, max: Option<u32>) -> InvitationLinkId {
        let id = InvitationLinkId::new();
        self.ok("InvitationLink", id, "create", &json!({"link_id":id,"account_id":100,"installation_id":1,
            "admin":{"account_id":100,"user_id":7},"description":"Request ownership","approval_required":true,
            "permission":"push","max_uses":max,"repos":[{"repo_id":10,"repo_full_name":"acme/api"}]})).await;
        id
    }
    async fn uses(&self, id: InvitationLinkId) -> Value {
        self.ok(
            "InvitationLink",
            id,
            "link_status",
            &json!({"link_id":id,"admin":{"account_id":100,"user_id":7}}),
        )
        .await["uses"]
            .clone()
    }
}
fn attempt(id: InvitationLinkId, user: u64) -> Value {
    json!({"link_id":id,"operation_id":RequestId::new(),"requester_id":user,"justification":"Docs"})
}
fn query(id: InvitationLinkId, receipt: &Value, user: u64) -> Value {
    json!({"link_id":id,"request_id":receipt["result"]["request_id"],"requester_id":user})
}
fn decision(id: InvitationLinkId, receipt: &Value, action: Value) -> Value {
    json!({"link_id":id,"request_id":receipt["result"]["request_id"],"operation_id":RequestId::new(),
        "admin":{"account_id":100,"user_id":7},"action":action})
}

#[tokio::test]
async fn request_authority_contract() {
    let runtime = Runtime::start().await;
    let r = &runtime;
    // A failed short initialization retains handoff progress, never another use.
    let unavailable_link = r.create(None).await;
    let unavailable_attempt = attempt(unavailable_link, 8);
    r.request_faults
        .unavailable
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        r.call(
            "InvitationLink",
            unavailable_link,
            "admit",
            &unavailable_attempt
        )
        .send()
        .await
        .unwrap()
        .status(),
        503
    );
    assert_eq!(r.uses(unavailable_link).await, 1);
    r.request_faults
        .unavailable
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let recovered = r
        .ok(
            "InvitationLink",
            unavailable_link,
            "admit",
            &unavailable_attempt,
        )
        .await;
    assert_eq!(recovered["result"]["kind"], "accepted");
    assert_eq!(r.uses(unavailable_link).await, 1);
    let fresh = attempt(unavailable_link, 8);
    r.request_faults
        .unavailable
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        r.call("InvitationLink", unavailable_link, "admit", &fresh)
            .send()
            .await
            .unwrap()
            .status(),
        503
    );
    r.request_faults
        .unavailable
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let rid = recovered["result"]["request_id"].as_str().unwrap();
    r.ok(
        "InvitationRequest",
        rid,
        "decide",
        &decision(unavailable_link, &recovered, json!({"kind":"decline"})),
    )
    .await;
    assert_eq!(
        r.ok("InvitationLink", unavailable_link, "admit", &fresh)
            .await["result"]["kind"],
        "accepted"
    );
    assert_eq!(r.uses(unavailable_link).await, 2);
    // Last-use serialization and immutable replay, including changed-input conflicts.
    let id = r.create(Some(1)).await;
    let a = attempt(id, 8);
    let b = attempt(id, 9);
    let (a_receipt, b_receipt) = tokio::join!(
        r.ok("InvitationLink", id, "admit", &a),
        r.ok("InvitationLink", id, "admit", &b)
    );
    assert_eq!(r.uses(id).await, 1);
    assert_ne!(a_receipt["result"]["kind"], b_receipt["result"]["kind"]);
    assert_eq!(r.ok("InvitationLink", id, "admit", &a).await, a_receipt);
    let mut changed = a.clone();
    changed["justification"] = json!("different");
    assert_eq!(
        r.call("InvitationLink", id, "admit", &changed)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );

    // Terminal-then-fresh admission consults the request; stale old timers do not release the new pointer.
    let id = r.create(None).await;
    let a = attempt(id, 8);
    let receipt = r.ok("InvitationLink", id, "admit", &a).await;
    let rid = receipt["result"]["request_id"].as_str().unwrap();
    let decline = decision(
        id,
        &receipt,
        json!({"kind":"decline","reason":"  private reason  "}),
    );
    let decided = r.ok("InvitationRequest", rid, "decide", &decline).await;
    assert_eq!(decided["outcome"], "applied");
    assert_eq!(
        decided["request"]["decision"]["decline_reason"],
        "private reason"
    );
    assert_eq!(
        r.ok("InvitationRequest", rid, "decide", &decline).await,
        decided
    );
    let safe = r
        .ok(
            "InvitationRequest",
            rid,
            "request_status",
            &query(id, &receipt, 8),
        )
        .await;
    assert_eq!(safe["decision"]["decline_reason"], Value::Null);
    let fresh = r.ok("InvitationLink", id, "admit", &attempt(id, 8)).await;
    assert_eq!(fresh["result"]["kind"], "accepted");
    r.ok("InvitationRequest", rid, "tick_expire", &Value::Null)
        .await;
    let blocked = r.ok("InvitationLink", id, "admit", &attempt(id, 8)).await;
    assert_eq!(blocked["result"]["reason"], "existing_request");
    assert_eq!(r.uses(id).await, 2);
    assert_eq!(r.ok("InvitationLink", id, "admit", &a).await, receipt);
    let mut different = decline.clone();
    different["action"] = json!({"kind":"approve"});
    assert_eq!(
        r.call("InvitationRequest", rid, "decide", &different)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    different["operation_id"] = json!(RequestId::new());
    assert_eq!(
        r.ok("InvitationRequest", rid, "decide", &different).await["outcome"],
        "incompatible"
    );

    // Concurrent approve/decline has one recorded winner and truthful losing response.
    let id = r.create(None).await;
    let receipt = r.ok("InvitationLink", id, "admit", &attempt(id, 8)).await;
    let rid = receipt["result"]["request_id"].as_str().unwrap();
    let approve = decision(id, &receipt, json!({"kind":"approve"}));
    let decline = decision(id, &receipt, json!({"kind":"decline"}));
    let (approved, declined) = tokio::join!(
        r.ok("InvitationRequest", rid, "decide", &approve),
        r.ok("InvitationRequest", rid, "decide", &decline)
    );
    assert_eq!(approved["request"]["state"], declined["request"]["state"]);
    assert_ne!(approved["outcome"], declined["outcome"]);

    // Initialization interruption cannot expose acceptance early; recovery retains the same use.
    let id = r.create(None).await;
    let a = attempt(id, 8);
    r.link_faults.arm("after-initialize");
    let sending = r.call("InvitationLink", id, "admit", &a);
    let pending = tokio::spawn(async move { sending.send().await.unwrap() });
    tokio::time::timeout(Duration::from_secs(5), async {
        while r.link_faults.attempts() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!pending.is_finished());
    r.link_faults.release();
    let receipt: Value = pending.await.unwrap().json().await.unwrap();
    assert_eq!(r.ok("InvitationLink", id, "admit", &a).await, receipt);
    assert_eq!(r.uses(id).await, 1);

    // A blocking verdict published before the deadline binds the rejection even if the link resumes late.
    let id = r.create(None).await;
    let deadline = chrono::Utc::now() + chrono::Duration::seconds(3);
    r.link_faults
        .set_clock(Some(deadline - chrono::Duration::days(7)));
    let original = r.ok("InvitationLink", id, "admit", &attempt(id, 8)).await;
    r.link_faults.set_clock(None);
    r.link_faults.arm("after-verdict");
    let a = attempt(id, 8);
    let sending = r.call("InvitationLink", id, "admit", &a);
    let pending = tokio::spawn(async move { sending.send().await.unwrap() });
    tokio::time::timeout(Duration::from_secs(5), async {
        while r.link_faults.attempts() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_secs(4)).await;
    r.link_faults.release();
    let rejected: Value = pending.await.unwrap().json().await.unwrap();
    assert_eq!(rejected["result"]["reason"], "existing_request");
    assert_eq!(r.ok("InvitationLink", id, "admit", &a).await, rejected);
    assert_eq!(
        r.ok("InvitationLink", id, "admit", &attempt(id, 8)).await["result"]["kind"],
        "accepted"
    );
    assert_eq!(r.uses(id).await, 2);
    let rid = original["result"]["request_id"].as_str().unwrap();
    // Actual invocation cleanup, followed by original decision/admission recovery.
    tokio::time::timeout(Duration::from_secs(15), async { loop {
        let result: Value = r.client.post(format!("{}/query", r.admin)).header("accept", "application/json")
            .json(&json!({"query":format!("SELECT id FROM sys_invocation WHERE target_service_name = 'InvitationRequest' AND target_service_key = '{rid}' AND status = 'completed'")}))
            .send().await.unwrap().json().await.unwrap();
        if result["rows"].as_array().unwrap().is_empty() { break; }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }}).await.expect("actual request invocation cleanup");
    assert_eq!(r.ok("InvitationLink", id, "admit", &a).await, rejected);
    assert_eq!(
        r.ok(
            "InvitationRequest",
            rid,
            "request_status",
            &query(id, &original, 8)
        )
        .await["state"],
        "expired"
    );
    // Strict deadline arbitration, including a fully journaled timely decision
    // whose after-images are applied after the deadline.
    for (offset, expected) in [(-1, "approved"), (0, "expired"), (1, "expired")] {
        let id = r.create(None).await;
        let receipt = r.ok("InvitationLink", id, "admit", &attempt(id, 8)).await;
        let rid = receipt["result"]["request_id"].as_str().unwrap();
        let deadline: chrono::DateTime<chrono::Utc> =
            serde_json::from_value(receipt["result"]["decision_deadline"].clone()).unwrap();
        *r.request_faults.clock.lock().unwrap() =
            Some(deadline + chrono::Duration::milliseconds(offset));
        let approve = decision(id, &receipt, json!({"kind":"approve"}));
        let decided = r.ok("InvitationRequest", rid, "decide", &approve).await;
        assert_eq!(decided["request"]["state"], expected);
        *r.request_faults.clock.lock().unwrap() = None;
    }
    for (stage, expected) in [
        ("before-decision", "expired"),
        ("after-decision", "approved"),
    ] {
        let id = r.create(None).await;
        let receipt = r.ok("InvitationLink", id, "admit", &attempt(id, 8)).await;
        let rid = receipt["result"]["request_id"].as_str().unwrap();
        let deadline: chrono::DateTime<chrono::Utc> =
            serde_json::from_value(receipt["result"]["decision_deadline"].clone()).unwrap();
        *r.request_faults.clock.lock().unwrap() =
            Some(deadline - chrono::Duration::milliseconds(1));
        *r.request_faults.stage.lock().unwrap() = Some(stage.into());
        r.request_faults
            .reached
            .store(0, std::sync::atomic::Ordering::SeqCst);
        let approve = decision(id, &receipt, json!({"kind":"approve"}));
        let sending = r.call("InvitationRequest", rid, "decide", &approve);
        let pending = tokio::spawn(async move { sending.send().await.unwrap() });
        tokio::time::timeout(Duration::from_secs(5), async {
            while r
                .request_faults
                .reached
                .load(std::sync::atomic::Ordering::SeqCst)
                == 0
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        *r.request_faults.clock.lock().unwrap() = Some(deadline);
        *r.request_faults.stage.lock().unwrap() = None;
        let decided: Value = pending.await.unwrap().json().await.unwrap();
        assert_eq!(decided["request"]["state"], expected);
        assert_eq!(
            r.ok("InvitationRequest", rid, "decide", &approve).await,
            decided
        );
        *r.request_faults.clock.lock().unwrap() = None;
    }
}
