//! #53 acceptance through real Restate ingress. No SQL adapter is bound.
#![cfg(feature = "integration")]

use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use admission_v1::{
    InvitationProjectionV1, InvitationRequestV1, ProjectionEnvelope, WorkflowEnvelope,
};
use ghinvite_core::InvitationLinkId;
use ghinvite_workflows::admission_v1;
use restate_sdk::context::{Context, ContextSideEffects, RunFuture, WorkflowContext};
use restate_sdk::endpoint::{
    Endpoint, HandleOptions, HandlerOptions, ProtocolMode, ServiceOptions,
};
use restate_sdk::errors::{HandlerError, TerminalError};
use restate_sdk::serde::Json;
use serde_json::{Value, json};
use tokio::time::{sleep, timeout};

struct Runtime {
    client: reqwest::Client,
    ingress: String,
    admin: String,
    server: tokio::task::JoinHandle<()>,
    received: Arc<Mutex<Received>>,
    faults: Arc<admission_v1::Faults>,
    transport: Arc<Transport>,
    offline: Arc<AtomicBool>,
}

#[derive(Default)]
struct Transport {
    tasks: Mutex<Vec<tokio::task::AbortHandle>>,
    max_body: AtomicUsize,
}

async fn serve(
    State((endpoint, transport)): State<(Endpoint, Arc<Transport>)>,
    request: Request,
) -> axum::response::Response {
    let (parts, body) = request.into_parts();
    let bytes = to_bytes(body, 1024 * 1024).await.unwrap();
    if parts.uri.path().contains("/InvitationLinkV1/") {
        transport.max_body.fetch_max(bytes.len(), Ordering::SeqCst);
    }
    let task = tokio::spawn(async move {
        let response = endpoint.handle_with_options(
            Request::from_parts(parts, Body::from(bytes)),
            HandleOptions {
                protocol_mode: ProtocolMode::RequestResponse,
            },
        );
        let (parts, body) = response.into_parts();
        let bytes = to_bytes(Body::new(body), 1024 * 1024).await.unwrap();
        axum::response::Response::from_parts(parts, Body::from(bytes))
    });
    transport.tasks.lock().unwrap().push(task.abort_handle());
    task.await.unwrap_or_else(|_| {
        axum::response::Response::builder()
            .status(503)
            .body(Body::from("fixture: endpoint interrupted"))
            .unwrap()
    })
}

#[derive(Default)]
struct Received {
    projections: Vec<ProjectionEnvelope>,
    workflows: Vec<WorkflowEnvelope>,
}

struct Consumer(Arc<Mutex<Received>>, Arc<AtomicBool>);

impl InvitationProjectionV1 for Consumer {
    async fn apply_transition(
        &self,
        ctx: Context<'_>,
        Json(envelope): Json<ProjectionEnvelope>,
    ) -> Result<(), TerminalError> {
        ctx.run(|| async {
            if self.1.load(Ordering::SeqCst) {
                return Err(std::io::Error::other("fixture: SQL projection unavailable").into());
            }
            self.0.lock().unwrap().projections.push(envelope.clone());
            Ok::<_, HandlerError>(())
        })
        .name("receive_projection")
        .retry_policy(
            restate_sdk::context::RunRetryPolicy::default()
                .initial_delay(Duration::from_millis(100))
                .max_delay(Duration::from_millis(200)),
        )
        .await
    }
}

impl InvitationRequestV1 for Consumer {
    async fn run(
        &self,
        ctx: WorkflowContext<'_>,
        Json(envelope): Json<WorkflowEnvelope>,
    ) -> Result<(), TerminalError> {
        ctx.run(|| async {
            self.0.lock().unwrap().workflows.push(envelope.clone());
            Ok::<_, HandlerError>(())
        })
        .name("receive_workflow")
        .await
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Runtime {
    async fn start() -> Self {
        let env = |key| {
            std::env::var(key).expect(
                "real Restate required: bash scripts/test-restate.sh authoritative_admission",
            )
        };
        let admin = env("RESTATE_ADMIN_URL");
        let ingress = env("RESTATE_INGRESS_URL");
        let host = env("RESTATE_ENDPOINT_HOST");
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap();
        timeout(Duration::from_secs(15), async {
            loop {
                if client
                    .get(format!("{admin}/health"))
                    .send()
                    .await
                    .is_ok_and(|r| r.status().is_success())
                {
                    break;
                }
                sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("Restate admin unavailable");
        let received = Arc::new(Mutex::new(Received::default()));
        let faults = Arc::new(admission_v1::Faults::default());
        let offline = Arc::new(AtomicBool::new(true));
        let endpoint = admission_v1::bind_with_faults(Endpoint::builder(), faults.clone())
            .bind(InvitationProjectionV1::serve(Consumer(
                received.clone(),
                offline.clone(),
            )))
            .bind_with_options(
                InvitationRequestV1::serve(Consumer(received.clone(), offline.clone())),
                ServiceOptions::new().handler(
                    "run",
                    HandlerOptions::new().workflow_retention(Duration::from_secs(2)),
                ),
            )
            .build();
        let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let transport = Arc::new(Transport::default());
        let app = axum::Router::new()
            .fallback(serve)
            .with_state((endpoint, transport.clone()));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let response = client
            .post(format!("{admin}/deployments"))
            .json(&json!({"uri": format!("http://{host}:{port}")}))
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "deployment: {}",
            response.text().await.unwrap()
        );
        Self {
            client,
            ingress,
            admin,
            server,
            received,
            faults,
            transport,
            offline,
        }
    }

    async fn command(&self, id: &str, handler: &str, input: &Value) -> reqwest::Response {
        self.client
            .post(format!("{}/InvitationLinkV1/{id}/{handler}", self.ingress))
            .json(input)
            .send()
            .await
            .unwrap()
    }

    async fn ok(&self, id: &str, handler: &str, input: &Value) -> Value {
        let response = self.command(id, handler, input).await;
        let status = response.status();
        let text = response.text().await.unwrap();
        assert!(status.is_success(), "{handler}: {status} {text}");
        serde_json::from_str(&text).unwrap()
    }

    async fn invocations(&self, id: &str) -> Vec<Value> {
        let response = self.client.post(format!("{}/query", self.admin))
            .header("accept", "application/json")
            .json(&json!({"query": format!("SELECT id, status FROM sys_invocation WHERE target_service_key = '{id}'")}))
            .send().await.unwrap();
        let status = response.status();
        let text = response.text().await.unwrap();
        assert!(status.is_success(), "introspection: {status} {text}");
        let value: Value = serde_json::from_str(&text).unwrap();
        value["rows"].as_array().expect(&text).clone()
    }
}

fn creation() -> Value {
    json!({
        "version": 1,
        "link_id": InvitationLinkId::new(),
        "admin": {"account_id": 100, "user_id": 7},
        "account_id": 100, "installation_id": 1,
        "description": "Workshop", "internal_note": "Private context",
        "expires_at": null, "max_uses": 1,
        "permission": "pull", "approval_required": true,
        "repos": [{"repo_id": 10, "repo_full_name": "acme/api"}]
    })
}

#[tokio::test]
async fn authoritative_admission_contract() {
    timeout(Duration::from_secs(120), async {
        let runtime = Runtime::start().await;
        let input = creation();
        let id = input["link_id"].as_str().unwrap();
        let created = runtime.ok(id, "create", &input).await;
        assert_eq!(created["link_id"], id);
        assert_eq!(created["uses"], 0);
        assert_eq!(created, runtime.ok(id, "create", &input).await);
        let mut changed = input.clone();
        changed["max_uses"] = json!(2);
        assert_eq!(runtime.command(id, "create", &changed).await.status(), 409);
        let mut foreign = creation();
        foreign["admin"]["account_id"] = json!(200);
        assert_eq!(
            runtime
                .command(foreign["link_id"].as_str().unwrap(), "create", &foreign)
                .await
                .status(),
            404
        );
        for field in ["description", "repos", "max_uses"] {
            let mut invalid = creation();
            invalid[field] = match field {
                "description" => json!(" "),
                "repos" => json!([]),
                _ => json!(0),
            };
            assert_eq!(
                runtime
                    .command(invalid["link_id"].as_str().unwrap(), "create", &invalid)
                    .await
                    .status(),
                400
            );
        }
        let operation = ghinvite_core::RequestId::new().to_string();
        let admission = json!({"version": 1, "link_id": id, "operation_id": operation,
            "requester_id": 11, "justification": " access "});
        let receipt = runtime.ok(id, "admit", &admission).await;
        assert_eq!(receipt["result"]["kind"], "accepted");
        assert_eq!(receipt["result"]["state"], "pending");
        assert_ne!(receipt["result"]["request_id"], operation);
        let admitted: chrono::DateTime<chrono::Utc> =
            serde_json::from_value(receipt["decided_at"].clone()).unwrap();
        let deadline: chrono::DateTime<chrono::Utc> =
            serde_json::from_value(receipt["result"]["decision_deadline"].clone()).unwrap();
        assert_eq!(deadline - admitted, chrono::Duration::days(7));
        let mut replay = admission.clone();
        replay["operation_id"] = json!(operation.to_lowercase());
        replay["justification"] = json!("access");
        assert_eq!(runtime.ok(id, "admit", &replay).await, receipt);
        for field in ["requester_id", "justification", "version"] {
            let mut changed = admission.clone();
            changed[field] = match field {
                "requester_id" => json!(12),
                "version" => json!(2),
                _ => json!("secret changed"),
            };
            let response = runtime.command(id, "admit", &changed).await;
            assert_eq!(response.status(), 409);
            let text = response.text().await.unwrap();
            assert!(!text.contains(receipt["result"]["request_id"].as_str().unwrap()));
            assert!(!text.contains("access"));
        }
        let status = runtime
            .ok(
                id,
                "request_status",
                &json!({"link_id": id,
            "request_id": receipt["result"]["request_id"], "requester_id": 11}),
            )
            .await;
        assert_eq!(status["state"], "pending");
        assert_eq!(status["justification"], "access");
        assert!(!status.to_string().contains("Private context"));
        assert_eq!(
            runtime
                .command(
                    id,
                    "request_status",
                    &json!({"link_id": id,
            "request_id": receipt["result"]["request_id"], "requester_id": 12})
                )
                .await
                .status(),
            404
        );
        let revoked = runtime
            .ok(
                id,
                "revoke",
                &json!({"link_id": id, "admin": input["admin"]}),
            )
            .await;
        assert_eq!(revoked["uses"], 1);
        assert!(revoked["revoked_at"].is_string());
        assert_eq!(runtime.ok(id, "admit", &admission).await, receipt);
        assert_eq!(created, runtime.ok(id, "create", &input).await);
        let mut fresh = admission.clone();
        fresh["operation_id"] = json!(ghinvite_core::RequestId::new());
        assert_eq!(
            runtime.ok(id, "admit", &fresh).await["result"],
            json!({"kind": "rejected", "reason": "revoked"})
        );
        assert!(
            runtime.received.lock().unwrap().projections.is_empty(),
            "projection outage must be real"
        );
        runtime.offline.store(false, Ordering::SeqCst);
        timeout(Duration::from_secs(10), async {
            loop {
                let ready = {
                    let received = runtime.received.lock().unwrap();
                    received.projections.len() == 3 && received.workflows.len() == 1
                };
                if ready {
                    break;
                }
                sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("durable consumers did not receive creation/admission/revoke");
        {
            let received = runtime.received.lock().unwrap();
            let workflow = &received.workflows[0];
            assert_eq!(workflow.version, 1);
            assert_eq!(
                workflow.request.request_id.to_string(),
                receipt["result"]["request_id"]
            );
            assert_eq!(workflow.request.decision_deadline, Some(deadline));
            assert_eq!(workflow.installation_id, 1);
            assert_eq!(workflow.repos[0].repo_full_name, "acme/api");
            assert!(
                !serde_json::to_string(workflow)
                    .unwrap()
                    .contains("Private context")
            );
            let events: Vec<_> = received
                .projections
                .iter()
                .flat_map(|e| e.events.iter())
                .collect();
            for kind in [
                "invitation_link.created",
                "request.created",
                "invitation_link.exhausted",
                "invitation_link.revoked",
            ] {
                assert_eq!(
                    events.iter().filter(|e| e.kind.as_str() == kind).count(),
                    1,
                    "{kind}"
                );
            }
        }
        admission_edges(&runtime).await;
        interruption_recovery(&runtime).await;
        queued_expiry(&runtime).await;
        imported_terminal_requests(&runtime).await;
        retention_and_bounded_history(&runtime).await;
    })
    .await
    .expect("authoritative admission acceptance exceeded 120 seconds");
}

async fn queued_expiry(runtime: &Runtime) {
    let mut input = creation();
    input["max_uses"] = json!(10);
    let now = chrono::Utc::now();
    runtime.faults.set_clock(Some(now));
    let expires = now + chrono::Duration::seconds(2);
    input["expires_at"] = json!(expires);
    let id = input["link_id"].as_str().unwrap();
    runtime.ok(id, "create", &input).await;
    runtime.faults.arm("after-decision");
    let first = json!({"version": 1, "link_id": id, "operation_id": ghinvite_core::RequestId::new(), "requester_id": 61});
    let second = json!({"version": 1, "link_id": id, "operation_id": ghinvite_core::RequestId::new(), "requester_id": 62});
    let first_call = runtime.ok(id, "admit", &first);
    tokio::pin!(first_call);
    tokio::select! {
        _ = &mut first_call => panic!("admission completed before fault release"),
        _ = async {
            timeout(Duration::from_secs(15), async {
                while runtime.faults.attempts() < 2 { sleep(Duration::from_millis(20)).await; }
            }).await.expect("checkpoint readiness");
        } => {}
    }
    let second_call = runtime.ok(id, "admit", &second);
    tokio::pin!(second_call);
    assert!(
        timeout(Duration::from_millis(100), &mut second_call)
            .await
            .is_err()
    );
    runtime.faults.set_clock(Some(expires));
    runtime.faults.release();
    for task in runtime.transport.tasks.lock().unwrap().drain(..) {
        if !task.is_finished() {
            task.abort();
        }
    }
    let (accepted, expired) = tokio::join!(first_call, second_call);
    assert_eq!(accepted["result"]["kind"], "accepted");
    assert_eq!(
        expired["result"]["reason"], "expired",
        "queued attempt used submission-time eligibility"
    );
    let deadline: chrono::DateTime<chrono::Utc> =
        serde_json::from_value(accepted["result"]["decision_deadline"].clone()).unwrap();
    assert!(
        deadline > expires,
        "pending deadline was capped at link expiry"
    );
    runtime.faults.set_clock(None);
}

async fn interruption_recovery(runtime: &Runtime) {
    for stage in [
        "before-decision",
        "after-decision",
        "after-link",
        "after-request",
        "after-blocker",
        "after-outcome",
        "after-projection-send",
        "after-workflow-send",
    ] {
        let mut input = creation();
        input["max_uses"] = json!(10);
        // Hold the clock before expiry until the runtime reaches the checkpoint;
        // neither CI scheduling nor lazy-state round trips consume this window.
        let now = chrono::Utc::now();
        runtime.faults.set_clock(Some(now));
        let expires =
            now + chrono::Duration::seconds(if stage.contains("decision") { 2 } else { 60 });
        input["expires_at"] = json!(expires);
        let id = input["link_id"].as_str().unwrap();
        runtime.ok(id, "create", &input).await;
        runtime.faults.arm(stage);
        let attempt = json!({"version": 1, "link_id": id, "operation_id": ghinvite_core::RequestId::new(), "requester_id": 41});
        let client = runtime.client.clone();
        let url = format!("{}/InvitationLinkV1/{id}/admit", runtime.ingress);
        let payload = attempt.clone();
        let admission = tokio::spawn(async move {
            let response = client.post(url).json(&payload).send().await.unwrap();
            assert!(response.status().is_success());
            response.json::<Value>().await.unwrap()
        });
        timeout(Duration::from_secs(15), async {
            while runtime.faults.attempts() < 2 {
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("checkpoint replay not reached");
        let admin = json!({"link_id": id, "admin": input["admin"]});
        let status = runtime.ok(id, "link_status", &admin);
        tokio::pin!(status);
        assert!(
            timeout(Duration::from_millis(100), &mut status)
                .await
                .is_err(),
            "status observed partial transition at {stage}"
        );
        if stage.contains("decision") {
            runtime.faults.set_clock(Some(expires));
        }
        runtime.faults.release();
        let mut aborted = 0;
        for task in runtime.transport.tasks.lock().unwrap().drain(..) {
            if !task.is_finished() {
                task.abort();
                aborted += 1;
            }
        }
        assert!(aborted > 0, "must interrupt a live SDK execution");
        let receipt = admission.await.unwrap();
        let expected_uses = if stage == "before-decision" { 0 } else { 1 };
        assert_eq!(status.await["uses"], expected_uses, "{stage}");
        if stage == "before-decision" {
            assert_eq!(receipt["result"]["reason"], "expired");
        } else {
            assert_eq!(receipt["result"]["kind"], "accepted", "{stage}");
            let request = runtime
                .ok(
                    id,
                    "request_status",
                    &json!({"link_id": id,
                "request_id": receipt["result"]["request_id"], "requester_id": 41}),
                )
                .await;
            assert_eq!(request["state"], "pending");
            assert_eq!(request["admitted_at"], receipt["decided_at"]);
            assert_eq!(
                request["decision_deadline"],
                receipt["result"]["decision_deadline"]
            );
            if !stage.contains("decision") {
                let mut repeat = attempt.clone();
                repeat["operation_id"] = json!(ghinvite_core::RequestId::new());
                assert_eq!(
                    runtime.ok(id, "admit", &repeat).await["result"]["reason"],
                    "existing_request",
                    "missing recovered blocker at {stage}"
                );
            }
        }
        runtime.ok(id, "revoke", &admin).await;
        assert_eq!(runtime.ok(id, "admit", &attempt).await, receipt, "{stage}");
        timeout(Duration::from_secs(10), async {
            loop {
                {
                    let received = runtime.received.lock().unwrap();
                    let requests: Vec<_> = received
                        .workflows
                        .iter()
                        .filter(|w| w.request.link_id.to_string() == id)
                        .collect();
                    let projections: Vec<_> = received
                        .projections
                        .iter()
                        .filter(|p| p.link.link_id.to_string() == id)
                        .collect();
                    if requests.len() == expected_uses && projections.len() == expected_uses + 2 {
                        let events: Vec<_> = projections
                            .iter()
                            .flat_map(|p| &p.events)
                            .map(|e| &e.event_id)
                            .collect();
                        let unique: std::collections::HashSet<_> = events.iter().collect();
                        assert_eq!(
                            unique.len(),
                            events.len(),
                            "duplicate logical audit intent at {stage}"
                        );
                        break;
                    }
                }
                sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("dispatch did not recover exactly once");
        runtime.faults.set_clock(None);
    }
}

async fn imported_terminal_requests(runtime: &Runtime) {
    // Seed historical state through the runtime maintenance API on new fixture
    // keys. This is fixture setup, not a cancellation/migration command.
    for terminal in ["cancelled", "declined", "expired"] {
        let mut input = creation();
        input["max_uses"] = json!(3);
        let id = input["link_id"].as_str().unwrap();
        let original_link = runtime.ok(id, "create", &input).await;
        let mut link = original_link.clone();
        link["uses"] = json!(1);
        link["revision"] = json!(2);
        let request_id = ghinvite_core::RequestId::new().to_string();
        let old_request = json!({"request_id": request_id, "link_id": id, "account_id": 100,
            "requester_id": 71, "justification": null, "state": terminal,
            "admitted_at": "2026-01-01T00:00:00Z", "decision_deadline": "2026-01-08T00:00:00Z", "revision": 2});
        let mut state = serde_json::Map::new();
        for (key, value) in [
            ("v1/link".to_owned(), link),
            ("v1/creation".to_owned(), original_link),
            (format!("v1/request/{request_id}"), old_request.clone()),
            ("v1/blocker/71".to_owned(), json!(request_id)),
        ] {
            state.insert(key, json!(serde_json::to_vec(&value).unwrap()));
        }
        let response = runtime
            .client
            .post(format!("{}/services/InvitationLinkV1/state", runtime.admin))
            .json(&json!({"object_key": id, "new_state": state}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            202,
            "fixture import: {}",
            response.text().await.unwrap()
        );
        let query = json!({"link_id": id, "request_id": request_id, "requester_id": 71});
        timeout(Duration::from_secs(10), async {
            loop {
                let response = runtime.command(id, "request_status", &query).await;
                if response.status().is_success() {
                    assert_eq!(response.json::<Value>().await.unwrap(), old_request);
                    break;
                }
                sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("historical fixture not imported");
        let attempt = json!({"version": 1, "link_id": id, "operation_id": ghinvite_core::RequestId::new(), "requester_id": 71});
        assert_eq!(
            runtime.ok(id, "admit", &attempt).await["result"]["kind"],
            "accepted"
        );
        assert_eq!(runtime.ok(id, "request_status", &query).await, old_request);
        assert_eq!(
            runtime
                .ok(
                    id,
                    "link_status",
                    &json!({"link_id": id, "admin": input["admin"]})
                )
                .await["uses"],
            2,
            "terminal use was refunded"
        );
    }
}

async fn retention_and_bounded_history(runtime: &Runtime) {
    let input = creation();
    let id = input["link_id"].as_str().unwrap();
    runtime.ok(id, "create", &input).await;
    let attempt = json!({"version": 1, "link_id": id, "operation_id": ghinvite_core::RequestId::new(), "requester_id": 51});
    let receipt = runtime.ok(id, "admit", &attempt).await;
    let request = receipt["result"]["request_id"].as_str().unwrap();
    assert!(
        !runtime.invocations(id).await.is_empty(),
        "command journal must first be retained"
    );
    assert!(
        !runtime.invocations(request).await.is_empty(),
        "workflow must first exist"
    );
    let cleanup = timeout(Duration::from_secs(15), async {
        loop {
            if runtime.invocations(id).await.is_empty()
                && runtime.invocations(request).await.is_empty()
            {
                break;
            }
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    assert!(
        cleanup.is_ok(),
        "retention did not expire: link {:?}, workflow {:?}",
        runtime.invocations(id).await,
        runtime.invocations(request).await
    );
    assert_eq!(runtime.ok(id, "admit", &attempt).await, receipt);
    // Accumulate > 512 KiB of retained rejected outcomes under the same link.
    // Request-response transport must still carry only directly accessed keys.
    for n in 0..40 {
        let rejected = json!({"version": 1, "link_id": id,
            "operation_id": ghinvite_core::RequestId::new(), "requester_id": 100 + n,
            "justification": "large retained private justification ".repeat(400)});
        assert_eq!(
            runtime.ok(id, "admit", &rejected).await["result"]["reason"],
            "exhausted"
        );
    }
    runtime.transport.max_body.store(0, Ordering::SeqCst);
    assert_eq!(runtime.ok(id, "admit", &attempt).await, receipt);
    assert_eq!(
        runtime
            .ok(
                id,
                "link_status",
                &json!({"link_id": id, "admin": input["admin"]})
            )
            .await["uses"],
        1
    );
    assert!(
        runtime.transport.max_body.load(Ordering::SeqCst) < 64 * 1024,
        "history eagerly transferred"
    );
    assert!(
        runtime.invocations(request).await.is_empty(),
        "replay resurrected completed workflow"
    );
    let received = runtime.received.lock().unwrap();
    assert_eq!(
        received
            .workflows
            .iter()
            .filter(|w| w.request.request_id.to_string() == request)
            .count(),
        1
    );
}

async fn admission_edges(runtime: &Runtime) {
    let mut input = creation();
    input["max_uses"] = json!(10);
    input["approval_required"] = json!(false);
    let id = input["link_id"].as_str().unwrap();
    runtime.ok(id, "create", &input).await;
    let mut attempt = json!({"version": 1, "link_id": id,
        "operation_id": ghinvite_core::RequestId::new(), "requester_id": 20});
    for malformed in [
        json!(null),
        json!(""),
        json!("../../bad"),
        json!("8ZZZZZZZZZZZZZZZZZZZZZZZZZ"),
        json!("0000000000000000000000000I"),
    ] {
        let mut bad = attempt.clone();
        bad["operation_id"] = malformed;
        assert_eq!(runtime.command(id, "admit", &bad).await.status(), 400);
    }
    let mut missing = attempt.clone();
    missing.as_object_mut().unwrap().remove("operation_id");
    assert_eq!(runtime.command(id, "admit", &missing).await.status(), 400);
    let accepted = runtime.ok(id, "admit", &attempt).await;
    assert_eq!(accepted["result"]["state"], "approved");
    assert_eq!(accepted["result"]["decision_deadline"], Value::Null);
    attempt["justification"] = json!(" \n\t ");
    assert_eq!(runtime.ok(id, "admit", &attempt).await, accepted);
    let operation = attempt["operation_id"].clone();
    attempt["operation_id"] = json!(ghinvite_core::RequestId::new());
    let rejected = runtime.ok(id, "admit", &attempt).await;
    assert_eq!(rejected["result"]["reason"], "existing_request");
    runtime
        .ok(
            id,
            "revoke",
            &json!({"link_id": id, "admin": input["admin"]}),
        )
        .await;
    assert_eq!(runtime.ok(id, "admit", &attempt).await, rejected);
    assert_eq!(
        runtime
            .ok(
                id,
                "link_status",
                &json!({"link_id": id, "admin": input["admin"]})
            )
            .await["uses"],
        1
    );

    let other = creation();
    let other_id = other["link_id"].as_str().unwrap();
    runtime.ok(other_id, "create", &other).await;
    attempt["link_id"] = json!(other_id);
    attempt["operation_id"] = operation;
    let other_request = runtime.ok(other_id, "admit", &attempt).await;
    assert_eq!(other_request["result"]["kind"], "accepted");
    assert_ne!(
        other_request["result"]["request_id"],
        accepted["result"]["request_id"]
    );

    let race_link = creation();
    let id = race_link["link_id"].as_str().unwrap();
    runtime.ok(id, "create", &race_link).await;
    let a = json!({"version": 1, "link_id": id, "operation_id": ghinvite_core::RequestId::new(), "requester_id": 31});
    let b = json!({"version": 1, "link_id": id, "operation_id": ghinvite_core::RequestId::new(), "requester_id": 32});
    let (a, b) = tokio::join!(runtime.ok(id, "admit", &a), runtime.ok(id, "admit", &b));
    let outcomes = [a, b];
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| o["result"]["kind"] == "accepted")
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| o["result"]["reason"] == "exhausted")
            .count(),
        1
    );
    assert_eq!(
        runtime
            .ok(
                id,
                "link_status",
                &json!({"link_id": id, "admin": race_link["admin"]})
            )
            .await["uses"],
        1
    );
}
